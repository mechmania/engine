use anyhow::Context;
use memmap::MmapMut;
use paste::paste;
use std::{
    fs::OpenOptions,
    hint,
    mem::offset_of,
    ops::Drop,
    path::Path,
    sync::atomic::{AtomicU8, Ordering},
    time::Duration,
};
use thiserror::Error;
use tokio::time::timeout;

use crate::game::{GameConfig, GameState, InitPosition, PaddleVelocity};

#[repr(u8)]
pub enum EngineStatus {
    Ready = 0,
    Busy = 1,
    Finished = 2,
}

macro_rules! count {
    () => (0usize);
    ($head:tt $($tail:tt)*) => (1usize + count!($($tail)*));
}

macro_rules! define_protocols {
    (
        $(
            $name:ident: ($msg:ty, $resp:ty, $timeout:expr)
        ),* $(,)?
    ) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        #[repr(u8)]
        pub enum ProtocolId {
            $(
                $name,
            )*
        }

        pub const PROTOCOL_COUNT: usize = count!($($name)*);

        pub trait Protocol {
            const ID: ProtocolId;
            const TIMEOUT: Duration;
            type Msg;
            type Response;
            fn msg_into_enum(msg: Self::Msg) -> ProtocolUnion;
            fn response_into_enum(response: Self::Response) -> ProtocolUnion;
        }
        paste! {
            $(
                pub struct [<$name Protocol>];

                impl Protocol for [<$name Protocol>] {
                    const ID: ProtocolId    = ProtocolId::$name;
                    const TIMEOUT: Duration = $timeout;
                    type Msg = $msg;
                    type Response = $resp;
                    fn msg_into_enum(msg: Self::Msg) -> ProtocolUnion {
                        ProtocolUnion::[<$name Msg>](msg)
                    }
                    fn response_into_enum(response: Self::Response) -> ProtocolUnion {
                        ProtocolUnion::[<$name Response>](response)
                    }
                }
            )*
            pub enum ProtocolUnion {
                $(
                    [<$name Msg>]($msg),
                    [<$name Response>]($resp),
                )*
            }
            impl ProtocolUnion {
                pub fn get_id(&self) -> ProtocolId {
                    match self {
                        $(
                            Self::[<$name Msg>](_) => ProtocolId::$name,
                            Self::[<$name Response>](_) => ProtocolId::$name,
                        )*
                    }
                }

                pub fn is_message(&self) -> bool {
                    match self {
                        $(
                            Self::[<$name Msg>](_) => true,
                            Self::[<$name Response>](_) => false,
                        )*
                    }
                }

                pub fn is_response(&self) -> bool {
                    match self {
                        $(
                            Self::[<$name Msg>](_) => false,
                            Self::[<$name Response>](_) => true,
                        )*
                    }
                }
            }
        }
    };
}

define_protocols! {
    Init: (GameConfig, InitPosition, Duration::from_millis(1000)),
    Tick: (GameState, PaddleVelocity, Duration::from_millis(20))
}

#[derive(Error, Debug)]
#[repr(C, u8)]
pub enum ResponseError {
    #[error("memory misaligned at 0x{address:x}, expecting alignment of 0x{alignment:x}")]
    AlignmentError { address: usize, alignment: usize } = 0,
    #[error("invalid enum discriminant {0}")]
    InvalidDiscriminant(u8) = 1,
    #[error("size mismatch (expected {expected}, actual {actual})")]
    SizeMismatch { expected: usize, actual: usize } = 2,
}

pub type ResponseResult<T> = Result<T, ResponseError>;

#[repr(C)]
struct Shm {
    sync: AtomicU8,
    union: ProtocolUnion,
}

