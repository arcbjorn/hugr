//! Hugr is a project memory and intelligence system for coding agents.
//!
//! The crate exists to back the `hugr` binary, so its public surface is
//! deliberately one entry point: [`run`] takes the process arguments and
//! executes the requested command. Everything else — the code graph, the
//! temporal memory store, the context compiler, the daemon, and the MCP
//! server — lives in private modules and is reached through the CLI or the
//! MCP protocol rather than as a library API.

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

/// Parses `args` as a Hugr command and runs it.
///
/// `args` is the full process argument list, including the executable name at
/// index 0, exactly as [`std::env::args`] yields it.
///
/// # Errors
///
/// Returns an [`Error`] when the arguments do not name a valid command or
/// when the command itself fails. The message is written for a terminal:
/// callers should print it rather than match on it.
pub async fn run(args: Vec<String>) -> Result<()> {
    let command = Command::parse(&args)?;
    commands::execute(command).await
}
