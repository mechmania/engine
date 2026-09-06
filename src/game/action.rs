use std::cell::RefCell;

use crate::game::team::{Team, TeamPair, TEAMS};

use super::{
    config::*,
    geom::{scan, ScanMask},
    state::*,
    util::*,
};
use rand::prelude::*;

thread_local! {
    static RNG: RefCell<SmallRng> = RefCell::new(SmallRng::from_rng(&mut rand::rng()));
}

fn with_rng<T>(f: impl FnOnce(&mut SmallRng) -> T) -> T {
    RNG.with(|rng| f(&mut rng.borrow_mut()))
}

fn handle_collision(state: &mut GameState, conf: &GameConfig) -> bool {
    
    let mut resolved = false;
    let mut i = 0;
    let r = conf.bot.radius;
    let width = MAP_SIZE as f32;

    // Bound before `bots` borrows `state` mutably.
    let payload = state.payload_pos();
    let min_payload_dist = conf.payload.radius + r;


    let mut bots: Vec<&mut BotState> = Vec::with_capacity(state.fleet_a.len as usize + state.fleet_b.len as usize);
    bots.extend(state.fleet_a.iter_mut());
    bots.extend(state.fleet_b.iter_mut());

    while i < COLLISION_MAX_ITERATIONS && !resolved {
        resolved = true;
        with_rng(|rng| { bots.shuffle(rng) });
        
        // wall
        for bot in &mut bots {
            if bot.pos.x - r < 0.0 {
                bot.pos.x = r + EPSILON;
                bot.vel.x = 0.0;
                resolved = false;
            }
            if bot.pos.x + r > width {
                bot.pos.x = width - r - EPSILON;
                bot.vel.x = 0.0;
                resolved = false;
            }
            if bot.pos.y - r < 0.0 {
                bot.pos.y = r + EPSILON;
                bot.vel.y = 0.0;
                resolved = false;
            }
            if bot.pos.y + r > width {
                bot.pos.y = width - r - EPSILON;
                bot.vel.y = 0.0;
                resolved = false;
            }
        }
        
        // wall tiles
        for bot in &mut bots {
            let lo = |v: f32| ((v - r).floor() as isize).clamp(0, MAP_SIZE as isize - 1);
            let hi = |v: f32| ((v + r).floor() as isize).clamp(0, MAP_SIZE as isize - 1);
            for tx in lo(bot.pos.x)..=hi(bot.pos.x) {
                for ty in lo(bot.pos.y)..=hi(bot.pos.y) {
                    if !conf.is_wall(tx, ty) {
                        continue;
                    }
                    let (x0, y0) = (tx as f32, ty as f32);

                    // Which side of the tile the bot is on, per axis; 0 means its center is
                    // within the tile's span on that axis.
                    let side = |p: f32, lo: f32| {
                        if p < lo {
                            -1
                        } else if p > lo + 1.0 {
                            1
                        } else {
                            0
                        }
                    };
                    let (sx, sy) = (side(bot.pos.x, x0), side(bot.pos.y, y0));

                    // Contacts on a face shared with another wall tile belong to that tile,
                    // not this one -- resolving them here catches a bot on the seam between
                    // two tiles of a flat wall and stops it sliding.
                    let interior = match (sx, sy) {
                        (0, 0) => false, // center inside the tile: always ours to fix
                        (0, _) => conf.is_wall(tx, ty + sy),
                        (_, 0) => conf.is_wall(tx + sx, ty),
                        // a corner, shared with the two tiles either side of it
                        _ => conf.is_wall(tx + sx, ty) || conf.is_wall(tx, ty + sy),
                    };
                    if interior {
                        continue;
                    }

                    let normal = if (sx, sy) == (0, 0) {
                        // The bot's center is inside the tile, so there is no outward
                        // direction to read off a nearest point. Leave by the nearest face
                        // that does not just put the bot inside the next wall along, and
                        // place it clear of that face rather than of the center.
                        let faces = [
                            (Vec2::new(-1.0, 0.0), bot.pos.x - x0, -1, 0),
                            (Vec2::new(1.0, 0.0), x0 + 1.0 - bot.pos.x, 1, 0),
                            (Vec2::new(0.0, -1.0), bot.pos.y - y0, 0, -1),
                            (Vec2::new(0.0, 1.0), y0 + 1.0 - bot.pos.y, 0, 1),
                        ];
                        let mut best = Vec2::new(1.0, 0.0);
                        let mut best_depth = f32::INFINITY;
                        for (dir, depth, dx, dy) in faces {
                            if conf.is_wall(tx + dx, ty + dy) {
                                continue;
                            }
                            if depth < best_depth {
                                best_depth = depth;
                                best = dir;
                            }
                        }
                        // Step out to the face, then clear of it.
                        bot.pos += best * best_depth;
                        best
                    } else {
                        // nearest point on the tile square [x0, x0 + 1] x [y0, y0 + 1]
                        let near = Vec2::new(
                            bot.pos.x.clamp(x0, x0 + 1.0),
                            bot.pos.y.clamp(y0, y0 + 1.0),
                        );
                        let delta = bot.pos - near;
                        if delta.norm_sq() >= r * r {
                            continue;
                        }
                        let normal = delta.normalize_or_zero();
                        bot.pos = near;
                        normal
                    };

                    bot.pos += normal * (r + EPSILON);
                    // Only the component driving the bot into the wall is lost, so a bot
                    // pushing at an angle keeps sliding along the face.
                    bot.vel -= normal * bot.vel.dot(normal);
                    resolved = false;
                }
            }
        }

        // payload
        for bot in &mut bots {
            let delta = bot.pos - payload;
            if delta.norm_sq() < min_payload_dist * min_payload_dist {
                let dir = delta.normalize_or_else(|| Vec2::new(1.0, 0.0));
                bot.pos = payload + dir * (min_payload_dist + EPSILON);
                bot.vel = Vec2::ZERO;
                resolved = false;
            }
        }

        // TODO upgrade stations, mining places idk


        i += 1;
    }



    return resolved
}

