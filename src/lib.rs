// `engine` renames the shared team-generic items absolutely (`Team::A`/`Team::B`) and
// `client` renames them relatively (`Team::Me`/`Team::Other`). Both at once is not a
// richer build, it is a silently wrong one: `mm_macros::teams` drops the module's
// trailing attributes, so the first `cfg_attr` wins and a `client` consumer gets
// `fleet_a`/`fleet_b` -- identical layout, wrong names, nothing fails. Build the FFI
// surface with `--no-default-features --features ffi`.
#[cfg(all(feature = "engine", feature = "client"))]
compile_error!(
    "features `engine` and `client` are mutually exclusive; \
     build the FFI surface with `--no-default-features --features ffi`"
);

pub mod game;
pub mod ipc;
pub mod args;
// The match-running engine itself: spawns bot processes, drives the tick loop via
// `game::action::eval_tick`, and names teams absolutely (`Team::A`/`Team::B`). Not part
// of the shared/client-side surface -- can't compile under `client`.
#[cfg(feature = "engine")]
pub mod engine;
pub mod timing;
#[cfg(feature = "ffi")]
pub mod ffi;
// no-op: testing mm-cli self-update

