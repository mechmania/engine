//! C-ABI surface for non-Rust bots (currently: the Python starterpack's `native/` shim
//! crate). See `dev/ffi.md` for the full design.
//!
//! Everything here is generated from a plain Rust fn by `mm_macros::mm_ffi_fn`, which
//! writes the `#[no_mangle] extern "C"` wrapper, the `catch_unwind`, and the descriptor
//! the Python binding generator reads. Adding an entry point is a one-line annotation,
//! not hand-written glue on both sides.
//!
//! # What crosses, and how
//!
//! Coordinates cross as loose `f32`s, never as a `Vec2` by value: small-struct-by-value
//! is the corner of the C ABI where `ctypes`, System V and MSVC are most likely to
//! disagree, and unpacking costs nothing. No allocation is ever handed to Python --
//! `mm_route_waypoints` writes into a caller-owned buffer. Everything behind a handle is
//! immutable for the life of the match, so Python may call from any thread.
//!
//! # Navigation and the graph
//!
//! There is no topology handle. `game::topology`'s queries all take `&GameConfig` and
//! read a process-global `OnceLock` that `init_topology` fills at handshake, so a handle
//! would mean re-threading `&MapTopology` through every query and every Rust call site,
//! and then holding a second ~328 KB graph in a process that already has one. The channel
//! owns the config instead, and the navigation calls take the channel purely to reach it.

use mm_macros::{mm_ffi_fn, mm_ffi_handle};

use crate::game::config::{GameConfig, Map, BOTS_MAX, MAP_SIZE, PAYLOAD_PATH_LEN};
use crate::game::mirror::{FfiAliasDesc, FfiConstDesc, FfiMirrorDesc, FfiMirrorType};
use crate::game::state::{BotId, FleetAction, GameState, StateOption};
use crate::game::team::{Team, TeamPair};
use crate::game::topology;
use crate::game::util::Vec2;
use crate::ipc::BotChannel;

// -----------------------------------------------------------------------------------
// codegen descriptors
// -----------------------------------------------------------------------------------

/// An ABI-safe type, as it appears in a generated signature.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FfiType {
    Void,
    F32,
    U32,
    I32,
    U8,
    Bool,
    /// A raw pointer. `pointee` is the type name as written -- either a handle
    /// (`MmChannel`) or a scalar the function writes through (`f32`).
    Ptr { pointee: &'static str, mutable: bool },
}

/// One `extern "C"` entry point, registered by `#[mm_ffi_fn]`.
pub struct FfiFnDesc {
    pub name: &'static str,
    pub ret: FfiType,
    pub args: &'static [(&'static str, FfiType)],
}

/// One opaque handle type, registered by `#[mm_ffi_handle]`.
pub struct FfiHandleDesc {
    pub name: &'static str,
    /// The symbol that reclaims it.
    pub free: &'static str,
}

inventory::collect!(FfiFnDesc);
inventory::collect!(FfiHandleDesc);

// The layout registry. The descriptors themselves are defined in `crate::game::mirror`,
// which is shared source compiled in every build; collecting them is `ffi`-only, because
// only the binding generator reads the whole set at once.
inventory::collect!(FfiMirrorDesc);
inventory::collect!(FfiAliasDesc);
inventory::collect!(FfiConstDesc);

/// Registers a concrete instantiation of a generic wire type.
///
/// `#[derive(FfiMirror)]` cannot do this itself: `inventory::submit!` needs a value at
/// static-init and there is no such thing as "one per monomorphization". So the live
/// instantiations are listed by hand, and the layout each one registers is the one measured
/// for *that* monomorphization -- `StateOption<Vec2>` and `StateOption<BotId>` have
/// different payload offsets, and both are right.
///
/// The registered `name` is the spelling a field carries (`"StateOption<Vec2>"`), which is
/// how the generator resolves the two to each other. Forgetting an entry is not silent: the
/// closure test in `ffi_test` fails on a field whose type resolves to nothing.
macro_rules! mm_ffi_instantiate {
    ($($name:literal: $ty:ty => [$($arg:literal),* $(,)?]),* $(,)?) => {
        $(
            inventory::submit! {
                FfiMirrorDesc {
                    name: $name,
                    generic_args: &[$($arg),*],
                    ..<$ty as FfiMirrorType>::DESC
                }
            }
        )*
    };
}

/// Registers a `type` alias in the wire closure. A derive cannot attach to an alias, and
/// the generator has no name resolution of its own, so the handful that exist are declared
/// here with their measured size and alignment.
macro_rules! mm_ffi_alias {
    ($($name:literal: $ty:ty => $target:literal),* $(,)?) => {
        $(
            inventory::submit! {
                FfiAliasDesc {
                    name: $name,
                    target: $target,
                    size: ::core::mem::size_of::<$ty>(),
                    align: ::core::mem::align_of::<$ty>(),
                }
            }
        )*
    };
}

/// Registers a `const` emitted to Python as a plain integer.
///
/// Two kinds live here: the array lengths the generator shapes `ctypes` arrays from, and
/// the `MM_*` status codes every fallible entry point returns. They share a registry
/// because they share a destination -- a module-level `NAME = <int>` in the generated
/// bindings -- and splitting them would buy a second `inventory::collect!` and nothing
/// else.
///
/// The array-length half is what the doc below is about.
///
/// A derive records a field's type as written -- `[BotAction; BOTS_MAX]` -- and cannot
/// resolve `BOTS_MAX`. Dividing the field's measured size by its element's recovers the
/// total element count, which settles a one-dimensional array but not a nested one: `Map`
/// is 1024 bytes whether it is 32x32 or 1024x1, and the generator has to pick one to shape
/// the `ctypes` array. So the lengths are declared, and the measured sizes cross-check them
/// (`every_array_length_resolves_and_agrees_with_the_measured_size`).
///
/// The values are also emitted to Python as plain constants, which is where bot code gets
/// `BOTS_MAX`.
macro_rules! mm_ffi_const {
    ($($name:literal: $value:expr),* $(,)?) => {
        $(
            inventory::submit! {
                FfiConstDesc { name: $name, value: $value }
            }
        )*
    };
}

// Every length spelled in the closure today: `[BotAction; BOTS_MAX]`,
// `[BotState; BOTS_MAX]`, `[bool; BOTS_MAX]`, `[Vec2; PAYLOAD_PATH_LEN]`,
// `[[MapTile; MAP_SIZE]; MAP_SIZE]`.
mm_ffi_const! {
    "BOTS_MAX": BOTS_MAX,
    "MAP_SIZE": MAP_SIZE,
    "PAYLOAD_PATH_LEN": PAYLOAD_PATH_LEN,
    "MM_OK": MM_OK as usize,
    "MM_CLOSED": MM_CLOSED as usize,
    "MM_TIMEOUT": MM_TIMEOUT as usize,
    "MM_MALFORMED": MM_MALFORMED as usize,
    "MM_IO": MM_IO as usize,
    "MM_PANIC": MM_PANIC as usize,
    // The budget a bot compares `mm_channel_budget`'s answer against. Emitted rather than
    // written into the Python by hand, for the same reason every struct here is generated.
    "COMPUTE_BANK_TICKS": crate::ipc::COMPUTE_BANK_TICKS as usize,
    "COMPUTE_REFILL_TICKS": crate::ipc::COMPUTE_REFILL_TICKS as usize,
}

// The name is spelled out rather than `stringify!`d: a field records its type the way the
// source writes it, normalized, and `stringify!` pretty-prints `<` and `;` with spaces
// around them. A typo here is caught, not silent -- `every_field_type_resolves` walks the
// spellings and fails on one that resolves to nothing.
mm_ffi_instantiate! {
    "StateOption<Vec2>": StateOption<Vec2> => ["Vec2"],
    "StateOption<BotId>": StateOption<BotId> => ["BotId"],
    "StateOption<Team>": StateOption<Team> => ["Team"],
    "TeamPair<u32>": TeamPair<u32> => ["u32"],
}

mm_ffi_alias! {
    "BotId": BotId => "u8",
    "Map": Map => "[[MapTile; MAP_SIZE]; MAP_SIZE]",
}

// -----------------------------------------------------------------------------------
// status codes
// -----------------------------------------------------------------------------------

