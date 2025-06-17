use anyhow::Context;
use paste::paste;
use memmap::MmapMut;
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

use crate::game::{GameConfig, GameState, InitPosition, PaddleVelocity};

#[repr(u8)]
pub enum EngineStatus {
    Ready    = 0,
    Busy     = 1,
    Finished = 2,
}

pub const TELEMETRY_SZ: usize = 1024;
pub const TELEMETRY_OLD: u8 = 0;
pub const TELEMETRY_NEW: u8 = 1;

macro_rules! define_protocols {
    (
        $(
            $name:ident: ($msg:ty, $resp:ty)
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
            type Msg;
            type Response;
        }
        paste! {
            $(
                pub struct [<$name Protocol>];

                impl Protocol for [<$name Protocol>] {
                    const ID: ProtocolId = ProtocolId::$name;
                    type Msg = $msg;
                    type Response = $resp;
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
    Init: (GameConfig, InitPosition),
    Tick: (GameState, PaddleVelocity)
}

#[derive(Error, Debug)]
pub enum DerefError {
    #[error("memory misaligned at 0x{address:x}, expecting alignment of 0x{alignment:x}")]
    AlignmentError { address: usize, alignment: usize },
    #[error("invalid enum discriminant {0}")]
    InvalidDiscriminant(u8),
    #[error("size mismatch (expected {expected}, actual {actual})")]
    SizeMismatch { expected: usize, actual: usize },
}

pub type DerefResult<T> = Result<T, DerefError>;

#[repr(C)]
struct Shm {
    sync: AtomicU8,
    union: ProtocolUnion,
    msg: [u8; TELEMETRY_SZ - 1],
    msg_sync: AtomicU8,
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
fn deref_sync<'a>(mmap: &'a MmapMut) -> &'a AtomicU8 {
    unsafe { &*(mmap.as_ptr().add(offset_of!(Shm, sync)) as *const AtomicU8) }
}

fn try_deref<'a>(mmap: &MmapMut) -> DerefResult<&'a mut Shm> {
    let addr = mmap.as_ptr() as usize;
    let len = mmap.deref().len();
    let align = align_of::<Shm>();

    if addr % align != 0 {
        return Err(DerefError::AlignmentError {
            address: addr,
            alignment: align,
        });
    }
    if len != size_of::<Shm>() {
        return Err(DerefError::SizeMismatch {
            expected: size_of::<Shm>(),
            actual: len,
        });
    }

    let discriminant = mmap.deref()[std::mem::offset_of!(Shm, stage)];
    if !(0..2).contains(&discriminant) {
        // TODO make this more enum agnostic
        return Err(DerefError::InvalidDiscriminant(discriminant));
    }

    Ok(unsafe { &mut *(mmap.as_ptr() as *mut Shm) })
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

    pub async fn msg(&self) -> MsgResult<

    pub async fn lock(&self) -> DerefResult<LockedStage<ENGINE_READY>> {
        cas_poll(deref_sync(&self.mmap), EngineStatus::Ready as u8, EngineStatus::Busy as u8).await;
        Ok(LockedStage {
            inner: try_deref(&self.mmap)?,
        })
    }
}

impl Drop for BotChannel {
    fn drop(&mut self) {
        deref_sync(&self.mmap).store(ENGINE_FINISHED, Ordering::Release);
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
            mmap: unsafe { MmapMut::map_mut(&file).with_context(|| "unable to memory map backing file")? },
        })
    }

    pub async fn lock(&self) -> DerefResult<LockedStage<ENGINE_BUSY>> {
        cas_poll(deref_sync(&self.mmap), ENGINE_BUSY, ENGINE_READY).await;
        Ok(LockedStage {
            inner: try_deref(&self.mmap)?,
        })
    }
}
