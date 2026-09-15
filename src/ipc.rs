use anyhow::Context;
use memmap::MmapMut;
use std::{
    cell::Cell,
    fs::OpenOptions,
    mem::{offset_of, size_of},
    ops::Drop,
    path::Path,
    sync::OnceLock,
    sync::atomic::{AtomicU8, Ordering},
    time::{Duration, Instant},
};
use crate::game::{
    config::GameConfig, mirror::Validate, state::{ FleetAction, GameState }, team::Team,
    topology::init_topology,
};
use thiserror::Error;
use tokio::time;

#[repr(u8)]
pub enum Handoff {
    BotTurn = 0,
    EngineTurn = 1,
    Closed = 2,
}

#[derive(Clone, mm_macros::FfiMirror)]
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

/// What a bot answers the handshake with, and what the engine checks it against.
///
/// The magic number alone only ever proved "a real handshake happened". Mixed with
/// [`Frame::LAYOUT_HASH`] it also proves the two processes agree about the *layout* of
/// everything that crosses the channel -- which is the failure a competitor actually hits,
/// because `mm-cli` pins one `mm-engine` rev and a starterpack pins another. A mismatch is
/// a clean fatal error at tick 0 instead of a match spent reading garbage.
///
/// The magic stays as the seed so a zero, or any other value a process that never wrote one
/// leaves behind, still fails.
pub const HANDSHAKE_FINGERPRINT: u64 =
    crate::game::mirror::mix(HANDSHAKE_MAGIC, Frame::LAYOUT_HASH);

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
    /// The bot's response was not a valid value of the type it claimed to be -- an
    /// out-of-range tag, or a `bool` that is neither 0 nor 1. Distinct from `Malformed`
    /// (a wrong handoff byte, a wrong frame tag, a channel closed mid-request) because it
    /// is the one error that is purely the bot's own doing, and the only one that costs it
    /// a single tick rather than its whole compute budget.
    #[error("bot sent an invalid action")]
    InvalidAction = 4,
}

impl ResponseError {
    /// Does this cost the bot its whole compute bank, or only the tick it spoiled?
    ///
    /// Everything here except an invalid action is a channel that stopped behaving -- a bot
    /// that hung past `TICK_HANG_TIMEOUT`, a mapping that is the wrong size, a frame tag
    /// from nowhere -- and keeps the old all-or-nothing penalty. An invalid action is
    /// different in kind: the bot built a bad struct, it did not run long, and charging it
    /// the bank would end a match over one mistyped byte.
    pub fn forfeits_budget(&self) -> bool {
        !matches!(self, ResponseError::InvalidAction)
    }
}

pub type ResponseResult<T> = Result<T, ResponseError>;

/// The mapping is sized by `Frame`, which is sized by its largest variant -- the handshake,
/// carrying `GameConfig`. Per-message traffic is variant-sized (see `EngineChannel::request`),
/// so this bounds address space rather than throughput, but it is still worth failing loudly
/// on: this is also the size every message memcpys around.
const _: () = assert!(
    size_of::<Frame>() <= 16_384,
    "Frame has outgrown its budget -- check what was added to GameConfig or GameState",
);

#[repr(C)]
pub(in super) struct SharedBlock {
    pub handoff: AtomicU8,
    // written by the bot, read by the engine, both synchronized by `handoff`'s release/acquire
    // exchange (same pattern as `frame` below) — the bot's self-measured CPU time (see
    // `BotChannel::handle_request`) spent producing the response currently in `frame`.
    pub cpu_time_nanos: u64,
    pub frame: Frame,
}

/// How long to spin before handing the thread back to the timer.
///
/// The two sides of a tick are only ~100us apart, but `tokio::time::sleep` cannot resolve
/// anything below its ~1ms timer granularity -- a `sleep(100us)` really costs ~1.2ms. So the
/// sleep path is a ~13x cliff, and the spin window has to be wide enough that a normal
/// handoff never reaches it, or the whole match falls into a 1ms-quantised ping-pong.
///
/// A *count* of spins cannot do that job: 1000 `yield_now()`s is ~94us on one machine and
/// something else entirely on another, so the cliff moves with the hardware and with however
/// much work the engine happens to do between handoffs.
const SPIN_WINDOW: Duration = Duration::from_millis(2);

/// Longest backoff once we are off the spin window and into the timer.
const MAX_BACKOFF: Duration = Duration::from_millis(5);