/// The value every fallible entry point returns. `0` is success; everything else is a
/// reason, and the one that matters is `MM_CLOSED` -- a bot that treats the end of a
/// match as an error exits non-zero and reads as a crash in the gamelog.
///
/// Emitted to Python as plain constants (see `mm_ffi_const!` above). No string crosses
/// the ABI: a `const char *` message would be the only `c_char` in the whole surface,
/// and Python maps a small integer to a message perfectly well on its own.
pub const MM_OK: i32 = 0;
/// The engine closed the channel. The match is over and the bot should exit 0.
pub const MM_CLOSED: i32 = 1;
/// Reserved. Nothing returns it yet: neither `BotChannel::await_tick` nor the Rust bot's
/// loop has a wall-clock bound -- `timing::TICK_HANG_TIMEOUT` is the *engine's* safety
/// net, applied from the other end of the mapping. Kept so the numbering matches the
/// spec rather than shifting when a bot-side deadline is added.
pub const MM_TIMEOUT: i32 = 2;
/// The caller passed something unusable -- a null out-pointer, a null action.
pub const MM_MALFORMED: i32 = 3;
/// The mapping could not be opened or the handshake did not complete.
pub const MM_IO: i32 = 4;
/// A `catch_unwind` caught. The handle is intact but its state is not to be trusted.
pub const MM_PANIC: i32 = 5;

// -----------------------------------------------------------------------------------
// the channel handle
// -----------------------------------------------------------------------------------

/// Everything a Python bot holds across calls: the mapping, the runtime that drives it,
/// the config the handshake captured, and the most recent tick.
///
/// The three channel primitives are `BotChannel`'s (`crate::ipc`), unchanged -- there is
/// one implementation of the protocol and Python gets the same one a Rust bot does. What
/// this type adds is the blocking adapter and the lifetimes.
///
/// **Blocking, and who owns the runtime.** `BotChannel`'s primitives are `async`, and the C ABI
/// is blocking by construction, so the handle owns a `new_current_thread` runtime and
/// every entry point is a `block_on`. Python never sees a future. The corollary is that
/// these must not be called from inside another tokio runtime -- irrelevant through
/// `ctypes`, which has no runtime, and the reason the Rust tests below drive the bot side
/// from a plain `std::thread`.
#[mm_ffi_handle]
pub struct MmChannel {
    rt: tokio::runtime::Runtime,
    chan: BotChannel,
    /// `None` until the handshake. The navigation queries need it, which is why they
    /// take the channel at all.
    config: Option<GameConfig>,
    /// A private copy, not a pointer into the live mapping: Python's lifetime story is
    /// then just "valid until the next `mm_channel_await_tick`", with no way for a
    /// stale pointer to observe the engine writing the next tick underneath it.
    state: Option<GameState>,
}

/// Borrows a handle for the entry points that advance the channel, or returns the
/// sentinel if Python passed null.
///
/// A null handle is the one caller mistake that is cheap to survive and fatal not to:
/// `ctypes` hands over whatever it has, including the `None` a failed constructor
/// returned, and dereferencing it would take the whole bot process down. The read-only
/// entry points below reach for a field rather than the handle, so they check inline.
macro_rules! channel_mut {
    ($ptr:expr, $sentinel:expr) => {
        match unsafe { $ptr.as_mut() } {
            Some(ch) => ch,
            None => return $sentinel,
        }
    };
}

/// The config the handshake captured, or the sentinel.
///
/// Folds the two ways a navigation query can have nothing to answer with -- a null handle
/// and a handle that has not handshaked yet -- into one place, because they are the same
/// answer: this pointer cannot serve a query, here is the documented failure value.
macro_rules! config {
    ($ptr:expr, $sentinel:expr) => {
        match unsafe { $ptr.as_ref() }.and_then(|ch| ch.config.as_ref()) {
            Some(conf) => conf,
            None => return $sentinel,
        }
    };
}

/// Opens the bot's end of the mapping the engine created, at the path it passed as
/// `argv[1]`. `NULL` on failure -- a missing file, a bad path, or a runtime that would
/// not start.
///
/// The path crosses as bytes and a length rather than a NUL-terminated `const char *`:
/// it would be the only string in the surface, and a length is what Python already has.
/// Non-UTF-8 paths are rejected; competitors do not have one.
///
/// # Safety
/// `path` must point at `path_len` readable bytes.
#[mm_ffi_fn]
pub unsafe fn mm_channel_open(path: *const u8, path_len: i32) -> *mut MmChannel {
    if path.is_null() || path_len <= 0 {
        return std::ptr::null_mut();
    }
    let bytes = unsafe { std::slice::from_raw_parts(path, path_len as usize) };
    let Ok(path) = std::str::from_utf8(bytes) else {
        return std::ptr::null_mut();
    };
    // `await_handoff` never awaits (it spins, then yields), so nothing here needs a driver; `enable_time` is kept
    // only so a future await does not panic. There is no IO driver and no worker pool.
    let Ok(rt) = tokio::runtime::Builder::new_current_thread().enable_time().build() else {
        return std::ptr::null_mut();
    };
    let Ok(chan) = BotChannel::from_path(path) else {
        return std::ptr::null_mut();
    };
    Box::into_raw(Box::new(MmChannel { rt, chan, config: None, state: None }))
}

/// Reads the engine's opening message: which team this bot is, written to `out_team`.
///
/// Also builds the navigation graph and captures the config -- both inside
/// `BotChannel::handshake`, which is where a Rust bot gets them too, so there is one
/// initialization path and one graph.
///
/// Every error here is reported as `MM_IO`. The underlying `anyhow` error distinguishes
/// "the engine closed the channel first" from "that was not a handshake", but only in its
/// message, and matching on a message to recover a status is worse than not having the
/// distinction: either way the bot cannot play and exits.
///
/// # Safety
/// `out_team` must point at a writable byte.
#[mm_ffi_fn(panic = MM_PANIC)]
pub unsafe fn mm_channel_handshake(ch: *mut MmChannel, out_team: *mut u8) -> i32 {
    let ch = channel_mut!(ch, MM_PANIC);
    if out_team.is_null() {
        return MM_MALFORMED;
    }
    match ch.rt.block_on(ch.chan.handshake()) {
        Ok((team, config)) => {
            unsafe { out_team.write(team) };
            ch.config = Some(config);
            MM_OK
        }
        Err(_) => MM_IO,
    }
}

/// The `GameConfig` the handshake captured -- the map, the bot radius and the stat
/// tables. Valid for the life of the handle; `NULL` before the handshake.
#[mm_ffi_fn]
pub fn mm_channel_config(ch: *const MmChannel) -> *const GameConfig {
    config!(ch, std::ptr::null())
}

/// Blocks until the engine hands over the next tick, then copies it into the handle.
///
/// `MM_OK` with the state reachable through `mm_channel_state`, or `MM_CLOSED` once the
/// engine has closed the channel -- the match is over, and the bot should exit 0 rather
/// than treat it as a failure.
#[mm_ffi_fn(panic = MM_PANIC)]
pub unsafe fn mm_channel_await_tick(ch: *mut MmChannel) -> i32 {
    let ch = channel_mut!(ch, MM_PANIC);
    // Destructured rather than used through `ch`: `await_tick` returns a reference
    // borrowed from `chan`, and the snapshot is written to `state`. Two different
    // fields, which borrowck accepts only once they are two different bindings.
    let MmChannel { rt, chan, state, .. } = ch;

    match rt.block_on(chan.await_tick()) {
        Some(tick) => {
            *state = Some(tick.clone());
            // The CPU-time window opens *here*, after the copy above, so the bot is
            // charged for its own thinking and not for the engine's handoff or for this
            // memcpy. `await_tick` stamps the clock before returning; re-stamping moves
            // the start to the last instant before Python gets the wheel.
            chan.stamp_cpu();
            MM_OK
        }
        None => {
            *state = None;
            MM_CLOSED
        }
    }
}

/// The state the last `mm_channel_await_tick` delivered. `NULL` before the first tick and
/// after the channel closes.
///
/// Valid until the next `mm_channel_await_tick` overwrites it -- it points into the
/// handle's own copy, never into the live mapping.
#[mm_ffi_fn]
pub fn mm_channel_state(ch: *const MmChannel) -> *const GameState {
    match unsafe { ch.as_ref() }.and_then(|ch| ch.state.as_ref()) {
        Some(state) => state as *const GameState,
        None => std::ptr::null(),
    }
}

