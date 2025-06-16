use std::sync::atomic::{ AtomicU8, Ordering };
use std::path::Path;
use std::fs::OpenOptions;
use std::time::{ Duration, SystemTime };
use std::ops::{ Deref, DerefMut, Drop };
use std::hint;
use memmap::MmapMut;
use crate::game::{
    GameState, 
    GameConfig,
    TickAction,
    InitAction,
};

pub const ENGINE_READY:    u8 = 0;
pub const ENGINE_BUSY:     u8 = 1;
pub const ENGINE_FINISHED: u8 = 2;

pub const TELEMETRY_SZ:  usize = 1024;
pub const TELEMETRY_OLD: u8 = 0;
pub const TELEMETRY_NEW: u8 = 1;

#[derive(Debug)]
pub enum DerefError {
    AlignmentError {
        address: usize,
        alignment: usize,
    },
    InvalidDiscriminant(u8),
    SizeMismatch {
        expected: usize,
        actual:   usize,
    }
}

pub type DerefResult<T> = Result<T, DerefError>;

#[repr(C, u8)] 
pub enum ShmStage {
    Init {
        config:     GameConfig,
        action:     InitAction,
    },
    Tick {
        state:      GameState,
        action:     TickAction,
    }
}

#[repr(transparent)]
pub struct LockedStage<'a, const R: u8> {
    inner: &'a mut Shm
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
    stage:     ShmStage,
    sync:      AtomicU8,
    msg:       [u8; TELEMETRY_SZ - 1],
    msg_sync:  AtomicU8
}

async fn cas_poll(au8: &AtomicU8, cmp: u8, swp: u8) {
    for i in 0.. {
        if au8.compare_exchange_weak(
            cmp,
            swp,
            Ordering::Acquire,
            Ordering::Relaxed
        ).is_ok() {
            return;
        }
        match i {
            0..100 => hint::spin_loop(),
            100..1000 => tokio::task::yield_now().await,
            _ => tokio::time::sleep(Duration::from_micros(i / 10)).await,
        }
    }
}

fn try_deref<'a>(mmap: &MmapMut) -> DerefResult<&'a mut Shm> {
    let addr = mmap.as_ptr() as usize;
    let len  = mmap.deref().len();
    let align = align_of::<Shm>();  
    if addr % align != 0 {
        return Err(DerefError::AlignmentError { address: addr, alignment: align });
    }
    if len != size_of::<Shm>() {
        return Err(DerefError::SizeMismatch { expected: size_of::<Shm>(), actual: len });
    }

    if (mmap.as_ptr() as usize) % align_of::<Shm>() != 0 {
        return Err(DerefError::AlignmentError {
            address:    mmap.as_ptr() as usize,
            alignment:  align_of::<Shm>()
        })
    }

    let discriminant = mmap.deref()[std::mem::offset_of!(Shm, stage)];
    if (0..2).contains(&discriminant) { // TODO make this more enum agnostic
        return Err(DerefError::InvalidDiscriminant(discriminant));
    }

    Ok(unsafe {
        &mut *(mmap.as_ptr() as *mut Shm)
    })
}

pub struct BotChannel {
    bkgfd:      tempfile::NamedTempFile,
    mmap:       MmapMut,
}

impl BotChannel {
    pub fn new() -> Self {
        let tf = tempfile::NamedTempFile::new().unwrap();
        tf.as_file().set_len(std::mem::size_of::<Shm>() as u64).unwrap();
        let mmap = unsafe { MmapMut::map_mut(tf.as_file()).unwrap() };
        let ret = Self {
            bkgfd: tf,
            mmap
        };
        try_deref(&ret.mmap).unwrap().sync.store(ENGINE_BUSY, Ordering::Release);
        return ret;
    }
    
    pub fn backing_file_path<'a>(&'a self) -> &'a Path {
        self.bkgfd.path()
    }

    pub async fn lock(&self) -> DerefResult<LockedStage<ENGINE_READY>> {
        cas_poll(&self.shm().sync, ENGINE_READY, ENGINE_BUSY).await;
        Ok(LockedStage { inner: try_deref(&self.mmap)? })
    }
}

impl Drop for BotChannel {
    fn drop(&mut self) {
        self.shm().sync.store(ENGINE_FINISHED, Ordering::Release);
    }
}

pub struct EngineChannel {
    mmap: MmapMut
}

impl EngineChannel {
    pub fn from_path<P: AsRef<Path>>(path: P) -> Self {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .unwrap();

        Self {
            mmap: unsafe { MmapMut::map_mut(&file).unwrap() }
        }
    }

    fn shm<'a>(&'a self) -> &'a mut Shm { // TODO worry about validation
        unsafe {
            &mut *(self.mmap.as_ptr() as *mut Shm)
        }
    }

    pub async fn lock(&self) -> LockedStage<ENGINE_BUSY> {
        cas_poll(&self.shm().sync, ENGINE_BUSY, ENGINE_READY).await;
        LockedStage { inner: self.shm() }
    }
}
