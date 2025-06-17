use game_runner::{
    game::{run_tick, GameConfig, GameState},
    ipc::{BotChannel, ShmStage},
};
use anyhow::{ Context, Result };
use std::process::Command;
use std::time::{ Instant, Duration };
use tokio::time::timeout;

const BOT_TIMEOUT: Duration = Duration::from_millis(1000);

#[tokio::main]
async fn main() {
    if let Err(e) = run().await {
        eprintln!("{:?}", e.context("a fatal error occured"));
    }
}

async fn run() -> Result<()> {
    let conf = GameConfig {
        height: 400,
        width: 800,
        paddle_length: 30,
        paddle_width: 5,
        ball_radius: 5,
        ball_speed: 5,
        winning_score: 5,
        max_ticks: 50000,
    };

    let on_init = |_| 0.0;
    let on_tick = |_, state: &GameState| {
        if state.ball_pos.1 > state.p0_pos {
            1.0
        } else {
            -1.0
        }
    };

    println!("{}", serde_json::to_string(&conf)?);

    let p1 = BotChannel::new()?;

    let _ = Command::new("./target/debug/bot")
        .arg(p1.backing_file_path())
        .spawn()
        .with_context(|| "failed to launch bot")?;

    let start = Instant::now();

    *p1.lock().await.with_context(|| "failure during initialization broadcast")? = ShmStage::Init {
        config: conf.clone(),
        action: 0.0,
    };

    let mut stage = p1.lock().await.with_context(|| "failure during initialization reciept")?;

    let ShmStage::Init { config: _, action } = *stage else {
        panic!();
    };

    let mut state = GameState {
        p0_pos: on_init(&conf),
        p1_pos: action,
        p0_score: 0,
        p1_score: 0,
        ball_pos: (0.0, 0.0),
        ball_vel: (-(conf.ball_speed as f64), conf.ball_speed as f64),
        tick: 0,
    };

    *stage = ShmStage::Tick {
        state: state.clone(),
        action: 0.0,
    };

    drop(stage);

    while state.tick < conf.max_ticks
        && state.p0_score < conf.winning_score
        && state.p1_score < conf.winning_score
    {
        let mut stage = timeout(BOT_TIMEOUT, p1.lock())
                            .await
                            .with_context(|| "bot timed out")?
                            .with_context(|| "invalid bot response")?;

        let ShmStage::Tick {
            state: ref mut shm_state,
            action,
        } = *stage
        else {
            panic!();
        };

        let p0 = on_tick(&conf, &state);
        run_tick(&mut state, &conf, p0, action);

        *shm_state = state.clone();

        println!("{}", serde_json::to_string(&state).expect("parse err"));
    }

    println!("# time elapsed: {:?}", start.elapsed());
    Ok(())
}