/// What this bot has left to spend, as of the tick `mm_channel_await_tick` last delivered.
///
/// Both out-parameters are in "ticks" -- multiples of the engine's own recent average
/// per-tick CPU cost, which is the unit the budget is actually denominated in. `remaining`
/// can be negative: an overspend is a debt, and while it is negative the bot is not called
/// at all. The bank's size and refill are the `COMPUTE_BANK_TICKS` / `COMPUTE_REFILL_TICKS`
/// constants.
///
/// Reads the values `BotChannel::await_tick` snapshotted, so it costs nothing and can be
/// called as often as a strategy likes. Before the first tick it reports a full bank.
///
/// # Safety
/// Both out-pointers must be valid for a write.
#[mm_ffi_fn(panic = MM_PANIC)]
pub unsafe fn mm_channel_budget(
    ch: *const MmChannel,
    out_remaining: *mut i64,
    out_last_charge: *mut u64,
) -> i32 {
    if ch.is_null() {
        return MM_PANIC;
    }
    if out_remaining.is_null() || out_last_charge.is_null() {
        return MM_MALFORMED;
    }
    let budget = crate::ipc::get_budget();
    unsafe {
        out_remaining.write(budget.remaining);
        out_last_charge.write(budget.last_charge);
    }
    MM_OK
}

/// Hands `action` back and ends the bot's turn, reporting the CPU time spent since
/// `mm_channel_await_tick` returned. A `respond` with no preceding `await_tick` reports
/// zero.
///
/// The bytes are handed over without being checked here, on purpose. An out-of-range enum
/// tag from a partially-filled Python buffer is invalid the instant the value exists, so
/// the check has to happen ahead of the read -- and it belongs engine-side, where it also
/// covers a Rust bot corrupting the mapping by other means. It does:
/// `EngineChannel::request` calls `Validate` on the raw mapping and refuses the action as
/// `ResponseError::InvalidAction`, which costs the bot that tick and nothing more.
///
/// # Safety
/// `action` must point at a valid `FleetAction`.
#[mm_ffi_fn(panic = MM_PANIC)]
pub unsafe fn mm_channel_respond(ch: *mut MmChannel, action: *const FleetAction) -> i32 {
    let ch = channel_mut!(ch, MM_PANIC);
    if action.is_null() {
        return MM_MALFORMED;
    }
    ch.chan.respond(unsafe { action.read() });
    MM_OK
}

// -----------------------------------------------------------------------------------
// navigation
// -----------------------------------------------------------------------------------

/// The direction to move this tick to get from `(fx, fy)` towards `(tx, ty)` around
/// walls, written to `out` as two floats. A delta, not a unit vector.
///
/// Never fails: with nowhere to route through it degrades to aiming straight at the
/// target and letting collision slide the bot along the wall. `out` is pre-filled with
/// `NaN` so a caught panic leaves that behind rather than a stale value -- the wrapper's
/// `void` return has nowhere else to report one.
///
/// # Safety
/// `out` must point at space for two `f32`s.
#[mm_ffi_fn]
pub unsafe fn mm_navigate_to(
    ch: *const MmChannel,
    fx: f32,
    fy: f32,
    tx: f32,
    ty: f32,
    out: *mut f32,
) {
    if out.is_null() {
        return;
    }
    unsafe {
        out.write(f32::NAN);
        out.add(1).write(f32::NAN);
    }
    let conf = config!(ch, ());
    let d = topology::navigate_to(conf, Vec2::new(fx, fy), Vec2::new(tx, ty));
    unsafe {
        out.write(d.x);
        out.add(1).write(d.y);
    }
}

/// The walking distance from `(fx, fy)` to `(tx, ty)` around walls.
///
/// `-1.0` when no route exists -- normally because one end is somewhere a bot cannot
/// stand. `NaN` only ever means a panic was caught, so the two stay distinguishable;
/// a real distance is never negative.
#[mm_ffi_fn]
pub fn mm_path_length(ch: *const MmChannel, fx: f32, fy: f32, tx: f32, ty: f32) -> f32 {
    let conf = config!(ch, f32::NAN);
    topology::path_length(conf, Vec2::new(fx, fy), Vec2::new(tx, ty)).unwrap_or(-1.0)
}

/// Whether a bot can walk the straight line from `(ax, ay)` to `(bx, by)` without
/// clipping a wall -- accounts for the bot's own radius, unlike `mm_line_of_sight`.
#[mm_ffi_fn]
pub fn mm_corridor_clear(ch: *const MmChannel, ax: f32, ay: f32, bx: f32, by: f32) -> bool {
    let conf = config!(ch, false);
    topology::corridor_clear(conf, Vec2::new(ax, ay), Vec2::new(bx, by))
}

/// Whether the straight line from `(ax, ay)` to `(bx, by)` is unobstructed by walls --
/// a zero-radius sightline, for deciding whether a blaster or healer can reach.
#[mm_ffi_fn]
pub fn mm_line_of_sight(ch: *const MmChannel, ax: f32, ay: f32, bx: f32, by: f32) -> bool {
    let conf = config!(ch, false);
    topology::has_line_of_sight(conf, Vec2::new(ax, ay), Vec2::new(bx, by))
}

/// Whether a disc of `radius` centred at `(x, y)` is clear of walls. Pass the bot radius
/// to ask whether a bot can stand there.
#[mm_ffi_fn]
pub fn mm_disc_free(ch: *const MmChannel, x: f32, y: f32, radius: f32) -> bool {
    let conf = config!(ch, false);
    topology::disc_free(conf, Vec2::new(x, y), radius)
}

/// The interior waypoints of the route from `(fx, fy)` to `(tx, ty)`, excluding both
/// ends, written to `out` as consecutive `x, y` pairs.
///
/// Returns the route's **full** waypoint count while writing only `min(count, cap)`
/// pairs, so a short buffer shows up as `returned > cap` and can be retried bigger
/// rather than silently truncating a plan. `0` means the two points see each other
/// directly, `-1` means no route exists, and `-2` means a panic was caught -- `-1` is
/// already spoken for here, which is why the sentinel is overridden.
///
/// # Safety
/// `out` must point at space for `2 * cap` `f32`s.
#[mm_ffi_fn(panic = -2)]
pub unsafe fn mm_route_waypoints(
    ch: *const MmChannel,
    fx: f32,
    fy: f32,
    tx: f32,
    ty: f32,
    out: *mut f32,
    cap: i32,
) -> i32 {
    let conf = config!(ch, -2);
    if cap < 0 || (cap > 0 && out.is_null()) {
        return -2;
    }
    let Some(route) =
        topology::route_waypoints(conf, Vec2::new(fx, fy), Vec2::new(tx, ty))
    else {
        return -1;
    };
    for (i, v) in route.iter().take(cap as usize).enumerate() {
        unsafe {
            out.add(i * 2).write(v.x);
            out.add(i * 2 + 1).write(v.y);
        }
    }
    route.len() as i32
}

// -----------------------------------------------------------------------------------
// game rules a bot has to be able to ask about
// -----------------------------------------------------------------------------------
//
// Everything below is something a Rust bot gets from the shared source for free and a
// Python bot has no route to: a rule (`payload_position`'s arc-length walk,
// `point_free`'s "at the bot's radius") or a piece of geometry. Crossing the ABI rather than
// re-deriving it in Python is the same bargain the navigation calls make -- one
// implementation, and no drift.

/// Center of the payload circle at capture progress `capture`, written to `out` as two
/// floats. `GameState::payload_pos()` is this called with `state.capture`.
///
/// `capture` is parameterized by arc length and clamped to `[-1.0, 1.0]`: `0.0` is the
/// center of the map, `1.0` is the far goal and `-1.0` is the near one. Taking it as an
/// argument rather than reading the handle's tick snapshot keeps the call pure, and
/// answers "where would the payload be at 0.5" as well as "where is it now".
///
/// Never fails. `out` is pre-filled with `NaN` so a caught panic leaves that behind
/// rather than a stale value -- the `void` return has nowhere else to report one.
///
/// # Safety
/// `out` must point at space for two `f32`s.
#[mm_ffi_fn]
pub unsafe fn mm_payload_pos(capture: f32, out: *mut f32) {
    if out.is_null() {
        return;
    }
    unsafe {
        out.write(f32::NAN);
        out.add(1).write(f32::NAN);
    }
    let p = crate::game::config::payload_position(capture);
    unsafe {
        out.write(p.x);
        out.add(1).write(p.y);
    }
}

