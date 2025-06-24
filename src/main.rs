use anyhow::{Context, Result};
use game_runner::{
    cli::*,
    game::{eval_reset, eval_tick, GameConfig, GameState},
    ipc::*,
};
use simple_moving_average::{SumTreeSMA, SMA};
use std::{
    path::Path,
    process::Stdio,
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    join,
    process::Command,
    sync::mpsc,
};

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
        max_ticks: 7200,
        spawn_ball_dist: 200,
        ball: BallConfig {
            friction: 0.99,
            radius: 5.0,
            capture_ticks: 50,
            stagnation_radius: 30,
            stagnation_ticks: 150,
        },
        player: PlayerConfig {
            radius: 7.5,
            pickup_radius: 15.0,
            speed: 3.0,
            pass_speed: 8.0,
            pass_error: 10.0,
            possession_slowdown: 0.75,
        },
        field: FieldConfig {
            width: 800,
            height: 600,
        },
        goal: GoalConfig {
            height: 150,
            thickness: 5,
            penalty_radius: 35,
        },
    };

    send!(
        tx,
        OutputSource::Gamelog,
        "{}",
        serde_json::to_string(&conf)?
    );

    let channel_a = BotChannel::new()?;
    let channel_b = BotChannel::new()?;

    let mut proc_a = spawn_bot(
        &cli.bot_a,
        channel_a.backing_file_path(),
        "bot_a",
        OutputSource::BotA,
        tx.clone(),
    )?;
    let mut proc_b = spawn_bot(
        &cli.bot_b,
        channel_b.backing_file_path(),
        "bot_b",
        OutputSource::BotB,
        tx.clone(),
    )?;

    let start = Instant::now();

    join!(
        async move {
            if !channel_a.msg::<HandshakeProtocol>(&HANDSHAKE_ENGINE, Duration::from_millis(1))
                .await
                .ok()
                .map(|res| res == HANDSHAKE_BOT)
                .unwrap_or(false) 
            {
                send!(tx, OutputSource::Engine, "### ERROR: bot a failed handshake");
                proc_a.kill();
            }
        },
        async move {
            if !channel_b.msg::<HandshakeProtocol>(&HANDSHAKE_ENGINE, Duration::from_millis(1))
                .await
                .ok()
                .map(|res| res == HANDSHAKE_BOT)
                .unwrap_or(false) 
            {
                send!(tx, OutputSource::Engine, "### ERROR: bot b failed handshake");
                proc_b.kill();
            }
        }
    );

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

        send!(
            tx,
            OutputSource::Gamelog,
            "{}",
            serde_json::to_string(&state)?
        );
    }

    let _ = join!(proc_a.kill(), proc_b.kill());

    send!(
        tx,
        OutputSource::Gamelog,
        "# time elapsed: {:?}",
        start.elapsed()
    );
    drop(tx);
    recv_task.await??;
    Ok(())
}

async fn get_bot_velocity(
    channel: &BotChannel,
    state: &GameState,
    time: Duration,
    tx: &mpsc::UnboundedSender<Message>,
    bot_name: &str,
) -> f64 {
    channel
        .msg::<TickProtocol>(state, time)
        .await
        .map_err(|e| {
            send!(
                tx,
                OutputSource::Gamelog,
                "### Bot {} error: {}",
                bot_name,
                e
            );
            e
        })
        .unwrap_or(0.0)
}

fn spawn_bot(
    command: &Path,
    backing_file: &Path,
    name: &str,
    source: OutputSource,
    tx: mpsc::UnboundedSender<Message>,
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
