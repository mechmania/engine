use anyhow::Context;
use memmap::MmapMut;
use std::{
    fs::OpenOptions,
    mem::offset_of,
    ops::Drop,
    path::Path,
    sync::atomic::{AtomicU8, Ordering},
    time::Duration,
};
use crate::game::{
    config::GameConfig, state::{ FleetAction, GameState }, team::Team
};
use thiserror::Error;
use tokio::time;

#[repr(u8)]
pub enum Handoff {
    BotTurn = 0,
    EngineTurn = 1,
    Closed = 2,
}

#[derive(Clone)]
#[repr(C)]
pub struct HandshakeRequest{
    pub team: Team,
    pub config: GameConfig
}

// very high security we hardcode the magic number into our source code
pub const HANDSHAKE_MAGIC: u64 = 0xabe119c019aaffcc;

mm_macros::protocols! {
    Handshake: (HandshakeRequest, u64), // TODO handshake should really not be its own protocol
    Tick: (GameState, FleetAction),
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
pub(in super) struct SharedBlock {
    pub handoff: AtomicU8,
    // written by the bot, read by the engine, both synchronized by `handoff`'s release/acquire
    // exchange (same pattern as `frame` below) — the bot's self-measured CPU time (see
    // `BotChannel::handle_request`) spent producing the response currently in `frame`.
    pub cpu_time_nanos: u64,
    pub frame: Frame,
}

#[inline(never)]
pub(in super) async fn await_handoff(handoff: &AtomicU8, until: u8) {
    for i in 0.. {
        if handoff.load(Ordering::Acquire) == until {
            return;
        }
        match i {
            0..1000 => std::thread::yield_now(),
            _ => tokio::time::sleep(Duration::from_micros(i / 10)).await,
        }
    }
}

// safe because we only grab one byte
#[inline]
pub(in super) fn handoff_byte<'a>(mmap: &'a [u8]) -> &'a AtomicU8 {
    unsafe { &*(mmap.as_ptr().add(offset_of!(SharedBlock, handoff)) as *const AtomicU8) }
}

pub struct EngineChannel {
    bkgfd: tempfile::NamedTempFile,
    mmap: MmapMut,
}

impl EngineChannel {
    pub fn new() -> anyhow::Result<Self> {
        let tf = tempfile::NamedTempFile::new()
            .with_context(|| "unable to create backing file for bot channel")?;
        tf.as_file()
            .set_len(std::mem::size_of::<SharedBlock>() as u64)
            .with_context(|| "unable to set backing file length")?;
        let mmap = unsafe {
            MmapMut::map_mut(tf.as_file()).with_context(|| "unable to memory map backing file")?
        };
        let ret = Self { bkgfd: tf, mmap };
        handoff_byte(&ret.mmap).store(Handoff::EngineTurn as u8, Ordering::Release);
        Ok(ret)
    }

    pub fn backing_file_path<'a>(&'a self) -> &'a Path {
        self.bkgfd.path()
    }

    /// Sends `request` and awaits the bot's response, subject to a wall-clock `timeout` (a hang
    /// safety net — see `crate::timing::TICK_HANG_TIMEOUT`). Returns the response alongside
    /// the CPU time the bot itself reported spending to produce it.
    pub async fn request<T: Protocol>(&self, request: &T::Request, timeout: Duration) -> ResponseResult<(T::Response, Duration)>
        where <T as Protocol>::Request : Clone
    {
        let ptr = self.mmap.as_ptr();
        let handoff = handoff_byte(&self.mmap);

        if handoff.load(Ordering::Acquire) != Handoff::EngineTurn as u8 {
            return Err(ResponseError::Malformed);
        }

        unsafe {
            std::ptr::copy_nonoverlapping(
                &T::request_into_frame(request.clone()) as *const Frame,
                ptr.add(offset_of!(SharedBlock, frame)) as *mut Frame,
                1
            )
        }

        handoff.store(Handoff::BotTurn as u8, Ordering::Release);

        time::timeout(timeout, await_handoff(
            handoff,
            Handoff::EngineTurn as u8
        )).await.map_err(|e| {
            handoff.store(Handoff::EngineTurn as u8, Ordering::Release);
            e
        })?;

        let addr = ptr as usize;
        let len = self.mmap.len();
        let align = align_of::<SharedBlock>();

        if len != size_of::<SharedBlock>() {
            return Err(ResponseError::SizeMismatch {
                expected: size_of::<SharedBlock>(),
                actual: len,
            });
        }
        if addr % align != 0 {
            return Err(ResponseError::AlignmentError {
                address: addr,
                alignment: align,
            });
        }

        let tag = self.mmap[offset_of!(SharedBlock, frame)];
        if tag != T::response_tag() {
            return Err(ResponseError::Malformed);
        }

        let frame = unsafe { &*(ptr.add(offset_of!(SharedBlock, frame)) as *const Frame) };
        let cpu_time_nanos = unsafe { *(ptr.add(offset_of!(SharedBlock, cpu_time_nanos)) as *const u64) };
        Ok((T::frame_into_response(frame.clone()), Duration::from_nanos(cpu_time_nanos)))
    }
}