/// Whether a bot can stand centred at `(x, y)` -- `mm_disc_free` at the bot's own radius.
///
/// Worth its own entry point because "the bot's radius" is the part Python would have to
/// know: `topology::point_free` reads `conf.bot.radius`, and a bot passing its own guess
/// would get a different answer from the engine's collision.
#[mm_ffi_fn]
pub fn mm_point_free(ch: *const MmChannel, x: f32, y: f32) -> bool {
    let conf = config!(ch, false);
    topology::point_free(conf, Vec2::new(x, y))
}

/// Distance from `(px, py)` to the segment `(ax, ay) - (bx, by)`.
///
/// Pure geometry, so no handle. `NaN` only on a caught panic.
#[mm_ffi_fn]
pub fn mm_point_seg_dist(px: f32, py: f32, ax: f32, ay: f32, bx: f32, by: f32) -> f32 {
    topology::point_seg_dist(Vec2::new(px, py), Vec2::new(ax, ay), Vec2::new(bx, by))
}

/// `deg` wrapped into `[0, 360)`, the range every angle in `GameState` is already in.
///
/// Pure arithmetic, so no handle. `NaN` only on a caught panic.
#[mm_ffi_fn]
pub fn mm_normalize_degrees(deg: f32) -> f32 {
    crate::game::util::normalize_degrees(deg)
}

/// `a - b` by the shortest way round, in degrees: the signed rotation that takes `b` to
/// `a`, in `[-180, 180]`.
///
/// Pure arithmetic, so no handle. `NaN` only on a caught panic.
#[mm_ffi_fn]
pub fn mm_diff_degrees(a: f32, b: f32) -> f32 {
    crate::game::util::diff_degrees(a, b)
}

/// A panic sentinel is only useful if `catch_unwind` actually catches, which it does not
/// under `panic = "abort"`. Fail the build rather than the match.
const _: () = {
    // `cfg!(panic = "abort")` is stable; a const assert keeps it at compile time.
    assert!(
        !cfg!(panic = "abort"),
        "the FFI surface needs unwinding panics: `catch_unwind` cannot catch under \
         `panic = \"abort\"`, and a panic escaping into C is undefined behaviour"
    );
};

#[cfg(test)]
mod ffi_test {
    use super::*;
    use crate::game::config::test_conf::{conf, sample_free_points};
    use crate::game::config::{MapTile, MAP_SIZE};

    // Every call below goes through this block, not through the Rust items above, so
    // what the tests exercise is the real symbol and the real C signature -- a
    // mismatch between the two is the whole class of bug this surface can have.
    //
    // `improper_ctypes` fires on every `MmChannel *` here: the handle holds a tokio
    // runtime and an `Option`, so it has no guaranteed layout. That is the point -- it
    // is opaque, the pointer is never dereferenced on the C side, and Python only ever
    // stores it and hands it back. The lint cannot see that, and there is nothing to fix.
    #[allow(improper_ctypes)]
    extern "C" {
        fn mm_channel_open(path: *const u8, path_len: i32) -> *mut MmChannel;
        fn mm_channel_handshake(ch: *mut MmChannel, out_team: *mut u8) -> i32;
        fn mm_channel_config(ch: *const MmChannel) -> *const GameConfig;
        fn mm_channel_await_tick(ch: *mut MmChannel) -> i32;
        fn mm_channel_state(ch: *const MmChannel) -> *const GameState;
        fn mm_channel_budget(ch: *const MmChannel, out_remaining: *mut i64, out_last_charge: *mut u64) -> i32;
        fn mm_channel_respond(ch: *mut MmChannel, action: *const FleetAction) -> i32;
        fn mm_channel_free(ch: *mut MmChannel);
        fn mm_navigate_to(
            ch: *const MmChannel,
            fx: f32,
            fy: f32,
            tx: f32,
            ty: f32,
            out: *mut f32,
        );
        fn mm_path_length(ch: *const MmChannel, fx: f32, fy: f32, tx: f32, ty: f32) -> f32;
        fn mm_corridor_clear(ch: *const MmChannel, ax: f32, ay: f32, bx: f32, by: f32) -> bool;
        fn mm_line_of_sight(ch: *const MmChannel, ax: f32, ay: f32, bx: f32, by: f32) -> bool;
        fn mm_disc_free(ch: *const MmChannel, x: f32, y: f32, radius: f32) -> bool;
        fn mm_route_waypoints(
            ch: *const MmChannel,
            fx: f32,
            fy: f32,
            tx: f32,
            ty: f32,
            out: *mut f32,
            cap: i32,
        ) -> i32;
        fn mm_payload_pos(capture: f32, out: *mut f32);
        fn mm_point_free(ch: *const MmChannel, x: f32, y: f32) -> bool;
        fn mm_point_seg_dist(px: f32, py: f32, ax: f32, ay: f32, bx: f32, by: f32) -> f32;
        fn mm_normalize_degrees(deg: f32) -> f32;
        fn mm_diff_degrees(a: f32, b: f32) -> f32;
    }

    /// A handle on the shipped arena, leaked for the process. The graph is a
    /// `OnceLock` anyway, so there is nothing to tear down between tests and an
    /// O(V^3) solve per test would dominate the suite.
    ///
    /// Built directly rather than through `mm_channel_open` + `mm_channel_handshake`:
    /// the navigation surface only needs a config, and making every one of these
    /// O(n^2) conformance pairs pay for a fake engine playing a handshake would buy
    /// nothing. The channel is real -- an `EngineChannel`'s own tempfile, so the
    /// mapping is the shape `BotChannel` expects -- it simply never carries a message.
    /// The real open/handshake path is what `a_channel_completes_a_match_through_the_c_surface`
    /// exercises.
    fn channel() -> *const MmChannel {
        use std::sync::OnceLock;
        static CH: OnceLock<usize> = OnceLock::new();
        *CH.get_or_init(|| {
            let engine = Box::leak(Box::new(crate::ipc::EngineChannel::new().unwrap()));
            let handle = Box::new(MmChannel {
                rt: tokio::runtime::Builder::new_current_thread()
                    .enable_time()
                    .build()
                    .unwrap(),
                chan: BotChannel::from_path(engine.backing_file_path()).unwrap(),
                config: Some(conf().clone()),
                state: None,
            });
            // The same call `BotChannel::handshake` would have made.
            topology::init_topology(&conf().map, conf().bot.radius);
            Box::into_raw(handle) as usize
        }) as *const MmChannel
    }

    /// The conformance property: for every pair of standable points, each entry point
    /// agrees with the safe Rust function it wraps.
    ///
    /// Asserted against `game::topology` rather than against recorded numbers on
    /// purpose -- `MAP_ART` is still being edited, and a pinned distance would fail on
    /// every map change while catching nothing about the ABI.
    #[test]
    fn every_entry_point_agrees_with_the_safe_rust_api() {
        let ch = channel();
        let conf = conf();
        let pts = sample_free_points(40);

        for (i, &a) in pts.iter().enumerate() {
            assert_eq!(
                unsafe { mm_disc_free(ch, a.x, a.y, conf.bot.radius) },
                topology::disc_free(conf, a, conf.bot.radius),
                "disc_free disagrees at {a:?}"
            );
            assert_eq!(
                unsafe { mm_point_free(ch, a.x, a.y) },
                topology::point_free(conf, a),
                "point_free disagrees at {a:?}"
            );

            for &b in &pts[i + 1..] {
                assert_eq!(
                    unsafe { mm_corridor_clear(ch, a.x, a.y, b.x, b.y) },
                    topology::corridor_clear(conf, a, b),
                    "corridor_clear disagrees on {a:?} -> {b:?}"
                );
                assert_eq!(
                    unsafe { mm_line_of_sight(ch, a.x, a.y, b.x, b.y) },
                    topology::has_line_of_sight(conf, a, b),
                    "line_of_sight disagrees on {a:?} -> {b:?}"
                );

                let want = topology::path_length(conf, a, b).unwrap_or(-1.0);
                let got = unsafe { mm_path_length(ch, a.x, a.y, b.x, b.y) };
                assert_eq!(got, want, "path_length disagrees on {a:?} -> {b:?}");

                let mut out = [0.0f32; 64];
                let n = unsafe {
                    mm_route_waypoints(ch, a.x, a.y, b.x, b.y, out.as_mut_ptr(), 32)
                };
                match topology::route_waypoints(conf, a, b) {
                    None => assert_eq!(n, -1, "route_waypoints should report no route"),
                    Some(route) => {
                        assert_eq!(n, route.len() as i32, "wrong waypoint count");
                        for (k, v) in route.iter().take(32).enumerate() {
                            assert_eq!((out[k * 2], out[k * 2 + 1]), (v.x, v.y));
                        }
                    }
                }

                let mut nav = [0.0f32; 2];
                unsafe { mm_navigate_to(ch, a.x, a.y, b.x, b.y, nav.as_mut_ptr()) };
                let want = topology::navigate_to(conf, a, b);
                assert_eq!((nav[0], nav[1]), (want.x, want.y), "navigate_to disagrees");

                // Three points, so the segment is `a -> b` and the query point is the
                // next sample along. No handle on this one -- pure geometry.
                let p = pts[(i + 1) % pts.len()];
                assert_eq!(
                    unsafe { mm_point_seg_dist(p.x, p.y, a.x, a.y, b.x, b.y) },
                    topology::point_seg_dist(p, a, b),
                    "point_seg_dist disagrees for {p:?} against {a:?} -> {b:?}"
                );
            }
        }
    }

