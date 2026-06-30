mod cli;
mod commands;
mod context;
mod discovery;
mod embedding;
mod migrations;
mod store;

use cli::Command;

pub async fn run(args: Vec<String>) -> Result<(), String> {
    let command = Command::parse(&args)?;
    commands::execute(command).await
}
