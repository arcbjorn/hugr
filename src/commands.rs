use crate::cli::{Command, help_text};
use crate::store::{Memory, Store};
use std::fs;
use std::path::Path;

pub async fn execute(command: Command) -> Result<(), String> {
    match command {
        Command::Init => init().await,
        Command::Status => status().await,
        Command::Remember { text } => remember(&text).await,
        Command::Recall { query } => recall(&query).await,
        Command::Context { task } => context(&task).await,
        Command::Improve => placeholder("improve", "memory consolidation is not implemented yet"),
        Command::Forget { query } => forget(query),
        Command::Doctor => doctor().await,
        Command::Help => {
            print!("{}", help_text());
            Ok(())
        }
    }
}

async fn init() -> Result<(), String> {
    let store = Store::open_current();
    store.init().await?;
    println!("initialized Hugr at {}", store.root().display());
    Ok(())
}

async fn status() -> Result<(), String> {
    let store = Store::open_current();
    let memories = store.memories().await?;

    println!("Hugr status");
    println!(
        "  store: {}",
        if store.exists() { "ready" } else { "missing" }
    );
    println!("  root: {}", store.root().display());
    println!("  memories: {}", memories.len());
    Ok(())
}

async fn remember(text: &str) -> Result<(), String> {
    let memory = Store::open_current().remember(text).await?;
    println!("remembered {}", memory.id);
    Ok(())
}

async fn recall(query: &str) -> Result<(), String> {
    let matches = Store::open_current().recall(query, 10).await?;

    if matches.is_empty() {
        println!("no memories matched '{query}'");
        return Ok(());
    }

    println!("Memory matches");
    for memory in matches {
        println!("- {} [{}]: {}", memory.id, memory.kind, memory.text);
    }
    Ok(())
}

async fn context(task: &str) -> Result<(), String> {
    let store = Store::open_current();
    let memories = store.recall(task, 5).await?;
    let files = discover_candidate_files(task, 12)?;

    println!("# Hugr Context Pack");
    println!();
    println!("## Task");
    println!("{task}");
    println!();
    println!("## Relevant Files");
    if files.is_empty() {
        println!("No file candidates found yet.");
    } else {
        for file in files {
            println!("- {file}");
        }
    }
    println!();
    println!("## Relevant Memories");
    print_memories_or_empty(&memories);
    println!();
    println!("## Suggested Path");
    println!("1. Inspect the relevant files and symbols.");
    println!("2. Check whether any memories are stale before relying on them.");
    println!("3. Make the smallest change that satisfies the task.");
    println!("4. Run the narrowest useful tests, then broaden if risk is unclear.");
    println!();
    println!("## Citations");
    if memories.is_empty() {
        println!("- No memory citations yet.");
    } else {
        for memory in memories {
            println!("- {}", memory.id);
        }
    }

    Ok(())
}

fn forget(query: Option<String>) -> Result<(), String> {
    match query {
        Some(query) => placeholder(
            "forget",
            &format!("forget matching '{query}' is not implemented yet"),
        ),
        None => placeholder(
            "forget",
            "forget requires a future selector such as --stale or a query",
        ),
    }
}

async fn doctor() -> Result<(), String> {
    let store = Store::open_current();
    println!("Hugr doctor");
    println!(
        "  current_dir: {}",
        std::env::current_dir()
            .map_err(|error| error.to_string())?
            .display()
    );
    println!("  store_exists: {}", store.exists());
    println!("  store_root: {}", store.root().display());
    println!("  memories_readable: {}", store.memories().await.is_ok());
    Ok(())
}

fn placeholder(command: &str, detail: &str) -> Result<(), String> {
    println!("hugr {command}: {detail}");
    Ok(())
}

fn print_memories_or_empty(memories: &[Memory]) {
    if memories.is_empty() {
        println!("No matching memories yet.");
    } else {
        for memory in memories {
            println!("- {} [{}]: {}", memory.id, memory.kind, memory.text);
        }
    }
}

fn discover_candidate_files(task: &str, limit: usize) -> Result<Vec<String>, String> {
    let terms = task
        .split(|char: char| !char.is_alphanumeric() && char != '_' && char != '-')
        .filter(|term| term.len() > 2)
        .map(|term| term.to_lowercase())
        .collect::<Vec<_>>();

    let mut scored = Vec::new();
    visit_files(Path::new("."), &terms, &mut scored)?;
    scored.sort_by(|left, right| right.0.cmp(&left.0).then_with(|| left.1.cmp(&right.1)));
    scored.truncate(limit);
    Ok(scored.into_iter().map(|(_, path)| path).collect())
}

fn visit_files(
    path: &Path,
    terms: &[String],
    scored: &mut Vec<(usize, String)>,
) -> Result<(), String> {
    let entries = fs::read_dir(path).map_err(|error| error.to_string())?;
    for entry in entries {
        let entry = entry.map_err(|error| error.to_string())?;
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();

        if should_skip(&name) {
            continue;
        }

        if path.is_dir() {
            visit_files(&path, terms, scored)?;
        } else if path.is_file() {
            let display = path
                .strip_prefix(".")
                .unwrap_or(&path)
                .display()
                .to_string();
            let normalized = display.to_lowercase();
            let score = terms
                .iter()
                .filter(|term| normalized.contains(term.as_str()))
                .count();
            if score > 0 || scored.len() < 20 {
                scored.push((score, display));
            }
        }
    }
    Ok(())
}

fn should_skip(name: &str) -> bool {
    matches!(
        name,
        ".git" | ".hugr" | ".agent-out" | ".worktrees" | "target" | "node_modules"
    )
}
