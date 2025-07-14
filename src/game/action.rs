use std::cell::RefCell;

use super::{config::*, state::*, util::*};
use rand::{prelude::*, seq::SliceRandom};

thread_local! {
    static RNG: RefCell<SmallRng> = RefCell::new(SmallRng::from_rng(&mut rand::rng()));
}

fn with_rng<T>(f: impl FnOnce(&mut SmallRng) -> T) -> T {
    RNG.with(|rng| f(&mut rng.borrow_mut()))
}

fn rand_player_iter<'a>(players: &'a [PlayerState]) -> std::vec::IntoIter<&'a PlayerState> {
    let mut ret: Vec<&PlayerState> = players.iter().collect();
    with_rng(|rng| ret.shuffle(rng));
    ret.into_iter()
}

fn rand_player_iter_mut<'a>(players: &'a mut [PlayerState]) -> std::vec::IntoIter<&'a mut PlayerState> {
    let mut ret: Vec<&mut PlayerState> = players.iter_mut().collect();
    with_rng(|rng| ret.shuffle(rng));
    ret.into_iter()
}

fn handle_player_collision(state: &mut GameState, conf: &GameConfig) {

    let mut iterations = 0;
    let mut resolved = false;
    let n = NUM_PLAYERS * 2;

    let mut pairs = Vec::new();
    // generate all pairs of player id's
    pairs.reserve_exact((n * (n - 1) / 2) as usize);
    for i in 0..n {
        for j in (i + 1)..n {
            pairs.push((i, j));
        }
    }

    while !resolved && iterations < COLLISION_MAX_ITERATIONS {
        resolved = true;
        with_rng(|rng| pairs.shuffle(rng));
        // player on player collision

        for (i, j) in pairs.iter().copied() {
            // safety: disjoint players
            let p1 = unsafe { &mut *state.players.as_mut_ptr().add(i as usize) };
            let p2 = unsafe { &mut *state.players.as_mut_ptr().add(j as usize) };
            let dist_sq = p1.pos.dist_sq(&p2.pos);
            let min_dist = p1.radius + p2.radius;
            if dist_sq < min_dist.powi(2) {
                resolved = false;
                let dist = dist_sq.sqrt();
                let dv = (p2.pos - p1.pos).normalize_or_else(|| {
                    let angle = with_rng(|rng| rng.random_range(0.0..(2.0 * PI)));
                    Vec2::from_angle_rad(angle)
                });
                let diff = min_dist - dist;
                let correction = (diff * 0.5 + EPSILON) * dv;
                p1.pos -= correction;
                p2.pos += correction;
            }
        }

        let br = conf.field.bottom_right();

        // player on wall collision
        for p in state.players.iter_mut() {
            if p.pos.x - p.radius < 0.0 {
                p.pos.x = p.radius + EPSILON;
                resolved = false;
            }
            if p.pos.x + p.radius > br.x {
                p.pos.x = br.x - p.radius - EPSILON;
                resolved = false;
            }
            if p.pos.y - p.radius < 0.0 {
                p.pos.y = p.radius + EPSILON;
                resolved = false;
            }
            if p.pos.y + p.radius > br.y {
                p.pos.y = br.y - p.radius - EPSILON;
                resolved = false;
            }
        }

        iterations += 1;
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

fn closer_pickup(a: &PlayerState, b: &PlayerState, c: &Vec2) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let pickup_ac = a.pos.dist(c) - a.pickup_radius;
    let pickup_bc = b.pos.dist(c) - b.pickup_radius;
    if pickup_ac <= 1.0 && pickup_bc <= 1.0 {
        return if with_rng(|rng| rng.random_bool(0.5)) {
            Ordering::Less
        } else {
            Ordering::Greater
        };
    }
    pickup_ac.total_cmp(&pickup_bc)
}

fn handle_ball_state(
    state: &mut GameState,
    conf: &GameConfig,
    actions: &mut PlayerArray<PlayerAction>,
) {
    use BallPossessionState::*;
    let mut resolved = false;

    let GameState {
        players,
        ball,
        ball_possession,
        ..
    } = state;

    if let Possessed {
        team,
        capture_ticks,
        ..
    } = ball_possession
    {
        let mut capturing = false;
        for opponent in &players[team.other()] {
            if ball.pos.dist_sq(&opponent.pos) <= opponent.pickup_radius.powi(2) {
                *capture_ticks += 1;
                capturing = true;
            }
        }
        if !capturing && *capture_ticks > 0 {
            *capture_ticks -= 1;
        }
    }

    while !resolved {
        resolved = true;
        match &mut state.ball_possession {
            Possessed {
                owner,
                team,
                capture_ticks,
            } => {
                if let StateOption::Some(pass) = actions[*owner as usize].pass {
                    let owner = *owner;
                    resolved = false;
                    actions[owner as usize].pass = StateOption::None;
                    let owner = &state.players[owner as usize];

                    let norm = pass.norm();
                    if norm == 0.0 {
                        continue;
                    }
                    let mut pass = pass / norm;
                    let norm = norm.clamp(EPSILON, 1.0);
                    pass *= norm;
                    // TODO port over colins pass logic
                    let err = with_rng(|rng| rng.random_range(-conf.player.pass_error..conf.player.pass_error));
                    pass.rotate_deg(err);
                    state.ball_possession = Passing { team: *team };
                    state.ball.vel = pass * conf.player.pass_speed;
                    state.ball.pos = owner.pos + owner.dir.normalize_or_zero() * (owner.radius + state.ball.radius);
                } else if *capture_ticks > conf.ball.capture_ticks {
                    resolved = false;
                    // get closest opponent to the ball
                    let closest_opponent = rand_player_iter(&state.players[team.other()])
                        .min_by(|a, b| closer_pickup(&a, &b, &state.ball.pos))
                        .unwrap()
                        .id;
                    state.ball_possession = Possessed {
                        owner: closest_opponent,
                        team: team.other(),
                        capture_ticks: 0,
                    };
                }
            }
            Passing { team } => {
                let closest_opponent = rand_player_iter(&state.players[team.other()])
                    .min_by(|a, b| closer_pickup(&a, &b, &state.ball.pos))
                    .unwrap();
                if closest_opponent.pos.dist_sq(&state.ball.pos)
                    <= closest_opponent.pickup_radius.powi(2)
                {
                    resolved = false;
                    state.ball_possession = Possessed {
                        owner: closest_opponent.id,
                        team: team.other(),
                        capture_ticks: 0,
                    };
                    continue;
                }
                let closest_teammate = rand_player_iter(&state.players[*team])
                    .min_by(|a, b| closer_pickup(&a, &b, &state.ball.pos))
                    .unwrap();
                if closest_teammate.pos.dist_sq(&state.ball.pos)
                    > closest_teammate.pickup_radius.powi(2)
                {
                    resolved = false;
                    state.ball_possession = Free;
                }
            }
            Free => {
                let closest = rand_player_iter(&state.players)
                    .min_by(|a, b| closer_pickup(&a, &b, &state.ball.pos))
                    .unwrap();
                if closest.pos.dist_sq(&state.ball.pos) <= closest.pickup_radius.powi(2) {
                    resolved = false;
                    state.ball_possession = Possessed {
                        owner: closest.id,
                        team: state.player_team(closest.id).unwrap(),
                        capture_ticks: 0,
                    }
                }
            }
        }
    }
}

fn handle_ball_stagnation(
    state: &mut GameState,
    conf: &GameConfig,
) -> bool {
    if state.ball.pos.dist_sq(&state.ball_stagnation.center) <= conf.ball.stagnation_radius.powi(2) {
        state.ball_stagnation.tick += 1;
    } else {
        state.ball_stagnation.center = state.ball.pos;
        state.ball_stagnation.tick = 0;
    }

    if state.ball_stagnation.tick >= conf.ball.stagnation_ticks {
        println!("# Ball stayed stagnant for too long! Resetting field...");
        return true;
    }
    false
}

fn handle_scoring(
    state: &mut GameState,
    conf: &GameConfig,
) -> bool {
    let center = Vec2::new(conf.field.width as f32 / 2.0, conf.field.height as f32 / 2.0);
    let h = conf.goal.height as f32;
    let goal_bounds = (center.y - h / 2.0)..(center.y + h / 2.0);
    if !goal_bounds.contains(&state.ball.pos.y) {
        return false;
    }
    if state.ball.pos.x - state.ball.radius <= conf.goal.thickness as f32 {
        println!("# Bot B scored");
        state.score.b += 1;
        return true;
    }
    if state.ball.pos.x + state.ball.radius >= (conf.field.width - conf.goal.thickness) as f32 {
        println!("# Bot A scored");
        state.score.a += 1;
        return true;
    }
    false
}

pub fn eval_reset(
    state: &mut GameState,
    conf: &GameConfig,
    formation: &TeamPair<[Vec2; NUM_PLAYERS as usize]>,
) {
    let center = conf.field.center();
    state.ball = BallState {
        pos: center,
        vel: Vec2::ZERO,
        radius: conf.ball.radius
    };
    state.ball_possession = BallPossessionState::Free;
    state.ball_stagnation = BallStagnationState {
        center,
        tick: 0,
    };

    let translations = [0.0, center.x];
    for ((dx, team), formation) in translations
        .iter()
        .copied()
        .zip(state.teams_mut().iter_mut()) 
        .zip(formation.iter())
    {
        for (player, pos) in team.iter_mut().zip(formation) {

            let mut pos = Vec2::new(
                pos.x.clamp(dx, dx + center.x), 
                pos.y.clamp(0.0, conf.field.height as f32)
            );

            if pos.dist_sq(&center) < conf.spawn_ball_dist.powi(2) {
                pos = center + (pos - center).normalize_or_else(|| {
                    Vec2::new((dx - EPSILON).signum(), 0.0)
                }) * conf.spawn_ball_dist;
            }

            player.pos = pos;
            player.dir = Vec2::ZERO;
        }
    }
}

pub fn eval_tick(
    state: &mut GameState, 
    conf: &GameConfig, 
    mut actions: PlayerArray<PlayerAction>
) -> bool {
    use BallPossessionState::*;
    for action in &mut actions {
        let norm = action.dir.norm();
        action.dir = action.dir.normalize_or_zero() * norm.clamp(0.0, 1.0);
        action.pass.option().map(|pass| pass.normalize_or_zero());
    }

    handle_ball_state(state, conf, &mut actions);

    for (player, action) in state.players.iter_mut().zip(actions.iter()) {
        let speed_modifier = match state.ball_possession {
            Possessed { owner, .. } if owner == player.id => conf.player.possession_slowdown,
            _ => 1.0
        };
        player.dir = action.dir * speed_modifier;
        //player.pos += player.dir * player.speed + Vec2::from_angle_rad(with_rng(|rng| rng.random_range(0.0..(2.0*PI)))) * 5.0;
        player.pos += player.dir * player.speed;
    }

    handle_player_collision(state, conf);

    if let Possessed { owner, .. } = state.ball_possession {
        state.ball.vel = Vec2::ZERO;
        let owner = &state.players[owner as usize];
        state.ball.pos = owner.pos + owner.dir.normalize_or_zero() * (owner.radius + state.ball.radius);
    } else {
        state.ball.pos += state.ball.vel;
        state.ball.vel *= conf.ball.friction;
        let (left, right, top, bottom) = (
            state.ball.radius,
            conf.field.width as f32 - state.ball.radius,
            state.ball.radius,
            conf.field.height as f32 - state.ball.radius,
        );
        if state.ball.pos.x < left {
            state.ball.pos.x = left + EPSILON;
            state.ball.vel.x *= -conf.ball.friction.powi(2);
        }
        if state.ball.pos.x > right {
            state.ball.pos.x = right - EPSILON;
            state.ball.vel.x *= -conf.ball.friction.powi(2);
        }
        if state.ball.pos.y < top {
            state.ball.pos.y = top + EPSILON;
            state.ball.vel.y *= -conf.ball.friction.powi(2);
        }
        if state.ball.pos.y > bottom {
            state.ball.pos.y = bottom - EPSILON;
            state.ball.vel.y *= -conf.ball.friction.powi(2);
        }
    }

    state.tick += 1;

    if handle_scoring(state, conf) {
        return true;
    }

    if handle_ball_stagnation(state, conf) {
        return true;
    }

    false
}