/// Advances `state.capture` by one tick of payload push.
///
/// Each team's bots within `conf.payload.capture_radius` of the payload center are counted;
/// while `|n_a - n_b| <= conf.payload.contest_diff` the payload is contested and holds
/// position. Past that threshold the leading team pushes toward the other team's goal at
/// `speed_per_bot` per bot of advantage beyond the threshold, capped at `max_speed`.
fn step_payload(state: &mut GameState, conf: &GameConfig) {
    let payload = state.payload_pos();
    let capture_radius_sq = conf.payload.capture_radius.powi(2);

    let mut counts = TeamPair::new(0i32, 0i32);
    for team in TEAMS {
        counts[team] = state.fleets()[team]
            .iter()
            .filter(|bot| bot.pos.dist_sq(&payload) <= capture_radius_sq)
            .count() as i32;
    }

    // Positive margin pushes `capture` toward `PAYLOAD_PATH`'s end (team B's goal); the
    // sign flip in `GameState::mirror` is what makes this side-agnostic for bots.
    let margin = counts[Team::A] - counts[Team::B];
    let excess = margin.abs() - conf.payload.contest_diff as i32;
    if excess <= 0 {
        return; // contested, or nobody on it
    }

    let speed = (conf.payload.speed_per_bot * excess as f32).min(conf.payload.max_speed);
    let delta = margin.signum() as f32 * speed / payload_path_len();
    state.capture = (state.capture + delta).clamp(-1.0, 1.0);
}

/// Resets every field of bot `id` to its starting values, placing it at `pos`.
fn reset_bot(team: Team, id: BotId, pos: Vec2, state: &mut GameState, conf: &GameConfig) {
    let fleet = &mut state.fleets_mut()[team];
    let Some(bot) = fleet.get_mut(id) else {
        return;
    };
    bot.id = id;
    bot.health = conf.bot.base_health; // TODO upgrades
    bot.pos = pos;
    bot.vel = Vec2::ZERO;
    bot.angle = 0.0;
    bot.turn_vel = 0.0;
    bot.next_fire_tick = 0;
    bot.invulnerable_until_tick = 0;
    bot.shot = StateOption::None;
}

/// Fires every battle bot whose action asks for it and whose blaster is off cooldown.
///
/// A shot is an instant ray from the bot's center along its facing, capped at
/// `base_blaster_range`. It passes through the shooter's own fleet and stops at the first
/// enemy bot, wall tile, map edge, payload or deposit; a ray that reaches full range
/// without meeting any of those bursts in the air at its endpoint. Wherever it stops,
/// every *enemy* bot whose hull is within `base_blaster_splash_radius` of that point takes
/// `base_blaster_damage`, with no falloff and no line-of-sight check. The bot the ray
/// actually hit is always inside the blast, since the impact point lies on its hull.
///
/// The splash radius is small next to a bot, so what it really punishes is stacking:
/// nothing separates overlapping bots (see `handle_collision`), and a blast on one of them
/// is a blast on all of them.
///
/// Damage does *not* stack on a single bot: a bot that is hit becomes invulnerable for
/// `base_invulnerability_ticks`, and the window opens on the tick of the hit, so it takes
/// at most one blast per tick no matter how many land on it. Which of several simultaneous
/// blasts claims it therefore depends on `bot_actions` order, which `eval_tick` shuffles --
/// unobservable while every blast deals the same damage.
///
/// Shots are resolved against the pre-damage state and applied together, so firing order
/// never decides a trade and a bot killed this tick still gets its shot off.
fn step_blasters(
    state: &mut GameState,
    conf: &GameConfig,
    bot_actions: &[(Team, BotId, &BotAction)],
) {
    let tick = state.tick;
    let range = conf.bot.base_blaster_range; // TODO upgrades

    // Last tick's beams are stale. Clearing them here is what makes `shot` transient: the
    // `Some` -> `None` transition is itself a diff, so the gamelog self-clears.
    for team in TEAMS {
        for bot in state.fleets_mut()[team].iter_mut() {
            bot.shot = StateOption::None;
        }
    }

    let mut blasts: Vec<(Team, Vec2)> = Vec::new();

    for (team, id, action) in bot_actions {
        let (team, id) = (*team, *id);
        let bot = &state.fleets()[team][id];

        if bot.class != action.special_action.class() {
            continue; // an action for a class this bot is not
        }
        if !matches!(action.special_action, SpecialAction::Battle { fire: true }) {
            continue;
        }
        if bot.next_fire_tick > tick {
            continue;
        }

        let origin = bot.pos;
        let dir = Vec2::from_angle_deg(bot.angle);
        // Own fleet is left out of the mask entirely, so allies neither block the shot nor
        // need `scan`'s `ignore` to keep the shooter from hitting itself.
        let mask = ScanMask::bots(team.other_team())
            | ScanMask::BOUNDARY
            | ScanMask::WALLS
            | ScanMask::PAYLOAD
            | ScanMask::DEPOSITS;
        let point = scan(state, conf, origin, dir, mask, None, range)
            .map(|hit| hit.point)
            .unwrap_or(origin + dir * range);

        blasts.push((team, point));

        let bot = &mut state.fleets_mut()[team][id];
        bot.shot = StateOption::Some(point);
        bot.next_fire_tick = tick + conf.bot.base_blaster_cooldown; // TODO upgrades
    }

    for (team, point) in &blasts {
        let reach = conf.bot.base_blaster_splash_radius + conf.bot.radius;
        for bot in state.fleets_mut()[team.other_team()].iter_mut() {
            if bot.invulnerable_until_tick > tick {
                continue;
            }
            if bot.pos.dist_sq(point) <= reach * reach {
                bot.health -= conf.bot.base_blaster_damage;
                // `.max(1)` so that even a config of 0 still blocks the remaining blasts of
                // this tick -- one blast per bot per tick is a rule, not a tuning knob.
                bot.invulnerable_until_tick = tick + conf.bot.base_invulnerability_ticks.max(1);
            }
        }
    }

    for team in TEAMS {
        let dead: Vec<BotId> = state.fleets()[team]
            .iter()
            .filter(|bot| bot.health <= 0.0)
            .map(|bot| bot.id)
            .collect();
        for id in dead {
            state.fleets_mut()[team].remove(id);
        }
    }
}

