use std::ops::{BitAnd, BitOr, BitOrAssign, Not};

use super::config::*;
use super::state::{BotId, GameState};
use super::team::{Team, TEAMS};
use super::util::Vec2;

/// The set of things a scan is allowed to hit.
///
/// Bits 0 and 1 are the two fleets, ordered to match `Team`'s discriminants -- see
/// `ScanMask::bots`.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
#[repr(C)]
pub struct ScanMask(u8);

impl ScanMask {
    pub const NONE: Self = Self(0);

    pub const BOTS_A: Self = Self(1 << 0);
    pub const BOTS_B: Self = Self(1 << 1);
    pub const BOUNDARY: Self = Self(1 << 2);
    pub const WALLS: Self = Self(1 << 3);
    pub const PAYLOAD: Self = Self(1 << 4);
    pub const DEPOSITS: Self = Self(1 << 5);

    pub const BOTS: Self = Self(Self::BOTS_A.0 | Self::BOTS_B.0);
    /// Everything a projectile cannot pass through.
    pub const SOLID: Self = Self(Self::BOTS.0 | Self::BOUNDARY.0 | Self::WALLS.0);
    pub const ALL: Self = Self(0b0011_1111);

    /// The bit for one team's fleet.
    pub const fn bots(team: Team) -> Self {
        Self(1 << team.index())
    }

    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }
}

