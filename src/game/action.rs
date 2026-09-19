use std::cell::RefCell;

use crate::game::team::{Team, TeamPair, TEAMS};

use super::{
    config::*,
    geom::{scan, ScanMask, ScanTarget},
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
    let deposits = [state.deposit_a.pos, state.deposit_b.pos];
    let min_deposit_dist = conf.deposit.radius + r;


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

        // deposits -- solid, exactly like the payload
        for bot in &mut bots {
            for deposit in deposits {
                let delta = bot.pos - deposit;
                if delta.norm_sq() < min_deposit_dist * min_deposit_dist {
                    let dir = delta.normalize_or_else(|| Vec2::new(1.0, 0.0));
                    bot.pos = deposit + dir * (min_deposit_dist + EPSILON);
                    bot.vel = Vec2::ZERO;
                    resolved = false;
                }
            }
        }

        // TODO upgrade stations

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


    if counts[Team::A] != 0 && counts[Team::B] != 0 {
        return;
    }

    // Positive margin pushes `capture` toward `PAYLOAD_PATH`'s end (team B's goal); the
    // sign flip in `GameState::mirror` is what makes this side-agnostic for bots.
    let margin = counts[Team::A] - counts[Team::B];

    let delta = margin.signum() as f32 * conf.payload.speed / payload_path_len();
    state.capture = (state.capture + delta).clamp(-1.0, 1.0);
}

/// Resets every field of bot `id` to its starting values, placing it at `pos`.
fn reset_bot(
    team: Team,
    id: BotId,
    pos: Vec2,
    class: BotClass,
    state: &mut GameState,
    conf: &GameConfig,
) {
    let health = conf.bot.health;
    let fleet = &mut state.fleets_mut()[team];
    let Some(bot) = fleet.get_mut(id) else {
        return;
    };
    bot.id = id;
    bot.health = health;
    bot.pos = pos;
    bot.vel = Vec2::ZERO;
    bot.angle = 0.0;
    bot.turn_vel = 0.0;
    bot.invulnerable_until_tick = 0;
    bot.special = SpecialState::new(class);
}