fn closer(a: &Vec2, b: &Vec2, c: &Vec2) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let dist_ac = a.dist_sq(c);
    let dist_bc = b.dist_sq(c);
    let eps_sq = EPSILON.powi(2);
    if dist_ac <= eps_sq && dist_bc <= eps_sq {
        return if rand::rng().random_bool(0.5) {
            Ordering::Less
        } else {
            Ordering::Greater
        };
    }
    dist_ac.total_cmp(&dist_bc)
}

pub fn eval_tick(
    state: &mut GameState, 
    conf: &GameConfig, 
    actions_a: FleetAction,
    actions_b: FleetAction
) {

    // let FleetAction { 
    //     bots: bot_actions_a, 
    //     fabricator_next: fabricator_next_a, 
    //     fabricator_on: fabricator_on_a 
    // } = actions_a;
    // let FleetAction { 
    //     bots: bot_actions_b, 
    //     fabricator_next: fabricator_next_b, 
    //     fabricator_on: fabricator_on_b 
    // } = actions_b;

    let mut bot_actions = Vec::with_capacity(state.fleet_a.len as usize + state.fleet_b.len as usize);
    bot_actions.extend(
        state.fleet_a
            .iter()
            .map(|bot| (Team::A, bot.id, &actions_a.bots[bot.id as usize]))
    );
    bot_actions.extend(
        state.fleet_b
            .iter()
            .map(|bot| (Team::B, bot.id, &actions_b.bots[bot.id as usize]))
    );
    with_rng(|rng| bot_actions.shuffle(rng));
    let bot_actions = bot_actions;



    // movement

    for (team, id, action) in &bot_actions {
        let bot = &mut state.fleets_mut()[*team][*id];
        
        let vel = action.move_action.direction * conf.bot.base_speed; // TODO upgrades
        bot.pos += vel;
        bot.vel = vel;

        let max_turn_speed = conf.bot.base_turn_speed; // TODO upgrades
        let mut move_to_angle = |deg: f32| {
            let diff = diff_degrees(deg, bot.angle);
            let turn_vel = diff.clamp(-max_turn_speed, max_turn_speed);
            bot.angle += turn_vel;
            bot.turn_vel = turn_vel;
        };

        match &action.turn_action {
            TurnAction::Direction { power } => {
                bot.angle += power * max_turn_speed;
                bot.turn_vel = power * max_turn_speed;
            },
            TurnAction::TargetRotation { deg } => {
                move_to_angle(*deg);
            },
            TurnAction::TargetPosition { pos } => {
                move_to_angle((*pos - bot.pos).angle_deg())
            },
        }

        bot.angle = normalize_degrees(bot.angle);

    }


    // payload
    step_payload(state, conf);

    // collision
    let collision_resolved = handle_collision(state, conf);

    // healing

    // damage
    step_blasters(state, conf, &bot_actions);

    // spawning TODO, right now it is very basic
    if state.tick % 300 == 0 {
        // Team A's own goal: the end of the payload path team B is pushing toward.
        let spawn = payload_position(-1.0);
        for t in TEAMS {
            let fleet = &mut state.fleets_mut()[t];
            if fleet.is_full() { continue }
            let new = fleet.add();

            // Spawn every bot in team A's frame and mirror B's into place, so the spawn
            // pose -- position *and* facing -- is symmetric by construction.
            reset_bot(t, new, spawn, state, conf);
            if matches!(t, Team::B) {
                state.fleets_mut()[t][new].mirror(conf);
            }
        }
    }
    
    state.tick += 1;
}

#[cfg(test)]
mod payload_test {
    use super::*;

    fn conf() -> GameConfig {
        GameConfig {
            max_ticks: 100,
            bot: BotConfig {
                radius: 0.75,
                base_speed: 0.1,
                base_health: 10.0,
                base_turn_speed: 10.0,
                base_blaster_cooldown: 60,
                base_invulnerability_ticks: 15,
                base_blaster_range: 10.0,
                base_blaster_damage: 3.0,
                base_blaster_splash_radius: 0.3,
            },
            payload: PayloadConfig {
                radius: 1.5,
                capture_radius: 3.0,
                speed_per_bot: 0.01,
                max_speed: 0.04,
                contest_diff: 1,
            },
            payload_path: PAYLOAD_PATH,
            deposit_count: 0,
            deposits: [Deposit::default(); DEPOSITS_MAX],
            map: [[MapTile::Empty; MAP_SIZE]; MAP_SIZE],
        }
    }

    fn add_bots(state: &mut GameState, conf: &GameConfig, team: Team, n: usize, pos: Vec2) {
        for _ in 0..n {
            let id = state.fleets_mut()[team].add();
            reset_bot(team, id, pos, state, conf);
        }
    }

    /// A state with `a` bots of team A and `b` of team B sitting on the payload, which is
    /// itself parked at progress `capture`.
    fn on_the_point(conf: &GameConfig, capture: f32, a: usize, b: usize) -> GameState {
        let mut state = GameState::new(conf);
        state.capture = capture;
        let pos = state.payload_pos();
        add_bots(&mut state, conf, Team::A, a, pos);
        add_bots(&mut state, conf, Team::B, b, pos);
        state
    }

    /// The `capture` step one bot of advantage past `contest_diff` buys.
    fn one_bot_step(conf: &GameConfig) -> f32 {
        conf.payload.speed_per_bot / payload_path_len()
    }

    #[test]
    fn an_empty_point_holds() {
        let conf = conf();
        let mut state = on_the_point(&conf, 0.0, 0, 0);
        step_payload(&mut state, &conf);
        assert_eq!(state.capture, 0.0);
    }