impl BitOr for ScanMask {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

impl BitOrAssign for ScanMask {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

impl BitAnd for ScanMask {
    type Output = Self;
    fn bitand(self, rhs: Self) -> Self {
        Self(self.0 & rhs.0)
    }
}

impl Not for ScanMask {
    type Output = Self;
    fn not(self) -> Self {
        Self(!self.0 & Self::ALL.0)
    }
}

/// What a scan hit.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ScanTarget {
    Bot { team: Team, id: BotId },
    /// The edge of the map.
    Boundary,
    /// A `MapTile::Wall` at tile coordinates `(x, y)`.
    Wall { x: u8, y: u8 },
    Payload,
    /// A deposit, named by the team it belongs to -- see `state::Deposit`.
    Deposit { team: Team },
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct ScanHit {
    /// Where the ray met the target.
    pub point: Vec2,
    /// Distance from the scan origin to `point`.
    pub dist: f32,
    pub target: ScanTarget,
}

/// Keeps `what` if it is nearer than everything seen so far.
fn consider(t: f32, what: ScanTarget, best_t: &mut f32, target: &mut Option<ScanTarget>) {
    if t < *best_t {
        *best_t = t;
        *target = Some(what);
    }
}

/// Nearest `t >= 0` along the unit vector `dir` at which the ray from `origin` meets the
/// circle, or `None` if it never does. An origin inside the circle hits at `t == 0`.
fn ray_circle(origin: Vec2, dir: Vec2, center: Vec2, radius: f32) -> Option<f32> {
    let m = center - origin;
    let b = m.dot(dir);
    let c = m.norm_sq() - radius * radius;

    // outside the circle and pointing away from it
    if c > 0.0 && b < 0.0 {
        return None;
    }

    let disc = b * b - c;
    if disc < 0.0 {
        return None;
    }

    Some((b - disc.sqrt()).max(0.0))
}

/// Distance along unit `dir` at which the ray from `origin` leaves `[0, MAP_SIZE]^2`.
/// The slab method, simplified by the fact that `origin` is normally inside the box.
fn ray_boundary(origin: Vec2, dir: Vec2) -> f32 {
    let max = MAP_SIZE as f32;
    let mut t = f32::INFINITY;

    for (o, d) in [(origin.x, dir.x), (origin.y, dir.y)] {
        // A component this small never reaches either slab face in finite distance.
        if d.abs() <= EPSILON {
            continue;
        }
        let face = if d > 0.0 { max } else { 0.0 };
        t = t.min((face - o) / d);
    }

    t.max(0.0)
}

/// Instant hitscan from `origin` along `direction`, returning the nearest thing in `mask`
/// that the ray meets.
///
/// The ray is always clipped to the map boundary; `ScanMask::BOUNDARY` controls only
/// whether that clip is *reported* as a hit. `max_dist` is a further cap on top of that --
/// pass `f32::INFINITY` for no cap.
///
/// `ignore` skips a single bot, normally the one firing: its own circle contains the
/// origin, so without this every scan hits the shooter. It is a `(Team, BotId)` pair
/// because `BotId` is a slot index and is only unique within one fleet.
///
/// Returns `None` for a zero `direction`, or when nothing in `mask` is in range.
/// `direction` need not be normalized.
pub fn scan(
    state: &GameState,
    conf: &GameConfig,
    origin: Vec2,
    direction: Vec2,
    mask: ScanMask,
    ignore: Option<(Team, BotId)>,
    max_dist: f32,
) -> Option<ScanHit> {
    let dir = direction.normalize_or_zero();
    if dir == Vec2::ZERO {
        return None;
    }

    let mut target: Option<ScanTarget> = None;

    // The boundary always terminates the ray, whether or not it counts as a hit, so
    // `best_t` starts there and nothing outside the arena can win below.
    let t_bound = ray_boundary(origin, dir);
    let mut best_t = t_bound.min(max_dist);
    if t_bound <= max_dist && mask.contains(ScanMask::BOUNDARY) {
        target = Some(ScanTarget::Boundary);
    }

    // bots
    for team in TEAMS {
        if !mask.contains(ScanMask::bots(team)) {
            continue;
        }
        for bot in state.fleets()[team].iter() {
            if ignore == Some((team, bot.id)) {
                continue;
            }
            if let Some(t) = ray_circle(origin, dir, bot.pos, conf.bot.radius) {
                consider(t, ScanTarget::Bot { team, id: bot.id }, &mut best_t, &mut target);
            }
        }
    }

    // payload
    if mask.contains(ScanMask::PAYLOAD) {
        if let Some(t) = ray_circle(origin, dir, state.payload_pos(), conf.payload.radius) {
            consider(t, ScanTarget::Payload, &mut best_t, &mut target);
        }
    }

    // deposits
    if mask.contains(ScanMask::DEPOSITS) {
        for team in TEAMS {
            let deposit = state.deposits()[team];
            if let Some(t) = ray_circle(origin, dir, deposit.pos, conf.deposit.radius) {
                consider(t, ScanTarget::Deposit { team }, &mut best_t, &mut target);
            }
        }
    }

    // walls
    if mask.contains(ScanMask::WALLS) {
        if let Some((t, x, y)) = ray_walls(conf, origin, dir, best_t) {
            consider(t, ScanTarget::Wall { x, y }, &mut best_t, &mut target);
        }
    }

    target.map(|target| ScanHit {
        point: origin + dir * best_t,
        dist: best_t,
        target,
    })
}

/// Amanatides-Woo grid traversal over the 1.0 x 1.0 tiles of `conf.map`, returning the
/// distance to the first `MapTile::Wall` entered and its tile coordinates. Gives up once
/// it passes `max_t` or leaves the map.
fn ray_walls(conf: &GameConfig, origin: Vec2, dir: Vec2, max_t: f32) -> Option<(f32, u8, u8)> {
    let size = MAP_SIZE as isize;

    let mut cell = [
        origin.x.floor() as isize,
        origin.y.floor() as isize,
    ];
    let o = [origin.x, origin.y];
    let d = [dir.x, dir.y];

    let mut step = [0isize; 2];
    // distance along the ray to the next tile boundary on each axis...
    let mut next = [f32::INFINITY; 2];
    // ...and the distance between successive boundaries on that axis.
    let mut delta = [f32::INFINITY; 2];

    for axis in 0..2 {
        if d[axis].abs() <= EPSILON {
            continue;
        }
        delta[axis] = 1.0 / d[axis].abs();
        if d[axis] > 0.0 {
            step[axis] = 1;
            next[axis] = ((cell[axis] + 1) as f32 - o[axis]) / d[axis];
        } else {
            step[axis] = -1;
            next[axis] = (cell[axis] as f32 - o[axis]) / d[axis];
        }
    }

    let mut t = 0.0;
    loop {
        if cell[0] < 0 || cell[0] >= size || cell[1] < 0 || cell[1] >= size {
            return None;
        }
        if t > max_t {
            return None;
        }
        if conf.is_wall(cell[0], cell[1]) {
            return Some((t, cell[0] as u8, cell[1] as u8));
        }

        // advance across whichever tile boundary comes first
        let axis = if next[0] <= next[1] { 0 } else { 1 };
        if !next[axis].is_finite() {
            return None;
        }
        t = next[axis];
        cell[axis] += step[axis];
        next[axis] += delta[axis];
    }
}

#[cfg(test)]
mod geom_test {
    use super::*;
    use crate::game::state::{Mirror, SpecialState, StateOption};
    use crate::game::config::MapTile;

