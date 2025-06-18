use game_runner::{
    game::{run_tick, GameConfig, GameState},
    ipc::*,
};
use anyhow::{ Context, Result };
use std::process::Command;
use std::time::Instant;

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

    let p1_init_pos = p1.msg::<InitProtocol>(conf.clone());

    let _ = Command::new("./target/debug/bot")
        .arg(p1.backing_file_path())
        .spawn()
        .with_context(|| "failed to launch bot")?;

    let start = Instant::now();

    let mut state = GameState {
        p0_pos: on_init(&conf),
        p1_pos: p1_init_pos.await.unwrap_or(0.0),
        p0_score: 0,
        p1_score: 0,
        ball_pos: (0.0, 0.0),
        ball_vel: (-(conf.ball_speed as f64), conf.ball_speed as f64),
        tick: 0,
    };

    while state.tick < conf.max_ticks
        && state.p0_score < conf.winning_score
        && state.p1_score < conf.winning_score
    {
        let p1_vel = p1.msg::<TickProtocol>(state.clone());

        let p0 = on_tick(&conf, &state);
        run_tick(&mut state, &conf, p0, p1_vel.await.unwrap());
        //run_tick(&mut state, &conf, p0, p1_vel.await.unwrap());

        println!("{}", serde_json::to_string(&state).expect("parse err"));
    }

    println!("# time elapsed: {:?}", start.elapsed());
    Ok(())
}
