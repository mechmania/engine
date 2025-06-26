use game_runner::game::{state::*, util::*, config::*};
use game_runner::ipc::{EngineChannel, Strategy, ResetMsg, HANDSHAKE_BOT};
use std::env::args;
use std::path::PathBuf;
use std::sync::OnceLock;

#[tokio::main]
async fn main() {
    if let Err(e) = run().await {
        eprintln!("{:?}", e.context("a fatal error occured"));
    }
}

static CONF: OnceLock<GameConfig> = OnceLock::new();


fn positions(state: &GameState) -> [PlayerAction; NUM_PLAYERS as usize] {
    let conf = CONF.get().unwrap();
    let our_team = Team::A;

    let our_goal_pos = Vec2::new(0.0, (conf.field.height as f32) * 0.5);
    let goal_pos = Vec2::new(conf.field.width as f32, (conf.field.height as f32) * 0.5);

    // Get ball owner
    let ball_owner = state.ball_owner();

    // Check if our team has possession
    let team_possession = if let Some(owner_id) = ball_owner {
        state.player_team(owner_id) == Some(our_team)
    } else {
        false
    };

    let free_ball = state.is_ball_free();

    // Find closest player to ball (excluding goalkeeper)
    let closest_to_ball = if free_ball {
        (1..NUM_PLAYERS as usize).min_by_key(|&i| {
            let dist_sq = state.ball.pos.dist_sq(&state.players[i].pos);
            (dist_sq * 1000.0) as u32 // Convert to integer for comparison
        })
    } else {
        None
    };

    std::array::from_fn(|id| {
        let player = &state.players[id];

        // Goalkeeper logic (player 0)
        if id == 0 {
            let ball_to_goal = state.ball.pos - our_goal_pos;
            let goalee_target = our_goal_pos + ball_to_goal.normalize_or_zero() * 20.0;
            let goalee_delta = goalee_target - player.pos;
            let move_dir = goalee_delta.normalize_or_zero();

            // Goalkeeper passing logic
            let pass_target = if ball_owner == Some(id as u32) {
                // Find best teammate to pass to
                let mut best_score = -1.0;
                let mut best_target = None;

                for teammate_id in 1..NUM_PLAYERS as usize {
                    let teammate = &state.players[teammate_id];

                    // Calculate pass score
                    let mut safety = 0.0;
                    for opponent_id in NUM_PLAYERS as usize..(NUM_PLAYERS * 2) as usize {
                        let opponent = &state.players[opponent_id];
                        safety += teammate.pos.dist(&opponent.pos);
                    }
                    safety /= NUM_PLAYERS as f32 * (conf.field.width as f32);

                    let offense = teammate.pos.x / (conf.field.width as f32);
                    let distance = 1.0 - player.pos.dist(&teammate.pos) / (conf.field.width as f32);

                    let score = offense + distance + safety;
                    if score > best_score {
                        best_score = score;
                        best_target = Some(teammate.pos);
                    }
                }

                best_target.map(|target| target - player.pos)
            } else {
                None
            };

            PlayerAction {
                dir: move_dir,
                pass: pass_target.into(),
            }
        }
        // Field players logic (players 1-3)
        else {
            // If this player has the ball
            if ball_owner == Some(id as u32) {
                // Check if close enough to goal to shoot
                let goal_dist_sq = goal_pos.dist_sq(&player.pos);
                if goal_dist_sq <= ((conf.field.width as f32) * 0.3) * ((conf.field.width as f32) * 0.3) {
                    let shoot_dir = goal_pos - player.pos;
                    return PlayerAction {
                        dir: Vec2::ZERO,
                        pass: StateOption::Some(shoot_dir.normalize_or_zero()),
                    };
                }

                // Check if under pressure from opponents
                let mut closest_opponent_dist_sq = f32::INFINITY;
                for opponent_id in NUM_PLAYERS as usize..(NUM_PLAYERS * 2) as usize {
                    let opponent = &state.players[opponent_id];
                    let dist_sq = player.pos.dist_sq(&opponent.pos);
                    if dist_sq < closest_opponent_dist_sq {
                        closest_opponent_dist_sq = dist_sq;
                    }
                }

                if closest_opponent_dist_sq < 30.0 * 30.0 {
                    // Find best teammate to pass to
                    let mut best_score = -1.0;
                    let mut best_target = None;

                    for teammate_id in 1..NUM_PLAYERS as usize {
                        if teammate_id == id {
                            continue;
                        }
                        let teammate = &state.players[teammate_id];

                        // Calculate pass score
                        let mut safety = 0.0;
                        for opponent_id in NUM_PLAYERS as usize..(NUM_PLAYERS * 2) as usize
                        {
                            let opponent = &state.players[opponent_id];
                            safety += teammate.pos.dist(&opponent.pos);
                        }
                        safety /= NUM_PLAYERS as f32 * (conf.field.width as f32);

                        let offense = teammate.pos.x / (conf.field.width as f32);
                        let distance = 1.0 - player.pos.dist(&teammate.pos) / (conf.field.width as f32);

                        let score = offense + distance + safety;
                        if score > best_score {
                            best_score = score;
                            best_target = Some(teammate.pos);
                        }
                    }

                    if let Some(target) = best_target {
                        let pass_dir = target - player.pos;
                        return PlayerAction {
                            dir: Vec2::ZERO,
                            pass: Some(pass_dir.normalize_or_zero()).into(),
                        };
                    }
                }

                // Move towards goal
                let move_dir = (goal_pos - player.pos).normalize_or_zero();
                PlayerAction {
                    dir: move_dir,
                    pass: None.into(),
                }
            }
            // If player doesn't have ball
            else {
                let attackers = NUM_PLAYERS - 1; // Excluding goalkeeper

                let target_pos = if team_possession || free_ball {
                    // If we have possession or ball is free, go to ball if closest
                    if let Some(closest_id) = closest_to_ball {
                        if closest_id == id {
                            // Go to ball
                            let move_dir =
                                (state.ball.pos - player.pos).normalize_or_zero();
                            return PlayerAction {
                                dir: move_dir,
                                pass: None.into(),
                            };
                        }
                    }
                    // Otherwise, position for attack
                    Vec2::new(
                        (conf.field.width as f32) * 0.8,
                        (conf.field.height as f32) / (attackers as f32 + 1.0) * id as f32,
                    )
                } else {
                    // Position for defense
                    Vec2::new(
                        (conf.field.width as f32) * 0.2,
                        (conf.field.height as f32) / (attackers as f32 + 1.0) * id as f32,
                    )
                };

                let move_dir = (target_pos - player.pos).normalize_or_zero();
                PlayerAction {
                    dir: move_dir,
                    pass: None.into(),
                }
            }
        }
    })

}