    #[test]
    fn a_contested_point_holds() {
        let conf = conf();
        // Equal counts, and a one-bot lead -- both within `contest_diff`.
        for (a, b) in [(2, 2), (2, 1), (1, 2), (5, 5)] {
            let mut state = on_the_point(&conf, 0.0, a, b);
            step_payload(&mut state, &conf);
            assert_eq!(state.capture, 0.0, "{a} vs {b} should be contested");
        }
    }

    #[test]
    fn bots_outside_the_capture_radius_do_not_count() {
        let conf = conf();
        let mut state = on_the_point(&conf, 0.0, 0, 0);
        let far = state.payload_pos() + Vec2::new(conf.payload.capture_radius + EPSILON, 0.0);
        add_bots(&mut state, &conf, Team::A, 5, far);
        step_payload(&mut state, &conf);
        assert_eq!(state.capture, 0.0);
    }

    #[test]
    fn each_team_pushes_toward_the_other_goal() {
        let conf = conf();
        let step = one_bot_step(&conf);

        let mut state = on_the_point(&conf, 0.0, 3, 1);
        step_payload(&mut state, &conf);
        assert_eq!(state.capture, step);

        let mut state = on_the_point(&conf, 0.0, 1, 3);
        step_payload(&mut state, &conf);
        assert_eq!(state.capture, -step);
    }

    #[test]
    fn the_push_scales_with_the_margin_up_to_max_speed() {
        let conf = conf();
        let step = one_bot_step(&conf);

        // margin 4, `contest_diff` 1 -> excess 3.
        let mut state = on_the_point(&conf, 0.0, 4, 0);
        step_payload(&mut state, &conf);
        assert_eq!(state.capture, 3.0 * step);

        // excess 19 would be 0.19/tick, well past `max_speed`.
        let mut state = on_the_point(&conf, 0.0, 20, 0);
        step_payload(&mut state, &conf);
        assert_eq!(state.capture, conf.payload.max_speed / payload_path_len());
    }

    #[test]
    fn capture_clamps_at_the_goals() {
        let conf = conf();
        for (sign, a, b) in [(1.0, 20, 0), (-1.0, 0, 20)] {
            let mut state = on_the_point(&conf, sign, a, b);
            step_payload(&mut state, &conf);
            assert_eq!(state.capture, sign);
        }
    }

    #[test]
    fn the_payload_is_solid() {
        let conf = conf();
        let mut state = on_the_point(&conf, 0.3, 1, 0);
        // A second bot sitting exactly on the center, the degenerate push-out direction.
        let payload = state.payload_pos();
        add_bots(&mut state, &conf, Team::A, 1, payload);

        assert!(handle_collision(&mut state, &conf));

        let min_dist = conf.payload.radius + conf.bot.radius;
        for bot in state.fleet_a.iter() {
            assert!(
                bot.pos.dist(&payload) >= min_dist,
                "bot at {:?} is inside the payload",
                bot.pos
            );
        }
    }

    #[test]
    fn the_push_is_mirror_symmetric() {
        let conf = conf();
        let mut state = on_the_point(&conf, 0.25, 3, 1);
        let mut mirrored = state.clone();
        mirrored.mirror(&conf);

        step_payload(&mut state, &conf);
        step_payload(&mut mirrored, &conf);

        assert_eq!(state.capture, -mirrored.capture);
    }
}

#[cfg(test)]
mod blaster_test {
    use super::*;
    use crate::game::diff::Diff;

    const DAMAGE: f32 = 3.0;
    const RANGE: f32 = 10.0;
    const HEALTH: f32 = 10.0;
    const COOLDOWN: u32 = 60;
    const INVULN: u32 = 15;

    fn conf() -> GameConfig {
        GameConfig {
            max_ticks: 100,
            bot: BotConfig {
                radius: 0.75,
                base_speed: 0.1,
                base_health: HEALTH,
                base_turn_speed: 10.0,
                base_blaster_cooldown: COOLDOWN,
                base_invulnerability_ticks: INVULN,
                base_blaster_range: RANGE,
                base_blaster_damage: DAMAGE,
                base_blaster_splash_radius: 0.3,
            },
            payload: PayloadConfig {
                radius: 1.5,
                capture_radius: 3.0,
                speed_per_bot: 0.01,
                max_speed: 0.04,
                contest_diff: 1,
            },
            payload_path: PAYLOAD_PATH,
            deposit_count: 0,
            deposits: [Deposit::default(); DEPOSITS_MAX],
            map: [[MapTile::Empty; MAP_SIZE]; MAP_SIZE],
        }
    }

    /// A state with nothing in it but the bots a test puts there. `capture` is parked at
    /// team A's goal so the payload sits in a corner, well clear of the y = 16 firing line
    /// every test below uses.
    fn state(conf: &GameConfig) -> GameState {
        let mut state = GameState::new(conf);
        state.capture = -1.0;
        state
    }

    fn spawn(state: &mut GameState, conf: &GameConfig, team: Team, pos: Vec2, angle: f32) -> BotId {
        let id = state.fleets_mut()[team].add();
        reset_bot(team, id, pos, state, conf);
        state.fleets_mut()[team][id].angle = angle;
        id
    }

    /// A `FleetAction` in which `ids` pull the trigger and nobody else does.
    fn firing(ids: &[BotId]) -> FleetAction {
        let mut action = FleetAction::new();
        for id in ids {
            action.bots[*id as usize].special_action = SpecialAction::Battle { fire: true };
        }
        action
    }

