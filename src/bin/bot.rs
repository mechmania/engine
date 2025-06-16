use game_runner::ipc::{ EngineChannel, ShmStage };
use std::path::PathBuf;
use std::env::args;

#[tokio::main]
async fn main () {
    let args = args();
    if args.len() < 2 {
        println!("usage: [bin name] [shmem path]");
        return;
    }

    let path = PathBuf::from(&args.skip(1).next().unwrap());

    let chan = EngineChannel::from_path(path);

    loop {
        match &mut *chan.lock().await {
            ShmStage::Init { config: _, action } => {
                *action = 0.0;
            },
            ShmStage::Tick { state, action} => {
                //*action = if state.ball_pos.1 > state.p0_pos {
                //    1.0
                //} else {
                //    -1.0
                //}
                *action = 0.0
            }
        }
    }
}