impl Drop for EngineChannel {
    fn drop(&mut self) {
        handoff_byte(&self.mmap).store(Handoff::Closed as u8, Ordering::Release);
    }
}

pub struct BotChannel {
    pub mmap: MmapMut,
}

impl BotChannel {
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


    pub async fn handle_request(&self, handlers: &Handlers) {
        let handoff = handoff_byte(&self.mmap);
        await_handoff( // TODO handle engine finish
            handoff,
            Handoff::BotTurn as u8
        ).await;

        // safe to deref because engine is trusted
        let frame = unsafe { &mut* (self.mmap.as_ptr().add(offset_of!(SharedBlock, frame)) as *mut Frame) };

        let cpu_start = cpu_time::ProcessTime::now();
        let response = handlers.respond(frame);
        let cpu_time = cpu_start.elapsed();

        *frame = response;

        let cpu_time_nanos = unsafe { &mut *(self.mmap.as_ptr().add(offset_of!(SharedBlock, cpu_time_nanos)) as *mut u64) };
        *cpu_time_nanos = cpu_time.as_nanos() as u64;

        handoff.store(Handoff::EngineTurn as u8, Ordering::Release);
    }
}

#[cfg(feature = "engine")]
#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::{
        config::{BotConfig, Deposit, PayloadConfig, DEPOSITS_MAX, MAP, PAYLOAD_PATH},
        team::Team,
    };

    // Exercises the `SharedBlock::cpu_time_nanos` plumbing end-to-end (bot side reports its own
    // CPU time in `BotChannel::handle_request`, engine side reads it back in
    // `EngineChannel::request`) without needing a real separate bot process — a background task
    // plays the "bot" role over the same backing file.
    #[tokio::test]
    async fn reports_bot_cpu_time() {
        let engine_channel = EngineChannel::new().unwrap();
        let bot_channel = BotChannel::from_path(engine_channel.backing_file_path()).unwrap();

        let handlers = Handlers {
            on_handshake: Box::new(|_request| {
                // burn a known amount of CPU so the reported time is unambiguously nonzero
                let start = cpu_time::ProcessTime::now();
                while start.elapsed() < Duration::from_millis(20) {}
                HANDSHAKE_MAGIC
            }),
            on_tick: Box::new(|_request| Default::default()),
        };

        let config = GameConfig {
            max_ticks: 1,
            bot: BotConfig {
                radius: 1.0,
                base_speed: 1.0,
                base_health: 1.0,
                base_turn_speed: 1.0,
                base_blaster_cooldown: 1,
                base_invulnerability_ticks: 1,
                base_blaster_range: 1.0,
                base_blaster_damage: 1.0,
                base_blaster_splash_radius: 1.0,
            },
            payload: PayloadConfig {
                radius: 1.0,
                capture_radius: 3.0,
                speed_per_bot: 0.01,
                max_speed: 0.04,
                contest_diff: 1,
            },
            payload_path: PAYLOAD_PATH,
            deposit_count: 0,
            deposits: [Deposit::default(); DEPOSITS_MAX],
            map: MAP,
        };

        let handshake_request = HandshakeRequest { team: Team::A, config };

        // both sides run on the current task concurrently, mirroring the real protocol's
        // request/response exchange over the shared `handoff` byte
        let (_, res) = tokio::join!(
            bot_channel.handle_request(&handlers),
            engine_channel.request::<HandshakeProtocol>(&handshake_request, Duration::from_secs(5)),
        );
        let (response, cpu_time) = res.unwrap();

        assert_eq!(response, HANDSHAKE_MAGIC);
        assert!(cpu_time >= Duration::from_millis(10), "reported cpu_time was implausibly small: {cpu_time:?}");
    }
}