fn ball_chase(state: &GameState) -> [PlayerAction; NUM_PLAYERS as usize] {

    let conf = CONF.get().unwrap();

    std::array::from_fn(|id| {
        match state.ball_possession {
            BallPossessionState::Possessed { owner, team, .. } if team == Team::A => {
                let me = &state.players[id];
                let goal_diff = conf.field.goal_b() - me.pos;
                PlayerAction {
                    dir: goal_diff,
                    //pass: (owner == id as u32 && goal_diff.norm() < 100.0).then_some(goal_diff)
                    pass: Some(goal_diff).into()
                }
            },
            _ => PlayerAction {
                dir: state.ball.pos - state.players[id].pos,
                pass: None.into(),
            }
        }
    })
}

fn nothing(_: &GameState) -> [PlayerAction; NUM_PLAYERS as usize] {
    std::array::from_fn(|_| {
        Default::default()
    })
}

async fn run() -> anyhow::Result<()> {
    let args = args();
    if args.len() < 2 {
        println!("usage: [bin name] [shmem path]");
        return Ok(());
    }

    let path = PathBuf::from(&args.skip(1).next().unwrap());
    let chan = EngineChannel::from_path(path)?;
    let strat = Strategy {
        on_handshake: Box::new(|_| HANDSHAKE_BOT),
        on_reset: Box::new(|msg| {
            let ResetMsg { config, .. } = msg;
            let _ = CONF.set(config.clone());
            let center = config.field.center();
            [center, center, center, center]
        }),
        on_tick: Box::new(ball_chase),
    };

    loop {
        chan.handle_msg(&strat).await;
    }
}