/// How a wait ended.
///
/// `Closed` is the peer saying the match is over, which is an ordinary event and not a
/// failure -- a bot that treats it as one exits non-zero and reads as a crash in the
/// gamelog. Before this existed both loops below spun forever on a dead channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in super) enum Handed {
    Turn,
    Closed,
}

#[inline(never)]
pub(in super) async fn await_handoff(handoff: &AtomicU8, until: u8) -> Handed {
    // One extra compare against a byte the loop has already loaded -- the hot path is
    // unchanged, and `until` is never `Closed` (nobody waits *for* the end).
    #[inline(always)]
    fn settled(handoff: &AtomicU8, until: u8) -> Option<Handed> {
        match handoff.load(Ordering::Acquire) {
            got if got == until => Some(Handed::Turn),
            got if got == Handoff::Closed as u8 => Some(Handed::Closed),
            _ => None,
        }
    }

    let start = Instant::now();
    loop {
        // `Instant::now` is not free, so only check the clock once per batch of spins
        for _ in 0..64 {
            if let Some(handed) = settled(handoff, until) {
                return handed;
            }
            std::hint::spin_loop();
        }
        if start.elapsed() >= SPIN_WINDOW {
            break;
        }
        std::thread::yield_now();
    }

    // The peer is doing real work (or is gone). Back off on the timer -- every one of these
    // costs ~1ms whatever we ask for, so ramp rather than hammer.
    for i in 1u32.. {
        if let Some(handed) = settled(handoff, until) {
            return handed;
        }
        tokio::time::sleep(MAX_BACKOFF.min(Duration::from_micros(100 * i as u64))).await;
    }
    unreachable!("the backoff loop only exits by returning")
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

    /// Tells the bot the match is over, so it can exit on its own terms.
    ///
    /// `Drop` does the same thing, but a drop happens after the engine has already torn
    /// down everything else -- too late to give the bot a window to notice. Idempotent.
    pub fn close(&self) {
        handoff_byte(&self.mmap).store(Handoff::Closed as u8, Ordering::Release);
    }

    /// Sends `request` and awaits the bot's response, subject to a wall-clock `timeout` (a hang
    /// safety net — see `crate::timing::TICK_HANG_TIMEOUT`). Returns the response alongside
    /// the CPU time the bot itself reported spending to produce it.
    pub async fn request<T: Protocol>(&self, request: &T::Request, timeout: Duration) -> ResponseResult<(T::Response, Duration)> {
        let ptr = self.mmap.as_ptr();
        let handoff = handoff_byte(&self.mmap);

        if handoff.load(Ordering::Acquire) != Handoff::EngineTurn as u8 {
            return Err(ResponseError::Malformed);
        }

        // Copies the payload straight from the caller's value into the mapping: no `Frame`
        // is built and no clone temporary exists. A bitwise copy is exactly right here --
        // a payload that crosses shared memory is `#[repr(C)]` plain data by construction,
        // which is the same assumption the reader on the other side already makes.
        // `Frame` is union-sized, so building one would round every message up to the
        // largest variant -- pure waste in the hot loop, where a tick's payload is the
        // smaller of the two.
        unsafe {
            let frame = ptr.add(offset_of!(SharedBlock, frame)) as *mut u8;
            std::ptr::copy_nonoverlapping(
                request as *const T::Request as *const u8,
                frame.add(Frame::PAYLOAD_OFFSET),
                size_of::<T::Request>(),
            );
            frame.write(T::request_tag());
        }

        handoff.store(Handoff::BotTurn as u8, Ordering::Release);

        let handed = time::timeout(timeout, await_handoff(
            handoff,
            Handoff::EngineTurn as u8
        )).await.map_err(|e| {
            handoff.store(Handoff::EngineTurn as u8, Ordering::Release);
            e
        })?;

        // Only the engine writes `Closed`, and only once it is done with this channel, so
        // seeing it mid-request means someone else is driving the mapping. Reported rather
        // than silently read as a response.
        if handed == Handed::Closed {
            return Err(ResponseError::Malformed);
        }

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

        // Check the bytes before making a value of them. `ptr::read` below materializes a
        // `T::Response` from whatever the bot wrote, and an enum with an undeclared
        // discriminant is an invalid value the instant it exists -- in the *engine's*
        // process, long before `sanitize` or `eval_tick` match on it. A Rust bot cannot
        // produce one; a bot filling the mapping by hand through the FFI can.
        let payload_at = offset_of!(SharedBlock, frame) + Frame::PAYLOAD_OFFSET;
        if !T::Response::validate(&self.mmap[payload_at..payload_at + size_of::<T::Response>()]) {
            return Err(ResponseError::InvalidAction);
        }

        // Same again on the way back: read out the response payload, not the whole frame.
        let response = unsafe {
            let frame = ptr.add(offset_of!(SharedBlock, frame));
            std::ptr::read(frame.add(Frame::PAYLOAD_OFFSET) as *const T::Response)
        };
        let cpu_time_nanos = unsafe { *(ptr.add(offset_of!(SharedBlock, cpu_time_nanos)) as *const u64) };
        Ok((response, Duration::from_nanos(cpu_time_nanos)))
    }
}

