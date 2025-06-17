use anyhow::Context;
use memmap::MmapMut;
use std::{
    fs::OpenOptions,
    hint,
    mem::offset_of,
    ops::{Deref, DerefMut, Drop},
    path::Path,
    sync::atomic::{AtomicU8, Ordering},
    time::Duration,
};
use thiserror::Error;

use crate::game::{GameConfig, GameState, InitAction, TickAction};

pub const ENGINE_READY: u8 = 0;
pub const ENGINE_BUSY: u8 = 1;
pub const ENGINE_FINISHED: u8 = 2;

pub const TELEMETRY_SZ: usize = 1024;
pub const TELEMETRY_OLD: u8 = 0;
pub const TELEMETRY_NEW: u8 = 1;

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

#[repr(C, u8)]
pub enum ShmStage {
    Init {
        config: GameConfig,
        action: InitAction,
    },
    Tick {
        state: GameState,
        action: TickAction,
    },
}

#[repr(transparent)]
pub struct LockedStage<'a, const R: u8> {
    inner: &'a mut Shm,
}

impl<'a, const R: u8> Deref for LockedStage<'a, R> {
    type Target = ShmStage;
    fn deref(&self) -> &Self::Target {
        &self.inner.stage
    }
}

impl<'a, const R: u8> DerefMut for LockedStage<'a, R> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner.stage
    }
}

impl<const R: u8> Drop for LockedStage<'_, R> {
    fn drop(&mut self) {
        self.inner.sync.store(R, Ordering::Release);
    }
}

#[repr(C)]
struct Shm {
    stage: ShmStage,
    sync: AtomicU8,
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
        deref_sync(&ret.mmap).store(ENGINE_BUSY, Ordering::Release);
        Ok(ret)
    }

    pub fn backing_file_path<'a>(&'a self) -> &'a Path {
        self.bkgfd.path()
    }

    pub async fn lock(&self) -> DerefResult<LockedStage<ENGINE_READY>> {
        cas_poll(deref_sync(&self.mmap), ENGINE_READY, ENGINE_BUSY).await;
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