    const R: f32 = 1.0;

    fn conf() -> GameConfig {
        GameConfig {
            max_ticks: 100,
            endgame_ticks: 0,
            bot: BotConfig {
                radius: R,
                speed: 1.0,
                health: 10.0,
                turn_speed: 1.0,
                blaster_cooldown: 5,
                base_invulnerability_ticks: 3,
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
                radius: 2.0,
                capture_radius: 3.0,
                speed: 0.01,
            },
            payload_path: PAYLOAD_PATH,
            // Team A's deposit; team B's is its mirror image at (8, 16).
            deposit: DepositConfig {
                pos: Vec2::new(24.0, 16.0),
                radius: 2.0,
                extractor_cap: 16,
            },
            fabricator: FabricatorConfig { interval: 100, rush_cost: 10.0, starting_tokens: 0.0 },
            map: [[MapTile::Empty; MAP_SIZE]; MAP_SIZE],
        }
    }

    fn state() -> GameState {
        GameState::new(&conf())
    }

    fn add_bot(state: &mut GameState, team: Team, pos: Vec2) -> BotId {
        let mut fleets = state.fleets_mut();
        let id = fleets[team].add();
        fleets[team][id].pos = pos;
        fleets[team][id].health = 10.0;
        id
    }

    fn approx(a: f32, b: f32) {
        assert!((a - b).abs() < 1e-4, "expected {b}, got {a}");
    }

    /// Fires east along y = 16 from the west edge, hitting bots and walls only.
    fn east(state: &GameState, conf: &GameConfig, mask: ScanMask) -> Option<ScanHit> {
        scan(
            state,
            conf,
            Vec2::new(0.0, 16.0),
            Vec2::new(1.0, 0.0),
            mask,
            None,
            f32::INFINITY,
        )
    }

    #[test]
    fn hits_the_nearer_of_two_collinear_bots() {
        let conf = conf();
        let mut s = state();
        let near = add_bot(&mut s, TEAMS[0], Vec2::new(10.0, 16.0));
        add_bot(&mut s, TEAMS[0], Vec2::new(20.0, 16.0));

        let hit = east(&s, &conf, ScanMask::BOTS).unwrap();
        assert_eq!(
            hit.target,
            ScanTarget::Bot {
                team: TEAMS[0],
                id: near
            }
        );
        approx(hit.dist, 9.0);
        approx(hit.point.x, 9.0);
        approx(hit.point.y, 16.0);
    }

    #[test]
    fn ignore_skips_the_shooter_but_not_the_bot_behind_it() {
        let conf = conf();
        let mut s = state();
        let shooter = add_bot(&mut s, TEAMS[0], Vec2::new(5.0, 16.0));
        let target = add_bot(&mut s, TEAMS[0], Vec2::new(10.0, 16.0));

        let origin = Vec2::new(5.0, 16.0);
        let dir = Vec2::new(1.0, 0.0);

        // without `ignore`, the shooter's own circle swallows the shot
        let self_hit = scan(&s, &conf, origin, dir, ScanMask::BOTS, None, f32::INFINITY).unwrap();
        assert_eq!(
            self_hit.target,
            ScanTarget::Bot {
                team: TEAMS[0],
                id: shooter
            }
        );
        approx(self_hit.dist, 0.0);

        let hit = scan(
            &s,
            &conf,
            origin,
            dir,
            ScanMask::BOTS,
            Some((TEAMS[0], shooter)),
            f32::INFINITY,
        )
        .unwrap();
        assert_eq!(
            hit.target,
            ScanTarget::Bot {
                team: TEAMS[0],
                id: target
            }
        );
        approx(hit.dist, 4.0);
    }

    #[test]
    fn ignore_is_scoped_to_one_team() {
        let conf = conf();
        let mut s = state();
        // same slot index on the other fleet
        let id = add_bot(&mut s, TEAMS[1], Vec2::new(10.0, 16.0));

        let hit = scan(
            &s,
            &conf,
            Vec2::new(0.0, 16.0),
            Vec2::new(1.0, 0.0),
            ScanMask::BOTS,
            Some((TEAMS[0], id)),
            f32::INFINITY,
        )
        .unwrap();
        assert_eq!(hit.target, ScanTarget::Bot { team: TEAMS[1], id });
    }

