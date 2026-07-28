mod cli;
mod code;
mod commands;
mod context;
mod daemon;
mod discovery;
mod edit;
mod embedding;
mod error;
mod eval;
mod impact;
mod indexer;
mod install;
mod json;
mod llm;
mod mcp;
mod migrations;
mod redact;
mod store;
mod testmap;
mod worktree;

pub use error::{Error, Result};

use cli::Command;

pub async fn run(args: Vec<String>) -> Result<()> {
    let command = Command::parse(&args)?;
    commands::execute(command).await
}
