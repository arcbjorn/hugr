mod cli;
mod code;
mod commands;
mod context;
mod daemon;
mod discovery;
mod edit;
mod embedding;
mod eval;
mod impact;
mod indexer;
mod install;
mod llm;
mod mcp;
mod migrations;
mod redact;
mod store;
mod testmap;
mod worktree;

use cli::Command;

pub async fn run(args: Vec<String>) -> Result<(), String> {
    let command = Command::parse(&args)?;
    commands::execute(command).await
}
