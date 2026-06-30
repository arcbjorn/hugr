mod cli;
mod code;
mod commands;
mod context;
mod discovery;
mod embedding;
mod impact;
mod indexer;
mod mcp;
mod migrations;
mod store;

use cli::Command;

pub async fn run(args: Vec<String>) -> Result<(), String> {
    let command = Command::parse(&args)?;
    commands::execute(command).await
}