impl Drop for EngineChannel {
    /// The backstop for every path that does not call `close` explicitly -- a panic, an
    /// early return, a test dropping the engine end.
    fn drop(&mut self) {
        self.close();
    }
}

/// The `GameConfig` this match was handshaked with.
///
/// A process-global for the same reason `game::topology`'s graph is one: set once at the
/// handshake, read-only for the rest of the match, and wanted by strategy code deep enough
/// down that threading it through every helper would be pure noise.
static CONFIG: OnceLock<GameConfig> = OnceLock::new();

pub fn get_config() -> &'static GameConfig {
    CONFIG.get().expect("get_config() was called before the handshake")
}

/// A bot's end of the channel.
///
/// The three primitives below are the whole bot side of the protocol: `handshake` once,
/// then `await_tick`/`respond` until `await_tick` says the match is over. `ffi.rs` wraps
/// exactly these for Python, so there is one implementation of the protocol rather than
/// one per language.
pub struct BotChannel {
    pub mmap: MmapMut,
    /// Stamped as `await_tick` hands the state over, consumed by `respond`: exactly the
    /// bot's think time, which is what the compute budget is meant to charge for. The
    /// spin/backoff `await_tick` burns waiting for the engine falls outside it.
    ///
    /// `Cell` rather than `&mut self` on `respond`, because the real shared state is the
    /// mapping and that is already behind `&self`. A bot is single-threaded.
    cpu_start: Cell<Option<cpu_time::ProcessTime>>,
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
            cpu_start: Cell::new(None),
        })
    }

    /// Byte offset `offset` into the mapping. Safe to hand out unchecked because the engine
    /// sized the backing file by `SharedBlock` before spawning this process.
    #[inline]
    fn at(&self, offset: usize) -> *mut u8 {
        unsafe { self.mmap.as_ptr().add(offset) as *mut u8 }
    }

    /// The engine's opening message: which team this bot is, and the config for the match.
    ///
    /// Also builds the navigation graph. It is a third of a megabyte and a pure function of
    /// `(map, radius)`, so it never crosses the wire -- every bot builds its own here. That
    /// costs milliseconds, which is why the handshake reports no CPU time and is bounded by
    /// `HANDSHAKE_TIMEOUT` wall clock instead of the compute budget.
    pub async fn handshake(&self) -> anyhow::Result<(u8, GameConfig)> {
        let handoff = handoff_byte(&self.mmap);
        if await_handoff(handoff, Handoff::BotTurn as u8).await == Handed::Closed {
            anyhow::bail!("the engine closed the channel before the handshake");
        }

        // safe to deref because engine is trusted
        let frame = self.at(offset_of!(SharedBlock, frame));

        let tag = unsafe { frame.read() };
        if tag != HandshakeProtocol::request_tag() {
            anyhow::bail!("expected a handshake request, got frame tag {tag}");
        }

        // Payload-sized in and out, never a whole `Frame` by value: the union is sized by
        // its largest variant, so materializing one puts a tick's worth of `GameState` on
        // this bot's stack to move a `u64`.
        let (team, config) = {
            let request = unsafe { &*(frame.add(Frame::PAYLOAD_OFFSET) as *const HandshakeRequest) };
            (request.team, request.config.clone())
        };

        init_topology(&config.map, config.bot.radius);
        // `set` rather than `unwrap`: a second handshake in one process only happens in
        // tests, and the config is identical there anyway.
        let _ = CONFIG.set(config.clone());

        unsafe {
            std::ptr::write(
                frame.add(Frame::PAYLOAD_OFFSET) as *mut u64,
                HANDSHAKE_FINGERPRINT,
            );
            frame.write(HandshakeProtocol::response_tag());
        }
        handoff.store(Handoff::EngineTurn as u8, Ordering::Release);

        Ok((team as u8, config))
    }

    /// Blocks until the engine hands over the next tick, or returns `None` once it has
    /// closed the channel -- the match is over and the bot should exit cleanly.
    ///
    /// The returned state borrows the mapping and is valid until the next call.
    pub async fn await_tick(&self) -> Option<&GameState> {
        let handoff = handoff_byte(&self.mmap);
        if await_handoff(handoff, Handoff::BotTurn as u8).await == Handed::Closed {
            return None;
        }

        let frame = self.at(offset_of!(SharedBlock, frame));
        let tag = unsafe { frame.read() };
        assert_eq!(
            tag,
            TickProtocol::request_tag(),
            "expected a tick request from the engine",
        );
        let state = unsafe { &*(frame.add(Frame::PAYLOAD_OFFSET) as *const GameState) };

        // The last thing before the bot gets the wheel: everything above is waiting, and a
        // bot is not charged for the engine's own turn. A caller with work of its own to
        // do before handing over -- the FFI's state snapshot -- re-stamps with
        // `stamp_cpu` rather than paying for it.
        self.stamp_cpu();
        Some(state)
    }

    /// Restarts the CPU-time window at this instant.
    ///
    /// `await_tick` already stamps the clock before it returns, which is the right answer
    /// for a Rust bot: the next thing that happens is strategy code. A bot behind the C
    /// ABI has one more step in between -- `mm_channel_await_tick` copies the state into
    /// the handle so Python gets a lifetime it can reason about -- and that memcpy is the
    /// engine's plumbing, not the bot's thinking. Re-stamping after it makes the charged
    /// window exactly the time the caller holds the wheel.
    pub fn stamp_cpu(&self) {
        self.cpu_start.set(Some(cpu_time::ProcessTime::now()));
    }

    /// Hands `action` back and ends the bot's turn, reporting the CPU time spent since
    /// `await_tick` returned. A `respond` with no preceding `await_tick` reports zero.
    pub fn respond(&self, action: FleetAction) {
        let cpu_time_nanos = self
            .cpu_start
            .take()
            .map_or(0, |start| start.elapsed().as_nanos() as u64);

        // Written over the request in place, payload-sized, for the same reason the
        // handshake is -- and the write has to land before the handoff byte does.
        unsafe {
            let frame = self.at(offset_of!(SharedBlock, frame));
            std::ptr::write(frame.add(Frame::PAYLOAD_OFFSET) as *mut FleetAction, action);
            frame.write(TickProtocol::response_tag());
            *(self.at(offset_of!(SharedBlock, cpu_time_nanos)) as *mut u64) = cpu_time_nanos;
        }

        handoff_byte(&self.mmap).store(Handoff::EngineTurn as u8, Ordering::Release);
    }
}

