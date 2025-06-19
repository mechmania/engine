use game_runner::{
    game::{run_tick, GameConfig, GameState},
    ipc::*,
    cli::*,
};
use tokio::{
    sync::mpsc,
    process::Command,
    join,
    io::{BufReader, AsyncBufReadExt},
};
use anyhow::{ Context, Result };
use std::{
    path::{ PathBuf, Path }, 
    fs::File,
    time::Instant,
    process::Stdio,
};


#[tokio::main]
async fn main() {
    if let Err(e) = run().await {
        eprintln!("{:?}", e.context("a fatal error occured"));
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

    println!("{}", serde_json::to_string(&conf)?);

    let channel_a = BotChannel::new()?;
    let channel_b = BotChannel::new()?;

    spawn_bot(&cli.bot_a, channel_a.backing_file_path(), "bot a", OutputSource::BotA, tx.clone())
        .with_context(|| "failed to launch bot a")?;
    spawn_bot(&cli.bot_b, channel_a.backing_file_path(), "bot b", OutputSource::BotA, tx.clone())
        .with_context(|| "failed to launch bot b")?;
    drop(tx);

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
    recv_task.await??;
    Ok(())
}

fn spawn_bot(
    command: &Path,
    backing_file: &Path,
    prefix: &'static str,
    source: OutputSource,
    tx: mpsc::UnboundedSender<Message>
) -> anyhow::Result<()>{
    let mut proc = Command::new(&command)
        .arg(backing_file)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("unable to spawn {prefix}"))?;

    let stdout = proc.stdout.take().unwrap();
    let stderr = proc.stderr.take().unwrap();

    tokio::task::spawn(async move {
        let tx_clone = tx.clone();
        let stdout_task = async move {
            let reader = BufReader::new(stdout).lines();
            tokio::pin!(reader);

            let mut reader = reader;
            while let Some(line) = reader.next_line().await? {
                tx_clone.send(Message {
                    msg: format!("#[{}]: {}", prefix, line),
                    source
                }).unwrap();
            }
            Ok::<_, std::io::Error>(())
        };

        let stderr_task = async move {
            let reader = BufReader::new(stderr).lines();
            tokio::pin!(reader);

            let mut reader = reader;
            while let Some(line) = reader.next_line().await? {
                tx.send(Message {
                    msg: format!("#[{}] ERR: {}", prefix, line),
                    source
                }).unwrap();
            }
            Ok::<_, std::io::Error>(())
        };
        let (res1, res2) = tokio::join!(stdout_task, stderr_task);

        res1?;
        res2?;
        Ok::<_, std::io::Error>(())
    });
    Ok(())
}