/// Applies every healer's channel for this tick.
///
/// A healer names its target by `BotId` -- always a bot in its own fleet, since `BotId` is
/// fleet-local -- but the heal only lands if the healer is also *facing* it: the target must
/// be within `base_heal_range` (center to center, unlike the blaster's splash, which measures
/// to the hull) and within half of `base_heal_arc_deg` of the healer's facing, with walls and
/// the arena boundary blocking line of sight. That facing gate is what gives `angle`,
/// `turn_vel` and `TurnAction` a job on this class; without it the healer would be the one
/// bot in the game that never needs to point anywhere.
///
/// Bots, the payload and deposits deliberately do *not* block the heal. Nothing separates
/// overlapping bots (see `handle_collision`), so letting an ally block would be arbitrary, and
/// the payload is not allowed to shield a bot from its own healer.
///
/// A healer may target itself, which short-circuits the geometry entirely -- the direction to
/// yourself is the zero vector, and its angle is meaningless.
///
/// There is no cooldown: this is a per-tick trickle, so `next_fire_tick` stays blaster-only.
/// Healers stack on one target, but the total any one bot receives per tick is capped at
/// `heal_per_tick_cap`, and no heal can push a bot past `base_health`.
///
/// Heals are accumulated against the pre-heal state and applied together. Two-phase for the
/// same reason as `step_blasters`, plus one of its own: applying the per-target cap greedily
/// would make the outcome depend on the `bot_actions` shuffle in `eval_tick`.
///
/// Note on the gamelog: `health` is a plain `Diff` leaf, so -- unlike `next_fire_tick` and
/// `invulnerable_until_tick`, which are absolute ticks precisely to stay out of the diff --
/// an active heal puts its target into every gamelog line for as long as it runs. That is
/// inherent to a continuous trickle and is accepted, not an oversight.
fn step_healers(
    state: &mut GameState,
    conf: &GameConfig,
    bot_actions: &[(Team, BotId, &BotAction)],
) {
    let range = conf.bot.base_heal_range;
    let half_arc = conf.bot.base_heal_arc_deg / 2.0;
    let heal_rate = conf.bot.heal_per_tick;
    let max_health = conf.bot.health;

    let mut heals: TeamPair<[f32; BOTS_MAX]> = TeamPair::new([0.0; BOTS_MAX], [0.0; BOTS_MAX]);

    // Last tick's channels are stale. Same self-clearing trick as `shot` -- see
    // `SpecialState::Healer`.
    for team in TEAMS {
        for bot in state.fleets_mut()[team].iter_mut() {
            if let SpecialState::Healer { healing } = &mut bot.special {
                *healing = StateOption::None;
            }
        }
    }

    for (team, id, action) in bot_actions {
        let (team, id) = (*team, *id);
        let bot = &state.fleets()[team][id];

        if bot.class() != action.special_action.class() {
            continue; // an action for a class this bot is not
        }
        let SpecialAction::Healer { fire: true, target } = action.special_action else {
            continue;
        };

        // A target that died on an earlier tick is already out of the fleet, so this also
        // covers stale ids. `target` indexes the healer's own fleet -- it can never name an
        // enemy.
        let Some(ally) = state.fleets()[team].get(target) else {
            continue;
        };

        // A healer cannot heal itself.
        if target == id {
            continue;
        }

        let to_ally = ally.pos - bot.pos;
        if to_ally.norm_sq() > range * range {
            continue;
        }
        if diff_degrees(to_ally.angle_deg(), bot.angle).abs() > half_arc {
            continue;
        }
        let blocked = scan(
            state,
            conf,
            bot.pos,
            to_ally,
            ScanMask::WALLS | ScanMask::BOUNDARY,
            None,
            to_ally.norm(),
        )
        .is_some();
        if blocked {
            continue;
        }

        heals[team][target as usize] += heal_rate;

        // Every gate has passed, so this channel is real and renderable. Recorded on the
        // *healer*, not the target: one bot can be healed by several at once, and it is the
        // healer that owns the beam.
        if let SpecialState::Healer { healing } = &mut state.fleets_mut()[team][id].special {
            *healing = StateOption::Some(target);
        }
    }

    for team in TEAMS {
        // A multiple of the heal rate -- see `BotConfig::heal_stack_cap`. Both
        // healer and target are in `team`: a healer can only ever name its own fleet.
        let cap = conf.bot.heal_stack_cap * heal_rate;
        for bot in state.fleets_mut()[team].iter_mut() {
            let amount = heals[team][bot.id as usize];
            if amount <= 0.0 {
                continue;
            }
            bot.health = (bot.health + amount.min(cap)).min(max_health);
        }
    }

    // No death sweep: healing cannot kill, and `step_blasters` sweeps immediately after.
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
    let range = conf.bot.blaster_range;
    let cooldown = conf.bot.blaster_cooldown;
    let damage = conf.bot.blaster_damage;

    // Last tick's beams are stale. Clearing them here is what makes `shot` transient: the
    // `Some` -> `None` transition is itself a diff, so the gamelog self-clears.
    for team in TEAMS {
        for bot in state.fleets_mut()[team].iter_mut() {
            if let SpecialState::Battle { shot, .. } = &mut bot.special {
                *shot = StateOption::None;
            }
        }
    }

    let mut blasts: Vec<(Team, Vec2)> = Vec::new();

    for (team, id, action) in bot_actions {
        let (team, id) = (*team, *id);
        let bot = &state.fleets()[team][id];

        if bot.class() != action.special_action.class() {
            continue; // an action for a class this bot is not
        }
        if !matches!(action.special_action, SpecialAction::Battle { fire: true }) {
            continue;
        }
        if bot.next_fire_tick() > tick {
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
        let reach = range;
        let point = scan(state, conf, origin, dir, mask, None, reach)
            .map(|hit| hit.point)
            .unwrap_or(origin + dir * reach);

        blasts.push((team, point));

        let bot = &mut state.fleets_mut()[team][id];
        if let SpecialState::Battle { next_fire_tick, shot } = &mut bot.special {
            *shot = StateOption::Some(point);
            *next_fire_tick = tick + cooldown;
        }
    }

    for (team, point) in &blasts {
        let splash = conf.bot.base_blaster_splash_radius + conf.bot.radius;
        for bot in state.fleets_mut()[team.other_team()].iter_mut() {
            if bot.invulnerable_until_tick > tick {
                continue;
            }
            if bot.pos.dist_sq(point) <= splash * splash {
                bot.health -= damage;
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
            remove_bot(state, team, id);
        }
    }
}

/// Removes a bot from its fleet, releasing whatever extraction slot it held so a dead bot
/// never lingers in a gamelog line holding one. `step_extractors` would drop it next tick
/// anyway; this just keeps the state honest within the tick it died.
fn remove_bot(state: &mut GameState, team: Team, id: BotId) {
    state.fleets_mut()[team].remove(id);
    for deposit in TEAMS {
        state.deposits_mut()[deposit].extractors[team] &= !(1u32 << id);
    }
}

/// A bot whose action requested self-destruct this tick dies instantly -- no damage to
/// anything else, just removal. Runs after damage, so a self-destructing bot still gets to
/// act (move, fire, heal, extract) on the tick it blows itself up, the same as one that dies
/// to a blast.
///
/// `bot_actions` is flattened before the tick's deaths, so a bot that self-destructed *and*
/// was killed by a blast this tick is still listed here after `step_blasters` already swept
/// it. The membership check is what makes that case a no-op -- `BotArray::remove` asserts,
/// so removing twice would panic the engine mid-match.
fn step_self_destruct(state: &mut GameState, bot_actions: &[(Team, BotId, &BotAction)]) {
    for (team, id, action) in bot_actions {
        if action.self_destruct && state.fleets()[*team].get(*id).is_some() {
            remove_bot(state, *team, *id);
        }
    }
}

/// Awards this tick's extraction slots and pays out the tokens they earn.
///
/// An extractor mines by *looking* at a deposit: one ray from its center along its facing,
/// capped at `base_extract_range`, blocked only by wall tiles and the arena boundary. Bots
/// and the payload deliberately do not block it -- the same reasoning as `step_healers`, and
/// it keeps a deposit from being denied by parking something in front of it. The deposit
/// itself is solid (see `handle_collision`), so the ray always terminates on its near hull.
///
/// Each deposit supports `conf.deposit.extractor_cap` extractors at once, **shared between
/// the teams**: fill them all and the enemy gets nothing from that deposit. Slots are sticky.
/// A bot already holding one keeps it as long as it still qualifies, and only what is left
/// over is open to newcomers, so a team cannot displace an entrenched one by piling on. A
/// holder that dies, sets `mine: false`, turns away, or loses line of sight simply fails to
/// re-qualify and its slot opens the same tick. A holder that swings its ray from one deposit
/// to the other arrives at the second as a newcomer.
///
/// Newcomers are admitted in `bot_actions` order, which `eval_tick` shuffles -- the
/// codebase's standing convention for a tie nothing else decides.
///
/// Membership is a `u32` bitmask per team per deposit (`state::Deposit::extractors`), so
/// admission is an `|=`, occupancy a `count_ones()`, and the whole step is one `scan` per
/// extractor and no sweep over `BOTS_MAX`.
fn step_extractors(
    state: &mut GameState,
    conf: &GameConfig,
    bot_actions: &[(Team, BotId, &BotAction)],
) {
    let range = conf.bot.base_extract_range;
    let cap = conf.deposit.extractor_cap as u32;

    // Last tick's channels are stale. Same self-clearing trick as `shot` and `healing`.
    for team in TEAMS {
        for bot in state.fleets_mut()[team].iter_mut() {
            if let SpecialState::Extractor { extracting } = &mut bot.special {
                *extracting = StateOption::None;
            }
        }
    }

    // Who is aiming at what, resolved once against the pre-award state. The third element
    // is the deposit, named by the team that owns it.
    let mut aimed: Vec<(Team, BotId, Team)> = Vec::with_capacity(bot_actions.len());

    for (team, id, action) in bot_actions {
        let (team, id) = (*team, *id);
        let bot = &state.fleets()[team][id];

        if bot.class() != action.special_action.class() {
            continue; // an action for a class this bot is not
        }
        if !matches!(action.special_action, SpecialAction::Extractor { mine: true }) {
            continue;
        }

        let dir = Vec2::from_angle_deg(bot.angle);
        let mask = ScanMask::WALLS | ScanMask::BOUNDARY | ScanMask::DEPOSITS;
        if let Some(hit) = scan(state, conf, bot.pos, dir, mask, None, range) {
            if let ScanTarget::Deposit { team: deposit } = hit.target {
                aimed.push((team, id, deposit));
            }
        }
    }

    // Rebuilt from scratch rather than edited, so everyone who died, stopped or looked away
    // is dropped for free -- only what re-qualifies below is carried over.
    let old = TeamPair::new(state.deposit_a.extractors, state.deposit_b.extractors);
    let mut new = TeamPair::new(TeamPair::new(0u32, 0u32), TeamPair::new(0u32, 0u32));

    // Incumbents first: a slot held last tick at this same deposit is kept.
    for (team, id, deposit) in &aimed {
        let bit = 1u32 << id;
        if old[*deposit][*team] & bit != 0 {
            new[*deposit][*team] |= bit;
        }
    }

    // Then newcomers, into whatever the incumbents left.
    for (team, id, deposit) in &aimed {
        let bit = 1u32 << id;
        if new[*deposit][*team] & bit != 0 {
            continue; // already an incumbent
        }
        let taken =
            new[*deposit][Team::A].count_ones() + new[*deposit][Team::B].count_ones();
        if taken >= cap {
            continue;
        }
        new[*deposit][*team] |= bit;
    }

    for deposit in TEAMS {
        state.deposits_mut()[deposit].extractors = new[deposit];
    }

    let rate = conf.bot.extract_rate;
    for deposit in TEAMS {
        for team in TEAMS {
            let mut bits = new[deposit][team];
            if bits == 0 {
                continue;
            }
            state.fabricators_mut()[team].tokens += rate * bits.count_ones() as f32;
            while bits != 0 {
                let id = bits.trailing_zeros() as BotId;
                bits &= bits - 1;
                if let SpecialState::Extractor { extracting } =
                    &mut state.fleets_mut()[team][id].special
                {
                    *extracting = StateOption::Some(deposit);
                }
            }
        }
    }
}

/// Builds one bot of `class` for `team`, or does nothing if the fleet is already full.
///
/// Every bot is placed in team A's frame and team B's is mirrored into position afterwards,
/// so the spawn pose -- position *and* facing -- is symmetric by construction rather than by
/// two hand-kept literals.
fn spawn_bot(team: Team, class: BotClass, state: &mut GameState, conf: &GameConfig) {
    if state.fleets()[team].is_full() {
        return;
    }
    // Team A's own goal: the end of the payload path team B is pushing toward.
    let pos = Vec2::new(
        conf.bot.radius + EPSILON,
        MAP_SIZE as f32 - conf.bot.radius - EPSILON,
    );
    let id = state.fleets_mut()[team].add();
    reset_bot(team, id, pos, class, state, conf);
    if matches!(team, Team::B) {
        state.fleets_mut()[team][id].mirror(conf);
    }
}

/// Runs both fabricators: this tick's purchases, then this tick's builds.
///
/// Order within a fleet is rush, then natural build. A rush lands before the natural build
/// and pushes the timer one tick if it was due, so **at most one bot per fleet enters per
/// tick** and the natural build is deferred rather than swallowed.
///
/// A rush spends tokens banked as of last tick: `step_extractors` runs later in
/// `eval_tick`, so this tick's mining pays for next tick's shopping. It cannot fail loudly
/// -- there is no error channel back to a bot, so an unaffordable request is simply not
/// applied, and `FabricatorState` not moving is the whole report.
///
/// In the endgame nothing is built at all: a rush is refused without being charged, and
/// the natural timer is ignored.
fn step_fabricators(state: &mut GameState, conf: &GameConfig, actions: TeamPair<&FleetAction>) {
    if state.in_endgame(conf) {
        return;
    }
    for team in TEAMS {
        let action = actions[team];

        // --- rush order ---
        // A rush is the natural build bought early: same class, same spawn, just paid for.
        // Checked against a full fleet *before* debiting, so a rush into a full fleet costs
        // nothing rather than buying a bot that cannot be placed.
        if action.rush_order
            && !state.fleets()[team].is_full()
            && state.fabricators()[team].tokens >= conf.fabricator.rush_cost
        {
            state.fabricators_mut()[team].tokens -= conf.fabricator.rush_cost;
            spawn_bot(team, action.fabricator_next.clone(), state, conf);

            // A natural build due this tick is pushed one tick rather than lost: the fleet
            // paid for a bot and should get both. `<=` and not `==` because the timer can
            // sit in the past, held by a full fleet.
            if state.fabricators()[team].next_bot_creation <= state.tick {
                state.fabricators_mut()[team].next_bot_creation = state.tick + 1;
            }
        }

        // --- natural build ---
        if state.fabricators()[team].next_bot_creation > state.tick {
            continue;
        }
        // A full fleet falls through *without* touching the timer, which is what makes the
        // build hold: `next_bot_creation` stays in the past and the bot arrives the tick a
        // slot opens.
        if state.fleets()[team].is_full() {
            continue;
        }
        spawn_bot(team, action.fabricator_next.clone(), state, conf);
        state.fabricators_mut()[team].next_bot_creation = state.tick + conf.fabricator.interval;
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

    // fabrication -- first, so a bot bought this tick is in the world for every step below.
    //
    // After `bot_actions` is frozen, deliberately: a bot built this tick has no action of
    // its own, since the fleet chose its actions without knowing which slot the new bot
    // would land in, and the stale entry sitting in that slot is not a command anyone gave.
    // So it does not move or fire on its first tick -- but it is in the world for every
    // step that sweeps the fleets, which is to say it can be shot, healed, shoved by
    // `handle_collision`, and counted for the payload push.
    step_fabricators(state, conf, TeamPair::new(&actions_a, &actions_b));



    // movement

    let speed = conf.bot.speed;
    let turn_speed = conf.bot.turn_speed;

    for (team, id, action) in &bot_actions {
        let max_turn_speed = turn_speed;
        let vel = action.move_action.direction * speed;
        let bot = &mut state.fleets_mut()[*team][*id];

        bot.pos += vel;
        bot.vel = vel;
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

    // extraction -- after collision, so a bot shoved off a deposit this tick does not get
    // paid for a sightline it no longer has, and before damage, so a bot killed this tick
    // still banks the tick it worked, the same way it still gets its shot off.
    step_extractors(state, conf, &bot_actions);

    // healing -- before damage, so a bot healed this tick can survive a blast this tick and
    // the death sweep at the end of `step_blasters` sees the healed value.
    step_healers(state, conf, &bot_actions);

    // damage
    step_blasters(state, conf, &bot_actions);

    // self-destruct -- after damage, so a bot that dies to a blast this tick is removed by
    // the same sweep either way and this step has nothing left to do for it.
    step_self_destruct(state, &bot_actions);

    state.tick += 1;
}

/// Why a match ended. Engine-only: bots are never told, and nothing in `GameState` carries it.
#[derive(serde::Serialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum EndReason {
    /// The payload reached the end of its track; the team whose goal it is loses.
    Payload,
    /// A fleet had no bots left during the endgame; the other fleet wins.
    Elimination,
    /// Tiebreak 1: the payload is on the loser's side of center.
    PayloadProgress,
    /// Tiebreak 2: larger total health over living bots.
    HealthPool,
    /// Tiebreak 3: more tokens banked.
    Tokens,
    /// Every tiebreak was level.
    Tie,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct MatchResult {
    /// `None` only for `EndReason::Tie`.
    pub winner: Option<Team>,
    pub reason: EndReason,
}

/// Whether the match is over after the tick just evaluated, and if so who won.
///
/// Checked in order: a payload at either goal ends the match at any time; in the endgame,
/// a fleet with no bots loses (both fleets empty on the same tick goes to the tiebreaks);
/// and once `max_ticks` is reached, the tiebreaks decide it.
pub fn match_result(state: &GameState, conf: &GameConfig) -> Option<MatchResult> {
    // `capture == 1.0` is team B's goal, so B has lost; `-1.0` is A's.
    if state.capture >= 1.0 {
        return Some(MatchResult { winner: Some(Team::A), reason: EndReason::Payload });
    }
    if state.capture <= -1.0 {
        return Some(MatchResult { winner: Some(Team::B), reason: EndReason::Payload });
    }

    if state.in_endgame(conf) {
        let fleets = state.fleets();
        match (fleets[Team::A].len == 0, fleets[Team::B].len == 0) {
            (true, true) => return Some(tiebreak(state)),
            (true, false) => {
                return Some(MatchResult { winner: Some(Team::B), reason: EndReason::Elimination })
            }
            (false, true) => {
                return Some(MatchResult { winner: Some(Team::A), reason: EndReason::Elimination })
            }
            (false, false) => {}
        }
    }

    (state.tick >= conf.max_ticks).then(|| tiebreak(state))
}

/// Payload side, then health pool, then tokens, then a tie.
fn tiebreak(state: &GameState) -> MatchResult {
    use std::cmp::Ordering;
    let fabricators = state.fabricators();
    let steps = [
        // Positive `capture` sits on team B's side, which is A ahead.
        (state.capture.total_cmp(&0.0), EndReason::PayloadProgress),
        (
            state.health_pool(Team::A).total_cmp(&state.health_pool(Team::B)),
            EndReason::HealthPool,
        ),
        (
            fabricators[Team::A].tokens.total_cmp(&fabricators[Team::B].tokens),
            EndReason::Tokens,
        ),
    ];
    for (ordering, reason) in steps {
        match ordering {
            Ordering::Greater => return MatchResult { winner: Some(Team::A), reason },
            Ordering::Less => return MatchResult { winner: Some(Team::B), reason },
            Ordering::Equal => {}
        }
    }
    MatchResult { winner: None, reason: EndReason::Tie }
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
    const HEAL: f32 = 0.5;
    const HEAL_RANGE: f32 = 3.0;
    const HEAL_ARC: f32 = 90.0;
    const HEAL_CAP: f32 = 1.5;

    fn conf() -> GameConfig {
        GameConfig {
            max_ticks: 100,
            endgame_ticks: 0,
            bot: BotConfig {
                radius: 0.75,
                speed: 0.1,
                health: HEALTH as f32,
                turn_speed: 10.0,
                blaster_cooldown: COOLDOWN as u32,
                base_invulnerability_ticks: INVULN,
                blaster_range: RANGE as f32,
                blaster_damage: DAMAGE as f32,
                base_blaster_splash_radius: 0.3,
                heal_per_tick: HEAL as f32,
                base_heal_range: HEAL_RANGE,
                base_heal_arc_deg: HEAL_ARC,
                heal_stack_cap: (HEAL_CAP) / (HEAL),
                base_extract_range: 5.0,
                extract_rate: 0.1,
            },
            payload: PayloadConfig {
                radius: 1.5,
                capture_radius: 3.0,
                speed: 0.01,
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
        reset_bot(team, id, pos, BotClass::Battle, state, conf);
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
        let point = s.fleets()[Team::A][shooter].shot().option().expect("no shot recorded");
        approx(point.x, 10.0 - conf.bot.radius);
        approx(point.y, 16.0);
        assert_eq!(s.fleets()[Team::A][shooter].next_fire_tick(), COOLDOWN);
    }

    #[test]
    fn not_firing_records_nothing() {
        let conf = conf();
        let (mut s, shooter, target) = duel(&conf, Vec2::new(10.0, 16.0));

        fire(&mut s, &conf, &FleetAction::new(), &FleetAction::new());

        approx(health(&s, Team::B, target), HEALTH);
        assert_eq!(s.fleets()[Team::A][shooter].shot(), StateOption::None);
        assert_eq!(s.fleets()[Team::A][shooter].next_fire_tick(), 0);
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
            assert_eq!(s.fleets()[Team::A][shooter].shot(), StateOption::None);
        }

        s.tick = COOLDOWN;
        fire(&mut s, &conf, &action, &FleetAction::new());
        approx(health(&s, Team::B, target), HEALTH - 2.0 * DAMAGE);
        assert_eq!(s.fleets()[Team::A][shooter].next_fire_tick(), 2 * COOLDOWN);
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

        assert!(s.fleets()[Team::A][a1].shot().option().is_some());
        assert!(s.fleets()[Team::A][a2].shot().option().is_some());
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
            assert!(s.fleets()[Team::A][shooter].shot().option().is_some());
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
        s.fleets_mut()[Team::A][shooter].special = SpecialState::new(BotClass::Healer);
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
        let point = s.fleets()[Team::A][shooter].shot().option().expect("no shot recorded");
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
        let point = s.fleets()[Team::A][shooter].shot().option().expect("no shot recorded");
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
        assert!(s.fleets()[Team::A][shooter].shot().option().is_some());
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
                assert_eq!(got.next_fire_tick(), want.next_fire_tick());
                assert_eq!(got.invulnerable_until_tick, want.invulnerable_until_tick);
                match (got.shot().option(), want.shot().option()) {
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
mod healer_test {
    use super::*;

    const HEALTH: f32 = 10.0;
    const HEAL: f32 = 0.5;
    const RANGE: f32 = 3.0;
    const ARC: f32 = 90.0; // full width, so the gate is +/- 45 degrees
    const CAP: f32 = 1.5; // three healers' worth

    fn conf() -> GameConfig {
        GameConfig {
            max_ticks: 100,
            endgame_ticks: 0,
            bot: BotConfig {
                radius: 0.75,
                speed: 0.1,
                health: HEALTH as f32,
                turn_speed: 10.0,
                blaster_cooldown: 60,
                base_invulnerability_ticks: 15,
                blaster_range: 10.0,
                blaster_damage: 3.0,
                base_blaster_splash_radius: 0.3,
                heal_per_tick: HEAL as f32,
                base_heal_range: RANGE,
                base_heal_arc_deg: ARC,
                heal_stack_cap: (CAP) / (HEAL),
                base_extract_range: 5.0,
                extract_rate: 0.1,
            },
            payload: PayloadConfig {
                radius: 1.5,
                capture_radius: 3.0,
                speed: 0.01,
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
            map: [[MapTile::Empty; MAP_SIZE]; MAP_SIZE],
        }
    }

    /// `capture` is parked at team A's goal so the payload sits in a corner, clear of the
    /// y = 16 line the tests work on -- except where a test moves it there deliberately.
    fn state(conf: &GameConfig) -> GameState {
        let mut state = GameState::new(conf);
        state.capture = -1.0;
        state
    }

    fn spawn(
        state: &mut GameState,
        conf: &GameConfig,
        team: Team,
        pos: Vec2,
        angle: f32,
        class: BotClass,
    ) -> BotId {
        let id = state.fleets_mut()[team].add();
        reset_bot(team, id, pos, class, state, conf);
        state.fleets_mut()[team][id].angle = angle;
        id
    }

    /// A `FleetAction` in which each of `healers` channels at `target`.
    fn healing(healers: &[BotId], target: BotId) -> FleetAction {
        let mut action = FleetAction::new();
        for id in healers {
            action.bots[*id as usize].special_action =
                SpecialAction::Healer { fire: true, target };
        }
        action
    }

    /// The flattened list `eval_tick` hands to `step_healers`, in a fixed order -- the step
    /// is order-independent, so the tests do not need the shuffle.
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

    fn heal(state: &mut GameState, conf: &GameConfig, a: &FleetAction, b: &FleetAction) {
        let actions = pairs(state, a, b);
        step_healers(state, conf, &actions);
    }

    fn approx(a: f32, b: f32) {
        assert!((a - b).abs() < 1e-4, "expected {b}, got {a}");
    }

    /// A healer at (5, 16) facing east and a wounded ally wherever the test wants it.
    /// Both start at half health.
    fn pair(conf: &GameConfig, ally: Vec2) -> (GameState, BotId, BotId) {
        let mut s = state(conf);
        let healer = spawn(&mut s, conf, Team::A, Vec2::new(5.0, 16.0), 0.0, BotClass::Healer);
        let target = spawn(&mut s, conf, Team::A, ally, 0.0, BotClass::Battle);
        s.fleets_mut()[Team::A][healer].health = HEALTH / 2.0;
        s.fleets_mut()[Team::A][target].health = HEALTH / 2.0;
        (s, healer, target)
    }

    #[test]
    fn an_ally_in_range_and_in_arc_is_healed() {
        let conf = conf();
        let (mut s, healer, target) = pair(&conf, Vec2::new(7.0, 16.0));

        heal(&mut s, &conf, &healing(&[healer], target), &FleetAction::new());

        approx(s.fleets()[Team::A][target].health, HEALTH / 2.0 + HEAL);
        // the healer does not heal itself as a side effect of healing someone else
        approx(s.fleets()[Team::A][healer].health, HEALTH / 2.0);
    }

    /// The renderable record of a channel. It has to follow the heal exactly -- a beam is
    /// drawn from it -- so it is set only once every gate has passed, and cleared on the
    /// first tick the channel stops.
    #[test]
    fn a_landed_heal_is_recorded_on_the_healer() {
        let conf = conf();
        let (mut s, healer, target) = pair(&conf, Vec2::new(7.0, 16.0));
        let channel = healing(&[healer], target);
        let idle = FleetAction::new();

        heal(&mut s, &conf, &channel, &idle);
        assert_eq!(
            s.fleets()[Team::A][healer].healing(),
            StateOption::Some(target)
        );
        // The record lives on the healer, never on the bot receiving the heal.
        assert_eq!(s.fleets()[Team::A][target].healing(), StateOption::None);

        // Still channeling: unchanged, which is what keeps a steady heal out of the gamelog.
        heal(&mut s, &conf, &channel, &idle);
        assert_eq!(
            s.fleets()[Team::A][healer].healing(),
            StateOption::Some(target)
        );

        // Channel dropped -- the per-tick clear is what makes the beam stop.
        heal(&mut s, &conf, &idle, &idle);
        assert_eq!(s.fleets()[Team::A][healer].healing(), StateOption::None);
    }

    #[test]
    fn a_heal_the_geometry_rejects_is_not_recorded() {
        let conf = conf();
        // In range, but 60 degrees off a +/- 45 degree arc: no health, so no beam either.
        let outside = Vec2::new(5.0, 16.0) + Vec2::from_angle_deg(60.0) * 2.0;
        let (mut s, healer, target) = pair(&conf, outside);

        heal(&mut s, &conf, &healing(&[healer], target), &FleetAction::new());

        approx(s.fleets()[Team::A][target].health, HEALTH / 2.0);
        assert_eq!(s.fleets()[Team::A][healer].healing(), StateOption::None);
    }

    #[test]
    fn a_healer_cannot_heal_itself() {
        let conf = conf();
        let (mut s, healer, _target) = pair(&conf, Vec2::new(7.0, 16.0));

        heal(&mut s, &conf, &healing(&[healer], healer), &FleetAction::new());

        approx(s.fleets()[Team::A][healer].health, HEALTH / 2.0);
        assert_eq!(s.fleets()[Team::A][healer].healing(), StateOption::None);
    }

    #[test]
    fn an_ally_out_of_range_is_not_healed() {
        let conf = conf();
        // dead ahead, but 4.0 away with a range of 3.0
        let (mut s, healer, target) = pair(&conf, Vec2::new(9.0, 16.0));

        heal(&mut s, &conf, &healing(&[healer], target), &FleetAction::new());

        approx(s.fleets()[Team::A][target].health, HEALTH / 2.0);
    }

    #[test]
    fn range_is_measured_center_to_center() {
        let conf = conf();
        // Just inside and just outside `RANGE`, ignoring the 0.75 hull the blaster's splash
        // would have counted.
        let (mut s, healer, target) = pair(&conf, Vec2::new(5.0 + RANGE - 0.01, 16.0));
        heal(&mut s, &conf, &healing(&[healer], target), &FleetAction::new());
        approx(s.fleets()[Team::A][target].health, HEALTH / 2.0 + HEAL);

        let (mut s, healer, target) = pair(&conf, Vec2::new(5.0 + RANGE + 0.01, 16.0));
        heal(&mut s, &conf, &healing(&[healer], target), &FleetAction::new());
        approx(s.fleets()[Team::A][target].health, HEALTH / 2.0);
    }

    #[test]
    fn the_facing_arc_gates_the_heal() {
        let conf = conf();
        // 30 degrees off the healer's facing -- inside the +/- 45 gate.
        let inside = Vec2::new(5.0, 16.0) + Vec2::from_angle_deg(30.0) * 2.0;
        let (mut s, healer, target) = pair(&conf, inside);
        heal(&mut s, &conf, &healing(&[healer], target), &FleetAction::new());
        approx(s.fleets()[Team::A][target].health, HEALTH / 2.0 + HEAL);

        // 60 degrees off -- in range, but outside the gate.
        let outside = Vec2::new(5.0, 16.0) + Vec2::from_angle_deg(60.0) * 2.0;
        let (mut s, healer, target) = pair(&conf, outside);
        heal(&mut s, &conf, &healing(&[healer], target), &FleetAction::new());
        approx(s.fleets()[Team::A][target].health, HEALTH / 2.0);
    }

    #[test]
    fn turning_to_face_an_ally_brings_it_into_the_arc() {
        let conf = conf();
        // Directly behind the healer: 180 degrees off, so nothing lands...
        let (mut s, healer, target) = pair(&conf, Vec2::new(3.0, 16.0));
        heal(&mut s, &conf, &healing(&[healer], target), &FleetAction::new());
        approx(s.fleets()[Team::A][target].health, HEALTH / 2.0);

        // ...until the healer points at it. This is the whole reason `angle` matters to a
        // class that names its target by id.
        s.fleets_mut()[Team::A][healer].angle = 180.0;
        heal(&mut s, &conf, &healing(&[healer], target), &FleetAction::new());
        approx(s.fleets()[Team::A][target].health, HEALTH / 2.0 + HEAL);
    }

    #[test]
    fn a_wall_blocks_the_heal() {
        let mut conf = conf();
        conf.map[6][16] = MapTile::Wall;
        let conf = conf;
        let (mut s, healer, target) = pair(&conf, Vec2::new(7.5, 16.0));

        heal(&mut s, &conf, &healing(&[healer], target), &FleetAction::new());

        approx(s.fleets()[Team::A][target].health, HEALTH / 2.0);
    }

    #[test]
    fn the_payload_does_not_block_the_heal() {
        let conf = conf();
        let mut s = state(&conf);
        // Park the payload at the centre of the map, squarely between the two bots.
        s.capture = 0.0;
        approx(s.payload_pos().x, 16.0);
        approx(s.payload_pos().y, 16.0);

        let healer = spawn(&mut s, &conf, Team::A, Vec2::new(14.0, 16.0), 0.0, BotClass::Healer);
        let target = spawn(&mut s, &conf, Team::A, Vec2::new(16.5, 16.0), 0.0, BotClass::Battle);
        s.fleets_mut()[Team::A][target].health = HEALTH / 2.0;

        heal(&mut s, &conf, &healing(&[healer], target), &FleetAction::new());

        approx(s.fleets()[Team::A][target].health, HEALTH / 2.0 + HEAL);
    }

    #[test]
    fn an_ally_in_the_way_does_not_block_the_heal() {
        let conf = conf();
        let (mut s, healer, target) = pair(&conf, Vec2::new(7.0, 16.0));
        // Nothing separates overlapping bots, so letting a body block would be arbitrary.
        spawn(&mut s, &conf, Team::A, Vec2::new(6.0, 16.0), 0.0, BotClass::Battle);

        heal(&mut s, &conf, &healing(&[healer], target), &FleetAction::new());

        approx(s.fleets()[Team::A][target].health, HEALTH / 2.0 + HEAL);
    }

    #[test]
    fn a_healer_pointed_at_itself_heals_nothing() {
        let conf = conf();
        let mut s = state(&conf);
        let healer = spawn(&mut s, &conf, Team::A, Vec2::new(5.0, 16.0), 123.0, BotClass::Healer);
        s.fleets_mut()[Team::A][healer].health = HEALTH / 2.0;

        heal(&mut s, &conf, &healing(&[healer], healer), &FleetAction::new());

        approx(s.fleets()[Team::A][healer].health, HEALTH / 2.0);
    }

    #[test]
    fn an_absent_target_is_ignored() {
        let conf = conf();
        let (mut s, healer, target) = pair(&conf, Vec2::new(7.0, 16.0));
        // An id that was never spawned, and one that spawned and then died.
        s.fleets_mut()[Team::A].remove(target);

        for id in [target, BOTS_MAX as BotId - 1] {
            heal(&mut s, &conf, &healing(&[healer], id), &FleetAction::new());
        }

        approx(s.fleets()[Team::A][healer].health, HEALTH / 2.0);
    }

    #[test]
    fn not_channeling_heals_nothing() {
        let conf = conf();
        let (mut s, healer, target) = pair(&conf, Vec2::new(7.0, 16.0));
        let mut action = FleetAction::new();
        action.bots[healer as usize].special_action =
            SpecialAction::Healer { fire: false, target };

        heal(&mut s, &conf, &action, &FleetAction::new());

        approx(s.fleets()[Team::A][target].health, HEALTH / 2.0);
    }

    #[test]
    fn the_heal_only_lands_for_a_matching_class() {
        let conf = conf();
        let (mut s, healer, target) = pair(&conf, Vec2::new(7.0, 16.0));

        // A healer given someone else's action does nothing.
        let mut action = FleetAction::new();
        action.bots[healer as usize].special_action = SpecialAction::Extractor { mine: true };
        heal(&mut s, &conf, &action, &FleetAction::new());
        approx(s.fleets()[Team::A][target].health, HEALTH / 2.0);

        // Nor does a battle bot handed a healer's action.
        s.fleets_mut()[Team::A][healer].special = SpecialState::new(BotClass::Battle);
        heal(&mut s, &conf, &healing(&[healer], target), &FleetAction::new());
        approx(s.fleets()[Team::A][target].health, HEALTH / 2.0);
    }

    #[test]
    fn a_heal_never_overheals() {
        let conf = conf();
        let (mut s, healer, target) = pair(&conf, Vec2::new(7.0, 16.0));
        s.fleets_mut()[Team::A][target].health = HEALTH - HEAL / 2.0;

        heal(&mut s, &conf, &healing(&[healer], target), &FleetAction::new());

        approx(s.fleets()[Team::A][target].health, HEALTH);
    }

    #[test]
    fn healers_stack_on_one_target_up_to_the_cap() {
        let conf = conf();
        // Enough healers ringed around the target that the cap, not the geometry, is what
        // limits the total. Each sits 2.0 away and faces straight at it.
        for (n, want) in [(1, HEAL), (2, 2.0 * HEAL), (3, CAP), (4, CAP), (8, CAP)] {
            let mut s = state(&conf);
            let target = spawn(&mut s, &conf, Team::A, Vec2::new(16.0, 16.0), 0.0, BotClass::Battle);
            s.fleets_mut()[Team::A][target].health = 1.0;

            let mut healers = Vec::new();
            for i in 0..n {
                let deg = i as f32 * 360.0 / n as f32;
                let pos = Vec2::new(16.0, 16.0) + Vec2::from_angle_deg(deg) * 2.0;
                healers.push(spawn(&mut s, &conf, Team::A, pos, deg + 180.0, BotClass::Healer));
            }

            heal(&mut s, &conf, &healing(&healers, target), &FleetAction::new());

            approx(s.fleets()[Team::A][target].health, 1.0 + want.min(CAP));
        }
    }

    #[test]
    fn a_healer_never_heals_the_enemy() {
        let conf = conf();
        let mut s = state(&conf);
        // Team B's bot occupies the same *id* the healer names, and stands right where a
        // valid ally would. `target` is fleet-local, so it must still resolve to team A.
        let healer = spawn(&mut s, &conf, Team::A, Vec2::new(5.0, 16.0), 0.0, BotClass::Healer);
        let ally = spawn(&mut s, &conf, Team::A, Vec2::new(7.0, 16.0), 0.0, BotClass::Battle);
        // Ids are allocated per fleet, so team B needs a filler to push its next bot onto
        // the same id the healer will name.
        spawn(&mut s, &conf, Team::B, Vec2::new(25.0, 25.0), 0.0, BotClass::Battle);
        let enemy = spawn(&mut s, &conf, Team::B, Vec2::new(7.0, 16.0), 0.0, BotClass::Battle);
        assert_eq!(ally, enemy, "the test needs the ids to collide");
        s.fleets_mut()[Team::A][ally].health = HEALTH / 2.0;
        s.fleets_mut()[Team::B][enemy].health = HEALTH / 2.0;

        heal(&mut s, &conf, &healing(&[healer], ally), &FleetAction::new());

        approx(s.fleets()[Team::A][ally].health, HEALTH / 2.0 + HEAL);
        approx(s.fleets()[Team::B][enemy].health, HEALTH / 2.0);
    }

    #[test]
    fn healing_resolves_before_damage() {
        let mut conf = conf();
        conf.bot.blaster_damage = 0.6;
        let conf = conf;

        let mut s = state(&conf);
        let healer = spawn(&mut s, &conf, Team::A, Vec2::new(5.0, 16.0), 0.0, BotClass::Healer);
        let target = spawn(&mut s, &conf, Team::A, Vec2::new(7.0, 16.0), 0.0, BotClass::Battle);
        let enemy = spawn(&mut s, &conf, Team::B, Vec2::new(10.0, 16.0), 180.0, BotClass::Battle);
        s.fleets_mut()[Team::A][target].health = 0.3;

        let mut shooting = FleetAction::new();
        shooting.bots[enemy as usize].special_action = SpecialAction::Battle { fire: true };

        let a = healing(&[healer], target);
        let actions = pairs(&s, &a, &shooting);
        // Same order `eval_tick` uses.
        step_healers(&mut s, &conf, &actions);
        step_blasters(&mut s, &conf, &actions);

        // 0.3 + 0.5 - 0.6 = 0.2: the heal landed first, so the bot lived through a blast
        // that would otherwise have killed it.
        assert!(s.fleets()[Team::A].get(target).is_some(), "the heal did not save it");
        approx(s.fleets()[Team::A][target].health, 0.2);
    }

    #[test]
    fn healing_is_mirror_symmetric() {
        let mut conf = conf();
        conf.map[3][3] = MapTile::Wall;
        conf.map[MAP_SIZE - 1 - 3][MAP_SIZE - 1 - 3] = MapTile::Wall;
        let conf = conf;

        let mut s = state(&conf);
        s.capture = 0.25;
        let ha = spawn(&mut s, &conf, Team::A, Vec2::new(5.0, 16.0), 0.0, BotClass::Healer);
        let ta = spawn(&mut s, &conf, Team::A, Vec2::new(7.0, 16.0), 0.0, BotClass::Battle);
        let hb = spawn(&mut s, &conf, Team::B, Vec2::new(20.0, 20.0), 45.0, BotClass::Healer);
        let tb = spawn(&mut s, &conf, Team::B, Vec2::new(21.5, 21.5), 0.0, BotClass::Battle);
        for (team, id) in [(Team::A, ta), (Team::B, tb)] {
            s.fleets_mut()[team][id].health = HEALTH / 2.0;
        }
        s.tick = 7;

        let (fa, fb) = (healing(&[ha], ta), healing(&[hb], tb));

        let mut mirrored = s.clone();
        mirrored.mirror(&conf);
        // `mirror` swaps the fleets, so team A's actions in the mirrored frame are B's.
        // `SpecialAction` is side-agnostic -- `target` is a fleet-local id.
        heal(&mut mirrored, &conf, &fb, &fa);
        mirrored.mirror(&conf);

        heal(&mut s, &conf, &fa, &fb);

        for team in TEAMS {
            let (got, want) = (&s.fleets()[team], &mirrored.fleets()[team]);
            assert_eq!(got.mask, want.mask);
            for (got, want) in got.iter().zip(want.iter()) {
                approx(got.health, want.health);
            }
        }
    }
}

#[cfg(test)]
mod fabricator_test {
    use super::*;

    const INTERVAL: u32 = 100;
    const RUSH_COST: f32 = 10.0;
    const MAX_TICKS: u32 = 1000;
    const ENDGAME_TICKS: u32 = 100;

    fn conf() -> GameConfig {
        GameConfig {
            max_ticks: MAX_TICKS,
            endgame_ticks: ENDGAME_TICKS,
            bot: BotConfig {
                radius: 0.75,
                speed: 0.1,
                health: 10.0,
                turn_speed: 10.0,
                blaster_cooldown: 60,
                base_invulnerability_ticks: 15,
                blaster_range: 10.0,
                blaster_damage: 3.0,
                base_blaster_splash_radius: 0.3,
                heal_per_tick: 0.05,
                base_heal_range: 3.0,
                base_heal_arc_deg: 90.0,
                heal_stack_cap: 3.0,
                base_extract_range: 5.0,
                extract_rate: 0.1,
            },
            payload: PayloadConfig {
                radius: 1.5,
                capture_radius: 3.0,
                speed: 0.01,
            },
            payload_path: PAYLOAD_PATH,
            deposit: DepositConfig {
                // Far off the map with no radius: these tests are not about deposits, and
                // a deposit at the real `DEPOSIT_POS` would be a solid circle in the way.
                pos: Vec2::new(-100.0, -100.0),
                radius: 0.0,
                extractor_cap: 16,
            },
            fabricator: FabricatorConfig { interval: INTERVAL, rush_cost: RUSH_COST, starting_tokens: 0.0 },
            map: [[MapTile::Empty; MAP_SIZE]; MAP_SIZE],
        }
    }

    /// A state one tick in, with `tokens` in both fabricators. Running the first tick gets
    /// the free tick-0 build out of the way, so a test starts from a settled timer rather
    /// than fighting the bot every fleet is given at tick 0.
    fn started(conf: &GameConfig, tokens: f32) -> GameState {
        let mut s = GameState::new(conf);
        s.capture = -1.0;
        eval_tick(&mut s, conf, FleetAction::new(), FleetAction::new());
        for team in TEAMS {
            s.fabricators_mut()[team].tokens = tokens;
        }
        s
    }

    fn rush() -> FleetAction {
        let mut action = FleetAction::new();
        action.rush_order = true;
        action
    }

    /// One `eval_tick` from tick 0, which is a build tick.
    fn build(conf: &GameConfig, a: FleetAction, b: FleetAction) -> GameState {
        let mut s = GameState::new(conf);
        s.capture = -1.0;
        eval_tick(&mut s, conf, a, b);
        s
    }

    #[test]
    fn the_fabricator_builds_the_class_it_was_asked_for() {
        let conf = conf();
        for class in [BotClass::Battle, BotClass::Healer, BotClass::Extractor] {
            let mut action = FleetAction::new();
            action.fabricator_next = class.clone();
            let s = build(&conf, action.clone(), action);

            for team in TEAMS {
                assert_eq!(s.fleets()[team].len, 1);
                assert_eq!(s.fleets()[team].iter().next().unwrap().class(), class);
            }
        }
    }
    #[test]
    fn the_natural_timer_fires_on_the_interval() {
        let conf = conf();
        let mut s = GameState::new(&conf);
        s.capture = -1.0;

        for tick in 0..=INTERVAL * 2 {
            eval_tick(&mut s, &conf, FleetAction::new(), FleetAction::new());
            // Ticks 0, INTERVAL and 2 * INTERVAL each build; nothing in between does.
            assert_eq!(
                s.fleets()[Team::A].len as u32,
                1 + tick / INTERVAL,
                "fleet size after tick {tick}"
            );
            assert_eq!(
                s.fabricators()[Team::A].next_bot_creation,
                (tick / INTERVAL + 1) * INTERVAL
            );
        }
    }

    #[test]
    fn a_full_fleet_holds_the_timer_until_a_slot_opens() {
        let conf = conf();
        let mut s = started(&conf, 0.0);

        while !s.fleets()[Team::A].is_full() {
            let id = s.fleets_mut()[Team::A].add();
            reset_bot(Team::A, id, Vec2::new(16.0, 16.0), BotClass::Battle, &mut s, &conf);
        }
        s.fabricators_mut()[Team::A].next_bot_creation = s.tick;

        eval_tick(&mut s, &conf, FleetAction::new(), FleetAction::new());
        assert!(s.fleets()[Team::A].is_full());
        assert!(
            s.fabricators()[Team::A].next_bot_creation < s.tick,
            "the timer holds in the past rather than spending the build on a full fleet"
        );

        // Open a slot and the held build lands on the very next tick.
        let victim = s.fleets()[Team::A].iter().next().unwrap().id;
        s.fleets_mut()[Team::A].remove(victim);
        eval_tick(&mut s, &conf, FleetAction::new(), FleetAction::new());
        assert!(s.fleets()[Team::A].is_full(), "the held build filled the slot");
    }

    #[test]
    fn a_rush_order_buys_a_bot_now() {
        let conf = conf();
        let mut s = started(&conf, RUSH_COST);
        let before = s.fleets()[Team::A].len;

        eval_tick(&mut s, &conf, rush(), FleetAction::new());

        assert_eq!(s.fleets()[Team::A].len, before + 1);
        assert_eq!(s.fabricators()[Team::A].tokens, 0.0);
        assert_eq!(s.fleets()[Team::B].len, before, "team B waited for its timer");
    }

    #[test]
    fn an_unaffordable_rush_order_builds_nothing() {
        let conf = conf();
        let mut s = started(&conf, RUSH_COST - 1.0);
        let before = s.fleets()[Team::A].len;

        eval_tick(&mut s, &conf, rush(), FleetAction::new());

        assert_eq!(s.fleets()[Team::A].len, before);
        assert_eq!(s.fabricators()[Team::A].tokens, RUSH_COST - 1.0);
    }

    #[test]
    fn a_rush_on_a_natural_tick_defers_the_natural_build_by_one() {
        let conf = conf();
        let mut s = started(&conf, RUSH_COST);
        let before = s.fleets()[Team::A].len;

        // Make the natural build fall due on this very tick.
        let collision_tick = s.tick;
        s.fabricators_mut()[Team::A].next_bot_creation = collision_tick;

        eval_tick(&mut s, &conf, rush(), FleetAction::new());
        assert_eq!(
            s.fleets()[Team::A].len,
            before + 1,
            "only the rushed bot -- one bot per fleet per tick"
        );
        assert_eq!(
            s.fabricators()[Team::A].next_bot_creation,
            collision_tick + 1,
            "the natural build is deferred, not swallowed"
        );

        // ...and it arrives on the next tick, so the fleet gets both bots.
        eval_tick(&mut s, &conf, FleetAction::new(), FleetAction::new());
        assert_eq!(s.fleets()[Team::A].len, before + 2);
        assert_eq!(
            s.fabricators()[Team::A].next_bot_creation,
            collision_tick + 1 + INTERVAL
        );
    }

    /// A state parked on the first tick of the endgame, with `tokens` in both fabricators.
    fn in_endgame(conf: &GameConfig, tokens: f32) -> GameState {
        let mut s = started(conf, tokens);
        s.tick = conf.max_ticks - conf.endgame_ticks;
        s
    }

    #[test]
    fn both_fleets_start_with_the_configured_tokens() {
        let mut conf = conf();
        conf.fabricator.starting_tokens = 16.0 * RUSH_COST;
        let s = GameState::new(&conf);
        for team in TEAMS {
            assert_eq!(s.fabricators()[team].tokens, 16.0 * RUSH_COST);
        }
    }

    #[test]
    fn nothing_is_built_in_the_endgame() {
        let conf = conf();
        let mut s = in_endgame(&conf, RUSH_COST);
        assert!(s.in_endgame(&conf));
        s.fabricators_mut()[Team::A].next_bot_creation = s.tick;
        let before = s.fleets()[Team::A].len;

        eval_tick(&mut s, &conf, rush(), FleetAction::new());
        assert_eq!(s.fleets()[Team::A].len, before, "neither the rush nor the timer builds");
        assert_eq!(s.fabricators()[Team::A].tokens, RUSH_COST, "and the rush is not charged");
    }

    #[test]
    fn the_last_tick_before_the_endgame_still_builds() {
        let conf = conf();
        let mut s = in_endgame(&conf, RUSH_COST);
        s.tick -= 1;
        assert!(!s.in_endgame(&conf));
        let before = s.fleets()[Team::A].len;

        eval_tick(&mut s, &conf, rush(), FleetAction::new());
        assert_eq!(s.fleets()[Team::A].len, before + 1);
    }
}

#[cfg(test)]
mod self_destruct_test {
    use super::*;
    use crate::game::config::test_conf;

    fn conf() -> GameConfig {
        test_conf::conf().clone()
    }

    /// A state with one full-health bot in each fleet, both parked at the same point --
    /// self-destruct has no splash, so where they stand does not matter.
    fn state(conf: &GameConfig) -> GameState {
        let mut s = GameState::new(conf);
        for team in TEAMS {
            let id = s.fleets_mut()[team].add();
            reset_bot(team, id, Vec2::new(16.0, 16.0), BotClass::Battle, &mut s, conf);
        }
        s
    }

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

    #[test]
    fn a_self_destructing_bot_is_removed_with_no_other_effect() {
        let conf = conf();
        let mut s = state(&conf);
        let destructing = s.fleets()[Team::A].iter().next().unwrap().id;
        let enemy = s.fleets()[Team::B].iter().next().unwrap().id;
        let enemy_health_before = s.fleets()[Team::B][enemy].health;

        let mut a = FleetAction::new();
        a.bots[destructing as usize].self_destruct = true;
        let b = FleetAction::new();
        let actions = pairs(&s, &a, &b);

        step_self_destruct(&mut s, &actions);

        assert_eq!(s.fleets()[Team::A].len, 0, "the self-destructing bot is gone");
        assert_eq!(s.fleets()[Team::B].len, 1, "the enemy fleet is untouched");
        assert_eq!(
            s.fleets()[Team::B][enemy].health,
            enemy_health_before,
            "no splash damage to anything else"
        );
    }

    /// `bot_actions` is built before the tick's deaths, so a bot killed by `step_blasters`
    /// is still in the list when `step_self_destruct` runs. Removing it a second time used
    /// to trip the assert in `BotArray::remove` and panic the engine mid-match.
    #[test]
    fn dying_to_a_blast_and_self_destructing_on_the_same_tick_is_not_a_double_removal() {
        let conf = conf();
        let mut s = state(&conf);
        let doomed = s.fleets()[Team::A].iter().next().unwrap().id;

        let mut a = FleetAction::new();
        a.bots[doomed as usize].self_destruct = true;
        let b = FleetAction::new();
        let actions = pairs(&s, &a, &b);

        // stand in for the death sweep at the end of `step_blasters`
        remove_bot(&mut s, Team::A, doomed);

        step_self_destruct(&mut s, &actions);

        assert_eq!(s.fleets()[Team::A].len, 0, "still gone, and no panic");
        assert_eq!(s.fleets()[Team::B].len, 1, "the enemy fleet is untouched");
    }

    #[test]
    fn not_self_destructing_leaves_both_fleets_alone() {
        let conf = conf();
        let mut s = state(&conf);
        let (a, b) = (FleetAction::new(), FleetAction::new());
        let actions = pairs(&s, &a, &b);

        step_self_destruct(&mut s, &actions);

        assert_eq!(s.fleets()[Team::A].len, 1);
        assert_eq!(s.fleets()[Team::B].len, 1);
    }
}

#[cfg(test)]
mod match_result_test {
    use super::*;
    use crate::game::config::test_conf;

    const MAX_TICKS: u32 = 900;
    const ENDGAME_TICKS: u32 = 300;

    fn conf() -> GameConfig {
        let mut conf = test_conf::conf().clone();
        conf.max_ticks = MAX_TICKS;
        conf.endgame_ticks = ENDGAME_TICKS;
        conf
    }

    /// A state at `tick` with one full-health bot in each fleet, the payload at center and
    /// equal tokens.
    fn state(conf: &GameConfig, tick: u32) -> GameState {
        let mut s = GameState::new(conf);
        s.tick = tick;
        for team in TEAMS {
            let id = s.fleets_mut()[team].add();
            reset_bot(team, id, Vec2::new(16.0, 16.0), BotClass::Battle, &mut s, conf);
        }
        s
    }

    fn remove_one(s: &mut GameState, team: Team) {
        let id = s.fleets()[team].iter().next().unwrap().id;
        s.fleets_mut()[team].remove(id);
    }

    fn result(winner: Option<Team>, reason: EndReason) -> Option<MatchResult> {
        Some(MatchResult { winner, reason })
    }

    #[test]
    fn a_match_in_progress_has_no_result() {
        let conf = conf();
        assert_eq!(match_result(&state(&conf, 10), &conf), None);
        assert_eq!(match_result(&state(&conf, MAX_TICKS - 1), &conf), None);
    }

    #[test]
    fn a_payload_at_a_goal_loses_that_team_the_match_at_any_time() {
        let conf = conf();
        let mut s = state(&conf, 10);
        s.capture = 1.0;
        assert_eq!(match_result(&s, &conf), result(Some(Team::A), EndReason::Payload));
        s.capture = -1.0;
        assert_eq!(match_result(&s, &conf), result(Some(Team::B), EndReason::Payload));

        // ...and it outranks a wipe on the same tick.
        s.tick = MAX_TICKS - ENDGAME_TICKS;
        remove_one(&mut s, Team::B);
        assert_eq!(match_result(&s, &conf), result(Some(Team::B), EndReason::Payload));
    }

    #[test]
    fn an_empty_fleet_loses_only_in_the_endgame() {
        let conf = conf();
        let endgame = MAX_TICKS - ENDGAME_TICKS;

        let mut s = state(&conf, endgame - 1);
        remove_one(&mut s, Team::A);
        assert_eq!(match_result(&s, &conf), None, "before the endgame a fleet can rebuild");

        s.tick = endgame;
        assert_eq!(match_result(&s, &conf), result(Some(Team::B), EndReason::Elimination));
    }

    #[test]
    fn a_double_wipe_goes_straight_to_the_tiebreaks() {
        let conf = conf();
        let mut s = state(&conf, MAX_TICKS - ENDGAME_TICKS);
        remove_one(&mut s, Team::A);
        remove_one(&mut s, Team::B);
        s.capture = -0.5;
        assert_eq!(match_result(&s, &conf), result(Some(Team::B), EndReason::PayloadProgress));
    }

    #[test]
    fn the_tiebreaks_run_payload_then_health_then_tokens_then_tie() {
        let conf = conf();
        let mut s = state(&conf, MAX_TICKS);

        s.capture = 0.25;
        assert_eq!(match_result(&s, &conf), result(Some(Team::A), EndReason::PayloadProgress));
        s.capture = -0.25;
        assert_eq!(match_result(&s, &conf), result(Some(Team::B), EndReason::PayloadProgress));

        s.capture = 0.0;
        assert_eq!(match_result(&s, &conf), result(None, EndReason::Tie));

        s.fabricators_mut()[Team::A].tokens += 1.0;
        assert_eq!(match_result(&s, &conf), result(Some(Team::A), EndReason::Tokens));

        let id = s.fleets()[Team::A].iter().next().unwrap().id;
        s.fleets_mut()[Team::A][id].health -= 1.0;
        assert_eq!(
            match_result(&s, &conf),
            result(Some(Team::B), EndReason::HealthPool),
            "health outranks tokens"
        );
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
            endgame_ticks: 0,
            bot: BotConfig {
                radius: R,
                speed: 0.1,
                health: 10.0,
                turn_speed: 10.0,
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
                radius: 1.5,
                capture_radius: 3.0,
                speed: 0.01,
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
        reset_bot(team, id, pos, BotClass::Battle, state, conf);
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
                worst = worst.max(conf.bot.radius - pos.dist(&near));
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
        // The rest of this module uses a deliberately fat bot against synthetic walls to
        // stress the contact maths. This test is about the *shipped* arena, whose corridors
        // are sized for the shipped radius, so it has to use that one.
        conf.bot.radius = BOT_RADIUS;
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
                s.fleets_mut()[Team::A][id].pos += *dir * conf.bot.speed;
                let before = s.fleets()[Team::A][id].pos;
                assert!(
                    handle_collision(&mut s, &conf),
                    "collision never settled for a bot at {before:?} moving {dir:?}",
                );
                let pos = s.fleets()[Team::A][id].pos;
                assert_eq!(overlap(&conf, pos), 0.0, "bot ended up inside a wall at {pos:?}");
            }
            s.fleets_mut()[Team::A].remove(id);
        }
    }
}

#[cfg(test)]
mod extractor_test {
    use super::*;

    const RANGE: f32 = 5.0;
    const RATE: f32 = 0.1;
    const CAP: u8 = 16;
    /// Team A's deposit sits at (16, 16); team B's is its mirror image, which is the same
    /// point. Every test that needs the two to be distinct overrides `pos`.
    const DEPOSIT: Vec2 = Vec2::new(20.0, 16.0);

    fn conf() -> GameConfig {
        GameConfig {
            max_ticks: 100,
            endgame_ticks: 0,
            bot: BotConfig {
                radius: 0.25,
                speed: 0.1,
                health: 10.0,
                turn_speed: 10.0,
                blaster_cooldown: 60,
                base_invulnerability_ticks: 15,
                blaster_range: 10.0,
                blaster_damage: 3.0,
                base_blaster_splash_radius: 0.3,
                heal_per_tick: 0.05,
                base_heal_range: 3.0,
                base_heal_arc_deg: 90.0,
                heal_stack_cap: (0.15) / (0.05),
                base_extract_range: RANGE,
                extract_rate: RATE as f32,
            },
            payload: PayloadConfig {
                radius: 1.5,
                capture_radius: 3.0,
                speed: 0.01,
            },
            payload_path: PAYLOAD_PATH,
            // Team A's at (20, 16), team B's at its mirror image (12, 16).
            deposit: DepositConfig {
                pos: DEPOSIT,
                radius: 1.0,
                extractor_cap: CAP,
            },
            fabricator: FabricatorConfig { interval: 100, rush_cost: 10.0, starting_tokens: 0.0 },
            map: [[MapTile::Empty; MAP_SIZE]; MAP_SIZE],
        }
    }

    /// `capture` parks the payload at team A's goal, out of the y = 16 lane the tests use.
    fn state(conf: &GameConfig) -> GameState {
        let mut state = GameState::new(conf);
        state.capture = -1.0;
        state
    }

    fn spawn(
        state: &mut GameState,
        conf: &GameConfig,
        team: Team,
        pos: Vec2,
        angle: f32,
    ) -> BotId {
        let id = state.fleets_mut()[team].add();
        reset_bot(team, id, pos, BotClass::Extractor, state, conf);
        state.fleets_mut()[team][id].angle = angle;
        id
    }

    /// A `FleetAction` in which every id in `miners` is extracting.
    fn mining(miners: &[BotId]) -> FleetAction {
        let mut action = FleetAction::new();
        for id in miners {
            action.bots[*id as usize].special_action = SpecialAction::Extractor { mine: true };
        }
        action
    }

    /// The flattened list `eval_tick` hands the step, in a fixed order. Newcomer admission
    /// *is* order-dependent, which is exactly what the cap tests want to pin down.
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

    fn extract(state: &mut GameState, conf: &GameConfig, a: &FleetAction, b: &FleetAction) {
        let actions = pairs(state, a, b);
        step_extractors(state, conf, &actions);
    }

    fn approx(a: f32, b: f32) {
        assert!((a - b).abs() < 1e-4, "expected {b}, got {a}");
    }

    fn slots(state: &GameState, deposit: Team, team: Team) -> u32 {
        state.deposits()[deposit].extractors[team]
    }

    /// One extractor west of team A's deposit, facing east at it.
    fn solo(conf: &GameConfig) -> (GameState, BotId) {
        let mut s = state(conf);
        let id = spawn(&mut s, conf, Team::A, Vec2::new(17.0, 16.0), 0.0);
        (s, id)
    }

    #[test]
    fn an_extractor_facing_a_deposit_claims_a_slot_and_earns_tokens() {
        let conf = conf();
        let (mut s, id) = solo(&conf);
        let (a, b) = (mining(&[id]), FleetAction::new());

        extract(&mut s, &conf, &a, &b);
        assert_eq!(slots(&s, Team::A, Team::A), 1 << id);
        approx(s.fabricators()[Team::A].tokens, RATE);
        assert_eq!(
            s.fleets()[Team::A][id].extracting(),
            StateOption::Some(Team::A)
        );

        // and it keeps paying, tick after tick
        extract(&mut s, &conf, &a, &b);
        approx(s.fabricators()[Team::A].tokens, RATE * 2.0);
        // the enemy fleet earned nothing
        approx(s.fabricators()[Team::B].tokens, 0.0);
    }

    #[test]
    fn mine_false_extracts_nothing() {
        let conf = conf();
        let (mut s, id) = solo(&conf);
        let (a, b) = (FleetAction::new(), FleetAction::new());

        extract(&mut s, &conf, &a, &b);
        assert_eq!(slots(&s, Team::A, Team::A), 0);
        approx(s.fabricators()[Team::A].tokens, 0.0);
        assert_eq!(s.fleets()[Team::A][id].extracting(), StateOption::None);
    }

    #[test]
    fn facing_away_extracts_nothing() {
        let conf = conf();
        let mut s = state(&conf);
        // East of team A's deposit and facing further east, so the ray meets neither it nor
        // team B's at (12, 16).
        let id = spawn(&mut s, &conf, Team::A, Vec2::new(23.0, 16.0), 0.0);

        extract(&mut s, &conf, &mining(&[id]), &FleetAction::new());
        assert_eq!(slots(&s, Team::A, Team::A), 0);
        approx(s.fabricators()[Team::A].tokens, 0.0);
    }

    #[test]
    fn beyond_extract_range_extracts_nothing() {
        let conf = conf();
        let mut s = state(&conf);
        // 5.5 from the deposit center, 4.5 from its hull -- inside range.
        let near = spawn(&mut s, &conf, Team::A, Vec2::new(14.5, 16.0), 0.0);
        extract(&mut s, &conf, &mining(&[near]), &FleetAction::new());
        assert_eq!(slots(&s, Team::A, Team::A), 1 << near);

        let mut s = state(&conf);
        // 6.5 away: the hull is 5.5 out, past RANGE.
        let far = spawn(&mut s, &conf, Team::A, Vec2::new(13.5, 16.0), 0.0);
        extract(&mut s, &conf, &mining(&[far]), &FleetAction::new());
        assert_eq!(slots(&s, Team::A, Team::A), 0);
    }

    #[test]
    fn a_wall_blocks_extraction() {
        let mut conf = conf();
        conf.map[18][16] = MapTile::Wall;
        let (mut s, id) = solo(&conf);

        extract(&mut s, &conf, &mining(&[id]), &FleetAction::new());
        assert_eq!(slots(&s, Team::A, Team::A), 0);
        approx(s.fabricators()[Team::A].tokens, 0.0);
    }

    /// Neither a bot nor the payload blocks the ray -- see `step_extractors`.
    #[test]
    fn bots_and_the_payload_do_not_block_extraction() {
        let conf = conf();
        let (mut s, id) = solo(&conf);
        // an enemy standing directly between the extractor and the deposit
        spawn(&mut s, &conf, Team::B, Vec2::new(18.5, 16.0), 0.0);
        // and the payload parked on top of it
        s.capture = 0.0;
        s.deposit_a.pos = s.payload_pos() + Vec2::new(3.0, 0.0);
        let west = s.payload_pos() - Vec2::new(2.5, 0.0);
        let id2 = spawn(&mut s, &conf, Team::A, west, 0.0);

        extract(&mut s, &conf, &mining(&[id, id2]), &FleetAction::new());
        assert_eq!(slots(&s, Team::A, Team::A), (1 << id) | (1 << id2));
    }

    /// Both deposits are live, and either team may mine either one.
    #[test]
    fn either_team_may_mine_either_deposit() {
        let conf = conf();
        let mut s = state(&conf);
        // Team A's deposit is at (20, 16); team B's at (12, 16). One bot from each team
        // faces each deposit from between them.
        let a = spawn(&mut s, &conf, Team::A, Vec2::new(16.0, 16.0), 180.0); // -> B's
        let b = spawn(&mut s, &conf, Team::B, Vec2::new(16.0, 16.0), 0.0); // -> A's

        extract(&mut s, &conf, &mining(&[a]), &mining(&[b]));
        assert_eq!(slots(&s, Team::B, Team::A), 1 << a);
        assert_eq!(slots(&s, Team::A, Team::B), 1 << b);
        approx(s.fabricators()[Team::A].tokens, RATE);
        approx(s.fabricators()[Team::B].tokens, RATE);
        assert_eq!(
            s.fleets()[Team::A][a].extracting(),
            StateOption::Some(Team::B)
        );
    }

    #[test]
    fn a_holder_that_stops_mining_releases_its_slot() {
        let conf = conf();
        let (mut s, id) = solo(&conf);
        extract(&mut s, &conf, &mining(&[id]), &FleetAction::new());
        assert_eq!(slots(&s, Team::A, Team::A), 1 << id);

        extract(&mut s, &conf, &FleetAction::new(), &FleetAction::new());
        assert_eq!(slots(&s, Team::A, Team::A), 0);
        assert_eq!(s.fleets()[Team::A][id].extracting(), StateOption::None);
        // the one tick it did work is still banked
        approx(s.fabricators()[Team::A].tokens, RATE);
    }

    #[test]
    fn a_holder_that_turns_away_releases_its_slot() {
        let conf = conf();
        let (mut s, id) = solo(&conf);
        let a = mining(&[id]);
        extract(&mut s, &conf, &a, &FleetAction::new());
        assert_eq!(slots(&s, Team::A, Team::A), 1 << id);

        s.fleets_mut()[Team::A][id].angle = 180.0;
        extract(&mut s, &conf, &a, &FleetAction::new());
        assert_eq!(slots(&s, Team::A, Team::A), 0);
    }

    /// Fills team A's deposit with `n` extractors of `team`, all in range and facing it.
    fn crowd(s: &mut GameState, conf: &GameConfig, team: Team, n: usize) -> Vec<BotId> {
        (0..n)
            .map(|i| {
                // fanned out along y so they are distinct bots, all with line of sight
                let y = 16.0 + (i as f32 - n as f32 / 2.0) * 0.01;
                spawn(s, conf, team, Vec2::new(17.0, y), 0.0)
            })
            .collect()
    }

    #[test]
    fn the_cap_is_shared_and_locks_out_the_seventeenth() {
        let conf = conf();
        let mut s = state(&conf);
        let full = crowd(&mut s, &conf, Team::A, CAP as usize);
        let late = crowd(&mut s, &conf, Team::A, 1)[0];

        let a = mining(&full.iter().copied().chain([late]).collect::<Vec<_>>());
        extract(&mut s, &conf, &a, &FleetAction::new());

        assert_eq!(slots(&s, Team::A, Team::A).count_ones(), CAP as u32);
        assert_eq!(slots(&s, Team::A, Team::A) & (1 << late), 0);
        approx(s.fabricators()[Team::A].tokens, RATE * CAP as f32);

        // Incumbents are sticky: another tick does not shuffle the latecomer in.
        extract(&mut s, &conf, &a, &FleetAction::new());
        assert_eq!(slots(&s, Team::A, Team::A) & (1 << late), 0);

        // ...until a slot actually opens.
        s.fleets_mut()[Team::A].remove(full[0]);
        extract(&mut s, &conf, &a, &FleetAction::new());
        assert_eq!(slots(&s, Team::A, Team::A) & (1 << late), 1 << late);
        assert_eq!(slots(&s, Team::A, Team::A).count_ones(), CAP as u32);
    }

    /// The cap is shared across the teams, so a saturated deposit denies the enemy outright.
    #[test]
    fn a_saturated_deposit_locks_the_enemy_out() {
        let conf = conf();
        let mut s = state(&conf);
        let full = crowd(&mut s, &conf, Team::A, CAP as usize);
        // team B's extractor faces team A's deposit from the other side
        let enemy = spawn(&mut s, &conf, Team::B, Vec2::new(23.0, 16.0), 180.0);

        let (a, b) = (mining(&full), mining(&[enemy]));
        extract(&mut s, &conf, &a, &b);

        assert_eq!(slots(&s, Team::A, Team::A).count_ones(), CAP as u32);
        assert_eq!(slots(&s, Team::A, Team::B), 0);
        approx(s.fabricators()[Team::B].tokens, 0.0);

        // One incumbent dies and the enemy takes the freed slot.
        s.fleets_mut()[Team::A].remove(full[0]);
        extract(&mut s, &conf, &a, &b);
        assert_eq!(slots(&s, Team::A, Team::B), 1 << enemy);
        approx(s.fabricators()[Team::B].tokens, RATE);
    }

    /// A holder that swings to the other deposit arrives there as a newcomer, with no
    /// standing from the slot it gave up.
    #[test]
    fn switching_deposits_forfeits_seniority() {
        let conf = conf();
        let mut s = state(&conf);
        // A saturated team-B deposit at (12, 16), plus one bot holding at team A's.
        let full = crowd(&mut s, &conf, Team::A, CAP as usize)
            .into_iter()
            .map(|id| {
                s.fleets_mut()[Team::A][id].pos = Vec2::new(15.0, 16.0);
                s.fleets_mut()[Team::A][id].angle = 180.0; // -> B's deposit
                id
            })
            .collect::<Vec<_>>();
        let swinger = spawn(&mut s, &conf, Team::A, Vec2::new(17.0, 16.0), 0.0);

        let a = mining(&full.iter().copied().chain([swinger]).collect::<Vec<_>>());
        extract(&mut s, &conf, &a, &FleetAction::new());
        assert_eq!(slots(&s, Team::B, Team::A).count_ones(), CAP as u32);
        assert_eq!(slots(&s, Team::A, Team::A), 1 << swinger);

        // It turns to the saturated deposit and gets nothing -- and has lost the other slot.
        s.fleets_mut()[Team::A][swinger].pos = Vec2::new(15.0, 16.0);
        s.fleets_mut()[Team::A][swinger].angle = 180.0;
        extract(&mut s, &conf, &a, &FleetAction::new());
        assert_eq!(slots(&s, Team::A, Team::A), 0);
        assert_eq!(slots(&s, Team::B, Team::A) & (1 << swinger), 0);
    }

    /// A bot killed this tick banks the tick it worked, then its slot is released inside the
    /// same tick by the death sweep -- it never shows up in a gamelog line holding one.
    #[test]
    fn the_death_sweep_releases_the_slot() {
        let conf = conf();
        let (mut s, id) = solo(&conf);
        let a = mining(&[id]);
        extract(&mut s, &conf, &a, &FleetAction::new());
        assert_eq!(slots(&s, Team::A, Team::A), 1 << id);

        s.fleets_mut()[Team::A][id].health = -1.0;
        let snapshot = s.clone();
        let idle = FleetAction::new();
        let actions = pairs(&snapshot, &a, &idle);
        step_blasters(&mut s, &conf, &actions);

        assert!(s.fleets()[Team::A].get(id).is_none());
        assert_eq!(slots(&s, Team::A, Team::A), 0);
    }

    /// Deposits are solid, like the payload.
    #[test]
    fn a_bot_cannot_stand_inside_a_deposit() {
        let conf = conf();
        let mut s = state(&conf);
        let id = spawn(&mut s, &conf, Team::A, DEPOSIT + Vec2::new(0.2, 0.0), 0.0);

        assert!(handle_collision(&mut s, &conf));
        let bot = &s.fleets()[Team::A][id];
        assert!(
            bot.pos.dist(&DEPOSIT) >= conf.deposit.radius + conf.bot.radius,
            "bot at {:?} is still inside the deposit",
            bot.pos
        );
    }

    /// The whole deposit/token half of the state has to survive the round trip a bot's view
    /// is built from, and one mirror has to hand team B its own deposit as `deposit_a`.
    #[test]
    fn mirroring_deposits_and_tokens() {
        let conf = conf();
        let mut s = state(&conf);
        let a = spawn(&mut s, &conf, Team::A, Vec2::new(17.0, 16.0), 0.0);
        let b = spawn(&mut s, &conf, Team::B, Vec2::new(15.0, 16.0), 180.0);
        extract(&mut s, &conf, &mining(&[a]), &mining(&[b]));
        s.fabricators_mut()[Team::B].tokens = 7.0;

        let before = s.clone();
        let mut m = s.clone();
        m.mirror(&conf);

        // The positions do not move: the deposit set maps onto itself under `mirror_pos`,
        // which is the whole point of placing them as a mirror pair. Team B sees "my
        // deposit" at exactly the coordinates team A sees its own at.
        approx(m.deposit_a.pos.x, 20.0);
        approx(m.deposit_b.pos.x, 12.0);
        // What does move is the contents. `deposit_a` is now team B's own deposit, and the
        // slot in it is held by B's bot, reported in the `team_a` -- "me" -- half.
        assert_eq!(m.deposits()[Team::A].extractors[Team::A], 1 << b);
        assert_eq!(m.deposits()[Team::B].extractors[Team::B], 1 << a);
        // tokens follow the fleets
        approx(m.fabricators()[Team::A].tokens, 7.0);
        approx(m.fabricators()[Team::B].tokens, RATE);
        // and the channel marker names the deposit from the other side
        assert_eq!(
            m.fleets()[Team::B][a].extracting(),
            StateOption::Some(Team::B)
        );

        m.mirror(&conf);
        // `GameState` is not `Debug` -- `BotArray` deliberately is not -- so compare flatly.
        assert!(m == before, "mirroring twice is not the identity");
    }
}
