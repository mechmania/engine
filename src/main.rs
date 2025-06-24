use anyhow::{Context, Result};
use game_runner::{
    cli::*,
    game::{
        action::{ eval_reset, eval_tick },
        state::{ GameState, PlayerAction, TeamPair },
        config::*,
        util::Vec2
    },
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

struct BotManager {
    channel: BotChannel,
    process: tokio::process::Child,
    name: String,
}

impl BotManager {
    fn spawn(
        command: &Path,
        name: &str,
        source: OutputSource,
        tx: mpsc::UnboundedSender<Message>,
    ) -> anyhow::Result<Self> {
        let channel = BotChannel::new()?;
        let mut process = Command::new(command)
            .arg(channel.backing_file_path())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("failed to spawn {}", name))?;

        let stdout = process.stdout.take().unwrap();
        let stderr = process.stderr.take().unwrap();

        let name_async = name.to_string();
        tokio::spawn(async move {
            let mut stdout_reader = BufReader::new(stdout).lines();
            let mut stderr_reader = BufReader::new(stderr).lines();

            loop {
                tokio::select! {
                    line = stdout_reader.next_line() => {
                        match line {
                            Ok(Some(line)) => send!(tx, source, "#[{}]: {}", &name_async, line),
                            Ok(None) | Err(_) => break,
                        }
                    }
                    line = stderr_reader.next_line() => {
                        match line {
                            Ok(Some(line)) => send!(tx, source, "#[{}] ERR: {}", &name_async, line),
                            Ok(None) | Err(_) => break,
                        }
                    }
                }
            }
        });

        Ok(Self {
            channel,
            process,
            name: name.to_string(),
        })
    }

    async fn handshake(&mut self, tx: &mpsc::UnboundedSender<Message>) {
        if !self.channel
            .msg::<HandshakeProtocol>(&HANDSHAKE_ENGINE, Duration::from_millis(1))
            .await
            .ok()
            .map(|res| res == HANDSHAKE_BOT)
            .unwrap_or(false)
        {
            send!(
                tx,
                OutputSource::Gamelog,
                "### FATAL ERROR: bot {} failed handshake",
                self.name
            );
            let _ = self.process.kill().await;
        }
    }

    async fn reset(&mut self, score: &TeamPair<u32>, conf: &GameConfig, tx: &mpsc::UnboundedSender<Message>) -> [Vec2; NUM_PLAYERS as usize] {
        self.channel
            .msg::<ResetProtocol>(&ResetMsg { score: score.clone(), config: conf.clone() }, Duration::from_millis(1))
            .await
            .unwrap_or_else(|e| {
                send!(
                    tx,
                    OutputSource::Gamelog,
                    "### [bot {}] error resetting: {e}",
                    self.name
                );
                Default::default()
            })
    }

    async fn tick(&mut self, state: &GameState, engine_time: Duration, tx: &mpsc::UnboundedSender<Message>) -> [PlayerAction; NUM_PLAYERS as usize] {
        self.channel
            .msg::<TickProtocol>(state, engine_time)
            .await
            .unwrap_or_else(|e| {
                send!(
                    tx,
                    OutputSource::Gamelog,
                    "### [bot {}] error on tick: {e}",
                    self.name
                );
                Default::default()
            })
    }
}

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
        spawn_ball_dist: 200.0,
        ball: BallConfig {
            friction: 0.99,
            radius: 5.0,
            capture_ticks: 50,
            stagnation_radius: 30.0,
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

    let (mut bot_a, mut bot_b) = (
        BotManager::spawn(&cli.bot_a, "A", OutputSource::BotA, tx.clone())?,
        BotManager::spawn(&cli.bot_b, "B", OutputSource::BotB, tx.clone())?,
    );

    let start = Instant::now();
    join!(bot_a.handshake(&tx), bot_b.handshake(&tx));
    let mut ma = SumTreeSMA::<_, _, 50>::from_zero(Duration::ZERO);

    let mut state = GameState::new(&conf);
    let mut needs_reset = true;

    while state.tick < conf.max_ticks {
        if needs_reset {
            let (formation_a, formation_b) = join!(
                bot_a.reset(&state.score, &conf, &tx), 
                bot_b.reset(&state.score, &conf, &tx)
            );
            let formation = TeamPair::new(formation_a, formation_b);
            eval_reset(&mut state, &conf, &formation);
        }

        let last_tick_time = std::cmp::max(ma.get_average(), Duration::from_micros(1));

        let (action_a, action_b) = join!(
            bot_a.tick(&state, last_tick_time, &tx), 
            bot_b.tick(&state, last_tick_time, &tx)
        );

        let actions = std::array::from_fn(|i| {
            if i < NUM_PLAYERS as usize {
                action_a[i].clone()
            } else {
                action_b[i - NUM_PLAYERS as usize].clone()
            }
        });

        let tick_start = Instant::now();
        needs_reset = eval_tick(&mut state, &conf, actions);
        ma.add_sample(tick_start.elapsed());

        send!(
            tx,
            OutputSource::Gamelog,
            "{}",
            serde_json::to_string(&state)?
        );
    }

    let _ = join!(bot_a.process.kill(), bot_b.process.kill());

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
