use game_runner::{
    game::{run_tick, GameConfig, GameState},
    ipc::*,
};
use tokio::join;
use clap::Parser;
use anyhow::{ Context, Result };
use std::{path::PathBuf, process::Command};
use std::time::Instant;

#[derive(Parser)]
#[command(version, about, long_about = None)]
struct Cli {
    /// path to bot a binary
    bot_a: PathBuf,
    /// path to bot b binary
    bot_b: PathBuf,

    /// output stdout from bot A
    #[arg(short = 'a')]
    output_bot_a: bool,

    /// output stdout from bot B
    #[arg(short = 'b')]
    output_bot_b: bool,

    /// output game log (by default this is set when none of -abg are passed)
    #[arg(short = 'g')]
    output_gamelog: bool,

    /// when specified, output will be written to this file
    #[arg(short, long)]
    output: Option<PathBuf>
}

#[tokio::main]
async fn main() {
    if let Err(e) = run().await {
        eprintln!("{:?}", e.context("a fatal error occured"));
    }
}

async fn run() -> Result<()> {
    let mut cli = Cli::parse();
    if !cli.output_bot_a && !cli.output_bot_b && !cli.output_gamelog {
        cli.output_gamelog = true;
    }
    let cli = cli;


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

    println!("{}", serde_json::to_string(&conf)?);

    let channel_a = BotChannel::new()?;
    let channel_b = BotChannel::new()?;

    let proc_a = Command::new(cli.bot_a)
        .arg(channel_a.backing_file_path())
        .spawn()
        .with_context(|| "failed to launch bot A")?;

    let proc_b = Command::new(cli.bot_b)
        .arg(channel_b.backing_file_path())
        .spawn()
        .with_context(|| "failed to launch bot B")?;

    let start = Instant::now();

    let (p0_init_pos, p1_init_pos) = join!(
         async { channel_a.msg::<InitProtocol>(&conf).await.unwrap_or(0.0) },
         async { channel_b.msg::<InitProtocol>(&conf).await.unwrap_or(0.0) },
    );

    let mut state = GameState {
        p0_pos: p0_init_pos,
        p1_pos: p1_init_pos,
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

        let (p0_vel, p1_vel) = join!(
            async {
                channel_a.msg::<TickProtocol>(&state)
                    .await
                    .map_err(|e| {
                        println!("### Unable to process response from bot A: {e}");
                        e
                    })
                    .unwrap_or(0.0)
            },
            async {
                channel_b.msg::<TickProtocol>(&state)
                    .await
                    .map_err(|e| {
                        println!("### Unable to process response from bot B: {e}");
                        e
                    })
                    .unwrap_or(0.0)
            }
        );

        run_tick(&mut state, &conf, p0_vel, p1_vel);

        println!("{}", serde_json::to_string(&state).expect("parse err"));
    }

    println!("# time elapsed: {:?}", start.elapsed());
    Ok(())
}
