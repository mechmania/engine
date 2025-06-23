use rand::{ prelude::*, seq::SliceRandom, distr::StandardUniform };
use super::{
    state::*,
    config::*,
    util::*,
};

fn handle_player_collision(state: &mut GameState, conf: &GameConfig) {
    let mut rng = rand::rng();

    let mut iterations = 0;
    let mut resolved = true;
    let n = NUM_PLAYERS * 2;

    let mut pairs = Vec::new();
    // generate all pairs of player id's
    pairs.reserve_exact((n * (n - 1) / 2) as usize);
    for i in 0..n {
        for j in (i+1)..n {
            pairs.push((i, j));
        }
    }

    while !resolved && iterations < COLLISION_MAX_ITERATIONS {
        resolved = true;
        pairs.shuffle(&mut rng);
        // player on player collision
        for (i, j) in pairs.iter().copied() {
            // i assert that i know what im doing
            // i am editing disjoint parts of the state object, and the borrow checker cannot
            // validate that, so i will deref raw pointers
            let p1 = unsafe { &mut *state.players.as_mut_ptr().add(i as usize) };
            let p2 = unsafe { &mut *state.players.as_mut_ptr().add(j as usize) };
            let dist_sq = p1.pos.dist_sq(p2.pos);
            let min_dist = p1.radius + p2.radius;
            if dist_sq < min_dist.powi(2) {
                resolved = false;
                let dist = dist_sq.sqrt();
                let dv = (p2.pos - p1.pos).normalize_or_else(|| {
                    let angle = rng.sample::<f32, _>(StandardUniform) * 2.0 * PI;
                    Vec2::from_angle_rad(angle)
                });
                let diff = min_dist - dist;
                let correction = (diff * 0.5 + EPSILON) * dv;
                p1.pos -= correction;
                p2.pos += correction;
            }
        }

        // player on wall collision
        for p in state.players.iter_mut() {
            if p.pos.x - p.radius < 0.0 {
                p.pos.x = p.radius + EPSILON;
                resolved = false;
            }
            if p.pos.x + p.radius < conf.field.width as f32 {
                p.pos.x = conf.field.width as f32 - p.radius - EPSILON;
                resolved = false;
            }
            if p.pos.y - p.radius < 0.0 {
                p.pos.y = p.radius + EPSILON;
                resolved = false;
            }
            if p.pos.y + p.radius < conf.field.height as f32 {
                p.pos.y = conf.field.height as f32 - p.radius - EPSILON;
                resolved = false;
            }
        }

        iterations += 1;
    }

}

fn handle_ball_state(state: &mut GameState, conf: &GameConfig, actions: PlayerArray<PlayerAction>) {
    use BallPossessionState::*;
    let mut resolved = false;

    let GameState { players, ball_possession, .. } = state;

    if let Possesed { team, capture_ticks, .. } = ball_possession {
        let ball_pos = unsafe { &(*(state as *const GameState)).ball.pos };
        let opponents:  = players.into()[team.other()];

        for opponent in opponents {
            if ball_pos.dist_sq(opponent.pos) <= opponent.pickup_radius.powi(2) {
                *capture_ticks -= 1;
            }
        }
    }

    while !resolved {
        resolved = true;
        let mut next_state = state.ball_possession.clone();
        match &state.ball_possession {
            Possesed { owner, team, capture_ticks } => {
            },
            _ => todo!()
        }
        state.ball_possession = next_state;
    }
}
