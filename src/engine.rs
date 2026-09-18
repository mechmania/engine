use crate::{
    args::*,
    game::{
        action::{eval_tick, match_result}, config::*, diff::Diff, state::{Action, FleetAction, GameState, Mirror}, team::Team
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
    /// What the bot's last charged tick cost, republished to it on the next one. Held here
    /// rather than read back out of `ComputeBudget` because the bank folds the refill and
    /// the charge into one number and cannot tell them apart afterwards.
    last_charge: u64,
    /// `--no-time-limit` turns this off: costs are still measured and still published, but a
    /// bot is never sat out, never forfeits and never times out. Local development only.
    enforce_time: bool,
    process: tokio::process::Child,
    io_task: tokio::task::JoinHandle<()>,
}

impl BotManager {
    fn spawn(
        command: &Path,
        name: &str,
        source: OutputSource,
        err_source: OutputSource,
        tx: mpsc::UnboundedSender<Message>,
        enforce_time: bool,
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
            let (mut stdout_done, mut stderr_done) = (false, false);

            while !stdout_done || !stderr_done {
                tokio::select! {
                    line = stdout_reader.next_line(), if !stdout_done => {
                        match line {
                            Ok(Some(line)) => send!(tx, source, "#[{}]: {}", &name_async, line),
                            Ok(None) | Err(_) => stdout_done = true,
                        }
                    }
                    line = stderr_reader.next_line(), if !stderr_done => {
                        match line {
                            Ok(Some(line)) => send!(tx, err_source, "#[{}] ERR: {}", &name_async, line),
                            Ok(None) | Err(_) => stderr_done = true,
                        }
                    }
                }
            }
        });

        Ok(Self {
            channel,
            name: name.to_string(),
            budget: ComputeBudget::new(),
            last_charge: 0,
            enforce_time,
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
    async fn handshake(&mut self, request: &HandshakeRequest) {
        if !self
            .channel
            .request::<HandshakeProtocol>(request, Some(HANDSHAKE_TIMEOUT))
            .await
            .map_err(|e| {
                eprintln!("### FATAL ERROR: bot {} failed handshake: {}", self.name, e);
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
                }
                matches
            })
            .unwrap_or(false)
        {
            let _ = self.process.kill().await;
        }
    }

    async fn tick(&mut self, state: &GameState, engine_time: Duration) -> FleetAction {
        if self.exited() {
            return Default::default();
        }
        // Out of compute: sit this tick out. The bot is still waiting on the channel and
        // simply sees a later tick next time it is asked.
        if self.enforce_time && self.budget.is_exhausted() {
            self.budget.refill();
            return Default::default();
        }

        // What it has to spend on the tick it is about to be handed, and what the last one
        // cost it. Published before the request, so the handoff that delivers the state
        // delivers these with it.
        self.channel.set_budget(self.budget.remaining(), self.last_charge);

        let timeout = self.enforce_time.then_some(TICK_HANG_TIMEOUT);
        let res = match self
            .channel
            .request::<TickProtocol>(state, timeout)
            .await
        {
            Ok((res, cpu_time)) => {
                // Charged even with enforcement off: the measurement is the whole point of
                // `--no-time-limit`, it is only the consequence that is suspended.
                self.last_charge = self.budget.charge(cpu_time, engine_time);
                res
            }
            Err(e) => {
                eprintln!("### [bot {}] error on tick: {e}", self.name);
                if self.enforce_time && e.forfeits_budget() {
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
        max_ticks: 9000,
        endgame_ticks: 3000,
        bot: BotConfig {
            radius: BOT_RADIUS,
            speed: 0.05,
            health: 10.0,
            turn_speed: 3.0,
            blaster_cooldown: 60,
            blaster_range: 10.0,
            blaster_damage: 3.0,
            // One healer exactly cancels one battle bot's sustained damage
            // (blaster_damage / blaster_cooldown = 3.0 / 60 = 0.05).
            heal_per_tick: 0.05,
            extract_rate: 0.05,
            base_invulnerability_ticks: 15,
            base_blaster_splash_radius: 0.3,
            base_heal_range: 3.0,
            base_heal_arc_deg: 90.0,
            // Three healers stack on one target; a fourth is wasted.
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
            // One extractor is worth 0.05 tokens/tick, so a saturated deposit pays 0.8.
            extractor_cap: 16,
        },
        fabricator: FabricatorConfig {
            interval: 200,
            rush_cost: 50.0,
            // Sixteen rush orders' worth.
            starting_tokens: 800.0,
        },
        map: MAP,
    };

    send!(
        tx,
        OutputSource::Gamelog,
        "{}",
        serde_json::to_string(&conf)?
    );

    // Loud, and in the log: a match run without enforcement is not a match, and its gamelog
    // must never be mistaken for one.
    let enforce_time = !args.no_time_limit;
    if !enforce_time {
        eprintln!(
            "WARNING: --no-time-limit: the compute budget and the {:?} hang timeout are \
             both off. Costs are still measured and reported to each bot, but nothing is \
             enforced -- this is not how a tournament match runs.",
            TICK_HANG_TIMEOUT,
        );
        send!(tx, OutputSource::Gamelog, "# time enforcement: disabled");
    }

    let (mut bot_a, mut bot_b) = (
        BotManager::spawn(&args.bot_a, "A", OutputSource::BotA, OutputSource::BotAErr, tx.clone(), enforce_time)?,
        BotManager::spawn(&args.bot_b, "B", OutputSource::BotB, OutputSource::BotBErr, tx.clone(), enforce_time)?,
    );

    let start = Instant::now();
    let (request_a, request_b) = (
        handshake_request(Team::A, &conf),
        handshake_request(Team::B, &conf),
    );
    join!(bot_a.handshake(&request_a), bot_b.handshake(&request_b));
    let mut engine_clock = EngineTickClock::new();

    let mut last_state: Option<GameState> = None;
    let mut state = GameState::new(&conf);

    let result = loop {
        let last_tick_time = engine_clock.average();
        // println!("engine tick time: {:?}", last_tick_time);

        let mut mirrored_state = state.clone();
        mirrored_state.mirror(&conf);

        let mut action_a = bot_a.tick(&state, last_tick_time).await;
        let mut action_b = bot_b.tick(&mirrored_state, last_tick_time).await;

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

        if let Some(result) = match_result(&state, &conf) {
            break result;
        }
    };

    let winner = result.winner.map(|team| match team {
        Team::A => "A",
        Team::B => "B",
    });
    println!("{}", serde_json::json!({"winner": winner}));
    send!(
        tx,
        OutputSource::Gamelog,
        "# result: {}",
        serde_json::json!({"winner": winner, "reason": result.reason, "tick": state.tick})
    );

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