async fn cas_poll(au8: &AtomicU8, cmp: u8, swp: u8) {
    for i in 0.. {
        if au8
            .compare_exchange_weak(cmp, swp, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
        {
            return;
        }
        match i {
            0..100 => hint::spin_loop(),
            100..1000 => tokio::task::yield_now().await,
            _ => tokio::time::sleep(Duration::from_micros(i / 10)).await,
        }
    }
}

// safe because we only grab one byte
#[inline]
fn deref_sync<'a>(mmap: &'a [u8]) -> &'a AtomicU8 {
    unsafe { &*(mmap.as_ptr().add(offset_of!(Shm, sync)) as *const AtomicU8) }
}

//fn try_deref<'a>(mmap: &'a [u8]) -> ResponseResult<&'a mut Shm> {
//    let addr = mmap.as_ptr() as usize;
//    let len = mmap.deref().len();
//    let align = align_of::<Shm>();
//
//    if addr % align != 0 {
//        return Err(ResponseError::AlignmentError {
//            address: addr,
//            alignment: align,
//        });
//    }
//    if len != size_of::<Shm>() {
//        return Err(ResponseError::SizeMismatch {
//            expected: size_of::<Shm>(),
//            actual: len,
//        });
//    }
//
//    let discriminant = mmap.deref()[std::mem::offset_of!(Shm, stage)];
//    if !(0..2).contains(&discriminant) {
//        // TODO make this more enum agnostic
//        return Err(ResponseError::InvalidDiscriminant(discriminant));
//    }
//
//    Ok(unsafe { &mut *(mmap.as_ptr() as *mut Shm) })
//}

pub struct BotChannel {
    bkgfd: tempfile::NamedTempFile,
    mmap: MmapMut,
}

impl BotChannel {
    pub fn new() -> anyhow::Result<Self> {
        let tf = tempfile::NamedTempFile::new()
            .with_context(|| "unable to create backing file for bot channel")?;
        tf.as_file()
            .set_len(std::mem::size_of::<Shm>() as u64)
            .with_context(|| "unable to set backing file length")?;
        let mmap = unsafe {
            MmapMut::map_mut(tf.as_file()).with_context(|| "unable to memory map backing file")?
        };
        let ret = Self { bkgfd: tf, mmap };
        deref_sync(&ret.mmap).store(EngineStatus::Busy as u8, Ordering::Release);
        Ok(ret)
    }

    pub fn backing_file_path<'a>(&'a self) -> &'a Path {
        self.bkgfd.path()
    }

    pub async fn msg<T: Protocol>(&self, msg: T::Msg) -> ResponseResult<T::Response> {
        let ptr = self.mmap.as_ptr();
        let sync = deref_sync(&self.mmap);

        assert_eq!(sync.load(Ordering::Acquire), EngineStatus::Busy as u8);

        unsafe {
            std::ptr::copy_nonoverlapping(
                &T::msg_into_enum(msg) as *const ProtocolUnion,
                ptr.add(offset_of!(Shm, union)) as *mut ProtocolUnion,
                1
            )
        }

        sync.store(EngineStatus::Ready as u8, Ordering::Release);

        let response = timeout(T::TIMEOUT, cas_poll(
            sync, 
            EngineStatus::Busy as u8,
            EngineStatus::Ready as u8
        )).await;

        todo!()
    }
}

impl Drop for BotChannel {
    fn drop(&mut self) {
        deref_sync(&self.mmap).store(EngineStatus::Finished as u8, Ordering::Release);
    }
}

pub struct EngineChannel {
    mmap: MmapMut,
}

impl EngineChannel {
    pub fn from_path<P: AsRef<Path>>(path: P) -> anyhow::Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .with_context(|| "unable to open backing file for engine channel")?;

        Ok(Self {
            mmap: unsafe {
                MmapMut::map_mut(&file).with_context(|| "unable to memory map backing file")?
            },
        })
    }

    pub async fn lock(&self) -> ResponseResult<LockedStage<ENGINE_BUSY>> {
        cas_poll(deref_sync(&self.mmap), ENGINE_BUSY, ENGINE_READY).await;
        Ok(LockedStage {
            inner: try_deref(&self.mmap)?,
        })
    }
}

