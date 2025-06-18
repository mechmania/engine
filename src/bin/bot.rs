use game_runner::ipc::{ EngineChannel, Strategy };
use std::path::PathBuf;
use std::env::args;

#[tokio::main]
async fn main() {
    if let Err(e) = run().await {
        eprintln!("{:?}", e.context("a fatal error occured"));
    }
}

async fn run () -> anyhow::Result<()> {
    let args = args();
    if args.len() < 2 {
        println!("usage: [bin name] [shmem path]");
        return Ok(());
    }

    let path = PathBuf::from(&args.skip(1).next().unwrap());
    let chan = EngineChannel::from_path(path)?;
    let strat = Strategy {
        on_init: Box::new(|_| 0.0),
        on_tick: Box::new(|state| {
            if state.ball_pos.1 > state.p1_pos {
                1.0
            } else {
                -1.0
            }
        })
    };

    loop {
        chan.handle_msg(&strat).await;
    }
}