    /// The rules that are not navigation: the payload's arc-length walk and the two
    /// angle helpers.
    ///
    /// Same shape as the conformance test above -- asserted against the safe function each
    /// one wraps, never against a recorded number, because every one of these reads config
    /// values that are still being tuned.
    #[test]
    fn the_game_rule_entry_points_agree_with_the_safe_rust_api() {
        let ch = channel();
        let conf = conf();

        // `capture` is clamped to [-1, 1] and the ends are the interesting cases, so walk
        // past both of them.
        for step in -24i32..=24 {
            let t = step as f32 / 20.0;
            let mut out = [0.0f32; 2];
            unsafe { mm_payload_pos(t, out.as_mut_ptr()) };
            let want = crate::game::config::payload_position(t);
            assert_eq!((out[0], out[1]), (want.x, want.y), "payload_pos disagrees at {t}");
        }

        for step in -40i32..=40 {
            let deg = step as f32 * 17.5;
            assert_eq!(
                unsafe { mm_normalize_degrees(deg) },
                crate::game::util::normalize_degrees(deg),
                "normalize_degrees disagrees at {deg}"
            );
            for other in [-181.0f32, -90.0, 0.0, 17.0, 180.0, 359.0] {
                assert_eq!(
                    unsafe { mm_diff_degrees(deg, other) },
                    crate::game::util::diff_degrees(deg, other),
                    "diff_degrees disagrees for {deg} -> {other}"
                );
            }
        }
    }


    /// A point outside every wall but enclosed by the map's padding border has no route
    /// anywhere, which is the `None` arm both of these have to report distinctly.
    #[test]
    fn an_unreachable_target_is_reported_not_faked() {
        let ch = channel();
        let conf = conf();
        // Dead centre of a wall tile: nowhere a bot can stand, so no route reaches it.
        let wall = (0..MAP_SIZE)
            .flat_map(|x| (0..MAP_SIZE).map(move |y| (x, y)))
            .find(|&(x, y)| conf.map[x][y] == MapTile::Wall)
            .map(|(x, y)| (x as f32 + 0.5, y as f32 + 0.5))
            .expect("the shipped map has walls");

        let from = sample_free_points(1)[0];
        assert!(topology::path_length(conf, from, Vec2::new(wall.0, wall.1)).is_none());

        let len = unsafe { mm_path_length(ch, from.x, from.y, wall.0, wall.1) };
        assert_eq!(len, -1.0, "no route must be -1.0, distinct from NaN-on-panic");
        assert!(!len.is_nan(), "no route must not look like a panic");

        let mut out = [0.0f32; 64];
        let n = unsafe {
            mm_route_waypoints(ch, from.x, from.y, wall.0, wall.1, out.as_mut_ptr(), 32)
        };
        assert_eq!(n, -1, "no route must be -1, distinct from -2-on-panic");
    }

    #[test]
    fn a_clear_sightline_needs_no_waypoints() {
        let ch = channel();
        let conf = conf();
        let pts = sample_free_points(40);
        let (a, b) = pts
            .iter()
            .enumerate()
            .flat_map(|(i, &a)| pts[i + 1..].iter().map(move |&b| (a, b)))
            .find(|&(a, b)| topology::corridor_clear(conf, a, b))
            .expect("some pair of sampled points sees each other");

        let mut out = [7.0f32; 4];
        let n = unsafe { mm_route_waypoints(ch, a.x, a.y, b.x, b.y, out.as_mut_ptr(), 2) };
        assert_eq!(n, 0, "a direct line of sight is zero interior waypoints");
        assert_eq!(out, [7.0; 4], "nothing should have been written");
    }

    /// The short-buffer contract: the full count comes back even though only `cap`
    /// pairs were written, so a caller sees `returned > cap` and retries bigger rather
    /// than walking a silently truncated plan.
    #[test]
    fn a_short_buffer_reports_the_full_count_and_writes_no_further() {
        let ch = channel();
        let conf = conf();
        let pts = sample_free_points(60);
        let (a, b, route) = pts
            .iter()
            .enumerate()
            .flat_map(|(i, &a)| pts[i + 1..].iter().map(move |&b| (a, b)))
            .find_map(|(a, b)| {
                topology::route_waypoints(conf, a, b)
                    .filter(|r| r.len() >= 2)
                    .map(|r| (a, b, r))
            })
            .expect("some sampled pair routes around a wall");

        const GUARD: f32 = -999.0;
        let mut out = [GUARD; 16];
        let n = unsafe { mm_route_waypoints(ch, a.x, a.y, b.x, b.y, out.as_mut_ptr(), 1) };

        assert_eq!(n, route.len() as i32, "must report the full count, not the written one");
        assert!(n > 1, "this pair was chosen to overflow a cap of 1");
        assert_eq!((out[0], out[1]), (route[0].x, route[0].y));
        assert!(
            out[2..].iter().all(|&f| f == GUARD),
            "wrote past `cap` into the caller's buffer"
        );
    }

    /// `ctypes` hands over whatever it has, including the `None` a failed constructor
    /// returned. Every entry point has to survive that rather than take the bot down.
    #[test]
    fn a_null_handle_returns_a_sentinel_instead_of_crashing() {
        let null: *const MmChannel = std::ptr::null();
        unsafe {
            assert!(mm_path_length(null, 1.0, 1.0, 2.0, 2.0).is_nan());
            assert!(!mm_corridor_clear(null, 1.0, 1.0, 2.0, 2.0));
            assert!(!mm_line_of_sight(null, 1.0, 1.0, 2.0, 2.0));
            assert!(!mm_disc_free(null, 1.0, 1.0, 0.25));
            assert!(!mm_point_free(null, 1.0, 1.0));
            assert_eq!(mm_route_waypoints(null, 1.0, 1.0, 2.0, 2.0, std::ptr::null_mut(), 0), -2);

            // `mm_payload_pos` takes no handle, so its only caller mistake is a null out
            // pointer -- a no-op, since `void` has nowhere to report one.
            mm_payload_pos(0.5, std::ptr::null_mut());

            let mut nav = [0.0f32; 2];
            mm_navigate_to(null, 1.0, 1.0, 2.0, 2.0, nav.as_mut_ptr());
            assert!(nav[0].is_nan() && nav[1].is_nan(), "out must be left as NaN");

            // Null is a documented no-op, not a double-free.
            mm_channel_free(std::ptr::null_mut());

            // The channel surface, same rule.
            let mut team = 0u8;
            assert_eq!(mm_channel_handshake(null as *mut MmChannel, &mut team), MM_PANIC);
            assert_eq!(mm_channel_await_tick(null as *mut MmChannel), MM_PANIC);
            assert!(mm_channel_config(null).is_null());
            assert!(mm_channel_state(null).is_null());
            assert_eq!(
                mm_channel_respond(null as *mut MmChannel, std::ptr::null()),
                MM_PANIC
            );

            assert!(mm_channel_open(std::ptr::null(), 4).is_null());
            assert!(mm_channel_open(b"/x".as_ptr(), -1).is_null());
        }
    }

    /// A path that is not a channel. `ctypes` will hand over whatever string the bot was
    /// launched with, and a typo has to be a `NULL` rather than a mapping of something
    /// else.
    #[test]
    fn opening_a_path_that_is_not_a_channel_returns_null() {
        let missing = "/nonexistent/mechmania/channel";
        let ch =
            unsafe { mm_channel_open(missing.as_ptr(), missing.len() as i32) };
        assert!(ch.is_null(), "a missing backing file must not produce a handle");
    }

