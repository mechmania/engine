use std::sync::atomic::{ AtomicU8, Ordering };
use std::path::Path;
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

#[repr(C)]
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
pub struct LockedStage<'a> {
    inner: &'a mut Shm
}

impl<'a> Deref for LockedStage<'a> {
    type Target = ShmStage;
    fn deref(&self) -> &Self::Target {
        &self.inner.stage
    }
}

impl<'a> DerefMut for LockedStage<'a> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner.stage
    }
}

impl Drop for LockedStage<'_> {
    fn drop(&mut self) {
        self.inner.sync.store(ENGINE_READY, Ordering::Release);
    }
}

#[repr(C)]
struct Shm {
    stage:     ShmStage,
    sync:      AtomicU8,
    msg:       [u8; TELEMETRY_SZ - 1],
    msg_sync:  AtomicU8
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
        ret.shm().sync.store(ENGINE_BUSY, Ordering::Release);
        return ret;
    }
    
    pub fn backing_file_path<'a>(&'a self) -> &'a Path {
        self.bkgfd.path()
    }

    fn shm<'a>(&'a self) -> &'a mut Shm { // TODO worry about validation
        unsafe {
            &mut *(self.mmap.as_ptr() as *mut Shm)
        }
    }

    pub async fn lock(&self) -> LockedStage {
        for i in 0.. {
            if self.shm().sync.compare_exchange_weak(
                ENGINE_READY, 
                ENGINE_BUSY, 
                Ordering::Acquire, 
                Ordering::Relaxed
            ).is_ok() {
                return LockedStage { inner: self.shm() };
            }
        
            match i {
                0..100 => hint::spin_loop(),
                100..1000 => tokio::task::yield_now().await,
                _ => tokio::time::sleep(Duration::from_micros(100)).await,
            }
        }

        unreachable!()
    }
}

impl Drop for BotChannel {
    fn drop(&mut self) {
        self.shm().sync.store(ENGINE_FINISHED, Ordering::Release);
    }
}
