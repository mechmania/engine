use mm_engine::{
    engine,
    args::parse_cli,
};

#[tokio::main]
async fn main() {
    let args = parse_cli();

    if let Err(e) = engine::run(args).await {
        eprintln!("{:?}", e.context("fatal error"));
        std::process::exit(1)
    }
}