#[cfg(feature = "engine")]
#[cfg(test)]
mod tests {
    use super::*;
    // The shipped arena, rather than a `GameConfig` literal of this module's own: every
    // added `BotConfig` field already breaks every literal in the crate at once.
    use crate::game::config::test_conf;
    use crate::game::state::BotAction;

    // Exercises the `SharedBlock::cpu_time_nanos` plumbing end-to-end (bot side reports its own
    // CPU time in `BotChannel::handle_request`, engine side reads it back in
    // `EngineChannel::request`) without needing a real separate bot process — a background task
    // plays the "bot" role over the same backing file.
    /// `EngineChannel::request` and `Handlers::respond_in_place` both write a payload at
    /// `Frame::PAYLOAD_OFFSET` rather than building a whole `Frame`. That offset is derived
    /// from `#[repr(u8, C)]`'s documented layout, and if the derivation is wrong every
    /// message silently decodes as garbage -- so it is checked against a real `Frame` here
    /// rather than trusted.
    #[test]
    fn payload_offset_matches_the_real_frame_layout() {
        let frame = Frame::TickResponse(FleetAction::default());
        let bytes = unsafe {
            std::slice::from_raw_parts(&frame as *const Frame as *const u8, size_of::<Frame>())
        };

        assert_eq!(bytes[0], TickProtocol::response_tag(), "the tag is not the first byte");

        let payload = unsafe { &*(bytes.as_ptr().add(Frame::PAYLOAD_OFFSET) as *const FleetAction) };
        let Frame::TickResponse(original) = &frame else { unreachable!() };
        assert!(
            payload == original,
            "the payload does not live at PAYLOAD_OFFSET ({})",
            Frame::PAYLOAD_OFFSET,
        );
    }