    /// The flattened list `eval_tick` hands to `step_blasters`, in a fixed order -- the
    /// step is order-independent, so the tests do not need the shuffle.
    fn pairs<'a>(
        state: &GameState,
        a: &'a FleetAction,
        b: &'a FleetAction,
    ) -> Vec<(Team, BotId, &'a BotAction)> {
        let mut res = Vec::new();
        for (team, actions) in [(Team::A, a), (Team::B, b)] {
            for bot in state.fleets()[team].iter() {
                res.push((team, bot.id, &actions.bots[bot.id as usize]));
            }
        }
        res
    }

    fn fire(state: &mut GameState, conf: &GameConfig, a: &FleetAction, b: &FleetAction) {
        let actions = pairs(state, a, b);
        step_blasters(state, conf, &actions);
    }

    fn health(state: &GameState, team: Team, id: BotId) -> f32 {
        state.fleets()[team][id].health
    }

    fn approx(a: f32, b: f32) {
        assert!((a - b).abs() < 1e-4, "expected {b}, got {a}");
    }

    /// A shooter at (5, 16) facing east, and one enemy wherever the test wants it.
    fn duel(conf: &GameConfig, enemy: Vec2) -> (GameState, BotId, BotId) {
        let mut s = state(conf);
        let shooter = spawn(&mut s, conf, Team::A, Vec2::new(5.0, 16.0), 0.0);
        let target = spawn(&mut s, conf, Team::B, enemy, 180.0);
        (s, shooter, target)
    }

    #[test]
    fn a_shot_damages_a_bot_in_the_line_of_fire() {
        let conf = conf();
        let (mut s, shooter, target) = duel(&conf, Vec2::new(10.0, 16.0));

        fire(&mut s, &conf, &firing(&[shooter]), &FleetAction::new());

        approx(health(&s, Team::B, target), HEALTH - DAMAGE);

        // the beam is recorded on the shooter, ending on the target's hull
        let point = s.fleets()[Team::A][shooter].shot.option().expect("no shot recorded");
        approx(point.x, 10.0 - conf.bot.radius);
        approx(point.y, 16.0);
        assert_eq!(s.fleets()[Team::A][shooter].next_fire_tick, COOLDOWN);
    }

    #[test]
    fn not_firing_records_nothing() {
        let conf = conf();
        let (mut s, shooter, target) = duel(&conf, Vec2::new(10.0, 16.0));

        fire(&mut s, &conf, &FleetAction::new(), &FleetAction::new());

        approx(health(&s, Team::B, target), HEALTH);
        assert_eq!(s.fleets()[Team::A][shooter].shot, StateOption::None);
        assert_eq!(s.fleets()[Team::A][shooter].next_fire_tick, 0);
    }

    #[test]
    fn the_cooldown_gates_firing() {
        let conf = conf();
        let (mut s, shooter, target) = duel(&conf, Vec2::new(10.0, 16.0));
        let action = firing(&[shooter]);

        fire(&mut s, &conf, &action, &FleetAction::new());
        approx(health(&s, Team::B, target), HEALTH - DAMAGE);

        // still the same tick, and every tick up to the cooldown
        for tick in [0, 1, COOLDOWN - 1] {
            s.tick = tick;
            fire(&mut s, &conf, &action, &FleetAction::new());
            approx(health(&s, Team::B, target), HEALTH - DAMAGE);
            assert_eq!(s.fleets()[Team::A][shooter].shot, StateOption::None);
        }

        s.tick = COOLDOWN;
        fire(&mut s, &conf, &action, &FleetAction::new());
        approx(health(&s, Team::B, target), HEALTH - 2.0 * DAMAGE);
        assert_eq!(s.fleets()[Team::A][shooter].next_fire_tick, 2 * COOLDOWN);
    }

    #[test]
    fn a_bot_takes_at_most_one_blast_per_tick() {
        let conf = conf();
        let mut s = state(&conf);
        let a1 = spawn(&mut s, &conf, Team::A, Vec2::new(2.0, 16.0), 0.0);
        let a2 = spawn(&mut s, &conf, Team::A, Vec2::new(3.0, 16.0), 0.0);
        let target = spawn(&mut s, &conf, Team::B, Vec2::new(10.0, 16.0), 180.0);

        // both shots land on the same target on the same tick
        fire(&mut s, &conf, &firing(&[a1, a2]), &FleetAction::new());

        assert!(s.fleets()[Team::A][a1].shot.option().is_some());
        assert!(s.fleets()[Team::A][a2].shot.option().is_some());
        approx(health(&s, Team::B, target), HEALTH - DAMAGE);
        assert_eq!(s.fleets()[Team::B][target].invulnerable_until_tick, INVULN);
    }

    #[test]
    fn invulnerability_blocks_damage_until_the_window_closes() {
        let conf = conf();
        let mut s = state(&conf);
        let a1 = spawn(&mut s, &conf, Team::A, Vec2::new(2.0, 16.0), 0.0);
        let a2 = spawn(&mut s, &conf, Team::A, Vec2::new(3.0, 16.0), 0.0);
        let a3 = spawn(&mut s, &conf, Team::A, Vec2::new(4.0, 16.0), 0.0);
        let a4 = spawn(&mut s, &conf, Team::A, Vec2::new(5.0, 16.0), 0.0);
        let target = spawn(&mut s, &conf, Team::B, Vec2::new(10.0, 16.0), 180.0);

        fire(&mut s, &conf, &firing(&[a1]), &FleetAction::new());
        approx(health(&s, Team::B, target), HEALTH - DAMAGE);

        // a shooter that has not fired yet, on every tick the window is still open -- one
        // per tick, since `COOLDOWN` outlasts `INVULN` and a reused shooter would be gated
        // by its own cooldown instead of by the target's invulnerability
        for (tick, shooter) in [(1, a2), (INVULN - 1, a3)] {
            s.tick = tick;
            fire(&mut s, &conf, &firing(&[shooter]), &FleetAction::new());
            // the shot itself still goes off -- it just does not hurt
            assert!(s.fleets()[Team::A][shooter].shot.option().is_some());
            approx(health(&s, Team::B, target), HEALTH - DAMAGE);
        }

        s.tick = INVULN;
        fire(&mut s, &conf, &firing(&[a4]), &FleetAction::new());
        approx(health(&s, Team::B, target), HEALTH - 2.0 * DAMAGE);
        assert_eq!(
            s.fleets()[Team::B][target].invulnerable_until_tick,
            2 * INVULN
        );
    }

    #[test]
    fn the_shot_only_fires_for_a_matching_class() {
        let conf = conf();
        let (mut s, shooter, target) = duel(&conf, Vec2::new(10.0, 16.0));

        // a battle bot handed someone else's special does nothing
        let mut action = FleetAction::new();
        action.bots[shooter as usize].special_action = SpecialAction::Extractor { mine: true };
        fire(&mut s, &conf, &action, &FleetAction::new());
        approx(health(&s, Team::B, target), HEALTH);

        // and neither does a non-battle bot handed a battle special
        s.fleets_mut()[Team::A][shooter].class = BotClass::Healer;
        fire(&mut s, &conf, &firing(&[shooter]), &FleetAction::new());
        approx(health(&s, Team::B, target), HEALTH);
    }

    #[test]
    fn a_wall_blocks_the_shot() {
        let mut conf = conf();
        conf.map[7][16] = MapTile::Wall;
        let (mut s, shooter, target) = duel(&conf, Vec2::new(10.0, 16.0));

        fire(&mut s, &conf, &firing(&[shooter]), &FleetAction::new());

        approx(health(&s, Team::B, target), HEALTH);
        let point = s.fleets()[Team::A][shooter].shot.option().expect("no shot recorded");
        approx(point.x, 7.0);
    }

    #[test]
    fn allies_neither_block_the_ray_nor_take_the_blast() {
        let conf = conf();
        let (mut s, shooter, target) = duel(&conf, Vec2::new(10.0, 16.0));
        // directly on the line of fire, and inside the target's blast
        let ally = spawn(&mut s, &conf, Team::A, Vec2::new(9.0, 16.0), 0.0);

        fire(&mut s, &conf, &firing(&[shooter]), &FleetAction::new());

        approx(health(&s, Team::B, target), HEALTH - DAMAGE);
        approx(health(&s, Team::A, ally), HEALTH);
        approx(health(&s, Team::A, shooter), HEALTH);
    }

    #[test]
    fn stacked_enemies_all_take_the_blast() {
        let conf = conf();
        let (mut s, shooter, first) = duel(&conf, Vec2::new(10.0, 16.0));
        // nothing separates overlapping bots, so this is a reachable state
        let second = spawn(&mut s, &conf, Team::B, Vec2::new(10.0, 16.0), 180.0);

        fire(&mut s, &conf, &firing(&[shooter]), &FleetAction::new());

        approx(health(&s, Team::B, first), HEALTH - DAMAGE);
        approx(health(&s, Team::B, second), HEALTH - DAMAGE);
    }

    #[test]
    fn a_bot_just_outside_the_splash_is_untouched() {
        let conf = conf();
        let (mut s, shooter, target) = duel(&conf, Vec2::new(10.0, 16.0));
        // blast lands at (9.25, 16); this bot's hull is 0.3 + eps clear of it
        let reach = conf.bot.base_blaster_splash_radius + conf.bot.radius;
        let bystander = spawn(
            &mut s,
            &conf,
            Team::B,
            Vec2::new(10.0 - conf.bot.radius, 16.0 + reach + EPSILON),
            180.0,
        );

        fire(&mut s, &conf, &firing(&[shooter]), &FleetAction::new());

        approx(health(&s, Team::B, target), HEALTH - DAMAGE);
        approx(health(&s, Team::B, bystander), HEALTH);
    }

    #[test]
    fn range_caps_the_ray() {
        let conf = conf();
        // hull starts at x = 16.25, past the shooter's reach of x = 15, and far enough
        // from the air burst at (15, 16) to be clear of the splash too
        let (mut s, shooter, target) = duel(&conf, Vec2::new(17.0, 16.0));

        fire(&mut s, &conf, &firing(&[shooter]), &FleetAction::new());

        approx(health(&s, Team::B, target), HEALTH);
        let point = s.fleets()[Team::A][shooter].shot.option().expect("no shot recorded");
        approx(point.x, 5.0 + RANGE);
        approx(point.y, 16.0);
    }

    #[test]
    fn a_miss_bursts_in_the_air_at_max_range() {
        let conf = conf();
        // off the line of fire, so the ray misses it entirely, but within the splash of
        // the endpoint at (15, 16)
        let (mut s, shooter, target) = duel(&conf, Vec2::new(15.0, 17.0));

        fire(&mut s, &conf, &firing(&[shooter]), &FleetAction::new());

        approx(health(&s, Team::B, target), HEALTH - DAMAGE);
    }

    #[test]
    fn a_killed_bot_leaves_the_fleet() {
        let conf = conf();
        let (mut s, shooter, target) = duel(&conf, Vec2::new(10.0, 16.0));
        s.fleets_mut()[Team::B][target].health = DAMAGE;
        let before = s.clone();

        fire(&mut s, &conf, &firing(&[shooter]), &FleetAction::new());

        assert!(s.fleets()[Team::B].get(target).is_none());
        assert_eq!(s.fleets()[Team::B].len, 0);
        assert_eq!(s.fleets()[Team::A].len, 1, "the shooter is untouched");

        let diff = GameState::diff_json(&before, &s).expect("a death must show in the diff");
        assert_eq!(diff["fleet_b"]["removed"], serde_json::json!([target]));
        assert!(s.fleets()[Team::A][shooter].shot.option().is_some());
    }

    /// A bot that dies this tick still gets its shot off, so a mutual kill is mutual.
    #[test]
    fn shots_resolve_simultaneously() {
        let conf = conf();
        let mut s = state(&conf);
        let a = spawn(&mut s, &conf, Team::A, Vec2::new(5.0, 16.0), 0.0);
        let b = spawn(&mut s, &conf, Team::B, Vec2::new(10.0, 16.0), 180.0);
        s.fleets_mut()[Team::A][a].health = DAMAGE;
        s.fleets_mut()[Team::B][b].health = DAMAGE;

        fire(&mut s, &conf, &firing(&[a]), &firing(&[b]));

        assert_eq!(s.fleets()[Team::A].len, 0);
        assert_eq!(s.fleets()[Team::B].len, 0);
    }

    #[test]
    fn firing_is_mirror_symmetric() {
        let mut conf = conf();
        conf.map[3][3] = MapTile::Wall;
        conf.map[MAP_SIZE - 1 - 3][MAP_SIZE - 1 - 3] = MapTile::Wall;
        let conf = conf;
        let mut s = state(&conf);
        s.capture = 0.25;
        let a = spawn(&mut s, &conf, Team::A, Vec2::new(5.0, 16.0), 0.0);
        spawn(&mut s, &conf, Team::A, Vec2::new(7.0, 20.0), 45.0);
        let b = spawn(&mut s, &conf, Team::B, Vec2::new(10.0, 16.0), 180.0);
        s.tick = 7;

        let (fa, fb) = (firing(&[a]), firing(&[b]));

        let mut mirrored = s.clone();
        mirrored.mirror(&conf);
        // `mirror` swaps the fleets, so team A's actions in the mirrored frame are B's.
        // The actions themselves are side-agnostic here (a bare `Battle { fire }`).
        fire(&mut mirrored, &conf, &fb, &fa);
        mirrored.mirror(&conf);

        fire(&mut s, &conf, &fa, &fb);

        for team in TEAMS {
            let (got, want) = (&s.fleets()[team], &mirrored.fleets()[team]);
            assert_eq!(got.mask, want.mask, "different bots survived");
            for (got, want) in got.iter().zip(want.iter()) {
                approx(got.health, want.health);
                approx(got.pos.x, want.pos.x);
                approx(got.pos.y, want.pos.y);
                assert_eq!(got.next_fire_tick, want.next_fire_tick);
                assert_eq!(got.invulnerable_until_tick, want.invulnerable_until_tick);
                match (got.shot.option(), want.shot.option()) {
                    (Some(got), Some(want)) => {
                        // not bit-identical: the ray is cast from a mirrored angle, so
                        // `from_angle_deg(a)` and `-from_angle_deg(a - 180)` differ in the
                        // last few bits
                        approx(got.x, want.x);
                        approx(got.y, want.y);
                    }
                    (None, None) => {}
                    (got, want) => panic!("shot mismatch: {got:?} vs {want:?}"),
                }
            }
        }
    }
}

