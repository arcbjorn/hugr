use crate::cli::{Command, OutputFormat, help_text};
use crate::context::{ContextPack, json_string};
use crate::discovery;
use crate::impact as impact_analysis;
use crate::indexer;
use crate::mcp;
use crate::store::{
    Memory, Store, SyncConflictSummary, SyncExecutionPlan, SyncPullResult, SyncPushResult,
    SyncRunHistory, SyncTableResult,
};
use crate::worktree;
use std::fmt::Write;
use std::path::Path;

pub async fn execute(command: Command) -> Result<(), String> {
    match command {
        Command::Init => init().await,
        Command::Status => status().await,
        Command::Remember { text } => remember(&text).await,
        Command::Recall { query, format } => recall(&query, format).await,
        Command::Context { task, format } => context(&task, format).await,
        Command::Index => index().await,
        Command::Impact { target, format } => impact(&target, format).await,
        Command::ProjectStatus => project_status().await,
        Command::SessionStart { task } => session_start(&task).await,
        Command::SessionEvent { kind, detail } => session_event(&kind, &detail).await,
        Command::SessionEnd { summary } => session_end(summary.as_deref()).await,
        Command::SyncStatus { format } => sync_status(format).await,
        Command::SyncPush { dry_run, format } => sync_push(dry_run, format).await,
        Command::SyncPull { dry_run, format } => sync_pull(dry_run, format).await,
        Command::SyncHistory { format } => sync_history(format).await,
        Command::Mcp => mcp::serve_stdio().await,
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
    println!("  storage: {}", store.storage_summary());
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
    let pack = compile_context_pack(task).await?;

    if format == OutputFormat::Json {
        println!("{}", pack.render_json());
    } else {
        print!("{}", pack.render_markdown());
    }

    Ok(())
}

pub(crate) async fn compile_context_pack(task: &str) -> Result<ContextPack, String> {
    let store = Store::open_current();
    let memories = store.recall(task, 5).await?;
    let sessions = store.recent_session_facts(task, 5).await?;
    let file_candidates = discovery::discover_candidate_files(Path::new("."), task, 12)?;
    indexer::index_candidates(&store, Path::new("."), &file_candidates).await?;
    let symbols = store.recall_symbols(task, 8).await?;
    let files = file_candidates
        .into_iter()
        .map(|candidate| candidate.path)
        .collect::<Vec<_>>();
    let affected_tests = store.likely_tests_for_files(&files, 5).await?;
    let branch_state = worktree::inspect(Path::new("."));
    Ok(ContextPack::with_sessions_symbols_tests_and_branch(
        task,
        files,
        memories,
        sessions,
        symbols,
        affected_tests,
        Some(branch_state),
    ))
}

async fn index() -> Result<(), String> {
    let summary = indexer::index_project(5000).await?;

    println!("indexed {} files", summary.file_count);
    println!("indexed {} symbols", summary.symbol_count);
    Ok(())
}

async fn impact(target: &str, format: OutputFormat) -> Result<(), String> {
    indexer::index_project(5000).await?;
    let store = Store::open_current();
    let report = impact_analysis::analyze(&store, target, 50).await?;

    if format == OutputFormat::Json {
        println!("{}", report.render_json());
    } else {
        print!("{}", report.render_markdown());
    }

    Ok(())
}

async fn project_status() -> Result<(), String> {
    let store = Store::open_current();
    let project = store.sync_current_project().await?;

    println!("Hugr project");
    println!("  id: {}", project.id);
    println!("  name: {}", project.name);
    println!("  root: {}", project.root_path);
    println!("  storage: {}", store.storage_summary());
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

async fn session_start(task: &str) -> Result<(), String> {
    let session = Store::open_current().start_session(task).await?;

    println!("started session {}", session.id);
    println!("  task: {}", session.task);
    println!(
        "  branch: {}",
        session.branch.as_deref().unwrap_or("unknown")
    );
    Ok(())
}

async fn session_event(kind: &str, detail: &str) -> Result<(), String> {
    let event = Store::open_current()
        .record_session_event(kind, detail)
        .await?;

    println!("recorded event {}", event.id);
    println!("  session: {}", event.session_id);
    println!("  kind: {}", event.kind);
    println!("  detail: {}", event.detail);
    Ok(())
}

async fn session_end(summary: Option<&str>) -> Result<(), String> {
    let session = Store::open_current().end_session(summary).await?;

    println!("ended session {}", session.id);
    if let Some(summary) = session.final_summary {
        println!("  summary: {summary}");
    }
    Ok(())
}

async fn sync_status(format: OutputFormat) -> Result<(), String> {
    let plan = Store::open_current().sync_execution_plan()?;

    if format == OutputFormat::Json {
        println!("{}", render_sync_status_json(&plan));
    } else {
        print!("{}", render_sync_status_text(&plan));
    }

    Ok(())
}

async fn sync_push(dry_run: bool, format: OutputFormat) -> Result<(), String> {
    let result = Store::open_current().sync_push(dry_run).await?;

    if format == OutputFormat::Json {
        println!("{}", render_sync_push_json(&result));
    } else {
        print!("{}", render_sync_push_text(&result));
    }

    Ok(())
}

async fn sync_pull(dry_run: bool, format: OutputFormat) -> Result<(), String> {
    let result = Store::open_current().sync_pull(dry_run).await?;

    if format == OutputFormat::Json {
        println!("{}", render_sync_pull_json(&result));
    } else {
        print!("{}", render_sync_pull_text(&result));
    }

    Ok(())
}

async fn sync_history(format: OutputFormat) -> Result<(), String> {
    let history = Store::open_current().sync_history(10).await?;

    if format == OutputFormat::Json {
        println!("{}", render_sync_history_json(&history));
    } else {
        print!("{}", render_sync_history_text(&history));
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
    println!("  storage: {}", store.storage_summary());
    println!("  memories_readable: {}", store.memories().await.is_ok());
    println!(
        "  embedding_provider: {}",
        store.embedding_provider_summary()
    );
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

fn render_sync_status_text(plan: &SyncExecutionPlan) -> String {
    let sync_classes = if plan.sync_classes.is_empty() {
        "none".to_string()
    } else {
        plan.sync_classes.join(",")
    };
    let explicit_opt_in_classes = if plan.explicit_opt_in_classes.is_empty() {
        "none".to_string()
    } else {
        plan.explicit_opt_in_classes.join(",")
    };

    format!(
        "Hugr sync\n  storage_mode: {}\n  backend: {}\n  status: {}\n  local_writes_enabled: {}\n  remote_configured: {}\n  remote_auth_configured: {}\n  remote_reads_enabled: {}\n  remote_writes_enabled: {}\n  sync_classes: {}\n  explicit_opt_in_classes: {}\n",
        plan.storage_mode,
        plan.backend,
        plan.status,
        plan.local_writes_enabled,
        plan.remote_configured,
        plan.remote_auth_configured,
        plan.remote_reads_enabled,
        plan.remote_writes_enabled,
        sync_classes,
        explicit_opt_in_classes
    )
}

fn render_sync_status_json(plan: &SyncExecutionPlan) -> String {
    format!(
        "{{\"storage_mode\":{},\"backend\":{},\"status\":{},\"local_writes_enabled\":{},\"remote_configured\":{},\"remote_auth_configured\":{},\"remote_reads_enabled\":{},\"remote_writes_enabled\":{},\"sync_classes\":{},\"explicit_opt_in_classes\":{}}}",
        json_string(&plan.storage_mode),
        json_string(&plan.backend),
        json_string(&plan.status),
        plan.local_writes_enabled,
        plan.remote_configured,
        plan.remote_auth_configured,
        plan.remote_reads_enabled,
        plan.remote_writes_enabled,
        render_string_array_json(&plan.sync_classes),
        render_string_array_json(&plan.explicit_opt_in_classes)
    )
}

fn render_string_array_json(values: &[String]) -> String {
    let mut rendered = String::from("[");
    for (index, value) in values.iter().enumerate() {
        if index > 0 {
            rendered.push(',');
        }
        rendered.push_str(&json_string(value));
    }
    rendered.push(']');
    rendered
}

fn render_run_id_json(run_id: &Option<String>) -> String {
    run_id
        .as_ref()
        .map(|id| json_string(id))
        .unwrap_or_else(|| "null".to_string())
}

fn render_sync_table_text(rendered: &mut String, table: &SyncTableResult, action: &str) {
    let _ = writeln!(
        rendered,
        "- {}.{}: {} rows ({}), inserted {}, updated {}, skipped {}, conflicts {}",
        table.class,
        table.table,
        table.row_count,
        action,
        table.inserted_count,
        table.updated_count,
        table.skipped_count,
        table.conflict_count
    );
    for conflict in &table.conflicts {
        let _ = writeln!(
            rendered,
            "  conflict {}: {}",
            conflict.reason, conflict.count
        );
    }
}

fn render_sync_table_json(table: &SyncTableResult) -> String {
    format!(
        "{{\"class\":{},\"table\":{},\"row_count\":{},\"inserted_count\":{},\"updated_count\":{},\"skipped_count\":{},\"conflict_count\":{},\"executed\":{},\"conflicts\":{}}}",
        json_string(&table.class),
        json_string(&table.table),
        table.row_count,
        table.inserted_count,
        table.updated_count,
        table.skipped_count,
        table.conflict_count,
        table.executed,
        render_sync_conflicts_json(&table.conflicts)
    )
}

fn render_sync_conflicts_json(conflicts: &[SyncConflictSummary]) -> String {
    let mut rendered = String::from("[");
    for (index, conflict) in conflicts.iter().enumerate() {
        if index > 0 {
            rendered.push(',');
        }
        let _ = write!(
            rendered,
            "{{\"reason\":{},\"count\":{}}}",
            json_string(&conflict.reason),
            conflict.count
        );
    }
    rendered.push(']');
    rendered
}

fn render_sync_tables_json(tables: &[SyncTableResult]) -> String {
    let mut rendered = String::from("[");
    for (index, table) in tables.iter().enumerate() {
        if index > 0 {
            rendered.push(',');
        }
        rendered.push_str(&render_sync_table_json(table));
    }
    rendered.push(']');
    rendered
}

fn render_sync_push_text(result: &SyncPushResult) -> String {
    let mut rendered = format!(
        "Hugr sync push\n  mode: {}\n  backend: {}\n  status: {}\n",
        if result.dry_run { "dry-run" } else { "execute" },
        result.backend,
        result.status
    );
    if let Some(run_id) = &result.run_id {
        let _ = writeln!(rendered, "  run_id: {run_id}");
    }

    for table in &result.tables {
        render_sync_table_text(
            &mut rendered,
            table,
            if table.executed { "pushed" } else { "planned" },
        );
    }

    rendered
}

fn render_sync_push_json(result: &SyncPushResult) -> String {
    format!(
        "{{\"run_id\":{},\"dry_run\":{},\"backend\":{},\"status\":{},\"tables\":{}}}",
        render_run_id_json(&result.run_id),
        result.dry_run,
        json_string(&result.backend),
        json_string(&result.status),
        render_sync_tables_json(&result.tables)
    )
}

fn render_sync_pull_text(result: &SyncPullResult) -> String {
    let mut rendered = format!(
        "Hugr sync pull\n  mode: {}\n  backend: {}\n  status: {}\n",
        if result.dry_run { "dry-run" } else { "execute" },
        result.backend,
        result.status
    );
    if let Some(run_id) = &result.run_id {
        let _ = writeln!(rendered, "  run_id: {run_id}");
    }

    for table in &result.tables {
        render_sync_table_text(
            &mut rendered,
            table,
            if table.executed { "pulled" } else { "planned" },
        );
    }

    rendered
}

fn render_sync_pull_json(result: &SyncPullResult) -> String {
    format!(
        "{{\"run_id\":{},\"dry_run\":{},\"backend\":{},\"status\":{},\"tables\":{}}}",
        render_run_id_json(&result.run_id),
        result.dry_run,
        json_string(&result.backend),
        json_string(&result.status),
        render_sync_tables_json(&result.tables)
    )
}

fn render_sync_history_text(history: &[SyncRunHistory]) -> String {
    let mut rendered = String::from("Hugr sync history\n");
    if history.is_empty() {
        rendered.push_str("  runs: 0\n");
        return rendered;
    }

    for run in history {
        let _ = writeln!(
            rendered,
            "Run {}\n  operation: {}\n  backend: {}\n  status: {}\n  started_at_ms: {}\n  ended_at_ms: {}",
            run.id, run.operation, run.backend, run.status, run.started_at_ms, run.ended_at_ms
        );
        for table in &run.tables {
            render_sync_table_text(&mut rendered, table, "recorded");
        }
    }

    rendered
}

fn render_sync_history_json(history: &[SyncRunHistory]) -> String {
    let mut rendered = String::from("{\"runs\":[");
    for (index, run) in history.iter().enumerate() {
        if index > 0 {
            rendered.push(',');
        }
        let _ = write!(
            rendered,
            "{{\"id\":{},\"operation\":{},\"backend\":{},\"status\":{},\"started_at_ms\":{},\"ended_at_ms\":{},\"tables\":{}}}",
            json_string(&run.id),
            json_string(&run.operation),
            json_string(&run.backend),
            json_string(&run.status),
            run.started_at_ms,
            run.ended_at_ms,
            render_sync_tables_json(&run.tables)
        );
    }
    rendered.push_str("]}");
    rendered
}

#[cfg(test)]
mod tests {
    use super::{
        render_recall_json, render_sync_history_json, render_sync_history_text,
        render_sync_pull_json, render_sync_pull_text, render_sync_push_json, render_sync_push_text,
        render_sync_status_json, render_sync_status_text,
    };
    use crate::store::{
        Memory, SyncConflictSummary, SyncExecutionPlan, SyncPullResult, SyncPushResult,
        SyncRunHistory, SyncTableResult,
    };

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

    #[test]
    fn sync_status_renderers_include_execution_plan() {
        let plan = SyncExecutionPlan {
            storage_mode: "hybrid".to_string(),
            backend: "direct_libsql".to_string(),
            local_writes_enabled: true,
            remote_configured: true,
            remote_auth_configured: true,
            remote_reads_enabled: true,
            remote_writes_enabled: true,
            sync_classes: vec!["memories".to_string(), "full_source".to_string()],
            explicit_opt_in_classes: vec!["full_source".to_string()],
            status: "remote_sync_ready".to_string(),
        };

        let text = render_sync_status_text(&plan);
        assert!(text.contains("backend: direct_libsql"));
        assert!(text.contains("explicit_opt_in_classes: full_source"));

        let json = render_sync_status_json(&plan);
        assert!(json.contains("\"storage_mode\":\"hybrid\""));
        assert!(json.contains("\"sync_classes\":[\"memories\",\"full_source\"]"));
    }

    #[test]
    fn sync_push_renderers_include_table_counts() {
        let result = SyncPushResult {
            run_id: None,
            dry_run: true,
            backend: "direct_libsql".to_string(),
            status: "dry_run".to_string(),
            tables: vec![SyncTableResult {
                class: "memories".to_string(),
                table: "memories".to_string(),
                row_count: 2,
                inserted_count: 0,
                updated_count: 0,
                skipped_count: 0,
                conflict_count: 0,
                executed: false,
                conflicts: Vec::new(),
            }],
        };

        let text = render_sync_push_text(&result);
        assert!(text.contains("mode: dry-run"));
        assert!(text.contains("memories.memories: 2 rows"));

        let json = render_sync_push_json(&result);
        assert!(json.contains("\"dry_run\":true"));
        assert!(json.contains("\"row_count\":2"));
    }

    #[test]
    fn sync_pull_renderers_include_table_counts() {
        let result = SyncPullResult {
            run_id: Some("sync_pull_7".to_string()),
            dry_run: true,
            backend: "direct_libsql".to_string(),
            status: "dry_run".to_string(),
            tables: vec![SyncTableResult {
                class: "memories".to_string(),
                table: "memories".to_string(),
                row_count: 2,
                inserted_count: 1,
                updated_count: 0,
                skipped_count: 1,
                conflict_count: 1,
                executed: false,
                conflicts: vec![SyncConflictSummary {
                    reason: "local_row_preserved".to_string(),
                    count: 1,
                }],
            }],
        };

        let text = render_sync_pull_text(&result);
        assert!(text.contains("mode: dry-run"));
        assert!(text.contains("memories.memories: 2 rows"));
        assert!(text.contains("conflict local_row_preserved: 1"));

        let json = render_sync_pull_json(&result);
        assert!(json.contains("\"run_id\":\"sync_pull_7\""));
        assert!(json.contains("\"dry_run\":true"));
        assert!(json.contains("\"row_count\":2"));
        assert!(json.contains("\"skipped_count\":1"));
    }

    #[test]
    fn sync_history_renderers_include_conflicts() {
        let history = vec![SyncRunHistory {
            id: "sync_pull_9".to_string(),
            operation: "pull".to_string(),
            backend: "direct_libsql".to_string(),
            status: "executed".to_string(),
            started_at_ms: 8,
            ended_at_ms: 9,
            tables: vec![SyncTableResult {
                class: "memories".to_string(),
                table: "memories".to_string(),
                row_count: 2,
                inserted_count: 1,
                updated_count: 0,
                skipped_count: 1,
                conflict_count: 1,
                executed: true,
                conflicts: vec![SyncConflictSummary {
                    reason: "local_row_preserved".to_string(),
                    count: 1,
                }],
            }],
        }];

        let text = render_sync_history_text(&history);
        assert!(text.contains("Run sync_pull_9"));
        assert!(text.contains("conflict local_row_preserved: 1"));

        let json = render_sync_history_json(&history);
        assert!(json.contains("\"operation\":\"pull\""));
        assert!(json.contains("\"conflict_count\":1"));
    }
}
