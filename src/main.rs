//! The `hugr` command-line entry point.
//!
//! Argument parsing and command dispatch live in the library; this binary
//! only forwards the process arguments and turns a failure into a message on
//! stderr plus a non-zero exit status.

use std::env;
use std::process;

#[tokio::main]
async fn main() {
    if let Err(error) = hugr::run(env::args().collect()).await {
        eprintln!("error: {error}");
        process::exit(1);
    }
}
