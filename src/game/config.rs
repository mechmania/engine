use serde::{ Serialize, Deserialize };
use super::util::*;

pub const EPSILON: f32 = 0.001;
pub const COLLISION_MAX_ITERATIONS: u32 = 100;
pub const BOTS_MAX: usize = 32;
pub const MAP_SIZE: usize = 32;

/// Every bot's radius. Named because more than the `BotConfig` default depends on it: the
/// navigation graph in `game::topology` is a function of it, and so is whether a given
/// corridor is a corridor at all.
pub const BOT_RADIUS: f32 = 0.25;

pub const PAYLOAD_PATH_LEN: usize = 7;

/// Team A's deposit. Team B's is this point's `mirror_pos` image, `(9.0, 2.0)` -- only
/// half the layout is written for the same reason only half of `MAP_ART` is.
///
/// Team A's half is the *high* half, `y = 16..31` -- that is where `build_map` puts
/// `MAP_ART` and where `eval_tick` spawns A -- so this sits in the pocket the art draws
/// with its `#####` at `x = 25..29` and its column at `x = 20`.
pub const DEPOSIT_POS: Vec2 = Vec2::new(23.0, 30.0);

// Represents the path from center to B
pub const PAYLOAD_PATH: [Vec2; PAYLOAD_PATH_LEN] = [
    Vec2::new(16.0, 16.0),
    Vec2::new(23.0, 16.0),
    Vec2::new(23.0, 11.0),
    Vec2::new(9.0, 11.0),
    Vec2::new(9.0, 6.0),
    Vec2::new(23.0, 6.0),
    Vec2::new(23.0, 3.0),
];

pub fn mirror_pos(pos: &mut Vec2) {
    pos.x = MAP_SIZE as f32 - pos.x;
    pos.y = MAP_SIZE as f32 - pos.y;
}

pub fn mirror_vel(vel: &mut Vec2) {
    vel.x *= -1.0;
    vel.y *= -1.0;
}

/// Total arc length of PAYLOAD_PATH.
pub fn payload_path_len() -> f32 {
    PAYLOAD_PATH.windows(2).map(|seg| seg[0].dist(&seg[1])).sum()
}

/// Position of the payload for a progress value `t`, normalized so that `t == 0.0`
/// is the center of the map, `t == 1.0` is team B's goal (the end of PAYLOAD_PATH)
/// and `t == -1.0` is team A's goal (the end of the mirrored path). `t` is
/// parameterized by arc length, so equal steps in `t` move the payload equal
/// distances, and it is clamped to [-1.0, 1.0].
pub fn payload_position(t: f32) -> Vec2 {
    let mirrored = t < 0.0;
    let mut remaining = t.abs().min(1.0) * payload_path_len();

    let mut pos = PAYLOAD_PATH[PAYLOAD_PATH.len() - 1];
    for seg in PAYLOAD_PATH.windows(2) {
        let (a, b) = (seg[0], seg[1]);
        let len = a.dist(&b);
        if remaining <= len {
            pos = a + (b - a) * if len == 0.0 { 0.0 } else { remaining / len };
            break;
        }
        remaining -= len;
    }

    if mirrored {
        mirror_pos(&mut pos);
    }
    pos
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default, mm_macros::FfiMirror)]
#[repr(u8)]
pub enum MapTile {
    #[default]
    Empty,
    Wall,
}

// Indexed `map[x][y]`. Tiles are 1.0 x 1.0, so tile (x, y) covers the world-space
// square [x, x + 1) x [y, y + 1).
pub type Map = [[MapTile; MAP_SIZE]; MAP_SIZE];

/// The arena's bottom half -- team A's side -- hand-written. `#` is a wall, anything else is
/// empty.
///
/// Rows read top-down -- the first line is `y = MAP_SIZE / 2` -- so this table is a picture
/// of the map exactly as the visualizer draws it: team A's half is the *high* half, and the
/// visualizer's `+y` points down the screen, so art row `r` is world row `MAP_SIZE / 2 + r`
/// and the last line is the bottom edge of the arena.
///
/// Only this half is written: `build_map` rotates it 180 degrees about the map centre to make
/// team B's half, so the layout is invariant under the rotation `mirror_pos` applies by
/// construction, which is what lets `GameState::mirror` leave the map alone. Edit this and
/// re-run `cargo test -- --nocapture render_graph` to see the whole arena, walls and
/// navigation vertices together.
const MAP_ART: [&[u8; MAP_SIZE]; MAP_SIZE / 2] = [
    b"####........................####", // y = 16
    b"......#..................#......",
    b"......#....###############......",
    b"......#..................#......", // y = 19
    b"......#..................#......",
    b"......#..................#......", // y = 21
    b"......#..................#......",
    b"....#################....#......",
    b".........................#......",
    b".........................#......", // y = 25
    b"#........................#......",
    b".........................#......", // y = 27
    b"....................#....#####..",
    b".##.................#...........",
    b"..#.................#...........", // y = 30
    b".....#..........................", // y = 31
];