#[cfg(test)]
mod wall_collision_test {
    use super::*;

    const R: f32 = 0.75;

    /// `capture` is parked at team A's goal in every test below, so the payload sits in a
    /// corner and never interferes with the walls a test puts down.
    fn conf() -> GameConfig {
        GameConfig {
            max_ticks: 100,
            bot: BotConfig {
                radius: R,
                base_speed: 0.1,
                base_health: 10.0,
                base_turn_speed: 10.0,
                base_blaster_cooldown: 60,
                base_invulnerability_ticks: 15,
                base_blaster_range: 10.0,
                base_blaster_damage: 3.0,
                base_blaster_splash_radius: 0.3,
            },
            payload: PayloadConfig {
                radius: 1.5,
                capture_radius: 3.0,
                speed_per_bot: 0.01,
                max_speed: 0.04,
                contest_diff: 1,
            },
            payload_path: PAYLOAD_PATH,
            deposit_count: 0,
            deposits: [Deposit::default(); DEPOSITS_MAX],
            map: [[MapTile::Empty; MAP_SIZE]; MAP_SIZE],
        }
    }

    fn wall(conf: &mut GameConfig, xs: std::ops::Range<usize>, ys: std::ops::Range<usize>) {
        for x in xs {
            for y in ys.clone() {
                conf.map[x][y] = MapTile::Wall;
            }
        }
    }

