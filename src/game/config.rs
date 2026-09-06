use serde::{ Serialize, Deserialize };
use super::util::*;

pub const EPSILON: f32 = 0.001;
pub const COLLISION_MAX_ITERATIONS: u32 = 100;
pub const BOTS_MAX: usize = 32;
pub const MAP_SIZE: usize = 32;
pub const DEPOSITS_MAX: usize = 8;

pub const PAYLOAD_PATH_LEN: usize = 5;

// Represents the path from center to B
pub const PAYLOAD_PATH: [Vec2; PAYLOAD_PATH_LEN] = [
    Vec2::new(16.0, 16.0),
    Vec2::new(5.0, 16.0),
    Vec2::new(5.0, 8.0),
    Vec2::new(27.0, 8.0),
    Vec2::new(27.0, 4.0),
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

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
#[repr(u8)]
pub enum MapTile {
    #[default]
    Empty,
    Wall,
}

// Indexed `map[x][y]`. Tiles are 1.0 x 1.0, so tile (x, y) covers the world-space
// square [x, x + 1) x [y, y + 1).
pub type Map = [[MapTile; MAP_SIZE]; MAP_SIZE];

/// How close a wall is allowed to get to the payload's path. Equal to the payload radius,
/// so the payload never clips a wall -- but only just, so a bot escorting it can be pinched
/// against one. Nothing enforces this at runtime; `wall_test::no_wall_blocks_the_payload`
/// asserts it against the hand-written layout below.
pub const WALL_PAYLOAD_CLEARANCE: f32 = 1.5;

/// The arena, hand-written. `#` is a wall, anything else is empty.
///
/// Rows read top-down -- the first line is `y = MAP_SIZE - 1` -- so this table is a picture
/// of the map as the visualizer draws it, not as `MAP` indexes it. `build_map` does the flip.
///
/// The layout is invariant under the 180 degree rotation `mirror_pos` applies, which is what
/// lets `GameState::mirror` leave it alone; `wall_test::the_map_is_mirror_symmetric` checks
/// that. Edit this and re-run `cargo test -- --nocapture render_map`.
const MAP_ART: [&[u8; MAP_SIZE]; MAP_SIZE] = [
    b"..........#####.................", // y = 31
    b"..........#####....#####........",
    b"..........#####....#####........",
    b"..........#####....#####........",
    b"...................#####........", // y = 27
    b"................................",
    b"................................",
    b"................................",
    b"................................",
    b"................................", // y = 22
    b".......................##.......",
    b".......................##.......",
    b"................................", // y = 19
    b"................................",
    b"................................",
    b"................................",
    b"................................",
    b"................................", // y = 14
    b"................................",
    b"................................", // y = 12
    b".......##.......................",
    b".......##.......................", // y = 10
    b"................................",
    b"................................",
    b"................................",
    b"................................", // y = 6
    b"................................",
    b"........#####...................", // y = 4
    b"........#####....#####..........",
    b"........#####....#####..........",
    b"........#####....#####..........", // y = 1
    b".................#####..........", // y = 0
];

/// Transcribes `MAP_ART` into `Map`. Pure index arithmetic -- the layout itself is the art.
const fn build_map() -> Map {
    let mut map = [[MapTile::Empty; MAP_SIZE]; MAP_SIZE];
    let mut y = 0;
    while y < MAP_SIZE {
        // row 0 of the art is the top of the map
        let row = MAP_ART[MAP_SIZE - 1 - y];
        let mut x = 0;
        while x < MAP_SIZE {
            if row[x] == b'#' {
                map[x][y] = MapTile::Wall;
            }
            x += 1;
        }
        y += 1;
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

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[repr(C)]
pub struct BotConfig {
    pub radius: f32, 
    pub base_speed: f32,
    pub base_health: f32,
    pub base_turn_speed: f32,
    pub base_blaster_cooldown: u32, // ticks
    /// Ticks of invulnerability granted by taking a blast, counted from the tick of the
    /// hit. Also what makes a bot take at most one blast per tick.
    pub base_invulnerability_ticks: u32,
    pub base_blaster_range: f32,
    pub base_blaster_damage: f32,
    /// Measured from the blast point to a bot's *hull*, not its center.
    pub base_blaster_splash_radius: f32,
}


#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[repr(C)]
pub struct PayloadConfig {
    pub radius: f32,
    /// A bot pushes the payload if its *center* is within this of the payload center.
    pub capture_radius: f32,
    /// World units per tick, per bot of advantage beyond `contest_diff`.
    pub speed_per_bot: f32,
    /// Cap on the per-tick push, in world units.
    pub max_speed: f32,
    /// `d`: the payload is contested while `|n_a - n_b| <= contest_diff`.
    pub contest_diff: u8,
}

/// A resource deposit. Static for the whole match, hence config and not state.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Default)]
#[repr(C)]
pub struct Deposit {
    pub pos: Vec2,
    pub radius: f32,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[repr(C)]
pub struct GameConfig {
    pub max_ticks: u32,
    pub bot: BotConfig,
    pub payload: PayloadConfig,
    /// The payload's route, center -> team B's goal -- normally `PAYLOAD_PATH`. Team A's
    /// half is its `mirror_pos` image, so only this one is carried. Here rather than left
    /// as a bare `const` so consumers that are not compiled from this file -- the
    /// visualizer, and the Python and Java ports -- get it from the log's config line
    /// instead of hand-copying the waypoints.
    pub payload_path: [Vec2; PAYLOAD_PATH_LEN],
    // `deposits[..deposit_count]` are live; the rest are padding. A fixed array because
    // `GameConfig` is embedded in `HandshakeRequest` and crosses shared memory as raw
    // `#[repr(C)]` bytes, so it cannot hold a `Vec`.
    pub deposit_count: u8,
    pub deposits: [Deposit; DEPOSITS_MAX],
    /// The arena's static wall layout -- normally `MAP`. Here rather than in `GameState`
    /// because it never changes: a `GameState` is cloned twice per tick, and this is 1 KiB.
    pub map: Map,
}

impl GameConfig {
    pub fn deposits(&self) -> &[Deposit] {
        &self.deposits[..self.deposit_count as usize]
    }

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

#[cfg(test)]
mod wall_test {
    use super::*;

    /// Nearest point of tile `(x, y)`'s square to `p`.
    fn nearest_in_tile(x: usize, y: usize, p: Vec2) -> Vec2 {
        Vec2::new(
            p.x.clamp(x as f32, x as f32 + 1.0),
            p.y.clamp(y as f32, y as f32 + 1.0),
        )
    }

    /// Every position the payload passes through, densely enough that consecutive samples
    /// are far closer together than a tile.
    fn payload_samples() -> Vec<Vec2> {
        let steps = (payload_path_len() * 20.0) as i32;
        (-steps..=steps)
            .map(|i| payload_position(i as f32 / steps as f32))
            .collect()
    }

    /// Tiles the payload's own disc sweeps over, plus `WALL_PAYLOAD_CLEARANCE` of margin.
    fn corridor() -> [[bool; MAP_SIZE]; MAP_SIZE] {
        let mut inside = [[false; MAP_SIZE]; MAP_SIZE];
        for p in payload_samples() {
            for x in 0..MAP_SIZE {
                for y in 0..MAP_SIZE {
                    if nearest_in_tile(x, y, p).dist(&p) < WALL_PAYLOAD_CLEARANCE {
                        inside[x][y] = true;
                    }
                }
            }
        }
        inside
    }

    /// `GameState::mirror` does not touch the map, on the grounds that a 180 degree
    /// rotation maps this layout onto itself. If that ever stops holding, team B sees a
    /// different arena than team A does at the same world coordinates.
    #[test]
    fn the_map_is_mirror_symmetric() {
        for x in 0..MAP_SIZE {
            for y in 0..MAP_SIZE {
                assert_eq!(
                    MAP[x][y],
                    MAP[MAP_SIZE - 1 - x][MAP_SIZE - 1 - y],
                    "tile ({x}, {y}) has no matching wall at its mirror"
                );
            }
        }
    }

    #[test]
    fn no_wall_blocks_the_payload() {
        let corridor = corridor();
        for x in 0..MAP_SIZE {
            for y in 0..MAP_SIZE {
                assert!(
                    !(corridor[x][y] && MAP[x][y] == MapTile::Wall),
                    "wall at ({x}, {y}) is within {WALL_PAYLOAD_CLEARANCE} of the payload path"
                );
            }
        }
    }

    /// A bot's centre can sit in a tile only if that tile and all eight around it are
    /// clear: its radius is 0.75 and the far corner of a neighbouring tile is 0.707 away,
    /// so anything narrower than a two-tile gap is not a corridor, it is a wall.
    fn passable(x: usize, y: usize) -> bool {
        for dx in -1i32..=1 {
            for dy in -1i32..=1 {
                let (nx, ny) = (x as i32 + dx, y as i32 + dy);
                if nx < 0 || ny < 0 || nx >= MAP_SIZE as i32 || ny >= MAP_SIZE as i32 {
                    continue; // the map edge is not a wall tile; the boundary handles it
                }
                if MAP[nx as usize][ny as usize] == MapTile::Wall {
                    return false;
                }
            }
        }
        true
    }

    fn tile_of(p: Vec2) -> (usize, usize) {
        (
            (p.x as usize).min(MAP_SIZE - 1),
            (p.y as usize).min(MAP_SIZE - 1),
        )
    }

    /// No wall may seal off a pocket: both spawns and the whole payload route have to sit
    /// in one connected region that a bot can actually walk.
    #[test]
    fn the_arena_is_connected() {
        let start = tile_of(PAYLOAD_PATH[0]);
        assert!(passable(start.0, start.1), "the map centre is not walkable");

        let mut seen = [[false; MAP_SIZE]; MAP_SIZE];
        let mut stack = vec![start];
        seen[start.0][start.1] = true;
        while let Some((x, y)) = stack.pop() {
            for (dx, dy) in [(1i32, 0i32), (-1, 0), (0, 1), (0, -1)] {
                let (nx, ny) = (x as i32 + dx, y as i32 + dy);
                if nx < 0 || ny < 0 || nx >= MAP_SIZE as i32 || ny >= MAP_SIZE as i32 {
                    continue;
                }
                let (nx, ny) = (nx as usize, ny as usize);
                if seen[nx][ny] || !passable(nx, ny) {
                    continue;
                }
                seen[nx][ny] = true;
                stack.push((nx, ny));
            }
        }

        // both spawns (`action::eval_tick` spawns at `payload_position(-1.0)` and mirrors
        // it for team B) and every waypoint of the route
        let mut required: Vec<Vec2> = vec![payload_position(-1.0), payload_position(1.0)];
        required.extend(PAYLOAD_PATH);
        for point in required {
            let (x, y) = tile_of(point);
            assert!(
                seen[x][y],
                "({x}, {y}) -- for {point:?} -- is walled off from the centre of the map"
            );
        }
    }

    /// Not an assertion, a picture. `cargo test -- --nocapture render_map`
    #[test]
    fn render_map() {
        let corridor = corridor();
        println!();
        for y in (0..MAP_SIZE).rev() {
            let row: String = (0..MAP_SIZE)
                .map(|x| match (MAP[x][y], corridor[x][y]) {
                    (MapTile::Wall, _) => '#',
                    (_, true) => '+',
                    _ => '.',
                })
                .collect();
            println!("{y:>2} {row}");
        }
    }
}