    /// The handshake used to size the whole `Frame` by two orders of magnitude: it carried
    /// the navigation graph, and a tick paid for that on every message unless the copy was
    /// variant-sized. The graph is a bot-side singleton now (see
    /// `game::topology::init_topology`), so the tick is the larger variant and the two are
    /// the same order of magnitude. The variant-sized copy still avoids materializing a
    /// whole `Frame` per message, but the gap it was defending is gone -- this is here to
    /// notice if anything puts it back.
    #[test]
    fn the_frame_is_sized_by_the_tick() {
        let tick = Frame::size_for_tag(TickProtocol::request_tag());
        let handshake = Frame::size_for_tag(HandshakeProtocol::request_tag());

        assert_eq!(tick, Frame::PAYLOAD_OFFSET + size_of::<GameState>());
        assert_eq!(handshake, Frame::PAYLOAD_OFFSET + size_of::<HandshakeRequest>());
        assert_eq!(size_of::<Frame>(), tick, "Frame should be sized by the tick");
        assert!(
            handshake < tick,
            "the handshake is {handshake} bytes against a tick's {tick} -- if the handshake \
             has grown back past the tick, check what was added to GameConfig",
        );
    }

    /// Exercises the `SharedBlock::cpu_time_nanos` plumbing end-to-end -- the bot stamps
    /// the clock in `await_tick` and reports the delta in `respond`, the engine reads it
    /// back in `EngineChannel::request` -- without needing a real separate bot process.
    #[tokio::test]
    async fn reports_bot_cpu_time() {
        let engine_channel = EngineChannel::new().unwrap();
        let bot_channel = BotChannel::from_path(engine_channel.backing_file_path()).unwrap();

        let conf = test_conf::conf();
        let state = GameState::new(conf);

        let bot = async {
            let tick = bot_channel.await_tick().await.expect("the channel is open");
            assert_eq!(tick.tick, 0, "the bot should see the state the engine sent");
            // burn a known amount of CPU so the reported time is unambiguously nonzero
            let start = cpu_time::ProcessTime::now();
            while start.elapsed() < Duration::from_millis(20) {}
            bot_channel.respond(FleetAction::default());
        };

        // both sides run on the current task concurrently, mirroring the real protocol's
        // request/response exchange over the shared `handoff` byte
        let (_, res) = tokio::join!(
            bot,
            engine_channel.request::<TickProtocol>(&state, Duration::from_secs(5)),
        );
        let (action, cpu_time) = res.unwrap();

        assert_eq!(action, FleetAction::default());
        assert!(cpu_time >= Duration::from_millis(10), "reported cpu_time was implausibly small: {cpu_time:?}");
    }

    /// The end of a match is an ordinary event, not a failure: `await_tick` reports it as
    /// `None` so the bot's loop ends and the process exits 0. Before `Handoff::Closed` was
    /// ever read, this hung until the engine sent SIGKILL.
    #[tokio::test]
    async fn a_closed_channel_ends_the_bot_loop() {
        let engine_channel = EngineChannel::new().unwrap();
        let bot_channel = BotChannel::from_path(engine_channel.backing_file_path()).unwrap();

        engine_channel.close();

        assert!(
            bot_channel.await_tick().await.is_none(),
            "a closed channel should end the loop, not deliver a tick",
        );
    }

    /// The same, but reached from the backoff tail rather than the spin window -- which is
    /// where a bot actually is when a match ends, since the engine has just spent a tick
    /// writing the gamelog. The two loops check `Closed` separately, so both are exercised.
    #[tokio::test]
    async fn a_channel_closed_during_the_backoff_ends_the_bot_loop() {
        let engine_channel = EngineChannel::new().unwrap();
        let bot_channel = BotChannel::from_path(engine_channel.backing_file_path()).unwrap();

        let closer = async {
            // long enough to be past SPIN_WINDOW, so the waiter is on the timer
            tokio::time::sleep(SPIN_WINDOW * 4).await;
            engine_channel.close();
        };

        let (tick, ()) = tokio::join!(bot_channel.await_tick(), closer);
        assert!(tick.is_none(), "a closed channel should end the loop, not deliver a tick");
    }