    fn spawn(state: &mut GameState, conf: &GameConfig, team: Team, pos: Vec2) -> BotId {
        let id = state.fleets_mut()[team].add();
        reset_bot(team, id, pos, state, conf);
        id
    }

    fn state(conf: &GameConfig) -> GameState {
        let mut state = GameState::new(conf);
        state.capture = -1.0;
        state
    }

    /// Deepest penetration of `pos`'s hull into any wall tile; zero if it is clear.
    fn overlap(conf: &GameConfig, pos: Vec2) -> f32 {
        let mut worst: f32 = 0.0;
        for x in 0..MAP_SIZE {
            for y in 0..MAP_SIZE {
                if conf.map[x][y] != MapTile::Wall {
                    continue;
                }
                let near = Vec2::new(
                    pos.x.clamp(x as f32, x as f32 + 1.0),
                    pos.y.clamp(y as f32, y as f32 + 1.0),
                );
                worst = worst.max(R - pos.dist(&near));
            }
        }
        worst
    }

    fn approx(a: f32, b: f32) {
        assert!((a - b).abs() < 1e-3, "expected {b}, got {a}");
    }

    #[test]
    fn a_bot_is_pushed_clear_of_a_wall_face() {
        let mut conf = conf();
        wall(&mut conf, 10..11, 10..11);
        let mut s = state(&conf);
        // just left of the tile, hull buried 0.55 into it
        let id = spawn(&mut s, &conf, Team::A, Vec2::new(9.8, 10.5));
        s.fleets_mut()[Team::A][id].vel = Vec2::new(0.1, 0.0);

        assert!(handle_collision(&mut s, &conf));

        let bot = &s.fleets()[Team::A][id];
        approx(bot.pos.x, 10.0 - R - EPSILON);
        approx(bot.pos.y, 10.5);
        // the component driving it into the wall is gone
        approx(bot.vel.x, 0.0);
    }

