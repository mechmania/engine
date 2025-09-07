use anyhow::{Context, Result};
use crate::{
    args::*,
    game::{
        action::{ eval_reset, eval_tick },
        state::{ Team ,GameState, PlayerAction, TeamPair, Mirror, mirror_pos },
        config::*,
        util::Vec2
    },
    ipc::*,
};
use simple_moving_average::{SumTreeSMA, SMA};
use std::{
    path::Path, process::Stdio, time::{Duration, Instant}
};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    join,
    process::Command,
    sync::mpsc,
};

const TOTAL_COMPUTE_TICKS: u32 = 100000;
const DELAY_TICKS: u32 = 2000;
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

struct BotManager {
    channel: BotChannel,
    name: String,
    ticks: u32,
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
            ticks: TOTAL_COMPUTE_TICKS,
            process,
            io_task,
        })
    }

    fn exited(&mut self) -> bool {
        self.process.try_wait().map_or(true, |status| status.is_some())
    }

    async fn handshake(&mut self, team: Team, config: &GameConfig, tx: &mpsc::UnboundedSender<Message>) {
        if !self.channel
            .msg::<HandshakeProtocol>(&HandshakeMsg { team, config: config.clone() }, HANDSHAKE_TIMEOUT)
            .await
            .map_err(|e| {
                self.ticks = 0;
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
            .map(|res| {
                let matches = res == HANDSHAKE_BOT;
                if !matches {
                    self.ticks = 0;
                    eprintln!("### FATAL ERROR: bot {} failed handshake: expected {}, got {}", self.name, HANDSHAKE_BOT, res);
                    send!(
                        tx,
                        OutputSource::Gamelog,
                        "### FATAL ERROR: bot {} failed handshake: expected {}, got {}",
                        self.name,
                        HANDSHAKE_BOT,
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

    async fn reset(&mut self, score: &TeamPair<u32>, engine_time: Duration, tx: &mpsc::UnboundedSender<Message>) -> [Vec2; NUM_PLAYERS as usize] {

        if self.exited() {
            return Default::default();
        }

        let time = Instant::now();
        let res = self.channel
            .msg::<ResetProtocol>(score, self.ticks * engine_time)
            .await
            .unwrap_or_else(|e| {
                eprintln!("### [bot {}] error resetting: {e}", self.name);
                send!(
                    tx,
                    OutputSource::Gamelog,
                    "### [bot {}] error resetting: {e}",
                    self.name
                );
                Default::default()
            });
        let elapsed = time.elapsed().div_duration_f64(engine_time) as u32;
        self.ticks = if elapsed <= DELAY_TICKS {
            TOTAL_COMPUTE_TICKS.min(self.ticks + DELAY_TICKS - elapsed)
        } else {
            self.ticks - self.ticks.min(elapsed - DELAY_TICKS)
        };
        res
    }

    async fn tick(&mut self, state: &GameState, engine_time: Duration, tx: &mpsc::UnboundedSender<Message>) -> [PlayerAction; NUM_PLAYERS as usize] {

        if self.exited() {
            return Default::default();
        }

        let time = Instant::now();
        let res = self.channel
            .msg::<TickProtocol>(state, self.ticks * engine_time)
            .await
            .unwrap_or_else(|e| {
                eprintln!("### [bot {}] error on tick: {e}", self.name);
                send!(
                    tx,
                    OutputSource::Gamelog,
                    "### [bot {}] error on tick: {e}",
                    self.name
                );
                Default::default()
            });
        let elapsed = time.elapsed().div_duration_f64(engine_time) as u32;
        // println!("bot {} took {} ticks", self.name, elapsed);
        self.ticks = if elapsed <= DELAY_TICKS {
            TOTAL_COMPUTE_TICKS.min(self.ticks + DELAY_TICKS - elapsed)
        } else {
            self.ticks - self.ticks.min(elapsed - DELAY_TICKS)
        };
        res
    }
}


pub async fn run(args: ArgConfig) -> Result<()> {
    let (tx, recv_task) = spawn_reciever(&args)?;

    let conf = GameConfig {
        max_ticks: 7200,
        endgame_ticks: 1000,
        spawn_ball_dist: 200.0,
        ball: BallConfig {
            friction: 0.99,
            radius: 5.0,
            capture_ticks: 50,
            stagnation_radius: 30.0,
            stagnation_ticks: 150,
        },
        player: PlayerConfig {
            radius: 10.0,
            pickup_radius: 25.0,
            speed: 4.0,
            pass_speed: 12.0,
            pass_error: 10.0,
            possession_slowdown: 0.75,
        },
        field: FieldConfig {
            width: 1000,
            height: 600,
        },
        goal: GoalConfig {
            normal_height: 150,
            thickness: 5,
            penalty_box_width: 80,
            penalty_box_height: 200,
            penalty_box_radius: 35,
        },
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
    join!(bot_a.handshake(Team::A, &conf, &tx), bot_b.handshake(Team::B, &conf, &tx));
    let mut ma = SumTreeSMA::<_, _, 50>::from_zero(Duration::from_millis(1));


    let mut state = GameState::new(&conf);
    let mut needs_reset = true;


    while state.tick < conf.max_ticks || (state.tick < (conf.max_ticks + conf.endgame_ticks) && state.score.a == state.score.b) {
        let last_tick_time = ma.get_average();

        if needs_reset {
            let mut mirrored_score = state.score;
            mirrored_score.mirror(&conf);
            let (formation_a, mut formation_b) = (
                bot_a.reset(&state.score, last_tick_time, &tx).await, 
                bot_b.reset(&mirrored_score, last_tick_time, &tx).await
            );
            formation_b.iter_mut().for_each(|pos| mirror_pos(pos, &conf));
            let formation = TeamPair::new(formation_a, formation_b);
            eval_reset(&mut state, &conf, &formation);
        }

        let mut mirrored_state = state.clone();
        mirrored_state.mirror(&conf);

        let (mut action_a, mut action_b) = join!(
            bot_a.tick(&state, last_tick_time, &tx), 
            bot_b.tick(&mirrored_state, last_tick_time, &tx)
        );

        action_a.iter_mut().for_each(|a| a.sanitize());
        action_b.iter_mut().for_each(|a| a.sanitize());

        let actions = std::array::from_fn(|i| {
            if i < NUM_PLAYERS as usize {
                action_a[i].clone()
            } else {
                let mut unmirrored = action_b[i - NUM_PLAYERS as usize].clone();
                unmirrored.mirror(&conf);
                unmirrored
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

    send!(
        tx,
        OutputSource::Gamelog,
        "# time elapsed: {:?}\n{}",
        start.elapsed(),
        if state.score.a > state.score.b {
            "# Winner: Bot A"
        } else if state.score.a < state.score.b {
            "# Winner: Bot B"
        } else {
            "# TIE"
        }
    );

    // Forcibly abort the I/O tasks (this drops their tx clones)
    bot_a.io_task.abort();
    bot_b.io_task.abort();

    // Kill processes
    let _ = join!(bot_a.process.kill(), bot_b.process.kill());

    // Now drop everything
    drop(tx);
    drop(bot_a);
    drop(bot_b);

    // This should work now
    let _ = recv_task.await;
    Ok(())
}
