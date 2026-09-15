pub mod config;
pub mod state;
// Layout metadata for the wire types, read by the Python binding generator, the action
// validator and the handshake fingerprint. Shared (hardlinked) and compiled in every build
// -- `ffi` implies `client`, but the validator that consumes this runs engine-side.
pub mod mirror;
#[cfg(feature = "engine")]
pub mod diff;
pub mod team;
// Simulation logic (tick evaluation, collision, blaster/healer mechanics). Not part of
// the shared-source set hardlinked into bot crates, and it hardcodes the engine's
// absolute `Team::A`/`Team::B` naming -- it cannot compile under `client`'s relative
// `Team::Me`/`Team::Other` renaming (see `mm_macros::teams` in `team.rs`).
#[cfg(feature = "engine")]
pub mod action;
pub mod util;
// Only consumed by `action`, and for the same reason (absolute `Team::A`/`Team::B`
// naming) can't compile under `client`.
#[cfg(feature = "engine")]
pub mod geom;
pub mod topology;