    #[test]
    fn only_the_normal_component_of_velocity_is_lost() {
        let mut conf = conf();
        wall(&mut conf, 10..11, 10..11);
        let mut s = state(&conf);
        let id = spawn(&mut s, &conf, Team::A, Vec2::new(9.8, 10.5));
        s.fleets_mut()[Team::A][id].vel = Vec2::new(0.1, 0.07);

        assert!(handle_collision(&mut s, &conf));

        let bot = &s.fleets()[Team::A][id];
        approx(bot.vel.x, 0.0);
        approx(bot.vel.y, 0.07); // tangential motion survives
    }

    /// A flat wall is many tiles, and each tile's own push-out would fight its neighbour's
    /// at the seam between them. The interior-face skip is what keeps a bot sliding.
    #[test]
    fn a_bot_slides_along_a_flat_wall_without_catching_on_a_seam() {
        let mut conf = conf();
        wall(&mut conf, 5..16, 10..11);
        let mut s = state(&conf);
        let id = spawn(&mut s, &conf, Team::A, Vec2::new(6.0, 11.5));

        let step = 0.1;
        for _ in 0..50 {
            {
                let bot = &mut s.fleets_mut()[Team::A][id];
                // driving into the wall and to the right, exactly as `eval_tick` would
                bot.pos += Vec2::new(step, -step);
                bot.vel = Vec2::new(step, -step);
            }
            assert!(handle_collision(&mut s, &conf));

            let bot = &s.fleets()[Team::A][id];
            approx(bot.pos.y, 11.0 + R + EPSILON);
            assert_eq!(overlap(&conf, bot.pos), 0.0);
        }

        // it crossed six tile seams and never lost a step of tangential travel
        approx(s.fleets()[Team::A][id].pos.x, 6.0 + 50.0 * step);
    }

    #[test]
    fn a_bot_in_a_concave_corner_resolves() {
        let mut conf = conf();
        wall(&mut conf, 10..11, 6..12); // vertical leg
        wall(&mut conf, 5..11, 11..12); // horizontal leg, meeting it at (10, 11)
        let mut s = state(&conf);
        let id = spawn(&mut s, &conf, Team::A, Vec2::new(9.6, 10.6));

        assert!(
            handle_collision(&mut s, &conf),
            "the corner did not settle within COLLISION_MAX_ITERATIONS"
        );
        assert_eq!(overlap(&conf, s.fleets()[Team::A][id].pos), 0.0);
    }

    #[test]
    fn a_bot_inside_a_wall_tile_is_ejected() {
        let mut conf = conf();
        wall(&mut conf, 10..11, 10..11);
        let mut s = state(&conf);
        // dead centre of the tile -- no outward direction to read off a nearest point
        let id = spawn(&mut s, &conf, Team::A, Vec2::new(10.5, 10.5));

        assert!(handle_collision(&mut s, &conf));
        assert_eq!(overlap(&conf, s.fleets()[Team::A][id].pos), 0.0);
    }

    /// A bot buried in a wall several tiles thick still comes out, rather than settling
    /// inside it.
    #[test]
    fn a_bot_inside_a_thick_wall_is_ejected() {
        let mut conf = conf();
        wall(&mut conf, 8..13, 8..13);
        let mut s = state(&conf);
        let id = spawn(&mut s, &conf, Team::A, Vec2::new(10.5, 10.5));

        assert!(handle_collision(&mut s, &conf));
        assert_eq!(overlap(&conf, s.fleets()[Team::A][id].pos), 0.0);
    }

    /// Wall resolution has to be side-agnostic like everything else: mirror the world,
    /// resolve, mirror back, and the bots must land where they landed unmirrored.
    #[test]
    fn wall_collision_is_mirror_symmetric() {
        let mut conf = conf();
        // a mirror-symmetric pair, as `config::MAP` guarantees for the real arena
        wall(&mut conf, 10..16, 10..12);
        wall(&mut conf, MAP_SIZE - 16..MAP_SIZE - 10, MAP_SIZE - 12..MAP_SIZE - 10);
        let conf = conf;

        let mut s = state(&conf);
        s.capture = 0.25;
        let a = spawn(&mut s, &conf, Team::A, Vec2::new(12.4, 12.3));
        let b = spawn(&mut s, &conf, Team::B, Vec2::new(11.0, 9.6));
        for (team, id) in [(Team::A, a), (Team::B, b)] {
            s.fleets_mut()[team][id].vel = Vec2::new(0.1, -0.1);
        }

        let mut mirrored = s.clone();
        mirrored.mirror(&conf);
        assert!(handle_collision(&mut mirrored, &conf));
        mirrored.mirror(&conf);

        assert!(handle_collision(&mut s, &conf));

        for team in TEAMS {
            for (got, want) in s.fleets()[team].iter().zip(mirrored.fleets()[team].iter()) {
                approx(got.pos.x, want.pos.x);
                approx(got.pos.y, want.pos.y);
                approx(got.vel.x, want.vel.x);
                approx(got.vel.y, want.vel.y);
            }
        }
    }

    /// The shipped arena, with bots walked across it from every direction: nothing tunnels
    /// and nothing gets stuck inside a wall.
    #[test]
    fn the_real_map_never_traps_a_bot() {
        let mut conf = conf();
        conf.map = MAP;
        let mut s = state(&conf);

        let dirs = [
            Vec2::new(1.0, 0.0),
            Vec2::new(-1.0, 0.0),
            Vec2::new(0.0, 1.0),
            Vec2::new(0.0, -1.0),
            Vec2::new(0.7071, 0.7071),
            Vec2::new(-0.7071, -0.7071),
        ];
        for (i, dir) in dirs.iter().enumerate() {
            let start = Vec2::new(2.0 + i as f32 * 4.0, 2.0);
            let id = spawn(&mut s, &conf, Team::A, start);
            for _ in 0..400 {
                s.fleets_mut()[Team::A][id].pos += *dir * conf.bot.base_speed;
                assert!(handle_collision(&mut s, &conf));
                let pos = s.fleets()[Team::A][id].pos;
                assert_eq!(overlap(&conf, pos), 0.0, "bot ended up inside a wall at {pos:?}");
            }
            s.fleets_mut()[Team::A].remove(id);
        }
    }
}