    #[test]
    fn grazes_and_misses() {
        let conf = conf();

        let mut hit_s = state();
        add_bot(&mut hit_s, TEAMS[0], Vec2::new(10.0, 16.0 + R - 0.01));
        assert!(east(&hit_s, &conf, ScanMask::BOTS).is_some());

        let mut miss_s = state();
        add_bot(&mut miss_s, TEAMS[0], Vec2::new(10.0, 16.0 + R + 0.01));
        assert!(east(&miss_s, &conf, ScanMask::BOTS).is_none());
    }

    #[test]
    fn boundary_is_a_hit_only_when_masked() {
        let conf = conf();
        let s = state();

        let hit = scan(
            &s,
            &conf,
            Vec2::new(16.0, 16.0),
            Vec2::new(1.0, 0.0),
            ScanMask::BOUNDARY,
            None,
            f32::INFINITY,
        )
        .unwrap();
        assert_eq!(hit.target, ScanTarget::Boundary);
        approx(hit.dist, 16.0);
        approx(hit.point.x, MAP_SIZE as f32);
        approx(hit.point.y, 16.0);

        assert!(east(&s, &conf, ScanMask::BOTS).is_none());
    }

    #[test]
    fn max_dist_caps_the_ray() {
        let conf = conf();
        let mut s = state();
        add_bot(&mut s, TEAMS[0], Vec2::new(10.0, 16.0));

        let short = scan(
            &s,
            &conf,
            Vec2::new(0.0, 16.0),
            Vec2::new(1.0, 0.0),
            ScanMask::BOTS | ScanMask::BOUNDARY,
            None,
            5.0,
        );
        assert!(short.is_none());
    }

    #[test]
    fn direction_is_normalized_and_zero_is_rejected() {
        let conf = conf();
        let mut s = state();
        add_bot(&mut s, TEAMS[0], Vec2::new(10.0, 16.0));

        let scaled = scan(
            &s,
            &conf,
            Vec2::new(0.0, 16.0),
            Vec2::new(5.0, 0.0),
            ScanMask::BOTS,
            None,
            f32::INFINITY,
        )
        .unwrap();
        approx(scaled.dist, 9.0);

        assert!(scan(
            &s,
            &conf,
            Vec2::new(0.0, 16.0),
            Vec2::ZERO,
            ScanMask::ALL,
            None,
            f32::INFINITY,
        )
        .is_none());
    }

    #[test]
    fn does_not_hit_bots_behind_the_origin() {
        let conf = conf();
        let mut s = state();
        add_bot(&mut s, TEAMS[0], Vec2::new(5.0, 16.0));

        let hit = scan(
            &s,
            &conf,
            Vec2::new(10.0, 16.0),
            Vec2::new(1.0, 0.0),
            ScanMask::BOTS,
            None,
            f32::INFINITY,
        );
        assert!(hit.is_none());
    }

    #[test]
    fn hits_the_face_of_a_wall_tile() {
        let mut conf = conf();
        let s = state();
        conf.map[20][16] = MapTile::Wall;

        let hit = east(&s, &conf, ScanMask::WALLS).unwrap();
        assert_eq!(hit.target, ScanTarget::Wall { x: 20, y: 16 });
        approx(hit.dist, 20.0);
        approx(hit.point.x, 20.0);
    }

    #[test]
    fn a_ray_starting_inside_a_wall_hits_immediately() {
        let mut conf = conf();
        let s = state();
        conf.map[10][16] = MapTile::Wall;

        let hit = scan(
            &s,
            &conf,
            Vec2::new(10.5, 16.5),
            Vec2::new(1.0, 0.0),
            ScanMask::WALLS,
            None,
            f32::INFINITY,
        )
        .unwrap();
        assert_eq!(hit.target, ScanTarget::Wall { x: 10, y: 16 });
        approx(hit.dist, 0.0);
    }

