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

fn ball_chase(state: &GameState) -> [PlayerAction; NUM_PLAYERS as usize] {

    let conf = CONF.get().unwrap();

    std::array::from_fn(|id| {
        match state.ball_possession {
            BallPossessionState::Possessed { owner, .. } if owner as usize == id => {
                let me = &state.players[id];
                let goal_diff = conf.field.goal_b() - me.pos;
                PlayerAction {
                    dir: goal_diff - Vec2::new(50.0, 0.0),
                    pass: (goal_diff.norm() < 300.0).then_some(goal_diff.normalize_or_zero()).into()
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
        on_reset: Box::new(|score| {
            let conf = CONF.get().unwrap();
            let _ = CONF.set(config.clone());
            let f = Vec2::new(config.field.width as f32 / 2.0, config.field.height as f32);
            [
                Vec2::new(f.x * 0.93, f.y * 1.0 / 5.0), 
                Vec2::new(f.x * 0.93, f.y * 2.2 / 5.0), 
                Vec2::new(f.x * 0.93, f.y * 2.8 / 5.0), 
                Vec2::new(f.x * 0.93, f.y * 4.0 / 5.0), 
            ]
        }),
        on_tick: Box::new(ball_chase),
    };

    loop {
        chan.handle_msg(&strat).await;
    }
}
