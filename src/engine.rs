use crate::{
    args::*,
    game::{
        action::eval_tick, config::*, diff::Diff, state::{Action, FleetAction, GameState, Mirror}, team::Team
    },
    ipc::*,
    timing::{ComputeBudget, EngineTickClock, TICK_HANG_TIMEOUT},
};
use anyhow::{Context, Result};
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

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

struct BotManager {
    channel: EngineChannel,
    name: String,
    budget: ComputeBudget,
    process: tokio::process::Child,
    io_task: tokio::task::JoinHandle<()>,
}

impl BotManager {
    fn spawn(
        command: &Path,
        name: &str,
        source: OutputSource,
        tx: mpsc::UnboundedSender<Message>,
    ) -> anyhow::Result<Self> {
        let channel = EngineChannel::new()?;
        let mut process = Command::new(command)
            .arg(channel.backing_file_path())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("failed to spawn {}", name))?;

        let stdout = process.stdout.take().unwrap();
        let stderr = process.stderr.take().unwrap();

        let name_async = name.to_string();
        let io_task = tokio::spawn(async move {
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
            name: name.to_string(),
            budget: ComputeBudget::new(),
            process,
            io_task,
        })
    }

    fn exited(&mut self) -> bool {
        self.process
            .try_wait()
            .map_or(true, |status| status.is_some())
    }

    async fn handshake(
        &mut self,
        team: Team,
        config: &GameConfig,
        tx: &mpsc::UnboundedSender<Message>,
    ) {
        if !self
            .channel
            .request::<HandshakeProtocol>(
                &HandshakeRequest {
                    team,
                    config: config.clone(),
                },
                HANDSHAKE_TIMEOUT,
            )
            .await
            .map_err(|e| {
                self.budget.forfeit();
                eprintln!("### FATAL ERROR: bot {} failed handshake: {}", self.name, e);
                send!(
                    tx,
                    OutputSource::Gamelog,
                    "### FATAL ERROR: bot {} failed handshake: {}",
                    self.name,
                    e
                );
                e
            })
            .ok()
            .map(|(res, _cpu_time)| {
                let matches = res == HANDSHAKE_MAGIC;
                if !matches {
                    self.budget.forfeit();
                    eprintln!(
                        "### FATAL ERROR: bot {} failed handshake: expected {}, got {}",
                        self.name, HANDSHAKE_MAGIC, res
                    );
                    send!(
                        tx,
                        OutputSource::Gamelog,
                        "### FATAL ERROR: bot {} failed handshake: expected {}, got {}",
                        self.name,
                        HANDSHAKE_MAGIC,
                        res
                    );
                }
                matches
            })
            .unwrap_or(false)
        {
            let _ = self.process.kill().await;
        }
    }

    async fn tick(
        &mut self,
        state: &GameState,
        engine_time: Duration,
        tx: &mpsc::UnboundedSender<Message>,
    ) -> FleetAction {
        if self.exited() {
            return Default::default();
        }

        let res = match self
            .channel
            .request::<TickProtocol>(state, TICK_HANG_TIMEOUT)
            .await
        {
            Ok((res, cpu_time)) => {
                let elapsed = self.budget.charge(cpu_time, engine_time);
                // println!("bot {} took {} ticks", self.name, elapsed);
                res
            }
            Err(e) => {
                eprintln!("### [bot {}] error on tick: {e}", self.name);
                send!(
                    tx,
                    OutputSource::Gamelog,
                    "### [bot {}] error on tick: {e}",
                    self.name
                );
                self.budget.forfeit();
                Default::default()
            }
        };
        res
    }
}

pub async fn run(args: ArgConfig) -> Result<()> {
    let (tx, recv_task) = spawn_reciever(&args)?;

    let conf = GameConfig {
        max_ticks: 7200,
        bot: BotConfig {
            radius: 0.45,
            base_speed: 0.1,
            base_health: 10.0,
            base_turn_speed: 10.0,
            base_blaster_cooldown: 60,
            base_invulnerability_ticks: 15,
            base_blaster_range: 10.0,
            base_blaster_damage: 3.0,
            base_blaster_splash_radius: 0.3,
        },
        payload: PayloadConfig {
            radius: 1.5,
            capture_radius: 3.0,
            speed_per_bot: 0.01,
            max_speed: 0.04,
            contest_diff: 1,
        },
        payload_path: PAYLOAD_PATH,
        // TODO: deposit layout. Place them in mirror-symmetric pairs so that `mirror_pos`
        // maps the set onto itself and `GameConfig` needs no `Mirror` impl.
        deposit_count: 0,
        deposits: [Deposit::default(); DEPOSITS_MAX],
        map: MAP,
    };

    send!(
        tx,
        OutputSource::Gamelog,
        "{}",
        serde_json::to_string(&conf)?
    );

    let (mut bot_a, mut bot_b) = (
        BotManager::spawn(&args.bot_a, "A", OutputSource::BotA, tx.clone())?,
        BotManager::spawn(&args.bot_b, "B", OutputSource::BotB, tx.clone())?,
    );

    let start = Instant::now();
    join!(
        bot_a.handshake(Team::A, &conf, &tx),
        bot_b.handshake(Team::B, &conf, &tx)
    );
    let mut engine_clock = EngineTickClock::new();

    let mut last_state: Option<GameState> = None;
    let mut state = GameState::new(&conf);
    let mut _needs_reset = true;
    let mut _endgame_reset = false;

    while state.tick < conf.max_ticks {
        let last_tick_time = engine_clock.average();
        // println!("engine tick time: {:?}", last_tick_time);

        let mut mirrored_state = state.clone();
        mirrored_state.mirror(&conf);

        let mut action_a = bot_a.tick(&state, last_tick_time, &tx).await;
        let mut action_b = bot_b.tick(&mirrored_state, last_tick_time, &tx).await;

        action_a.sanitize();
        action_b.sanitize();
        action_b.mirror(&conf);

        engine_clock.time(|| eval_tick(&mut state, &conf, action_a, action_b));

        if let Some(last_state) = last_state {
            let diff = GameState::diff_json(&last_state, &state);
            if let Some(diff) = diff {
                send!(
                    tx,
                    OutputSource::Gamelog,
                    "{}",
                    serde_json::to_string(&diff)?
                );
            }

        } else {
            send!(
                tx,
                OutputSource::Gamelog,
                "{}",
                serde_json::to_string(&state)?
            );
        }

        last_state = Some(state.clone());
    }

    // let winner = if state.score.a > state.score.b {
    //     Some("Bot A")
    // } else if state.score.a < state.score.b {
    //     Some("Bot B")
    // } else {
    //     None
    // };

    let winner: Option<&str> = None;

    send!(
        tx,
        OutputSource::Gamelog,
        "# time elapsed: {:?}",
        start.elapsed()
    );

    bot_a.io_task.abort();
    bot_b.io_task.abort();

    let _ = join!(bot_a.process.kill(), bot_b.process.kill());

    drop(tx);
    drop(bot_a);
    drop(bot_b);

    let _ = recv_task.await;

    Ok(())
}