/// Transcribes `MAP_ART` into `Map`, writing each wall twice: once where the art puts it and
/// once at its 180 degree image, which fills in the half the art does not spell out. Pure
/// index arithmetic -- the layout itself is the art.
const fn build_map() -> Map {
    let mut map = [[MapTile::Empty; MAP_SIZE]; MAP_SIZE];
    let mut r = 0;
    while r < MAP_SIZE / 2 {
        let row = MAP_ART[r];
        let mut x = 0;
        while x < MAP_SIZE {
            if row[x] == b'#' {
                // Art row `r` goes in verbatim at `y = MAP_SIZE / 2 + r`: team A's half is
                // the *high* half, and `+y` is down on screen, so the art reads as drawn.
                map[x][MAP_SIZE / 2 + r] = MapTile::Wall;
                // ...and its 180 degree image fills team B's half.
                map[MAP_SIZE - 1 - x][MAP_SIZE / 2 - 1 - r] = MapTile::Wall;
            }
            x += 1;
        }
        r += 1;
    }
    map
}

pub const MAP: Map = build_map();

// #[derive(Serialize, Deserialize, Clone, PartialEq)]
// #[repr(C)]
// pub struct PlayerConfig {
//     pub radius: f32, 
//     pub pickup_radius: f32,
//     pub speed: f32,
//     pub pass_speed: f32,
//     pub pass_error: f32,
//     pub possession_slowdown: f32,
// }

/// The fabricator's own knobs. Tokens buy bots and nothing else.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, mm_macros::FfiMirror)]
#[repr(C)]
pub struct FabricatorConfig {
    /// Ticks between natural builds. A natural build is free -- this cadence is the
    /// income-independent trickle that keeps a wiped fleet from being unable to rebuild.
    pub interval: u32,
    /// Flat token price of a rush order, which builds a bot on the spot regardless of the
    /// timer. Flat rather than scaling: the timer already bounds how fast bodies arrive,
    /// since at most one bot per fleet enters per tick.
    pub rush_cost: f32,
    /// Tokens each fleet holds on tick 0.
    pub starting_tokens: f32,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, mm_macros::FfiMirror)]
#[repr(C)]
pub struct BotConfig {
    pub radius: f32,
    /// World units per tick.
    pub speed: f32,
    pub health: f32,
    /// Degrees per tick.
    pub turn_speed: f32,
    /// Ticks between shots.
    pub blaster_cooldown: u32,
    pub blaster_range: f32,
    pub blaster_damage: f32,
    /// Health per tick one healer restores to its target. Continuous while the healer
    /// channels -- there is no heal cooldown, so `next_fire_tick` stays blaster-only.
    pub heal_per_tick: f32,
    /// Tokens one extractor adds to its fleet each tick it holds an extraction slot.
    pub extract_rate: f32,

    // Everything below is fixed. `base_` marks a per-bot stat; a name without it is a rule
    // of the game rather than a stat at all.
    /// Ticks of invulnerability granted by taking a blast, counted from the tick of the
    /// hit. Also what makes a bot take at most one blast per tick.
    pub base_invulnerability_ticks: u32,
    /// Measured from the blast point to a bot's *hull*, not its center.
    pub base_blaster_splash_radius: f32,
    /// Healer-to-target reach, measured center to center -- unlike the blaster's splash,
    /// which measures to the hull.
    pub base_heal_range: f32,
    /// Full arc width in degrees. The target must lie within half of this of the healer's
    /// facing, which is what gives `angle` a job on a class that names its target by id.
    pub base_heal_arc_deg: f32,
    /// How many healers' worth of healing one bot can receive per tick, in total, as a
    /// multiple of `heal_per_tick`: "three healers stack, a fourth is wasted".
    pub heal_stack_cap: f32,
    /// How far an extractor's ray reaches. Only walls and the map boundary block it --
    /// neither bots nor the payload do, so an extractor can mine through a crowd.
    pub base_extract_range: f32,
}


#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, mm_macros::FfiMirror)]
#[repr(C)]
pub struct PayloadConfig {
    pub radius: f32,
    /// A bot pushes the payload if its *center* is within this of the payload center.
    pub capture_radius: f32,
    /// World units per tick
    pub speed: f32,
}

/// The static half of the deposits. The mutable half -- who is currently extracting --
/// lives in `GameState` as `state::Deposit`.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, mm_macros::FfiMirror)]
#[repr(C)]
pub struct DepositConfig {
    /// Team A's deposit, normally `DEPOSIT_POS`. Team B's is its `mirror_pos` image, so
    /// only this one is carried -- the same reason `payload_path` carries only half the
    /// route. `GameState` holds both positions outright; this copy is here so a consumer
    /// reading only the log's config line still knows the layout.
    pub pos: Vec2,
    pub radius: f32,
    /// Extraction slots per deposit, shared by both teams -- a team that fills all of them
    /// locks the other out until its bots die, stop mining or look away. Not `base_`: this
    /// is a rule, not an upgradeable stat.
    pub extractor_cap: u8,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, mm_macros::FfiMirror)]