    /// Before the handshake there is no config, so there is nothing for a navigation
    /// query to answer with. Each reports its own documented failure value rather than
    /// guessing -- the same values a null handle gets, because it is the same answer.
    #[test]
    fn a_navigation_query_before_the_handshake_is_a_sentinel() {
        let engine = crate::ipc::EngineChannel::new().unwrap();
        let path = engine.backing_file_path().to_str().unwrap();
        let ch = unsafe { mm_channel_open(path.as_ptr(), path.len() as i32) };
        assert!(!ch.is_null(), "opening a real backing file should work");

        unsafe {
            assert!(mm_channel_config(ch).is_null(), "no config before the handshake");
            assert!(mm_channel_state(ch).is_null(), "no state before the first tick");
            assert!(mm_path_length(ch, 1.0, 1.0, 2.0, 2.0).is_nan());
            assert!(!mm_corridor_clear(ch, 1.0, 1.0, 2.0, 2.0));
            assert!(!mm_line_of_sight(ch, 1.0, 1.0, 2.0, 2.0));
            assert!(!mm_disc_free(ch, 1.0, 1.0, 0.25));
            assert_eq!(mm_route_waypoints(ch, 1.0, 1.0, 2.0, 2.0, std::ptr::null_mut(), 0), -2);

            let mut nav = [0.0f32; 2];
            mm_navigate_to(ch, 1.0, 1.0, 2.0, 2.0, nav.as_mut_ptr());
            assert!(nav[0].is_nan() && nav[1].is_nan());

            mm_channel_free(ch);
        }
    }

    /// The whole protocol through the C ABI: open, handshake, one tick, and the close
    /// that ends the match.
    ///
    /// The bot side runs on a `std::thread` because every entry point `block_on`s the
    /// handle's own runtime, and a `block_on` inside another runtime panics -- which is
    /// also exactly the constraint a Python bot is under, and why `channel.py` will have
    /// nothing async in it. The engine side is the real `EngineChannel`, so what is being
    /// checked is the two implementations agreeing over the mapping, not a mock.
    #[test]
    fn a_channel_completes_a_match_through_the_c_surface() {
        use crate::ipc::{EngineChannel, HandshakeProtocol, TickProtocol, HANDSHAKE_FINGERPRINT};
        use crate::game::state::GameState;
        use crate::game::team::Team;
        use std::time::Duration;

        let engine = EngineChannel::new().unwrap();
        let path = engine.backing_file_path().to_str().unwrap().to_owned();

        let bot = std::thread::spawn(move || {
            let ch = unsafe { mm_channel_open(path.as_ptr(), path.len() as i32) };
            assert!(!ch.is_null(), "mm_channel_open failed on a live channel");

            let mut team = 0xffu8;
            assert_eq!(unsafe { mm_channel_handshake(ch, &mut team) }, MM_OK);
            assert!(!unsafe { mm_channel_config(ch) }.is_null(), "the config should survive");

            assert_eq!(unsafe { mm_channel_await_tick(ch) }, MM_OK);
            let state = unsafe { mm_channel_state(ch) };
            assert!(!state.is_null(), "MM_OK must come with a state");
            let tick = unsafe { (*state).tick };

            // Burn a known amount of CPU inside the window, so the reported time is
            // unambiguously the caller's think time rather than noise.
            let start = cpu_time::ProcessTime::now();
            while start.elapsed() < Duration::from_millis(20) {}

            let mut action = FleetAction::default();
            action.rush_order = true;
            assert_eq!(unsafe { mm_channel_respond(ch, &action) }, MM_OK);

            // The engine closes after this; the end of a match is not an error.
            assert_eq!(unsafe { mm_channel_await_tick(ch) }, MM_CLOSED);
            assert!(unsafe { mm_channel_state(ch) }.is_null(), "a closed channel has no state");

            unsafe { mm_channel_free(ch) };
            (team, tick)
        });

        let rt = tokio::runtime::Runtime::new().unwrap();
        let conf = conf();
        let state = GameState::new(conf);
        let (magic, action, cpu_time) = rt.block_on(async {
            let request = crate::ipc::HandshakeRequest {
                team: Team::Other,
                config: conf.clone(),
            };
            let (magic, _) = engine
                .request::<HandshakeProtocol>(&request, Some(Duration::from_secs(10)))
                .await
                .expect("the handshake should complete");
            let (action, cpu_time) = engine
                .request::<TickProtocol>(&state, Some(Duration::from_secs(10)))
                .await
                .expect("the tick should complete");
            engine.close();
            (magic, action, cpu_time)
        });

        let (team, tick) = bot.join().expect("the bot thread should not panic");

        assert_eq!(
            magic, HANDSHAKE_FINGERPRINT,
            "the bot answered the handshake wrong"
        );
        assert_eq!(team, Team::Other as u8, "the bot read the wrong team");
        assert_eq!(tick, state.tick, "the bot saw a different state than the engine sent");
        assert!(action.rush_order, "the action did not round-trip");
        assert!(
            cpu_time >= Duration::from_millis(10),
            "reported cpu_time was implausibly small: {cpu_time:?}",
        );
    }

    /// Two entry points that do nothing but panic, so the sentinel path is exercised
    /// deterministically rather than by finding an input that happens to trip an
    /// assertion somewhere in `topology`.
    mod panic_probe {
        use super::mm_ffi_fn;

        #[mm_ffi_fn]
        pub fn mm_test_panics_f32() -> f32 {
            panic!("deliberate");
        }

        #[mm_ffi_fn(panic = -2)]
        pub fn mm_test_panics_i32() -> i32 {
            panic!("deliberate");
        }
    }

    extern "C" {
        fn mm_test_panics_f32() -> f32;
        fn mm_test_panics_i32() -> i32;
    }

    /// The sentinels are only worth anything if `catch_unwind` is really in the path.
    /// A panic crossing into C is undefined behaviour, so "it did not crash" is not
    /// enough -- the wrapper has to return the sentinel it promised.
    #[test]
    fn a_panic_becomes_a_sentinel_rather_than_unwinding_into_c() {
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let f = unsafe { mm_test_panics_f32() };
        let i = unsafe { mm_test_panics_i32() };
        std::panic::set_hook(prev);

        assert!(f.is_nan(), "default f32 sentinel should be NaN, got {f}");
        assert_eq!(i, -2, "the overridden sentinel should win over the -1 default");
    }

    /// The registrations the session-3 binding generator reads. A macro that silently
    /// stopped emitting them would be invisible until that session.
    #[test]
    fn every_entry_point_registers_itself_for_codegen() {
        let fns: Vec<&str> = inventory::iter::<FfiFnDesc>().map(|d| d.name).collect();
        for want in [
            "mm_channel_open",
            "mm_channel_handshake",
            "mm_channel_config",
            "mm_channel_await_tick",
            "mm_channel_state",
            "mm_channel_respond",
            "mm_navigate_to",
            "mm_path_length",
            "mm_corridor_clear",
            "mm_line_of_sight",
            "mm_disc_free",
            "mm_route_waypoints",
            "mm_payload_pos",
            "mm_point_free",
            "mm_point_seg_dist",
            "mm_normalize_degrees",
            "mm_diff_degrees",
        ] {
            assert!(fns.contains(&want), "{want} is not registered; found {fns:?}");
        }

        let handles: Vec<&str> = inventory::iter::<FfiHandleDesc>().map(|d| d.name).collect();
        assert_eq!(handles, ["MmChannel"]);
        let free = inventory::iter::<FfiHandleDesc>().next().unwrap().free;
        assert_eq!(free, "mm_channel_free");

        // The descriptor has to describe the signature, not just name it -- that is what
        // the generator turns into `argtypes`.
        let pl = inventory::iter::<FfiFnDesc>()
            .find(|d| d.name == "mm_path_length")
            .unwrap();
        assert_eq!(pl.ret, FfiType::F32);
        assert_eq!(
            pl.args[0],
            ("ch", FfiType::Ptr { pointee: "MmChannel", mutable: false })
        );
        assert_eq!(pl.args[1], ("fx", FfiType::F32));

        // The count, not just the membership. `inventory` registers through link-section
        // statics, and a linker that drops an object file nothing references drops the
        // registrations in it silently -- the same failure that makes `native/src/lib.rs`'s
        // re-export load-bearing. `mm-ffi-codegen` links this crate as an rlib and would
        // then emit a short binding set with nothing to complain about, so the size of the
        // registry is asserted here where a shortfall is unambiguous.
        //
        // The `mm_test_panics_*` probes below register too, hence the two extra.
        assert_eq!(fns.len(), 18 + 2, "registered entry points: {fns:?}");
    }
}

