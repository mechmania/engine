use anyhow::Context;
use memmap::MmapMut;
use pastey::paste;
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
use tokio::time;

use crate::game::{GameConfig, GameState, InitPosition, PaddleVelocity};

#[repr(u8)]
pub enum EngineStatus {
    Ready = 0,
    Busy = 1,
    Finished = 2,
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

        pub trait Protocol {
            const ID: ProtocolId;
            const TIMEOUT_FACTOR: u32;
            type Msg;
            type Response;
            fn msg_into_enum(msg: Self::Msg) -> ProtocolUnion;
            fn response_into_enum(response: Self::Response) -> ProtocolUnion;
            fn enum_into_msg(variant: ProtocolUnion) -> Self::Msg;
            fn enum_into_response(variant: ProtocolUnion) -> Self::Response;
            fn msg_discriminant() -> u8 {
                return Self::ID as u8 * 2;
            }
            fn response_discriminant() -> u8 {
                return Self::ID as u8 * 2 + 1;
            }
        }

        paste! {
            $(
                pub struct [<$name Protocol>];

                impl Protocol for [<$name Protocol>] {
                    const ID: ProtocolId    = ProtocolId::$name;
                    const TIMEOUT_FACTOR: u32 = $timeout;
                    type Msg = $msg;
                    type Response = $resp;
                    fn msg_into_enum(msg: Self::Msg) -> ProtocolUnion {
                        ProtocolUnion::[<$name Msg>](msg)
                    }
                    fn response_into_enum(response: Self::Response) -> ProtocolUnion {
                        ProtocolUnion::[<$name Response>](response)
                    }
                    fn enum_into_msg(variant: ProtocolUnion) -> Self::Msg {
                        if let ProtocolUnion::[<$name Msg>](ret) = variant {
                            ret
                        } else {
                            panic!()
                        }
                    }
                    fn enum_into_response(variant: ProtocolUnion) -> Self::Response {
                        if let ProtocolUnion::[<$name Response>](ret) = variant {
                            ret
                        } else {
                            panic!()
                        }
                    }
                }
            )*
            #[derive(Clone)]
            pub enum ProtocolUnion {
                $(
                    [<$name Msg>]($msg),
                    [<$name Response>]($resp),
                )*
            }

            pub struct Strategy {
                $(
                    pub [<on_ $name:lower>]: Box<dyn Fn(&$msg) -> $resp>,
                )*
            }
            
            impl Strategy {
                pub fn handle_msg(&self, msg: &ProtocolUnion) -> ProtocolUnion {
                    match msg {
                        $(
                            ProtocolUnion::[<$name Msg>]([<$msg:lower>]) => ProtocolUnion::[<$name Response>]((self.[<on_ $name:lower>])([<$msg:lower>])),
                        )*
                        _ => panic!("engine sent invalid message")
                    }
                }
            }
        }
    };
}

define_protocols! {
    Init: (GameConfig, InitPosition, 1000),
    Tick: (GameState, PaddleVelocity, 20)
}

#[derive(Error, Debug)]
#[repr(C, u8)]
pub enum ResponseError {
    #[error("memory misaligned at 0x{address:x}, expecting alignment of 0x{alignment:x}")]
    AlignmentError { address: usize, alignment: usize } = 0,
    #[error("malformed response")]
    Malformed = 1,
    #[error("size mismatch (expected {expected}, actual {actual})")]
    SizeMismatch { expected: usize, actual: usize } = 2,
    #[error("response timed out")]
    Timeout(#[from] time::error::Elapsed) = 3,
}

pub type ResponseResult<T> = Result<T, ResponseError>;

#[repr(C)]
struct Shm {
    sync: AtomicU8,
    union: ProtocolUnion,
}

async fn poll(au8: &AtomicU8, cmp: u8) {
    for i in 0.. {
        if au8.load(Ordering::Acquire) == cmp {
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

    pub async fn msg<T: Protocol>(&self, msg: &T::Msg, engine_time: Duration) -> ResponseResult<T::Response> 
        where <T as Protocol>::Msg : Clone 
    {
        let ptr = self.mmap.as_ptr();
        let sync = deref_sync(&self.mmap);

        if sync.load(Ordering::Acquire) != EngineStatus::Busy as u8 {
            return Err(ResponseError::Malformed);
        }

        unsafe {
            std::ptr::copy_nonoverlapping(
                &T::msg_into_enum(msg.clone()) as *const ProtocolUnion,
                ptr.add(offset_of!(Shm, union)) as *mut ProtocolUnion,
                1
            )
        }

        sync.store(EngineStatus::Ready as u8, Ordering::Release);

        time::timeout(engine_time * T::TIMEOUT_FACTOR, poll(
            sync, 
            EngineStatus::Busy as u8
        )).await.map_err(|e| {
            sync.store(EngineStatus::Busy as u8, Ordering::Release);
            e
        })?;

        let addr = ptr as usize;
        let len = self.mmap.len();
        let align = align_of::<Shm>();

        if len != size_of::<Shm>() {
            return Err(ResponseError::SizeMismatch {
                expected: size_of::<Shm>(),
                actual: len,
            });
        }
        if addr % align != 0 {
            return Err(ResponseError::AlignmentError {
                address: addr,
                alignment: align,
            });
        }

        let discriminant = self.mmap[offset_of!(Shm, union)];
        if discriminant != T::response_discriminant() {
            return Err(ResponseError::Malformed);
        }

        let union = unsafe { &*(ptr.add(offset_of!(Shm, union)) as *const ProtocolUnion) };
        Ok(T::enum_into_response(union.clone()))
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

    pub async fn handle_msg(&self, strategy: &Strategy) {
        let sync = deref_sync(&self.mmap);
        poll( // TODO handle engine finish
            sync, 
            EngineStatus::Ready as u8
        ).await;

        // safe to deref because engine is trusted
        let msg = unsafe { &mut* (self.mmap.as_ptr().add(offset_of!(Shm, union)) as *mut ProtocolUnion) };
        let response = strategy.handle_msg(msg);
        *msg = response;

        sync.store(EngineStatus::Busy as u8, Ordering::Release);
    }
}