#[repr(C)]
pub struct GameConfig {
    /// Total match length, endgame included.
    pub max_ticks: u32,
    /// Length of the endgame, the last phase of the match: it starts at
    /// `max_ticks - endgame_ticks`, and no bot is built from then on.
    pub endgame_ticks: u32,
    pub bot: BotConfig,
    pub payload: PayloadConfig,
    /// The payload's route, center -> team B's goal -- normally `PAYLOAD_PATH`. Team A's
    /// half is its `mirror_pos` image, so only this one is carried. Here rather than left
    /// as a bare `const` so consumers that are not compiled from this file -- the
    /// visualizer, and the Python and Java ports -- get it from the log's config line
    /// instead of hand-copying the waypoints.
    pub payload_path: [Vec2; PAYLOAD_PATH_LEN],
    pub deposit: DepositConfig,
    pub fabricator: FabricatorConfig,
    /// The arena's static wall layout -- normally `MAP`. Here rather than in `GameState`
    /// because it never changes: a `GameState` is cloned twice per tick, and this is 1 KiB.
    ///
    /// The navigation graph derived from this map is deliberately *not* here -- it is a
    /// third of a megabyte, the engine never reads it, and a bot builds its own from these
    /// tiles at handshake. See `game::topology::init_topology`.
    pub map: Map,
}

impl GameConfig {
    /// Whether tile `(x, y)` is solid. Anything off the grid is not -- the map boundary is
    /// a separate thing, handled by `geom::ray_boundary` and `action::handle_collision`.
    pub fn is_wall(&self, x: isize, y: isize) -> bool {
        let size = MAP_SIZE as isize;
        if x < 0 || x >= size || y < 0 || y >= size {
            return false;
        }
        self.map[x as usize][y as usize] == MapTile::Wall
    }
}

/// The shipped arena as a whole `GameConfig`, shared by every test that needs one.
///
/// Deliberately one literal rather than one per test module: adding a `BotConfig` or
/// `PayloadConfig` field already breaks every `GameConfig` literal in the crate at once,
/// and each extra copy is another place to fix it. Lives here rather than in a test
/// module so `topology`'s tests and `ffi`'s can both reach it -- they never compile
/// under the same feature set.
#[cfg(test)]
pub(crate) mod test_conf {
    use super::*;
    use crate::game::topology::{init_topology, point_free};
    use crate::game::util::Vec2;
    use std::sync::OnceLock;

    /// The shipped arena and the shipped radius. Built once -- an O(V^3) solve per test
    /// would make this module the slowest thing in the suite for no benefit.
    pub(crate) fn conf() -> &'static GameConfig {
        static CONF: OnceLock<GameConfig> = OnceLock::new();
        init_topology(&MAP, BOT_RADIUS);
        CONF.get_or_init(|| GameConfig {
            max_ticks: 7200,
            endgame_ticks: 0,
            bot: BotConfig {
                radius: BOT_RADIUS,
                speed: 0.05,
                health: 10.0,
                turn_speed: 3.0,
                blaster_cooldown: 60,
                base_invulnerability_ticks: 15,
                blaster_range: 10.0,
                blaster_damage: 3.0,
                base_blaster_splash_radius: 0.3,
                heal_per_tick: 0.05,
                base_heal_range: 3.0,
                base_heal_arc_deg: 90.0,
                heal_stack_cap: (0.15) / (0.05),
                base_extract_range: 5.0,
                extract_rate: 0.1,
            },
            payload: PayloadConfig {
                radius: 0.75,
                capture_radius: 2.5,
                speed: 0.02,
            },
            payload_path: PAYLOAD_PATH,
            deposit: DepositConfig {
                // Far off the map with no radius: these tests are not about deposits, and
                // a deposit at the real `DEPOSIT_POS` would be a solid circle in the way.
                pos: Vec2::new(-100.0, -100.0),
                radius: 0.0,
                extractor_cap: 16,
            },
            fabricator: FabricatorConfig { interval: 100, rush_cost: 10.0, starting_tokens: 0.0 },
            map: MAP,
        })
    }

    /// Deterministic xorshift, so a failure is reproducible.
    pub(crate) struct Rng(pub(crate) u64);
    impl Rng {
        pub(crate) fn next_f32(&mut self) -> f32 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            (self.0 >> 40) as f32 / (1u32 << 24) as f32
        }
    }

    /// Points a bot could actually be standing on.
    pub(crate) fn sample_free_points(n: usize) -> Vec<Vec2> {
        let conf = conf();
        let mut rng = Rng(0x5eed_1234_9abc_def1);
        let mut out = Vec::with_capacity(n);
        while out.len() < n {
            let p = Vec2::new(
                rng.next_f32() * MAP_SIZE as f32,
                rng.next_f32() * MAP_SIZE as f32,
            );
            if point_free(conf, p) {
                out.push(p);
            }
        }
        out
    }
}