    #[test]
    fn walls_occlude_bots_but_only_from_in_front() {
        let mut conf = conf();
        conf.map[10][16] = MapTile::Wall;
        let mask = ScanMask::BOTS | ScanMask::WALLS;

        let mut occluded = state();
        add_bot(&mut occluded, TEAMS[0], Vec2::new(12.0, 16.0));
        let hit = east(&occluded, &conf, mask).unwrap();
        assert_eq!(hit.target, ScanTarget::Wall { x: 10, y: 16 });

        let mut clear = state();
        let id = add_bot(&mut clear, TEAMS[0], Vec2::new(5.0, 16.0));
        let hit = east(&clear, &conf, mask).unwrap();
        assert_eq!(hit.target, ScanTarget::Bot { team: TEAMS[0], id });
        approx(hit.dist, 4.0);
    }

    #[test]
    fn mask_excludes_nearer_targets() {
        let mut conf = conf();
        let mut s = state();
        conf.map[10][16] = MapTile::Wall;
        add_bot(&mut s, TEAMS[0], Vec2::new(5.0, 16.0));

        let hit = east(&s, &conf, ScanMask::WALLS).unwrap();
        assert_eq!(hit.target, ScanTarget::Wall { x: 10, y: 16 });
    }

    #[test]
    fn hits_the_payload_and_deposits() {
        let conf = conf();
        let s = state();

        // capture == 0.0 puts the payload at the center of the map
        let hit = scan(
            &s,
            &conf,
            Vec2::new(0.0, 16.0),
            Vec2::new(1.0, 0.0),
            ScanMask::PAYLOAD,
            None,
            f32::INFINITY,
        )
        .unwrap();
        assert_eq!(hit.target, ScanTarget::Payload);
        approx(hit.dist, 14.0);

        let hit = scan(
            &s,
            &conf,
            Vec2::new(0.0, 16.0),
            Vec2::new(1.0, 0.0),
            ScanMask::DEPOSITS,
            None,
            f32::INFINITY,
        )
        .unwrap();
        // Team B's deposit, the mirror image at (8, 16), is the one this ray meets first.
        assert_eq!(hit.target, ScanTarget::Deposit { team: Team::B });
        approx(hit.dist, 6.0);

        // ...and team A's at (24, 16) is what the same ray finds coming the other way.
        let hit = scan(
            &s,
            &conf,
            Vec2::new(32.0, 16.0),
            Vec2::new(-1.0, 0.0),
            ScanMask::DEPOSITS,
            None,
            f32::INFINITY,
        )
        .unwrap();
        assert_eq!(hit.target, ScanTarget::Deposit { team: Team::A });
        approx(hit.dist, 6.0);
    }

    #[test]
    fn mirroring_bot_state_twice_is_the_identity() {
        let conf = conf();
        let mut s = state();

        let a = add_bot(&mut s, Team::A, Vec2::new(4.0, 7.0));
        let b = add_bot(&mut s, Team::B, Vec2::new(21.0, 3.0));
        for (team, id, angle, turn_vel) in [(Team::A, a, 30.0, 2.5), (Team::B, b, 200.0, -1.5)] {
            let bot = &mut s.fleets_mut()[team][id];
            bot.vel = Vec2::new(0.3, -0.4);
            bot.angle = angle;
            bot.turn_vel = turn_vel;
            bot.special = SpecialState::Battle {
                next_fire_tick: 42,
                shot: StateOption::Some(Vec2::new(11.0, 2.0)),
            };
        }
        let original = s.clone();

        s.mirror(&conf);
        s.mirror(&conf);

        assert!(s == original, "a double mirror must restore the state exactly");
    }

    #[test]
    fn mirroring_flips_a_bots_facing() {
        let conf = conf();
        let mut s = state();
        let id = add_bot(&mut s, Team::A, Vec2::new(4.0, 7.0));
        s.fleets_mut()[Team::A][id].angle = 30.0;

        s.mirror(&conf);

        // Team A's fleet is now team B's, and the bot faces the other way.
        let bot = &s.fleets()[Team::B][id];
        approx(bot.angle, 210.0);
        approx(bot.pos.x, MAP_SIZE as f32 - 4.0);
        approx(bot.pos.y, MAP_SIZE as f32 - 7.0);
    }

    #[test]
    fn mirroring_preserves_turn_vel() {
        let conf = conf();
        let mut s = state();
        let id = add_bot(&mut s, Team::A, Vec2::new(4.0, 7.0));
        s.fleets_mut()[Team::A][id].turn_vel = 2.5;

        s.mirror(&conf);

        // A 180 degree rotation is orientation-preserving, so a signed turn rate is unchanged.
        approx(s.fleets()[Team::B][id].turn_vel, 2.5);
    }
}
