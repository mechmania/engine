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

/// How long a bot gets to notice `Handoff::Closed` and exit on its own before it is killed.
///
/// A bot waiting on the channel is spinning in `await_handoff`, so it sees the flag within
/// microseconds; this is scheduling slack, not polling latency. The point is that a finished match looks like a clean exit rather than a SIGKILL:
/// a bot that treats the end of a match as a fatal error exits non-zero and reads as a
/// crash in the gamelog.
const SHUTDOWN_GRACE: Duration = Duration::from_millis(500);

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

    /// The handshake never touches `ComputeBudget`: it is bounded by `HANDSHAKE_TIMEOUT`
    /// wall clock and nothing else, and the bot reports no CPU time for it. Failing it kills
    /// the bot process outright, which is what actually takes the bot out of the match --
    /// `tick` sees `exited()` and returns a default action from then on.
    async fn handshake(
        &mut self,
        request: &HandshakeRequest,
        tx: &mpsc::UnboundedSender<Message>,
    ) {
        if !self
            .channel
            .request::<HandshakeProtocol>(request, HANDSHAKE_TIMEOUT)
            .await
            .map_err(|e| {
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
            .map(|(res, _)| {
                // Not just "did a bot answer" any more: the answer is a hash of the layout
                // of everything that crosses the channel, so a bot built against a
                // different `mm-engine` rev fails here rather than spending the match
                // reading fields at the wrong offsets.
                let matches = res == HANDSHAKE_FINGERPRINT;
                if !matches {
                    eprintln!(
                        "### FATAL ERROR: bot {} failed handshake: protocol layout mismatch \
                         (expected {:#x}, got {:#x}) -- the bot was built against a \
                         different version of the engine",
                        self.name, HANDSHAKE_FINGERPRINT, res
                    );
                    send!(
                        tx,
                        OutputSource::Gamelog,
                        "### FATAL ERROR: bot {} failed handshake: protocol layout mismatch \
                         (expected {:#x}, got {:#x}) -- the bot was built against a \
                         different version of the engine",
                        self.name,
                        HANDSHAKE_FINGERPRINT,
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
                if e.forfeits_budget() {
                    self.budget.forfeit();
                }
                Default::default()
            }
        };
        res
    }
}

fn handshake_request(team: Team, config: &GameConfig) -> HandshakeRequest {
    HandshakeRequest { team, config: config.clone() }
}

pub async fn run(args: ArgConfig) -> Result<()> {
    let (tx, recv_task) = spawn_reciever(&args)?;

    let conf = GameConfig {
        max_ticks: 7200,
        bot: BotConfig {
            radius: BOT_RADIUS,
            speed: StatUpgrade {
                value: [0.050, 0.056, 0.062, 0.068, 0.075],
                cost: 25.0,
            },
            health: StatUpgrade {
                value: [10.0, 12.0, 14.0, 16.0, 20.0],
                cost: 25.0,
            },
            turn_speed: StatUpgrade {
                value: [3.0, 3.5, 4.0, 4.5, 5.0],
                cost: 15.0,
            },
            blaster_cooldown: StatUpgrade {
                value: [60.0, 52.0, 45.0, 39.0, 33.0],
                cost: 30.0,
            },
            blaster_range: StatUpgrade {
                value: [10.0, 11.0, 12.0, 13.0, 15.0],
                cost: 15.0,
            },
            blaster_damage: StatUpgrade {
                value: [3.0, 3.5, 4.0, 4.5, 5.0],
                cost: 30.0,
            },
            // At level 0 one healer exactly cancels one battle bot's sustained damage
            // (blaster_damage / blaster_cooldown = 3.0 / 60 = 0.05).
            heal_per_tick: StatUpgrade {
                value: [0.050, 0.060, 0.070, 0.085, 0.100],
                cost: 20.0,
            },
            extract_rate: StatUpgrade {
                value: [0.100, 0.125, 0.150, 0.175, 0.200],
                cost: 20.0,
            },
            base_invulnerability_ticks: 15,
            base_blaster_splash_radius: 0.3,
            base_heal_range: 3.0,
            base_heal_arc_deg: 90.0,
            // Three healers stack on one target; a fourth is wasted, at every level.
            heal_stack_cap: 3.0,
            base_extract_range: 5.0,
        },
        payload: PayloadConfig {
            radius: 0.75,
            capture_radius: 2.5,
            speed: 0.02,
        },
        payload_path: PAYLOAD_PATH,
        deposit: DepositConfig {
            pos: DEPOSIT_POS,
            radius: 0.5,
            // One level-0 extractor is worth 0.1 tokens/tick, so a saturated deposit pays 1.6.
            extractor_cap: 16,
        },
        fabricator: FabricatorConfig {
            interval: 100,
            // Around 500 extractor-ticks: a fleet mining with four extractors buys a rush
            // roughly every 125 ticks, so paying for bodies beats waiting but does not
            // trivially outrun the free cadence.
            rush_cost: 50.0,
        },
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
    let (request_a, request_b) = (
        handshake_request(Team::A, &conf),
        handshake_request(Team::B, &conf),
    );
    join!(
        bot_a.handshake(&request_a, &tx),
        bot_b.handshake(&request_b, &tx)
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

    // Tell both bots the match is over, then give them a window to exit on their own.
    // `EngineChannel::drop` also closes, but that happens after everything else is torn
    // down -- far too late for a bot to act on.
    bot_a.channel.close();
    bot_b.channel.close();
    let _ = tokio::time::timeout(
        SHUTDOWN_GRACE,
        async { join!(bot_a.process.wait(), bot_b.process.wait()) },
    ).await;

    // Anything still alive ignored the close; it gets the old treatment.
    let _ = join!(bot_a.process.kill(), bot_b.process.kill());

    // After the grace window, not before it: a bot's parting stdout is still worth logging.
    bot_a.io_task.abort();
    bot_b.io_task.abort();

    drop(tx);
    drop(bot_a);
    drop(bot_b);

    let _ = recv_task.await;

    Ok(())
}
