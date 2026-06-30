use crate::cli::{Command, OutputFormat, help_text};
use crate::context::{ContextPack, json_string};
use crate::store::{Memory, Store};
use std::fmt::Write;
use std::fs;
use std::path::Path;

pub async fn execute(command: Command) -> Result<(), String> {
    match command {
        Command::Init => init().await,
        Command::Status => status().await,
        Command::Remember { text } => remember(&text).await,
        Command::Recall { query, format } => recall(&query, format).await,
        Command::Context { task, format } => context(&task, format).await,
        Command::ProjectStatus => project_status().await,
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
    let project = if store.exists() {
        store.project().await?
    } else {
        None
    };

    println!("Hugr status");
    println!(
        "  store: {}",
        if store.exists() { "ready" } else { "missing" }
    );
    println!("  root: {}", store.root().display());
    println!("  memories: {}", memories.len());
    if let Some(project) = project {
        println!("  project: {}", project.name);
        println!("  project_root: {}", project.root_path);
    }
    Ok(())
}

async fn remember(text: &str) -> Result<(), String> {
    let memory = Store::open_current().remember(text).await?;
    println!("remembered {}", memory.id);
    Ok(())
}

async fn recall(query: &str, format: OutputFormat) -> Result<(), String> {
    let matches = Store::open_current().recall(query, 10).await?;

    if format == OutputFormat::Json {
        println!("{}", render_recall_json(query, &matches));
        return Ok(());
    }

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

async fn context(task: &str, format: OutputFormat) -> Result<(), String> {
    let store = Store::open_current();
    let memories = store.recall(task, 5).await?;
    let files = discover_candidate_files(task, 12)?;
    let pack = ContextPack::new(task, files, memories);

    if format == OutputFormat::Json {
        println!("{}", pack.render_json());
    } else {
        print!("{}", pack.render_markdown());
    }

    Ok(())
}

async fn project_status() -> Result<(), String> {
    let project = Store::open_current().sync_current_project().await?;

    println!("Hugr project");
    println!("  id: {}", project.id);
    println!("  name: {}", project.name);
    println!("  root: {}", project.root_path);
    println!(
        "  git_remote: {}",
        project.git_remote.as_deref().unwrap_or("unknown")
    );
    println!(
        "  default_branch: {}",
        project.default_branch.as_deref().unwrap_or("unknown")
    );
    println!("  created_at_ms: {}", project.created_at_ms);
    println!("  updated_at_ms: {}", project.updated_at_ms);
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

fn render_recall_json(query: &str, memories: &[Memory]) -> String {
    let mut rendered = String::new();

    rendered.push('{');
    let _ = write!(rendered, "\"query\":{},\"memories\":[", json_string(query));
    for (index, memory) in memories.iter().enumerate() {
        if index > 0 {
            rendered.push(',');
        }
        let _ = write!(
            rendered,
            "{{\"id\":{},\"created_at_ms\":{},\"kind\":{},\"text\":{}}}",
            json_string(&memory.id),
            memory.created_at_ms,
            json_string(&memory.kind),
            json_string(&memory.text)
        );
    }
    rendered.push_str("]}");

    rendered
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

#[cfg(test)]
mod tests {
    use super::render_recall_json;
    use crate::store::Memory;

    #[test]
    fn recall_json_includes_query_and_memories() {
        let json = render_recall_json(
            "plugin hooks",
            &[Memory {
                id: "mem_1".to_string(),
                created_at_ms: 7,
                kind: "fact".to_string(),
                text: "plugin hooks run after configuration is loaded".to_string(),
            }],
        );

        assert!(json.contains("\"query\":\"plugin hooks\""));
        assert!(json.contains("\"id\":\"mem_1\""));
        assert!(json.contains("\"created_at_ms\":7"));
    }
}
