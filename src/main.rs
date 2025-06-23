use game_runner::{
    game::{eval_tick, eval_reset, GameConfig, GameState},
    ipc::*,
    cli::*,
};
use tokio::{
    sync::mpsc,
    process::Command,
    join,
    io::{BufReader, AsyncBufReadExt},
};
use anyhow::{Context, Result};
use std::{
    path::Path, 
    time::{ Instant, Duration },
    process::Stdio,
};
use simple_moving_average::{ SumTreeSMA, SMA };

#[tokio::main]
async fn main() {
    if let Err(e) = run().await {
        eprintln!("{:?}", e.context("fatal error"));
    }
}

async fn run() -> Result<()> {
    let cli = parse_cli();
    let (tx, recv_task) = spawn_reciever(&cli)?;

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

    send!(tx, OutputSource::Gamelog, "{}", serde_json::to_string(&conf)?);

    let channel_a = BotChannel::new()?;
    let channel_b = BotChannel::new()?;

    let mut proc_a = spawn_bot(&cli.bot_a, channel_a.backing_file_path(), "bot_a", OutputSource::BotA, tx.clone())?;
    let mut proc_b = spawn_bot(&cli.bot_b, channel_b.backing_file_path(), "bot_b", OutputSource::BotB, tx.clone())?;

    let start = Instant::now();

    let (p0_init_pos, p1_init_pos) = join!(
        channel_a.msg::<HandshakeProtocol>(&, Duration::from_millis(1)),
        channel_b.msg::<HandshakeProtocol>(&, Duration::from_millis(1)),
    );

    let mut state = GameState {
        p0_pos: p0_init_pos.unwrap_or(0.0),
        p1_pos: p1_init_pos.unwrap_or(0.0),
        p0_score: 0,
        p1_score: 0,
        ball_pos: (0.0, 0.0),
        ball_vel: (-(conf.ball_speed as f64), conf.ball_speed as f64),
        tick: 0,
    };

    let mut ma = SumTreeSMA::<_, _, 50>::from_zero(Duration::ZERO);

    while state.tick < conf.max_ticks
        && state.p0_score < conf.winning_score
        && state.p1_score < conf.winning_score
    {
        let last_tick_time = std::cmp::max(ma.get_average(), Duration::from_millis(1));
        let (p0_vel, p1_vel) = join!(
            get_bot_velocity(&channel_a, &state, last_tick_time, &tx, "A"),
            get_bot_velocity(&channel_b, &state, last_tick_time, &tx, "B")
        );

        let tick_start = Instant::now();
        run_tick(&mut state, &conf, p0_vel, p1_vel);
        ma.add_sample(tick_start.elapsed());

        send!(tx, OutputSource::Gamelog, "{}", serde_json::to_string(&state)?);
    }

    let _ = join!(proc_a.kill(), proc_b.kill());

    send!(tx, OutputSource::Gamelog, "# time elapsed: {:?}", start.elapsed());
    drop(tx);
    recv_task.await??;
    Ok(())
}

async fn get_bot_velocity(
    channel: &BotChannel,
    state: &GameState,
    time: Duration,
    tx: &mpsc::UnboundedSender<Message>,
    bot_name: &str
) -> f64 {
    channel.msg::<TickProtocol>(state, time)
        .await
        .map_err(|e| {
            send!(tx, OutputSource::Gamelog, "### Bot {} error: {}", bot_name, e);
            e
        })
        .unwrap_or(0.0)
}

fn spawn_bot(
    command: &Path,
    backing_file: &Path,
    name: &str,
    source: OutputSource,
    tx: mpsc::UnboundedSender<Message>
) -> Result<tokio::process::Child> {
    let mut proc = Command::new(command)
        .arg(backing_file)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("failed to spawn {}", name))?;

    let stdout = proc.stdout.take().unwrap();
    let stderr = proc.stderr.take().unwrap();
    let name = name.to_string();

    tokio::spawn(async move {
        let mut stdout_reader = BufReader::new(stdout).lines();
        let mut stderr_reader = BufReader::new(stderr).lines();
        
        loop {
            tokio::select! {
                line = stdout_reader.next_line() => {
                    match line {
                        Ok(Some(line)) => send!(tx, source, "#[{}]: {}", name, line),
                        Ok(None) | Err(_) => break,
                    }
                }
                line = stderr_reader.next_line() => {
                    match line {
                        Ok(Some(line)) => send!(tx, source, "#[{}] ERR: {}", name, line),
                        Ok(None) | Err(_) => break,
                    }
                }
            }
        }
    });
    
    Ok(proc)
}