#[cfg(test)]
mod mirror_test {
    use super::*;
    use crate::game::config::MAP_SIZE;
    use crate::game::mirror::{FfiConstDesc, FfiKind, FfiMirrorDesc};
    use crate::game::state::{SpecialState, TurnAction};
    use std::collections::{BTreeMap, BTreeSet};

    fn registry() -> BTreeMap<&'static str, &'static FfiMirrorDesc> {
        let mut out = BTreeMap::new();
        for desc in inventory::iter::<FfiMirrorDesc> {
            assert!(
                out.insert(desc.name, desc).is_none(),
                "{} is registered twice",
                desc.name
            );
        }
        out
    }

    fn desc(name: &str) -> &'static FfiMirrorDesc {
        desc_of(name)
    }

    fn desc_of(name: &str) -> &'static FfiMirrorDesc {
        registry()
            .get(name)
            .unwrap_or_else(|| panic!("{name} is not registered"))
    }

    fn fields(name: &str) -> &'static [crate::game::mirror::FfiFieldDesc] {
        match desc(name).kind {
            FfiKind::Struct { fields } => fields,
            _ => panic!("{name} is not a struct"),
        }
    }

    fn field(name: &str, field: &str) -> &'static crate::game::mirror::FfiFieldDesc {
        fields(name)
            .iter()
            .find(|f| f.name == field)
            .unwrap_or_else(|| panic!("{name} has no field {field}"))
    }

    fn variants(name: &str) -> &'static [crate::game::mirror::FfiVariantDesc] {
        match desc(name).kind {
            FfiKind::UnitEnum { variants } | FfiKind::DataEnum { variants, .. } => variants,
            _ => panic!("{name} is not an enum"),
        }
    }

    /// The whole wire closure, plus the generic instantiations that cannot register
    /// themselves. A type added to a payload and not to this list is a type the generator
    /// will not emit -- and Python would then read the field after it at the wrong offset.
    #[test]
    fn every_wire_type_registers_itself() {
        let expected: BTreeSet<&str> = [
            // state.rs -- note `StateOption` and `TeamPair` themselves are absent: a
            // generic type cannot register, so only its instantiations appear.
            "BotState",
            "SpecialState",
            "MoveAction",
            "TurnAction",
            "SpecialAction",
            "BotAction",
            "BotClass",
            "FleetAction",
            "BotArray",
            "Deposit",
            "FabricatorState",
            "GameState",
            // config.rs
            "MapTile",
            "FabricatorConfig",
            "BotConfig",
            "PayloadConfig",
            "DepositConfig",
            "GameConfig",
            // util.rs / team.rs / ipc.rs
            "Vec2",
            "Team",
            "HandshakeRequest",
            // the explicit instantiations
            "StateOption<Vec2>",
            "StateOption<BotId>",
            "StateOption<Team>",
            "TeamPair<u32>",
        ]
        .into_iter()
        .collect();

        let actual: BTreeSet<&str> = registry().keys().copied().collect();
        assert_eq!(actual, expected);
    }

    /// Every type a field names has to be something the generator can emit: a registered
    /// type, a registered alias, or an ABI primitive. This is what makes a forgotten
    /// `StateOption<..>` instantiation a failure here rather than mangled Python later.
    #[test]
    fn every_field_type_resolves() {
        const PRIMITIVES: &[&str] = &["f32", "u32", "i32", "u8", "u64", "bool"];

        let registry = registry();
        let aliases: BTreeSet<&str> = inventory::iter::<FfiAliasDesc>
            .into_iter()
            .map(|a| a.name)
            .collect();

        /// Peel `[T; N]` down to its element spelling. Lengths need no constant table:
        /// the generator divides the field's measured size by the element's.
        fn element(mut ty: &str) -> &str {
            while let Some(inner) = ty.strip_prefix('[') {
                ty = inner.rsplit_once(';').expect("an array spells its length").0;
            }
            ty
        }

        for desc in registry.values() {
            let all = match desc.kind {
                FfiKind::Struct { fields } => fields.to_vec(),
                FfiKind::UnitEnum { .. } => vec![],
                FfiKind::DataEnum { variants, .. } => {
                    variants.iter().flat_map(|v| v.fields.to_vec()).collect()
                }
            };
            for f in all {
                let ty = element(f.ty);
                // A generic impl's own fields are spelled with its parameters; the
                // registered instantiations are what carry resolvable spellings.
                // A registered instantiation spells its fields with the generic
                // parameters; substitute the argument it was registered with, which is
                // what the generator does too.
                let ty = match desc.generic_params.iter().position(|p| *p == ty) {
                    Some(i) => desc.generic_args[i],
                    None => ty,
                };
                assert!(
                    PRIMITIVES.contains(&ty) || registry.contains_key(ty) || aliases.contains(ty),
                    "{}::{} is a `{}`, which resolves to nothing -- add it to the derive, \
                     to `mm_ffi_instantiate!` or to `mm_ffi_alias!`",
                    desc.name,
                    f.name,
                    f.ty,
                );
            }
        }
    }

    /// The length constants, pinned. The generator shapes every `ctypes` array from these,
    /// so a change to `BOTS_MAX` should read as a deliberate edit here rather than surface
    /// as a silently reshaped Python array.
    #[test]
    fn the_constants_are_the_ones_the_source_declares() {
        let consts: BTreeMap<&str, usize> = inventory::iter::<FfiConstDesc>
            .into_iter()
            .map(|c| (c.name, c.value))
            .collect();

        assert_eq!(consts["BOTS_MAX"], 32);
        assert_eq!(consts["MAP_SIZE"], 32);
        assert_eq!(consts["PAYLOAD_PATH_LEN"], 7);

        // The status codes share the registry, because they share a destination: a
        // module-level integer in the generated bindings. Pinned for the same reason --
        // Python matches on these values, so a renumbering is a breaking change.
        assert_eq!(consts["MM_OK"], 0);
        assert_eq!(consts["MM_CLOSED"], 1);
        assert_eq!(consts["MM_TIMEOUT"], 2);
        assert_eq!(consts["MM_MALFORMED"], 3);
        assert_eq!(consts["MM_IO"], 4);
        assert_eq!(consts["MM_PANIC"], 5);

        // The budget limits a bot compares `mm_channel_budget`'s answer against. Pinned to
        // the `ipc` constants rather than to literals: the point of emitting them at all is
        // that Python and the engine cannot disagree about the bank.
        assert_eq!(consts["COMPUTE_BANK_TICKS"], crate::ipc::COMPUTE_BANK_TICKS as usize);
        assert_eq!(consts["COMPUTE_REFILL_TICKS"], crate::ipc::COMPUTE_REFILL_TICKS as usize);

        assert_eq!(consts.len(), 11);
    }

    /// The cross-check that keeps size division honest. A declared length is only useful if
    /// it agrees with the bytes: for every array field, the product of its dimensions times
    /// its element's size has to be the size the derive measured. A forgotten constant, a
    /// typo'd value, or a dimension read in the wrong order fails here rather than shaping a
    /// generated array wrong.
    #[test]
    fn every_array_length_resolves_and_agrees_with_the_measured_size() {
        let registry = registry();
        let consts: BTreeMap<&str, usize> = inventory::iter::<FfiConstDesc>
            .into_iter()
            .map(|c| (c.name, c.value))
            .collect();
        let aliases: BTreeMap<&str, &FfiAliasDesc> = inventory::iter::<FfiAliasDesc>
            .into_iter()
            .map(|a| (a.name, a))
            .collect();

        /// `[[MapTile; MAP_SIZE]; MAP_SIZE]` -> (`MapTile`, ["MAP_SIZE", "MAP_SIZE"]),
        /// outermost dimension first. Mirrors what the generator parses.
        fn peel(mut ty: &str) -> (&str, Vec<&str>) {
            let mut dims = Vec::new();
            while let Some(inner) = ty.strip_prefix('[') {
                let (elem, len) = inner.rsplit_once(';').expect("an array spells its length");
                dims.push(len.trim().trim_end_matches(']').trim());
                ty = elem;
            }
            (ty, dims)
        }

        /// The measured size of a resolved element spelling.
        fn size_of_element(
            ty: &str,
            registry: &BTreeMap<&'static str, &'static FfiMirrorDesc>,
            aliases: &BTreeMap<&str, &FfiAliasDesc>,
        ) -> usize {
            match ty {
                "u8" | "bool" => 1,
                "f32" | "u32" | "i32" => 4,
                "u64" => 8,
                _ => registry
                    .get(ty)
                    .map(|d| d.size)
                    .or_else(|| aliases.get(ty).map(|a| a.size))
                    .unwrap_or_else(|| panic!("{ty} resolves to nothing")),
            }
        }

        let mut checked = 0;
        for desc in registry.values() {
            let all = match desc.kind {
                FfiKind::Struct { fields } => fields.to_vec(),
                FfiKind::UnitEnum { .. } => vec![],
                FfiKind::DataEnum { variants, .. } => {
                    variants.iter().flat_map(|v| v.fields.to_vec()).collect()
                }
            };
            for f in all {
                // A field may name an alias that is itself an array (`map: Map`), so
                // resolve the spelling before peeling it.
                let spelling = match desc.generic_params.iter().position(|p| *p == f.ty) {
                    Some(i) => desc.generic_args[i],
                    None => f.ty,
                };
                let spelling = match aliases.get(spelling) {
                    Some(a) if a.target.starts_with('[') => a.target,
                    _ => spelling,
                };
                let (elem, dims) = peel(spelling);
                if dims.is_empty() {
                    continue;
                }
                let count: usize = dims
                    .iter()
                    .map(|d| {
                        *consts.get(d).unwrap_or_else(|| {
                            panic!(
                                "{}::{} is a `{}`, whose length `{d}` is not registered -- \
                                 add it to `mm_ffi_const!`",
                                desc.name, f.name, f.ty,
                            )
                        })
                    })
                    .product();
                assert_eq!(
                    count * size_of_element(elem, &registry, &aliases),
                    f.size,
                    "{}::{} is a `{}`: {count} x `{elem}` does not measure {} bytes",
                    desc.name,
                    f.name,
                    f.ty,
                    f.size,
                );
                checked += 1;
            }
        }
        assert!(checked >= 5, "only {checked} array fields found; the walk is not reaching them");
    }

    /// Both aliases in the closure resolve to something real, and to the right size.
    #[test]
    fn aliases_are_registered_with_their_measured_size() {
        let aliases: BTreeMap<&str, &FfiAliasDesc> = inventory::iter::<FfiAliasDesc>
            .into_iter()
            .map(|a| (a.name, a))
            .collect();

        assert_eq!(aliases["BotId"].target, "u8");
        assert_eq!(aliases["BotId"].size, 1);
        assert_eq!(aliases["Map"].target, "[[MapTile; MAP_SIZE]; MAP_SIZE]");
        assert_eq!(aliases["Map"].size, MAP_SIZE * MAP_SIZE);
        assert_eq!(aliases.len(), 2);
    }

    /// Pinned by hand so a `repr` regression fails loudly. The derive's own shadow-layout
    /// asserts are compile-time and cover the *model*; these cover the numbers Python will
    /// be generated against.
    #[test]
    fn the_pinned_layouts_are_what_python_will_be_generated_against() {
        assert_eq!((desc("Vec2").size, desc("Vec2").align), (8, 4));
        assert_eq!(field("Vec2", "x").offset, 0);
        assert_eq!(field("Vec2", "y").offset, 4);

        // A one-byte tag, not the four a plain `#[repr(C)]` enum would give it.
        let FfiKind::DataEnum { payload_offset, .. } = desc("TurnAction").kind else {
            panic!("TurnAction is a data enum")
        };
        assert_eq!((desc("TurnAction").size, payload_offset), (12, 4));
        assert_eq!(desc("SpecialState").size, 20);

        // `BotClass` is the one that shrank, 4 bytes to 1.
        assert_eq!((desc("BotClass").size, desc("BotClass").align), (1, 1));

        // ... which moves what follows it in `FleetAction`.
        assert_eq!(
            field("FleetAction", "fabricator_next").offset + 1,
            field("FleetAction", "rush_order").offset
        );
    }

    /// Tag bytes are what `Validate` will check an incoming action against and what the
    /// fingerprint hashes, so they are pinned rather than assumed.
    #[test]
    fn tags_are_the_discriminants_the_source_declares() {
        let state_option: Vec<(&str, u8)> = variants("StateOption<Vec2>")
            .iter()
            .map(|v| (v.name, v.tag))
            .collect();
        assert_eq!(state_option, vec![("None", 0), ("Some", 1)]);

        let teams: Vec<u8> = variants("Team").iter().map(|v| v.tag).collect();
        assert_eq!(teams, vec![0, 1]);

        let classes: Vec<u8> = variants("BotClass").iter().map(|v| v.tag).collect();
        assert_eq!(classes, vec![0, 1, 2]);
    }

    /// The point of registering instantiations separately: each one is measured for its
    /// own monomorphization, so the payload offsets legitimately differ.
    #[test]
    fn each_instantiation_carries_its_own_measured_layout() {
        let byte = desc("StateOption<BotId>");
        let vec2 = desc("StateOption<Vec2>");

        assert_eq!((byte.size, byte.align), (2, 1));
        assert_eq!((vec2.size, vec2.align), (12, 4));

        let (FfiKind::DataEnum { payload_offset: b, .. }, FfiKind::DataEnum { payload_offset: v, .. }) =
            (byte.kind, vec2.kind)
        else {
            panic!("both are data enums")
        };
        assert_eq!((b, v), (1, 4), "the payload sits after the tag's alignment padding");

        // Each instantiation carries the parameter it substitutes and what it
        // substituted, which is how the generator turns a field spelled `T` into `Vec2`.
        assert_eq!(vec2.generic_params, &["T"]);
        assert_eq!(vec2.generic_args, &["Vec2"]);
        assert_eq!(byte.generic_args, &["BotId"]);
    }

    /// The shadow-layout trick verified against reality, not against itself. `offset_of!`
    /// cannot reach into an enum variant on stable, so the derive models what the Reference
    /// says `#[repr(u8, C)]` is and measures the model; this reads the bytes of a real
    /// value and confirms the tag and the payload are where the descriptor claims.
    #[test]
    fn a_recorded_offset_finds_the_real_payload_in_real_bytes() {
        fn bytes<T>(value: &T) -> &[u8] {
            unsafe { std::slice::from_raw_parts(value as *const T as *const u8, size_of::<T>()) }
        }
        fn read_f32(raw: &[u8], at: usize) -> f32 {
            f32::from_ne_bytes(raw[at..at + 4].try_into().unwrap())
        }
        fn variant(ty: &str, name: &str) -> &'static crate::game::mirror::FfiVariantDesc {
            variants(ty).iter().find(|v| v.name == name).expect("a declared variant")
        }

        let turn = TurnAction::TargetPosition { pos: Vec2::new(3.5, -7.25) };
        let raw = bytes(&turn);
        let desc = variant("TurnAction", "TargetPosition");
        assert_eq!(raw[0], desc.tag, "the tag is the first byte");
        let pos = desc.fields.iter().find(|f| f.name == "pos").unwrap();
        assert_eq!(read_f32(raw, pos.offset), 3.5);
        assert_eq!(read_f32(raw, pos.offset + 4), -7.25);

        // A variant whose payload is itself a tagged enum -- the case `Validate` has to
        // walk, and the one a wrong payload offset would corrupt silently.
        let special = SpecialState::Healer { healing: StateOption::Some(7u8) };
        let raw = bytes(&special);
        let desc = variant("SpecialState", "Healer");
        assert_eq!(raw[0], desc.tag);
        let healing = desc.fields.iter().find(|f| f.name == "healing").unwrap();
        assert_eq!(healing.ty, "StateOption<BotId>");
        let inner = desc_of("StateOption<BotId>");
        let FfiKind::DataEnum { payload_offset, .. } = inner.kind else {
            panic!("StateOption is a data enum")
        };
        assert_eq!(raw[healing.offset], 1, "the inner tag says `Some`");
        assert_eq!(raw[healing.offset + payload_offset], 7, "and the id follows it");
    }

    /// An `ffi` build is a `client` build, which is the naming competitors are handed.
    /// The fingerprint excludes names precisely because this differs per build.
    #[test]
    fn a_client_build_records_the_relative_team_naming() {
        let names: Vec<&str> = fields("GameState").iter().map(|f| f.name).collect();
        assert!(names.contains(&"fleet_me"), "got {names:?}");
        assert!(names.contains(&"fleet_other"), "got {names:?}");

        let teams: Vec<&str> = variants("Team").iter().map(|v| v.name).collect();
        assert_eq!(teams, vec!["Me", "Other"]);
    }
}
