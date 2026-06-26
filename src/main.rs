use std::env;
use std::process;

#[tokio::main]
async fn main() {
    if let Err(error) = hugr::run(env::args().collect()).await {
        eprintln!("error: {error}");
        process::exit(1);
    }
}