    /// The engine must refuse an action whose bytes are not a valid value *before* it makes
    /// one. `respond` cannot express this -- it takes a `FleetAction`, and the type system
    /// is exactly what stops a Rust bot from producing an invalid one -- so the bad byte is
    /// written the way an FFI bot writes it: straight into the mapping.
    #[tokio::test]
    async fn an_invalid_action_is_refused_before_it_is_read() {
        let engine_channel = EngineChannel::new().unwrap();
        let bot_channel = BotChannel::from_path(engine_channel.backing_file_path()).unwrap();

        let conf = test_conf::conf();
        let state = GameState::new(conf);

        let bot = async {
            bot_channel.await_tick().await.expect("the channel is open");
            bot_channel.respond(FleetAction::default());
            // `TurnAction` declares tags 0..=2. Reading this as one is UB the moment the
            // value exists -- which would be in the engine's process, not the bot's.
            unsafe {
                let frame = bot_channel.at(offset_of!(SharedBlock, frame));
                let action = frame.add(Frame::PAYLOAD_OFFSET) as *mut u8;
                action
                    .add(offset_of!(FleetAction, bots) + offset_of!(BotAction, turn_action))
                    .write(3);
            }
        };

        let (_, res) = tokio::join!(
            bot,
            engine_channel.request::<TickProtocol>(&state, Duration::from_secs(5)),
        );

        assert!(
            matches!(res, Err(ResponseError::InvalidAction)),
            "expected InvalidAction, got {:?}",
            res.map(|(action, _)| action),
        );
    }

    /// The penalty split the validator exists to make possible: a bad struct costs the
    /// tick it spoiled, a channel that stopped behaving still costs the bank. Stated as a
    /// property of the error rather than read off `BotManager::tick`, which needs two real
    /// bot processes to reach.
    #[test]
    fn only_an_invalid_action_escapes_the_forfeit() {
        assert!(!ResponseError::InvalidAction.forfeits_budget());
        assert!(ResponseError::Malformed.forfeits_budget());
        assert!(ResponseError::SizeMismatch { expected: 1, actual: 2 }.forfeits_budget());
        assert!(ResponseError::AlignmentError { address: 1, alignment: 2 }.forfeits_budget());
    }

    /// The fingerprint is the handshake's answer, and it has to be more than the magic it
    /// is seeded from -- a build that skipped the layout would otherwise pass.
    #[test]
    fn the_handshake_answer_carries_the_layout() {
        assert_ne!(
            HANDSHAKE_FINGERPRINT, HANDSHAKE_MAGIC,
            "the fingerprint is not mixing the layout in at all",
        );
        assert_ne!(HANDSHAKE_FINGERPRINT, 0);
        assert_ne!(Frame::LAYOUT_HASH, crate::game::mirror::HASH_BASIS);
    }

    /// What the fingerprint is for: two types of the same size whose *layout* differs hash
    /// differently. A hash that dropped offsets, or tags, would call these equal and let a
    /// skewed pair of builds run a whole match reading the wrong fields.
    #[test]
    fn layouts_that_differ_hash_differently() {
        use crate::game::mirror::LayoutHash;

        #[derive(mm_macros::FfiMirror)]
        #[repr(C)]
        struct Pair {
            a: u32,
            b: f32,
        }

        // Same size and alignment, one field's type swapped.
        #[derive(mm_macros::FfiMirror)]
        #[repr(C)]
        struct Swapped {
            a: f32,
            b: f32,
        }

        // Same size and alignment again, one extra declared tag.
        #[derive(mm_macros::FfiMirror)]
        #[repr(u8)]
        enum Two {
            A = 0,
            B = 1,
        }

        #[derive(mm_macros::FfiMirror)]
        #[repr(u8)]
        enum Three {
            A = 0,
            B = 1,
            C = 2,
        }

        assert_eq!(size_of::<Pair>(), size_of::<Swapped>());
        assert_ne!(Pair::HASH, Swapped::HASH, "a field's type is part of the layout");
        assert_eq!(size_of::<Two>(), size_of::<Three>());
        assert_ne!(Two::HASH, Three::HASH, "a variant's tag is part of the layout");
    }
}

