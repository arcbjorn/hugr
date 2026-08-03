use crate::cli::{Command, MemoryWriteArgs, OutputFormat, help_text};
use crate::code::CodeSymbol;
use crate::context::ContextPack;
use crate::daemon;
use crate::discovery;
use crate::edit;
use crate::error::{Error, Result};
use crate::eval;
use crate::impact as impact_analysis;
use crate::indexer;
use crate::install;
use crate::json;
use crate::llm;
use crate::mcp;
use crate::store::{
    DiagnosticInput, DuplicateMemoryGroup, ForgetResult, Memory, MemoryConsolidationResult,
    MemoryMaintenanceReport, MemorySource, MemoryWriteOptions, SessionFact, SessionPromotionResult,
    SessionSynthesis, StaleMemoryCandidate, StaleRetirementResult, Store, SyncExecutionPlan,
    SyncPullResult, SyncPushResult, SyncRunHistory, SyncTableResult,
};
use crate::worktree;
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::fmt::Write;
use std::io::{self, Write as IoWrite};
use std::path::Path;
use std::process::Command as ProcessCommand;

pub(crate) async fn execute(command: Command) -> Result<()> {
    match command {
        Command::Init => init().await,
        Command::Status => status().await,
        Command::Remember {
            text,
            options,
            global,
        } => remember(&text, &options, global).await,
        Command::Recall {
            query,
            format,
            global,
        } => recall(&query, format, global).await,
        Command::Context {
            task,
            format,
            budget,
        } => context(&task, format, budget).await,
        Command::Index { paths } => index(&paths).await,
        Command::Symbols { query, format } => symbols(&query, format).await,
        Command::Impact { target, format } => impact(&target, format).await,
        Command::ReplaceSymbol {
            path,
            name,
            kind,
            body,
            format,
        } => replace_symbol(&path, &name, kind.as_deref(), &body, format).await,
        Command::RenameSymbol {
            path,
            name,
            new_name,
            kind,
            format,
        } => rename_symbol(&path, &name, &new_name, kind.as_deref(), format).await,
        Command::MoveSymbol {
            source_path,
            name,
            destination_path,
            kind,
            rewrite_references,
            format,
        } => {
            move_symbol(
                &source_path,
                &name,
                &destination_path,
                kind.as_deref(),
                rewrite_references,
                format,
            )
            .await
        }
        Command::ProjectStatus => project_status().await,
        Command::SessionStart { task } => session_start(&task).await,
        Command::SessionEvent { kind, detail } => session_event(&kind, &detail).await,
        Command::SessionEnd { summary } => session_end(summary.as_deref()).await,
        Command::SessionPromote { format, llm } => session_promote(format, llm).await,
        Command::SyncStatus { format } => sync_status(format).await,
        Command::SyncPush { dry_run, format } => sync_push(dry_run, format).await,
        Command::SyncPull { dry_run, format } => sync_pull(dry_run, format).await,
        Command::SyncHistory { format } => sync_history(format).await,
        Command::Mcp => mcp::serve_stdio().await,
        Command::Daemon { addr } => daemon::serve(daemon::DaemonConfig { addr }).await,
        Command::Run { command } => run_observed_command(&command).await,
        Command::Observe { status, command } => observe_shell_command(status, &command).await,
        Command::ShellHook { shell } => shell_hook(&shell),
        Command::Improve {
            execute,
            duplicates,
            stale,
            format,
        } => improve(execute, duplicates, stale, format).await,
        Command::Forget {
            query,
            format,
            global,
        } => forget(&query, format, global).await,
        Command::Eval {
            from_git,
            max_files,
            min_hit_rate,
            format,
        } => {
            let min_hit_rate = min_hit_rate
                .as_deref()
                .map(parse_min_hit_rate)
                .transpose()?;
            eval::run(eval::EvalOptions {
                from_git,
                max_files,
                min_hit_rate,
                format,
            })
            .await
        }
        Command::Install { agent, shared } => install::install(&agent, shared),
        Command::Hook { agent, event } => install::hook(&agent, &event).await,
        Command::Doctor => doctor().await,
        Command::Help => {
            print!("{}", help_text());
            Ok(())
        }
    }
}

async fn init() -> Result<()> {
    let store = Store::open_current();
    store.init().await?;
    println!("initialized Hugr at {}", store.root().display());
    Ok(())
}

async fn status() -> Result<()> {
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

async fn remember(text: &str, options: &MemoryWriteArgs, global: bool) -> Result<()> {
    let store = store_for_scope(global)?;
    let write_options = memory_write_options_from_args(options)?;
    let memory = if write_options == MemoryWriteOptions::default() && !global {
        store.remember(text).await?
    } else {
        store.remember_with_options(text, write_options).await?
    };
    if global {
        println!("remembered {} (global)", memory.id);
    } else {
        println!("remembered {}", memory.id);
    }
    Ok(())
}

fn store_for_scope(global: bool) -> Result<Store> {
    if global {
        Store::open_global()
    } else {
        Ok(Store::open_current())
    }
}

fn memory_write_options_from_args(args: &MemoryWriteArgs) -> Result<MemoryWriteOptions> {
    Ok(MemoryWriteOptions {
        source: args.source.as_ref().map(|source| MemorySource {
            kind: source.kind.clone(),
            locator: source.locator.clone(),
        }),
        confidence: args
            .confidence
            .as_deref()
            .map(parse_memory_confidence)
            .transpose()?,
        sensitivity: args.sensitivity.clone(),
        valid_from: args.valid_from.clone(),
        valid_to: args.valid_to.clone(),
    })
}

fn parse_min_hit_rate(value: &str) -> Result<f64> {
    value
        .parse::<f64>()
        .ok()
        .filter(|parsed| (0.0..=1.0).contains(parsed))
        .ok_or_else(|| {
            Error::msg(format!(
                "--min-hit-rate must be a number between 0 and 1, got '{value}'"
            ))
        })
}

fn parse_memory_confidence(value: &str) -> Result<f64> {
    value
        .parse::<f64>()
        .map_err(|_| Error::msg("memory confidence must be a number"))
}

async fn recall(query: &str, format: OutputFormat, global: bool) -> Result<()> {
    let matches = store_for_scope(global)?.recall(query, 10).await?;

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

async fn context(task: &str, format: OutputFormat, budget: Option<usize>) -> Result<()> {
    let pack = compile_context_pack_with_file_candidates(task, budget)
        .await?
        .0;

    if format == OutputFormat::Json {
        println!("{}", pack.render_json());
    } else {
        print!("{}", pack.render_markdown());
    }

    Ok(())
}

const MIN_CONTEXT_TOKEN_BUDGET: usize = 500;

/// Budgets come from the explicit flag/tool argument first, then the
/// `HUGR_CONTEXT_TOKEN_BUDGET` environment variable, then the default. Budgets
/// below the minimum would trim every section and produce useless packs, so
/// they are rejected rather than clamped silently.
pub(crate) fn resolve_context_token_budget(
    explicit: Option<usize>,
    env_lookup: impl Fn(&str) -> Option<String>,
) -> Result<usize> {
    let budget = match explicit {
        Some(value) => value,
        None => match env_lookup("HUGR_CONTEXT_TOKEN_BUDGET") {
            Some(value) => value.trim().parse::<usize>().map_err(|_| {
                format!("HUGR_CONTEXT_TOKEN_BUDGET must be a positive integer, got '{value}'")
            })?,
            None => crate::context::DEFAULT_CONTEXT_TOKEN_BUDGET,
        },
    };
    if budget < MIN_CONTEXT_TOKEN_BUDGET {
        return Err(Error::msg(format!(
            "context token budget must be at least {MIN_CONTEXT_TOKEN_BUDGET}, got {budget}"
        )));
    }
    Ok(budget)
}

/// Compiles a context pack and also returns the pre-budget relevant-file
/// candidate paths, so callers like `hugr eval` can attribute misses to
/// retrieval versus budget trimming.
pub(crate) async fn compile_context_pack_with_file_candidates(
    task: &str,
    budget: Option<usize>,
) -> Result<(ContextPack, Vec<String>)> {
    let token_budget = resolve_context_token_budget(budget, |name| std::env::var(name).ok())?;
    let store = Store::open_current();
    let mut memories = store.recall(task, 5).await?;
    // User-level memories join project recall best-effort: a missing HOME or
    // absent global store must never fail project context compilation.
    if let Ok(global_store) = Store::open_global()
        && let Ok(global_memories) = global_store.recall(task, 3).await
    {
        let seen = memories
            .iter()
            .map(|memory| memory.id.clone())
            .collect::<HashSet<_>>();
        memories.extend(
            global_memories
                .into_iter()
                .filter(|memory| !seen.contains(&memory.id)),
        );
    }
    let relevant_memory_ids = memories
        .iter()
        .map(|memory| memory.id.clone())
        .collect::<HashSet<_>>();
    let stale_candidates = store
        .memory_maintenance_report()
        .await?
        .stale_candidates
        .into_iter()
        .filter(|candidate| {
            relevant_memory_ids.contains(&candidate.newer_memory.id)
                || relevant_memory_ids.contains(&candidate.older_memory.id)
        })
        .collect::<Vec<_>>();
    let sessions = store.recent_session_facts(task, 5).await?;
    let file_candidates = discovery::merge_file_candidates(
        discovery::discover_candidate_files(Path::new("."), task, 12)?,
        store.source_embedding_file_candidates(task, 12).await?,
        12,
    );
    indexer::index_candidates(&store, Path::new("."), &file_candidates).await?;
    let symbols = store.recall_symbols(task, 8).await?;
    let files = file_candidates
        .iter()
        .map(|candidate| candidate.path.clone())
        .collect::<Vec<_>>();
    let affected_tests = store.likely_tests_for_files(&files, 5).await?;
    let graph_neighbors = store
        .context_graph_neighbors(task, &files, &symbols, 12)
        .await?;
    let freshness_signals = store
        .context_freshness_signals(&files, &symbols, 12)
        .await?;
    let diagnostics = store.recent_diagnostics(task, &files, &symbols, 8).await?;
    let branch_state = worktree::inspect(Path::new("."));
    let pack = ContextPack::with_inputs_and_budget(
        task,
        file_candidates,
        memories,
        sessions,
        symbols,
        affected_tests,
        Some(branch_state),
        stale_candidates,
        graph_neighbors,
        freshness_signals,
        diagnostics,
        token_budget,
    );
    store
        .record_context_pack(&pack.task, &pack.render_json())
        .await?;
    Ok((pack, files))
}

async fn index(paths: &[String]) -> Result<()> {
    if !paths.is_empty() {
        return index_incremental(paths).await;
    }

    let summary = indexer::index_project(5000).await?;

    println!("indexed {} files", summary.file_count);
    println!("indexed {} symbols", summary.symbol_count);
    println!(
        "file_roles: {}",
        indexer::format_classifications(&summary.file_roles)
    );
    println!(
        "languages: {}",
        indexer::format_classifications(&summary.languages)
    );
    println!(
        "symbol_kinds: {}",
        indexer::format_classifications(&summary.symbol_kinds)
    );
    if !summary.pruned.is_empty() {
        println!(
            "pruned: {} missing files, {} symbols, {} references",
            summary.pruned.missing_paths, summary.pruned.symbols, summary.pruned.references
        );
    }
    Ok(())
}

async fn index_incremental(paths: &[String]) -> Result<()> {
    let summary = indexer::refresh_paths(5000, paths).await?;

    println!("reparsed {} files", summary.reparsed_files);
    println!("rescanned {} reference files", summary.reference_files);
    println!("indexed {} symbols", summary.symbol_count);
    if !summary.pruned.is_empty() {
        println!(
            "pruned: {} missing files, {} symbols, {} references",
            summary.pruned.missing_paths, summary.pruned.symbols, summary.pruned.references
        );
    }
    Ok(())
}

async fn symbols(query: &str, format: OutputFormat) -> Result<()> {
    indexer::index_project(5000).await?;
    let store = Store::open_current();
    let symbols = store.symbols_for_target(query, 25).await?;

    if format == OutputFormat::Json {
        println!("{}", render_symbols_json(query, &symbols));
    } else {
        print!("{}", render_symbols_text(query, &symbols));
    }

    Ok(())
}

async fn impact(target: &str, format: OutputFormat) -> Result<()> {
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

/// Writes every planned file, or leaves the working tree untouched.
///
/// `plan_rename` and `plan_move` already refuse to emit a refactor that does
/// not parse, but writing the results in a plain loop reintroduced exactly the
/// partial state those checks exist to prevent: a failure on the third file
/// left the first two rewritten and the rest stale, so the tree no longer
/// compiled even though the command reported an error and the user reasonably
/// assumed nothing had changed.
///
/// Staging every file next to its destination first turns the common failures
/// — an unwritable file, a full disk, a read-only checkout — into errors that
/// happen before any destination is touched. The commit step is then a
/// sequence of same-directory renames, which is atomic per file on POSIX and
/// the closest thing to an all-or-nothing multi-file write without a journal.
/// A failure during that final step is reported with the files already
/// swapped, since there is no way to unwind it reliably.
fn write_planned_files(action: &str, files: &[(&str, &str)]) -> Result<()> {
    let mut staged = Vec::with_capacity(files.len());

    // Stage first: this is where permission and space errors surface, while
    // every destination still holds its original contents.
    for (path, contents) in files {
        let staged_path = staging_path(Path::new(path));
        if let Err(error) = std::fs::write(&staged_path, contents) {
            for (temporary, _) in &staged {
                let _: std::io::Result<()> = std::fs::remove_file(temporary);
            }
            return Err(Error::with_source(
                format!("hugr {action} cannot write {path}: {error}"),
                error,
            ));
        }
        staged.push((staged_path, *path));
    }

    for (staged_path, path) in &staged {
        std::fs::rename(staged_path, path).map_err(|error| {
            Error::with_source(
                format!("hugr {action} cannot replace {path}: {error}"),
                error,
            )
        })?;
    }

    Ok(())
}

/// A sibling of `path`, so the staged file lands on the same filesystem and
/// the commit step is a rename rather than a cross-device copy.
///
/// The suffix is [`crate::discovery::STAGING_SUFFIX`], which the file walker
/// and the daemon's watcher both skip: a crash between staging and committing
/// would otherwise leave a file the next index run treats as real source.
fn staging_path(path: &Path) -> std::path::PathBuf {
    let file_name = path.file_name().map_or_else(
        || String::from("hugr"),
        |name| name.to_string_lossy().into(),
    );
    path.with_file_name(format!(".{file_name}{}", crate::discovery::STAGING_SUFFIX))
}

async fn replace_symbol(
    path: &str,
    name: &str,
    kind: Option<&str>,
    body: &str,
    format: OutputFormat,
) -> Result<()> {
    let store = Store::open_current();
    if !store.supports_local_source_edits()? {
        return Err(Error::msg("hugr replace-symbol edits the local working tree and is not available in remote Hugr API mode"
                .to_string()));
    }

    let raw = std::fs::read_to_string(path).map_err(|error| {
        Error::with_source(
            format!("hugr replace-symbol cannot read {path}: {error}"),
            error,
        )
    })?;
    let line_ending = edit::LineEnding::detect(&raw);
    let contents = edit::normalize_line_endings(&raw);
    let planned = edit::plan_replacement(path, &contents, name, kind, body)?;

    let rendered = line_ending.apply(&planned.contents);
    write_planned_files("replace-symbol", &[(path, rendered.as_str())])?;

    // Refresh the index so symbols, impact, and context reflect the edit immediately.
    indexer::index_project(5000).await?;

    let summary = &planned.summary;
    let detail = format!(
        "replace-symbol {} {} at {}:{}-{} -> {}:{}-{}",
        summary.kind,
        summary.name,
        summary.path,
        summary.old_line_start,
        summary.old_line_end,
        summary.path,
        summary.new_line_start,
        summary.new_line_end
    );
    store
        .record_session_event_if_active("edit", &detail)
        .await?;

    if format == OutputFormat::Json {
        println!("{}", summary.render_json());
    } else {
        print!("{}", summary.render_markdown());
    }

    Ok(())
}

async fn rename_symbol(
    path: &str,
    name: &str,
    new_name: &str,
    kind: Option<&str>,
    format: OutputFormat,
) -> Result<()> {
    let store = Store::open_current();
    if !store.supports_local_source_edits()? {
        return Err(Error::msg("hugr rename-symbol edits the local working tree and is not available in remote Hugr API mode"
                .to_string()));
    }

    indexer::index_project(5000).await?;

    let contents = std::fs::read_to_string(path).map_err(|error| {
        Error::with_source(
            format!("hugr rename-symbol cannot read {path}: {error}"),
            error,
        )
    })?;
    let target = edit::resolve_symbol_in_source(path, &contents, name, kind, "rename")?;
    let references = store
        .references_to_symbols(std::slice::from_ref(&target), 2000)
        .await?;
    let mut paths = references
        .iter()
        .map(|reference| reference.path.clone())
        .chain(std::iter::once(target.path.clone()))
        .collect::<HashSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    paths.sort();

    let mut files = Vec::new();
    // Each file keeps its own ending: a repository can legitimately mix them,
    // and a rename must not rewrite endings in files it only touches in one
    // place.
    let mut line_endings = HashMap::new();
    for path in paths {
        let raw = std::fs::read_to_string(&path).map_err(|error| {
            Error::with_source(
                format!("hugr rename-symbol cannot read {path}: {error}"),
                error,
            )
        })?;
        line_endings.insert(path.clone(), edit::LineEnding::detect(&raw));
        files.push((path, edit::normalize_line_endings(&raw)));
    }

    let planned = edit::plan_rename(&target, &references, files, new_name)?;
    let rendered = planned
        .files
        .iter()
        .map(|file| {
            let ending = line_endings.get(&file.path).copied().unwrap_or_default();
            (file.path.as_str(), ending.apply(&file.contents))
        })
        .collect::<Vec<_>>();
    write_planned_files(
        "rename-symbol",
        &rendered
            .iter()
            .map(|(path, contents)| (*path, contents.as_str()))
            .collect::<Vec<_>>(),
    )?;

    indexer::index_project(5000).await?;

    let summary = &planned.summary;
    let changed_paths = summary
        .changed_files
        .iter()
        .map(|file| file.path.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let detail = format!(
        "rename-symbol {} {} -> {} at {}:{}-{}; changed files: {}",
        summary.kind,
        summary.old_name,
        summary.new_name,
        summary.target_path,
        summary.line_start,
        summary.line_end,
        changed_paths
    );
    store
        .record_session_event_if_active("edit", &detail)
        .await?;

    if format == OutputFormat::Json {
        println!("{}", summary.render_json());
    } else {
        print!("{}", summary.render_markdown());
    }

    Ok(())
}

async fn move_symbol(
    source_path: &str,
    name: &str,
    destination_path: &str,
    kind: Option<&str>,
    rewrite_references: bool,
    format: OutputFormat,
) -> Result<()> {
    let store = Store::open_current();
    if !store.supports_local_source_edits()? {
        return Err(Error::msg("hugr move-symbol edits the local working tree and is not available in remote Hugr API mode"
                .to_string()));
    }

    indexer::index_project(5000).await?;

    let raw_source = std::fs::read_to_string(source_path).map_err(|error| {
        Error::with_source(
            format!("hugr move-symbol cannot read {source_path}: {error}"),
            error,
        )
    })?;
    let mut line_endings = HashMap::new();
    line_endings.insert(
        source_path.to_string(),
        edit::LineEnding::detect(&raw_source),
    );
    let source_contents = edit::normalize_line_endings(&raw_source);
    let (destination_contents, destination_ending) =
        read_optional_destination(destination_path, "hugr move-symbol")?;
    line_endings.insert(destination_path.to_string(), destination_ending);
    let target = edit::resolve_symbol_in_source(source_path, &source_contents, name, kind, "move")?;
    let references = store
        .references_to_symbols(std::slice::from_ref(&target), 2000)
        .await?;
    let reference_files = if rewrite_references {
        read_reference_files(
            &references,
            source_path,
            destination_path,
            "hugr move-symbol",
        )?
        .into_iter()
        .map(|(path, contents, ending)| {
            line_endings.insert(path.clone(), ending);
            (path, contents)
        })
        .collect()
    } else {
        Vec::new()
    };
    let planned = edit::plan_move(
        &target,
        &references,
        &source_contents,
        destination_path,
        &destination_contents,
        reference_files,
        rewrite_references,
    )?;

    let rendered = planned
        .files
        .iter()
        .map(|file| {
            let ending = line_endings.get(&file.path).copied().unwrap_or_default();
            (file.path.as_str(), ending.apply(&file.contents))
        })
        .collect::<Vec<_>>();
    write_planned_files(
        "move-symbol",
        &rendered
            .iter()
            .map(|(path, contents)| (*path, contents.as_str()))
            .collect::<Vec<_>>(),
    )?;

    indexer::index_project(5000).await?;

    let summary = &planned.summary;
    let detail = format!(
        "move-symbol {} {} from {}:{}-{} to {}",
        summary.kind,
        summary.name,
        summary.source_path,
        summary.old_line_start,
        summary.old_line_end,
        summary.destination_path
    );
    store
        .record_session_event_if_active("edit", &detail)
        .await?;

    if format == OutputFormat::Json {
        println!("{}", summary.render_json());
    } else {
        print!("{}", summary.render_markdown());
    }

    Ok(())
}

/// Reads a move destination, which may not exist yet. Line endings are
/// normalised for the planner; a new file gets [`LineEnding::Lf`] by default.
fn read_optional_destination(path: &str, command: &str) -> Result<(String, edit::LineEnding)> {
    match std::fs::read_to_string(path) {
        Ok(contents) => Ok((
            edit::normalize_line_endings(&contents),
            edit::LineEnding::detect(&contents),
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            Ok((String::new(), edit::LineEnding::default()))
        }
        Err(error) => Err(Error::msg(format!("{command} cannot read {path}: {error}"))),
    }
}

fn read_reference_files(
    references: &[crate::code::CodeReference],
    source_path: &str,
    destination_path: &str,
    command: &str,
) -> Result<Vec<(String, String, edit::LineEnding)>> {
    let mut paths = references
        .iter()
        .map(|reference| reference.path.clone())
        .filter(|path| path != source_path && path != destination_path)
        .collect::<HashSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    paths.sort();

    paths
        .into_iter()
        .map(|path| {
            std::fs::read_to_string(&path)
                .map(|contents| {
                    (
                        path.clone(),
                        edit::normalize_line_endings(&contents),
                        edit::LineEnding::detect(&contents),
                    )
                })
                .map_err(|error| {
                    Error::with_source(format!("{command} cannot read {path}: {error}"), error)
                })
        })
        .collect()
}

async fn project_status() -> Result<()> {
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

async fn session_start(task: &str) -> Result<()> {
    let session = Store::open_current().start_session(task).await?;

    println!("started session {}", session.id);
    println!("  task: {}", session.task);
    println!(
        "  branch: {}",
        session.branch.as_deref().unwrap_or("unknown")
    );
    Ok(())
}

async fn session_event(kind: &str, detail: &str) -> Result<()> {
    let event = Store::open_current()
        .record_session_event(kind, detail)
        .await?;

    println!("recorded event {}", event.id);
    println!("  session: {}", event.session_id);
    println!("  kind: {}", event.kind);
    println!("  detail: {}", event.detail);
    Ok(())
}

async fn session_end(summary: Option<&str>) -> Result<()> {
    let session = Store::open_current().end_session(summary).await?;

    println!("ended session {}", session.id);
    if let Some(summary) = session.final_summary {
        println!("  summary: {summary}");
    }
    Ok(())
}

async fn session_promote(format: OutputFormat, llm: bool) -> Result<()> {
    let store = Store::open_current();
    let result = if llm {
        let synthesizer = llm::ChatSynthesizer::from_env()?;
        let synthesize = |task: &str, facts: &[SessionFact]| -> Result<SessionSynthesis> {
            let lines = facts
                .iter()
                .map(|fact| format!("{}: {}", fact.kind, fact.detail))
                .collect::<Vec<_>>();
            let text = synthesizer.synthesize(task, &lines)?;
            Ok(SessionSynthesis {
                text,
                provider: synthesizer.provider().to_string(),
                model: synthesizer.model().to_string(),
            })
        };
        store
            .promote_latest_session_with_synthesis(Some(&synthesize))
            .await?
    } else {
        store.promote_latest_session().await?
    };

    if format == OutputFormat::Json {
        println!("{}", render_session_promotion_json(&result));
    } else {
        print!("{}", render_session_promotion_text(&result));
    }

    Ok(())
}

async fn run_observed_command(command: &[String]) -> Result<()> {
    let Some(program) = command.first() else {
        return Err(Error::msg("hugr run requires a command"));
    };

    let output = ProcessCommand::new(program)
        .args(command.iter().skip(1))
        .output()
        .map_err(|error| {
            Error::with_source(
                format!("failed to run '{}': {error}", command.join(" ")),
                error,
            )
        })?;

    io::stdout().write_all(&output.stdout)?;
    io::stderr().write_all(&output.stderr)?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let detail = command_observation_detail(command, output.status.code(), &stdout, &stderr);
    let store = Store::open_current();
    let _ = store
        .record_session_event_if_active("command", &detail)
        .await?;
    let diagnostics = command_diagnostics(command, &stdout, &stderr);
    if !diagnostics.is_empty() {
        let _ = store.record_diagnostics(&diagnostics).await?;
    }

    if output.status.success() {
        Ok(())
    } else {
        Err(Error::msg(format!(
            "observed command exited with status {}",
            output
                .status
                .code()
                .map_or_else(|| "signal".to_string(), |code| code.to_string())
        )))
    }
}

async fn observe_shell_command(status: i32, command: &[String]) -> Result<()> {
    if command.is_empty() {
        return Err(Error::msg(
            "hugr observe command requires a command".to_string(),
        ));
    }

    let detail = command_observation_detail(command, Some(status), "", "");
    let _ = Store::open_current()
        .record_session_event_if_active("command", &detail)
        .await?;
    Ok(())
}

fn shell_hook(shell: &str) -> Result<()> {
    print!("{}", shell_hook_text(shell)?);
    Ok(())
}

fn shell_hook_text(shell: &str) -> Result<&'static str> {
    match shell {
        "zsh" => Ok(ZSH_SHELL_HOOK),
        "bash" => Ok(BASH_SHELL_HOOK),
        _ => Err(Error::msg(
            "hugr shell-hook supports bash or zsh".to_string(),
        )),
    }
}

const ZSH_SHELL_HOOK: &str = r#"# Hugr shell observation hook for zsh.
# Source with: eval "$(hugr shell-hook zsh)"
if [[ -z "${HUGR_SHELL_HOOK_LOADED:-}" ]]; then
  HUGR_SHELL_HOOK_LOADED=1
  autoload -Uz add-zsh-hook
  _hugr_preexec() {
    HUGR_LAST_COMMAND="$1"
  }
  _hugr_precmd() {
    # `status` is a read-only alias for `$?` in zsh, so `local status=$?`
    # aborts the function with "read-only variable: status" on every prompt
    # and the observe call below never runs.
    local hugr_status=$?
    if [[ -n "${HUGR_LAST_COMMAND:-}" ]]; then
      command "${HUGR_BIN:-hugr}" observe command --status "$hugr_status" -- "$HUGR_LAST_COMMAND" >/dev/null 2>&1 || true
      unset HUGR_LAST_COMMAND
    fi
  }
  add-zsh-hook preexec _hugr_preexec
  add-zsh-hook precmd _hugr_precmd
fi
"#;

const BASH_SHELL_HOOK: &str = r#"# Hugr shell observation hook for bash.
# Source with: eval "$(hugr shell-hook bash)"
if [[ -z "${HUGR_SHELL_HOOK_LOADED:-}" ]]; then
  HUGR_SHELL_HOOK_LOADED=1
  _hugr_debug_trap() {
    local command_line="$BASH_COMMAND"
    case "$command_line" in
      _hugr_*|*"hugr observe command"*) return ;;
    esac
    HUGR_LAST_COMMAND="$command_line"
  }
  _hugr_prompt_command() {
    # `status` is not special in bash, but the zsh hook cannot use that name
    # and keeping both the same avoids reintroducing the difference.
    local hugr_status=$?
    if [[ -n "${HUGR_LAST_COMMAND:-}" ]]; then
      command "${HUGR_BIN:-hugr}" observe command --status "$hugr_status" -- "$HUGR_LAST_COMMAND" >/dev/null 2>&1 || true
      unset HUGR_LAST_COMMAND
    fi
  }
  trap _hugr_debug_trap DEBUG
  PROMPT_COMMAND="_hugr_prompt_command${PROMPT_COMMAND:+;$PROMPT_COMMAND}"
fi
"#;

fn command_observation_detail(
    command: &[String],
    status_code: Option<i32>,
    stdout: &str,
    stderr: &str,
) -> String {
    let mut detail = format!(
        "command: {}; status: {}",
        command.join(" "),
        status_code.map_or_else(|| "signal".to_string(), |code| code.to_string())
    );
    if let Some(stdout_tail) = output_tail(stdout) {
        let _ = write!(detail, "; stdout_tail: {stdout_tail}");
    }
    if let Some(stderr_tail) = output_tail(stderr) {
        let _ = write!(detail, "; stderr_tail: {stderr_tail}");
    }
    detail
}

fn output_tail(output: &str) -> Option<String> {
    let output = output.trim();
    if output.is_empty() {
        return None;
    }

    let mut chars = output.chars().rev().take(300).collect::<Vec<_>>();
    chars.reverse();
    Some(chars.into_iter().collect::<String>())
}

fn command_diagnostics(command: &[String], stdout: &str, stderr: &str) -> Vec<DiagnosticInput> {
    let command_text = command.join(" ");
    let mut diagnostics = parse_diagnostics_from_output("stdout", stdout, &command_text);
    diagnostics.extend(parse_diagnostics_from_output(
        "stderr",
        stderr,
        &command_text,
    ));
    diagnostics.truncate(20);
    diagnostics
}

fn parse_diagnostics_from_output(
    stream: &str,
    output: &str,
    command: &str,
) -> Vec<DiagnosticInput> {
    let lines = output.lines().collect::<Vec<_>>();
    let mut diagnostics = Vec::new();
    let mut pending_rust = None::<PendingRustDiagnostic>;

    for line in &lines {
        if let Some((severity, code, message)) = parse_rust_diagnostic_header(line) {
            if let Some(pending) = pending_rust.take() {
                diagnostics.push(pending.into_input(stream, command, None, None));
            }
            pending_rust = Some(PendingRustDiagnostic {
                severity,
                code,
                message,
            });
            continue;
        }

        if let Some((path, line_start)) = parse_rust_location(line) {
            if let Some(pending) = pending_rust.take() {
                diagnostics.push(pending.into_input(stream, command, Some(path), Some(line_start)));
            }
            continue;
        }

        if let Some(input) = parse_colon_diagnostic(line, stream, command) {
            if let Some(pending) = pending_rust.take() {
                diagnostics.push(pending.into_input(stream, command, None, None));
            }
            diagnostics.push(input);
        }
    }

    if let Some(pending) = pending_rust {
        diagnostics.push(pending.into_input(stream, command, None, None));
    }

    dedupe_diagnostics(diagnostics)
}

#[derive(Debug)]
struct PendingRustDiagnostic {
    severity: String,
    code: Option<String>,
    message: String,
}

impl PendingRustDiagnostic {
    fn into_input(
        self,
        stream: &str,
        command: &str,
        path: Option<String>,
        line_start: Option<i64>,
    ) -> DiagnosticInput {
        DiagnosticInput {
            source: format!("command_{stream}"),
            path,
            line_start,
            line_end: None,
            severity: self.severity,
            code: self.code,
            message: self.message,
            command: Some(command.to_string()),
        }
    }
}

fn parse_rust_diagnostic_header(line: &str) -> Option<(String, Option<String>, String)> {
    let trimmed = line.trim();
    let severity = if trimmed.starts_with("error") {
        "error"
    } else if trimmed.starts_with("warning") {
        "warning"
    } else {
        return None;
    };
    let colon = trimmed.find(':')?;
    let header = trimmed[..colon].trim();
    let code = header
        .find('[')
        .and_then(|start| header[start + 1..].find(']').map(|end| (start, end)))
        .map(|(start, end)| header[start + 1..start + 1 + end].to_string());
    let message = trimmed[colon + 1..].trim();
    (!message.is_empty()).then(|| (severity.to_string(), code, message.to_string()))
}

fn parse_rust_location(line: &str) -> Option<(String, i64)> {
    let trimmed = line.trim();
    let location = trimmed.strip_prefix("-->")?.trim();
    let mut parts = location.rsplitn(3, ':');
    let _column = parts.next()?;
    let line_start = parts.next()?.parse::<i64>().ok()?;
    let path = parts.next()?.trim();
    (!path.is_empty()).then(|| (path.to_string(), line_start))
}

fn parse_colon_diagnostic(line: &str, stream: &str, command: &str) -> Option<DiagnosticInput> {
    let trimmed = line.trim();
    let lower = trimmed.to_lowercase();
    let severity = if lower.contains(": error:") {
        "error"
    } else if lower.contains(": warning:") {
        "warning"
    } else {
        return None;
    };
    let marker = format!(": {severity}:");
    let marker_index = lower.find(&marker)?;
    let message = trimmed[marker_index + marker.len()..].trim();
    if message.is_empty() {
        return None;
    }

    let location = &trimmed[..marker_index];
    let mut parts = location.rsplitn(3, ':');
    let _column = parts.next()?;
    let line_start = parts.next()?.parse::<i64>().ok()?;
    let path = parts.next()?.trim();
    if path.is_empty() {
        return None;
    }

    Some(DiagnosticInput {
        source: format!("command_{stream}"),
        path: Some(path.to_string()),
        line_start: Some(line_start),
        line_end: None,
        severity: severity.to_string(),
        code: None,
        message: message.to_string(),
        command: Some(command.to_string()),
    })
}

fn dedupe_diagnostics(diagnostics: Vec<DiagnosticInput>) -> Vec<DiagnosticInput> {
    let mut seen = HashSet::new();
    diagnostics
        .into_iter()
        .filter(|diagnostic| {
            seen.insert(format!(
                "{}|{}|{}|{}|{}",
                diagnostic.source,
                diagnostic.path.as_deref().unwrap_or(""),
                diagnostic
                    .line_start
                    .map(|line| line.to_string())
                    .unwrap_or_default(),
                diagnostic.severity,
                diagnostic.message
            ))
        })
        .collect()
}

async fn sync_status(format: OutputFormat) -> Result<()> {
    let plan = Store::open_current().sync_execution_plan()?;

    if format == OutputFormat::Json {
        println!("{}", render_sync_status_json(&plan));
    } else {
        print!("{}", render_sync_status_text(&plan));
    }

    Ok(())
}

async fn sync_push(dry_run: bool, format: OutputFormat) -> Result<()> {
    let result = Store::open_current().sync_push(dry_run).await?;

    if format == OutputFormat::Json {
        println!("{}", render_sync_push_json(&result));
    } else {
        print!("{}", render_sync_push_text(&result));
    }

    Ok(())
}

async fn sync_pull(dry_run: bool, format: OutputFormat) -> Result<()> {
    let result = Store::open_current().sync_pull(dry_run).await?;

    if format == OutputFormat::Json {
        println!("{}", render_sync_pull_json(&result));
    } else {
        print!("{}", render_sync_pull_text(&result));
    }

    Ok(())
}

async fn sync_history(format: OutputFormat) -> Result<()> {
    let history = Store::open_current().sync_history(10).await?;

    if format == OutputFormat::Json {
        println!("{}", render_sync_history_json(&history));
    } else {
        print!("{}", render_sync_history_text(&history));
    }

    Ok(())
}

async fn improve(execute: bool, duplicates: bool, stale: bool, format: OutputFormat) -> Result<()> {
    if execute {
        if duplicates == stale {
            return Err(Error::msg(
                "hugr improve --execute requires exactly one of --duplicates or --stale"
                    .to_string(),
            ));
        }
        if duplicates {
            let result = Store::open_current()
                .consolidate_duplicate_memories()
                .await?;
            if format == OutputFormat::Json {
                println!("{}", render_consolidation_json(&result));
            } else {
                print!("{}", render_consolidation_text(&result));
            }
        } else {
            let result = Store::open_current().retire_stale_memories().await?;
            if format == OutputFormat::Json {
                println!("{}", render_stale_retirement_json(&result));
            } else {
                print!("{}", render_stale_retirement_text(&result));
            }
        }
        return Ok(());
    }

    let report = Store::open_current().memory_maintenance_report().await?;
    if format == OutputFormat::Json {
        println!("{}", render_improve_json(&report));
    } else {
        print!("{}", render_improve_text(&report));
    }

    Ok(())
}

async fn forget(query: &str, format: OutputFormat, global: bool) -> Result<()> {
    let result = store_for_scope(global)?.forget(query, 25).await?;

    if format == OutputFormat::Json {
        println!("{}", render_forget_json(&result));
    } else {
        print!("{}", render_forget_text(&result));
    }

    Ok(())
}

async fn doctor() -> Result<()> {
    let store = Store::open_current();
    println!("Hugr doctor");
    println!("  current_dir: {}", std::env::current_dir()?.display());
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

/// The `recall --json` envelope. A struct rather than `serde_json::json!`
/// because `Map` sorts its keys and this output puts `query` first.
#[derive(Serialize)]
struct RecallJson<'a> {
    query: &'a str,
    memories: &'a [Memory],
}

fn render_recall_json(query: &str, memories: &[Memory]) -> String {
    json::render(&RecallJson { query, memories })
}

fn render_symbols_text(query: &str, symbols: &[CodeSymbol]) -> String {
    let mut rendered = format!(
        "Hugr symbols\n  query: {query}\n  matches: {}\n",
        symbols.len()
    );

    for symbol in symbols {
        let language = symbol.language.as_deref().unwrap_or("unknown");
        let _ = writeln!(
            rendered,
            "- {} {} at {} [{}]: {}",
            symbol.kind,
            symbol.name,
            code_symbol_location(symbol),
            language,
            symbol.signature
        );
    }

    rendered
}

/// The `symbols --json` envelope; see [`RecallJson`] for why this is a
/// struct and not a `json!` map.
#[derive(Serialize)]
struct SymbolsJson<'a> {
    query: &'a str,
    symbols: &'a [CodeSymbol],
}

fn render_symbols_json(query: &str, symbols: &[CodeSymbol]) -> String {
    json::render(&SymbolsJson { query, symbols })
}

fn code_symbol_location(symbol: &CodeSymbol) -> String {
    match symbol.line_end {
        Some(line_end) if line_end > symbol.line_start => {
            format!("{}:{}-{}", symbol.path, symbol.line_start, line_end)
        }
        _ => format!("{}:{}", symbol.path, symbol.line_start),
    }
}

fn render_session_promotion_text(result: &SessionPromotionResult) -> String {
    format!(
        "Hugr session promotion\n  session: {}\n  task: {}\n  facts: {}\n  memory: {}\n",
        result.session_id, result.task, result.fact_count, result.memory.id
    )
}

fn render_session_promotion_json(result: &SessionPromotionResult) -> String {
    json::render(result)
}

fn render_forget_text(result: &ForgetResult) -> String {
    let mut rendered = format!(
        "Hugr forget\n  query: {}\n  forgotten: {}\n  forgotten_at: {}\n",
        result.query, result.forgotten_count, result.forgotten_at
    );

    for memory in &result.memories {
        let _ = writeln!(
            rendered,
            "- {} [{}]: {}",
            memory.id, memory.kind, memory.text
        );
    }

    rendered
}

fn render_forget_json(result: &ForgetResult) -> String {
    json::render(result)
}

fn render_improve_text(report: &MemoryMaintenanceReport) -> String {
    let mut rendered = format!(
        "Hugr improve\n  active_memories: {}\n  retired_memories: {}\n  duplicate_groups: {}\n  stale_candidates: {}\n",
        report.active_count,
        report.retired_count,
        report.duplicate_groups.len(),
        report.stale_candidates.len()
    );

    for group in &report.duplicate_groups {
        let _ = writeln!(
            rendered,
            "- duplicate: {} ({} memories)",
            group.normalized_text,
            group.memories.len()
        );
        for memory in &group.memories {
            let _ = writeln!(
                rendered,
                "  - {} [{}]: {}",
                memory.id, memory.kind, memory.text
            );
        }
    }

    for candidate in &report.stale_candidates {
        let _ = writeln!(
            rendered,
            "- stale_candidate: {} ({}) shared: {}",
            candidate.reason,
            candidate.signal,
            candidate.shared_terms.join(",")
        );
        let _ = writeln!(
            rendered,
            "  newer {} [{}]: {}",
            candidate.newer_memory.id, candidate.newer_memory.kind, candidate.newer_memory.text
        );
        let _ = writeln!(
            rendered,
            "  older {} [{}]: {}",
            candidate.older_memory.id, candidate.older_memory.kind, candidate.older_memory.text
        );
    }

    rendered
}

fn render_improve_json(report: &MemoryMaintenanceReport) -> String {
    json::render(report)
}

fn render_consolidation_text(result: &MemoryConsolidationResult) -> String {
    let mut rendered = format!(
        "Hugr improve\n  action: duplicates\n  executed_at: {}\n  duplicate_groups: {}\n  kept: {}\n  retired: {}\n",
        result.executed_at,
        result.duplicate_groups.len(),
        result.kept_memories.len(),
        result.retired_memories.len()
    );

    for memory in &result.kept_memories {
        let _ = writeln!(
            rendered,
            "- kept {} [{}]: {}",
            memory.id, memory.kind, memory.text
        );
    }
    for memory in &result.retired_memories {
        let _ = writeln!(
            rendered,
            "- retired {} [{}]: {}",
            memory.id, memory.kind, memory.text
        );
    }

    rendered
}

/// The `improve --duplicates --execute` envelope. `action` is a literal the
/// result struct does not carry, so the wire shape is spelled out here.
#[derive(Serialize)]
struct ConsolidationJson<'a> {
    action: &'static str,
    executed_at: &'a str,
    duplicate_groups: &'a [DuplicateMemoryGroup],
    kept_memories: &'a [Memory],
    retired_memories: &'a [Memory],
}

fn render_consolidation_json(result: &MemoryConsolidationResult) -> String {
    json::render(&ConsolidationJson {
        action: "duplicates",
        executed_at: &result.executed_at,
        duplicate_groups: &result.duplicate_groups,
        kept_memories: &result.kept_memories,
        retired_memories: &result.retired_memories,
    })
}

fn render_stale_retirement_text(result: &StaleRetirementResult) -> String {
    let mut rendered = format!(
        "Hugr improve\n  action: stale\n  executed_at: {}\n  stale_candidates: {}\n  kept: {}\n  retired: {}\n",
        result.executed_at,
        result.stale_candidates.len(),
        result.kept_memories.len(),
        result.retired_memories.len()
    );

    for memory in &result.kept_memories {
        let _ = writeln!(
            rendered,
            "- kept {} [{}]: {}",
            memory.id, memory.kind, memory.text
        );
    }
    for memory in &result.retired_memories {
        let _ = writeln!(
            rendered,
            "- retired {} [{}]: {}",
            memory.id, memory.kind, memory.text
        );
    }

    rendered
}

/// The `improve --stale --execute` envelope; see [`ConsolidationJson`].
#[derive(Serialize)]
struct StaleRetirementJson<'a> {
    action: &'static str,
    executed_at: &'a str,
    stale_candidates: &'a [StaleMemoryCandidate],
    kept_memories: &'a [Memory],
    retired_memories: &'a [Memory],
}

fn render_stale_retirement_json(result: &StaleRetirementResult) -> String {
    json::render(&StaleRetirementJson {
        action: "stale",
        executed_at: &result.executed_at,
        stale_candidates: &result.stale_candidates,
        kept_memories: &result.kept_memories,
        retired_memories: &result.retired_memories,
    })
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
    let remote_endpoint = plan.remote_endpoint.as_deref().unwrap_or("none");
    let api_contract_version = plan.api_contract_version.as_deref().unwrap_or("none");
    let api_routes = if plan.api_routes.is_empty() {
        "none".to_string()
    } else {
        plan.api_routes.join(",")
    };

    format!(
        "Hugr sync\n  storage_mode: {}\n  backend: {}\n  status: {}\n  local_writes_enabled: {}\n  remote_configured: {}\n  remote_auth_configured: {}\n  remote_reads_enabled: {}\n  remote_writes_enabled: {}\n  remote_endpoint: {}\n  api_contract_version: {}\n  api_routes: {}\n  sync_classes: {}\n  explicit_opt_in_classes: {}\n",
        plan.storage_mode,
        plan.backend,
        plan.status,
        plan.local_writes_enabled,
        plan.remote_configured,
        plan.remote_auth_configured,
        plan.remote_reads_enabled,
        plan.remote_writes_enabled,
        remote_endpoint,
        api_contract_version,
        api_routes,
        sync_classes,
        explicit_opt_in_classes
    )
}

/// The `sync status --json` shape. Written out rather than derived from
/// [`SyncExecutionPlan`] because the wire order puts `status` third while the
/// struct declares it last; deriving would silently move it.
#[derive(Serialize)]
struct SyncStatusJson<'a> {
    storage_mode: &'a str,
    backend: &'a str,
    status: &'a str,
    local_writes_enabled: bool,
    remote_configured: bool,
    remote_auth_configured: bool,
    remote_reads_enabled: bool,
    remote_writes_enabled: bool,
    remote_endpoint: Option<&'a str>,
    api_contract_version: Option<&'a str>,
    api_routes: &'a [String],
    sync_classes: &'a [String],
    explicit_opt_in_classes: &'a [String],
}

fn render_sync_status_json(plan: &SyncExecutionPlan) -> String {
    json::render(&SyncStatusJson {
        storage_mode: &plan.storage_mode,
        backend: &plan.backend,
        status: &plan.status,
        local_writes_enabled: plan.local_writes_enabled,
        remote_configured: plan.remote_configured,
        remote_auth_configured: plan.remote_auth_configured,
        remote_reads_enabled: plan.remote_reads_enabled,
        remote_writes_enabled: plan.remote_writes_enabled,
        remote_endpoint: plan.remote_endpoint.as_deref(),
        api_contract_version: plan.api_contract_version.as_deref(),
        api_routes: &plan.api_routes,
        sync_classes: &plan.sync_classes,
        explicit_opt_in_classes: &plan.explicit_opt_in_classes,
    })
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
    json::render(result)
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
    json::render(result)
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

/// The `sync history --json` envelope; a struct so `runs` stays the only
/// key and the field order is explicit.
#[derive(Serialize)]
struct SyncHistoryJson<'a> {
    runs: &'a [SyncRunHistory],
}

fn render_sync_history_json(history: &[SyncRunHistory]) -> String {
    json::render(&SyncHistoryJson { runs: history })
}

#[cfg(test)]
mod tests {
    use super::{
        command_diagnostics, command_observation_detail, output_tail, render_consolidation_json,
        render_consolidation_text, render_forget_json, render_forget_text, render_improve_json,
        render_improve_text, render_recall_json, render_session_promotion_json,
        render_session_promotion_text, render_stale_retirement_json, render_stale_retirement_text,
        render_symbols_json, render_symbols_text, render_sync_history_json,
        render_sync_history_text, render_sync_pull_json, render_sync_pull_text,
        render_sync_push_json, render_sync_push_text, render_sync_status_json,
        render_sync_status_text, resolve_context_token_budget, shell_hook_text,
        write_planned_files,
    };
    use crate::code::CodeSymbol;
    use crate::store::{
        DuplicateMemoryGroup, ForgetResult, Memory, MemoryConsolidationResult,
        MemoryMaintenanceReport, SessionPromotionResult, StaleMemoryCandidate,
        StaleRetirementResult, SyncConflictSummary, SyncExecutionPlan, SyncPullResult,
        SyncPushResult, SyncRunHistory, SyncTableResult,
    };

    #[test]
    fn resolves_context_budgets_from_flag_env_and_default() {
        let no_env = |_: &str| None;
        let env_8000 = |name: &str| (name == "HUGR_CONTEXT_TOKEN_BUDGET").then(|| "8000".into());
        let env_bad = |name: &str| (name == "HUGR_CONTEXT_TOKEN_BUDGET").then(|| "lots".into());

        assert_eq!(
            resolve_context_token_budget(Some(16000), no_env).unwrap(),
            16000
        );
        assert_eq!(resolve_context_token_budget(None, env_8000).unwrap(), 8000);
        assert_eq!(
            resolve_context_token_budget(None, no_env).unwrap(),
            crate::context::DEFAULT_CONTEXT_TOKEN_BUDGET
        );
        assert!(resolve_context_token_budget(Some(100), no_env).is_err());
        assert!(resolve_context_token_budget(None, env_bad).is_err());
        assert_eq!(
            resolve_context_token_budget(Some(16000), env_8000).unwrap(),
            16000,
            "explicit budget wins over the environment"
        );
    }

    #[test]
    fn command_observation_detail_records_status_and_output_tails() {
        let detail = command_observation_detail(
            &["cargo".to_string(), "test".to_string()],
            Some(0),
            "tests passed\n",
            "",
        );

        assert!(detail.contains("command: cargo test"));
        assert!(detail.contains("status: 0"));
        assert!(detail.contains("stdout_tail: tests passed"));
        assert!(!detail.contains("stderr_tail"));

        let long = "x".repeat(400);
        assert_eq!(output_tail(&long).unwrap().len(), 300);
    }

    #[test]
    fn command_diagnostics_parse_rust_and_colon_formats() {
        let diagnostics = command_diagnostics(
            &["cargo".to_string(), "test".to_string()],
            "src/lib.rs:9:5: warning: unused variable: hook\n",
            "error[E0425]: cannot find value `hook` in this scope\n  --> src/plugin_hooks.rs:12:9\n",
        );

        assert_eq!(diagnostics.len(), 2);
        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic.path.as_deref() == Some("src/plugin_hooks.rs")
                && diagnostic.line_start == Some(12)
                && diagnostic.severity == "error"
                && diagnostic.code.as_deref() == Some("E0425")
                && diagnostic.message.contains("cannot find value")
        }));
        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic.path.as_deref() == Some("src/lib.rs")
                && diagnostic.line_start == Some(9)
                && diagnostic.severity == "warning"
                && diagnostic.message.contains("unused variable")
        }));
    }

    #[test]
    fn symbol_renderers_include_locations() {
        let symbols = vec![CodeSymbol {
            path: "src/plugin_hooks.rs".to_string(),
            language: Some("rust".to_string()),
            name: "PluginHooks".to_string(),
            kind: "struct".to_string(),
            line_start: 3,
            line_end: Some(8),
            signature: "pub struct PluginHooks".to_string(),
        }];

        let text = render_symbols_text("PluginHooks", &symbols);
        let json = render_symbols_json("PluginHooks", &symbols);

        assert!(text.contains("struct PluginHooks at src/plugin_hooks.rs:3-8"));
        assert!(json.contains(r#""name":"PluginHooks""#));
        assert!(json.contains(r#""line_end":8"#));
    }

    #[test]
    fn shell_hooks_call_quiet_observe_endpoint() {
        let zsh = shell_hook_text("zsh").unwrap();
        let bash = shell_hook_text("bash").unwrap();

        assert!(zsh.contains("add-zsh-hook preexec"));
        assert!(zsh.contains("observe command --status"));
        assert!(bash.contains("trap _hugr_debug_trap DEBUG"));
        assert!(bash.contains("observe command --status"));
        assert!(shell_hook_text("fish").is_err());
    }

    /// The hooks are shell source, so substring assertions cannot tell whether
    /// they actually run. `local status=$?` passes every text check and still
    /// aborts `_hugr_precmd` on each prompt, because `status` is a read-only
    /// alias for `$?` in zsh — the observe call never fired and every prompt
    /// printed "read-only variable: status".
    ///
    /// Skipped when the shell is unavailable rather than failing, so the suite
    /// still runs on machines without zsh.
    #[test]
    fn the_zsh_hook_runs_without_shell_errors() {
        let Some(output) = run_hook_in_shell(
            "zsh",
            &["-f", "-c"],
            shell_hook_text("zsh").unwrap(),
            "HUGR_BIN=/bin/echo; HUGR_LAST_COMMAND='ls -la'; _hugr_precmd",
        ) else {
            return;
        };

        assert!(
            !output.contains("read-only"),
            "zsh hook reported a shell error: {output}"
        );
        assert!(
            output.contains("observe command"),
            "zsh hook did not reach the observe call: {output}"
        );
    }

    #[test]
    fn the_bash_hook_runs_without_shell_errors() {
        let Some(output) = run_hook_in_shell(
            "bash",
            &["--noprofile", "--norc", "-c"],
            shell_hook_text("bash").unwrap(),
            "HUGR_BIN=/bin/echo; HUGR_LAST_COMMAND='ls -la'; _hugr_prompt_command",
        ) else {
            return;
        };

        assert!(
            output.contains("observe command"),
            "bash hook did not reach the observe call: {output}"
        );
    }

    /// Sources `hook` in `shell` and runs `script`, returning the combined
    /// output. `None` means the shell is not installed.
    ///
    /// The hook silences the observe call with `>/dev/null 2>&1`, so the
    /// redirect is stripped to make the call observable.
    fn run_hook_in_shell(shell: &str, args: &[&str], hook: &str, script: &str) -> Option<String> {
        let hook = hook.replace(">/dev/null 2>&1", "");
        let output = std::process::Command::new(shell)
            .args(args)
            .arg(format!("{hook}\n{script}\n"))
            .output()
            .ok()?;

        let mut combined = String::from_utf8_lossy(&output.stdout).into_owned();
        combined.push_str(&String::from_utf8_lossy(&output.stderr));
        Some(combined)
    }

    #[test]
    fn session_promotion_renderers_include_memory() {
        let result = SessionPromotionResult {
            session_id: "ses_1".to_string(),
            task: "stabilize plugin registry".to_string(),
            fact_count: 2,
            memory: Memory {
                id: "mem_1".to_string(),
                created_at_ms: 7,
                kind: "fact".to_string(),
                text: "Session promoted finding".to_string(),
                structured_payload: Some(
                    r#"{"source":{"type":"session_promotion","session_id":"ses_1"}}"#.to_string(),
                ),
            },
        };

        let text = render_session_promotion_text(&result);
        let json = render_session_promotion_json(&result);
        let parsed = serde_json::from_str::<serde_json::Value>(&json).unwrap();

        assert!(text.contains("session: ses_1"));
        assert!(text.contains("memory: mem_1"));
        assert!(json.contains("\"session_id\":\"ses_1\""));
        assert!(json.contains("\"memory\""));
        assert_eq!(
            parsed["memory"]["structured_payload"]["source"]["type"],
            "session_promotion"
        );
    }

    #[test]
    fn recall_json_includes_query_and_memories() {
        let json = render_recall_json(
            "plugin hooks",
            &[Memory {
                id: "mem_1".to_string(),
                created_at_ms: 7,
                kind: "fact".to_string(),
                text: "plugin hooks run after configuration is loaded".to_string(),
                structured_payload: None,
            }],
        );

        assert!(json.contains("\"query\":\"plugin hooks\""));
        assert!(json.contains("\"id\":\"mem_1\""));
        assert!(json.contains("\"created_at_ms\":7"));
    }

    #[test]
    fn forget_renderers_include_retired_memories() {
        let result = ForgetResult {
            query: "plugin hooks".to_string(),
            forgotten_count: 1,
            forgotten_at: "42".to_string(),
            memories: vec![Memory {
                id: "mem_1".to_string(),
                created_at_ms: 7,
                kind: "fact".to_string(),
                text: "plugin hooks run after configuration is loaded".to_string(),
                structured_payload: None,
            }],
        };

        let text = render_forget_text(&result);
        assert!(text.contains("forgotten: 1"));
        assert!(text.contains("mem_1 [fact]"));

        let json = render_forget_json(&result);
        assert!(json.contains("\"forgotten_count\":1"));
        assert!(json.contains("\"query\":\"plugin hooks\""));
    }

    #[test]
    fn improve_renderers_include_duplicate_groups() {
        let report = MemoryMaintenanceReport {
            active_count: 2,
            retired_count: 1,
            duplicate_groups: vec![DuplicateMemoryGroup {
                normalized_text: "plugin hooks".to_string(),
                memories: vec![
                    Memory {
                        id: "mem_1".to_string(),
                        created_at_ms: 7,
                        kind: "fact".to_string(),
                        text: "plugin hooks".to_string(),
                        structured_payload: None,
                    },
                    Memory {
                        id: "mem_2".to_string(),
                        created_at_ms: 8,
                        kind: "fact".to_string(),
                        text: "Plugin hooks".to_string(),
                        structured_payload: None,
                    },
                ],
            }],
            stale_candidates: vec![StaleMemoryCandidate {
                reason: "opposing_terms".to_string(),
                signal: "after_vs_before".to_string(),
                shared_terms: vec!["hooks".to_string(), "plugin".to_string(), "run".to_string()],
                newer_memory: Memory {
                    id: "mem_new".to_string(),
                    created_at_ms: 9,
                    kind: "fact".to_string(),
                    text: "plugin hooks run before configuration".to_string(),
                    structured_payload: None,
                },
                older_memory: Memory {
                    id: "mem_old".to_string(),
                    created_at_ms: 7,
                    kind: "fact".to_string(),
                    text: "plugin hooks run after configuration".to_string(),
                    structured_payload: None,
                },
            }],
        };

        let text = render_improve_text(&report);
        assert!(text.contains("active_memories: 2"));
        assert!(text.contains("duplicate: plugin hooks"));
        assert!(text.contains("stale_candidate: opposing_terms"));

        let json = render_improve_json(&report);
        assert!(json.contains("\"retired_count\":1"));
        assert!(json.contains("\"normalized_text\":\"plugin hooks\""));
        assert!(json.contains("\"signal\":\"after_vs_before\""));
    }

    #[test]
    fn consolidation_renderers_include_retired_memories() {
        let result = MemoryConsolidationResult {
            executed_at: "42".to_string(),
            duplicate_groups: vec![DuplicateMemoryGroup {
                normalized_text: "plugin hooks".to_string(),
                memories: vec![
                    Memory {
                        id: "mem_keep".to_string(),
                        created_at_ms: 8,
                        kind: "fact".to_string(),
                        text: "plugin hooks".to_string(),
                        structured_payload: None,
                    },
                    Memory {
                        id: "mem_retire".to_string(),
                        created_at_ms: 7,
                        kind: "fact".to_string(),
                        text: "Plugin hooks".to_string(),
                        structured_payload: None,
                    },
                ],
            }],
            kept_memories: vec![Memory {
                id: "mem_keep".to_string(),
                created_at_ms: 8,
                kind: "fact".to_string(),
                text: "plugin hooks".to_string(),
                structured_payload: None,
            }],
            retired_memories: vec![Memory {
                id: "mem_retire".to_string(),
                created_at_ms: 7,
                kind: "fact".to_string(),
                text: "Plugin hooks".to_string(),
                structured_payload: None,
            }],
        };

        let text = render_consolidation_text(&result);
        assert!(text.contains("action: duplicates"));
        assert!(text.contains("retired: 1"));

        let json = render_consolidation_json(&result);
        assert!(json.contains("\"action\":\"duplicates\""));
        assert!(json.contains("\"id\":\"mem_retire\""));
    }

    #[test]
    fn stale_retirement_renderers_include_retired_memories() {
        let result = StaleRetirementResult {
            executed_at: "42".to_string(),
            stale_candidates: vec![StaleMemoryCandidate {
                reason: "opposing_terms".to_string(),
                signal: "after_vs_before".to_string(),
                shared_terms: vec!["hooks".to_string(), "plugin".to_string(), "run".to_string()],
                newer_memory: Memory {
                    id: "mem_new".to_string(),
                    created_at_ms: 8,
                    kind: "fact".to_string(),
                    text: "plugin hooks run before configuration".to_string(),
                    structured_payload: None,
                },
                older_memory: Memory {
                    id: "mem_old".to_string(),
                    created_at_ms: 7,
                    kind: "fact".to_string(),
                    text: "plugin hooks run after configuration".to_string(),
                    structured_payload: None,
                },
            }],
            kept_memories: vec![Memory {
                id: "mem_new".to_string(),
                created_at_ms: 8,
                kind: "fact".to_string(),
                text: "plugin hooks run before configuration".to_string(),
                structured_payload: None,
            }],
            retired_memories: vec![Memory {
                id: "mem_old".to_string(),
                created_at_ms: 7,
                kind: "fact".to_string(),
                text: "plugin hooks run after configuration".to_string(),
                structured_payload: None,
            }],
        };

        let text = render_stale_retirement_text(&result);
        assert!(text.contains("action: stale"));
        assert!(text.contains("retired: 1"));

        let json = render_stale_retirement_json(&result);
        assert!(json.contains("\"action\":\"stale\""));
        assert!(json.contains("\"signal\":\"after_vs_before\""));
        assert!(json.contains("\"id\":\"mem_old\""));
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
            remote_endpoint: Some("https://hugr.example".to_string()),
            api_contract_version: Some("hugr-api-v1".to_string()),
            api_routes: vec!["GET /v1/sync/status".to_string()],
            sync_classes: vec!["memories".to_string(), "full_source".to_string()],
            explicit_opt_in_classes: vec!["full_source".to_string()],
            status: "remote_sync_ready".to_string(),
        };

        let text = render_sync_status_text(&plan);
        assert!(text.contains("backend: direct_libsql"));
        assert!(text.contains("remote_endpoint: https://hugr.example"));
        assert!(text.contains("api_contract_version: hugr-api-v1"));
        assert!(text.contains("explicit_opt_in_classes: full_source"));

        let json = render_sync_status_json(&plan);
        assert!(json.contains("\"storage_mode\":\"hybrid\""));
        assert!(json.contains("\"api_routes\":[\"GET /v1/sync/status\"]"));
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

    /// Fixtures covering both `Some` and `None` for every optional field, a
    /// payload that parses as JSON, one that does not, and text that needs
    /// escaping.
    fn snapshot_memories() -> Vec<Memory> {
        vec![
            Memory {
                id: "mem_1".to_string(),
                created_at_ms: 10,
                kind: "fact".to_string(),
                text: "quote \" backslash \\ newline \n tab \t unicode ✓".to_string(),
                structured_payload: Some(r#"{"source":{"type":"session"}}"#.to_string()),
            },
            Memory {
                id: "mem_2".to_string(),
                created_at_ms: 20,
                kind: "note".to_string(),
                text: String::new(),
                structured_payload: None,
            },
            Memory {
                id: "mem_3".to_string(),
                created_at_ms: 30,
                kind: "fact".to_string(),
                text: "plain".to_string(),
                structured_payload: Some("not json at all".to_string()),
            },
        ]
    }

    fn snapshot_symbols() -> Vec<CodeSymbol> {
        vec![
            CodeSymbol {
                path: "src/plugin_hooks.rs".to_string(),
                language: Some("rust".to_string()),
                name: "run_after_config".to_string(),
                kind: "function".to_string(),
                line_start: 12,
                line_end: Some(40),
                signature: "pub fn run_after_config()".to_string(),
            },
            CodeSymbol {
                path: "src/other.rs".to_string(),
                language: None,
                name: "helper".to_string(),
                kind: "function".to_string(),
                line_start: 1,
                line_end: None,
                signature: String::new(),
            },
        ]
    }

    /// Pins the bytes of the `--json` output for `recall` and `symbols`.
    /// These are a CLI contract that agents parse, so a reorder or an
    /// escaping change should fail here rather than downstream.
    #[test]
    fn renders_stable_recall_and_symbol_json() {
        assert_eq!(
            render_recall_json("plugin \"hooks\"", &snapshot_memories()),
            RECALL_SNAPSHOT
        );
        assert_eq!(
            render_symbols_json("PluginHooks", &snapshot_symbols()),
            SYMBOLS_SNAPSHOT
        );
    }

    const RECALL_SNAPSHOT: &str = r#"{"query":"plugin \"hooks\"","memories":[{"id":"mem_1","created_at_ms":10,"kind":"fact","text":"quote \" backslash \\ newline \n tab \t unicode ✓","structured_payload":{"source":{"type":"session"}}},{"id":"mem_2","created_at_ms":20,"kind":"note","text":"","structured_payload":null},{"id":"mem_3","created_at_ms":30,"kind":"fact","text":"plain","structured_payload":"not json at all"}]}"#;

    const SYMBOLS_SNAPSHOT: &str = r#"{"query":"PluginHooks","symbols":[{"path":"src/plugin_hooks.rs","language":"rust","name":"run_after_config","kind":"function","line_start":12,"line_end":40,"signature":"pub fn run_after_config()"},{"path":"src/other.rs","language":null,"name":"helper","kind":"function","line_start":1,"line_end":null,"signature":""}]}"#;

    fn snapshot_tables() -> Vec<SyncTableResult> {
        vec![
            SyncTableResult {
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
            },
            SyncTableResult {
                class: "embeddings".to_string(),
                table: "memory_embeddings".to_string(),
                row_count: 0,
                inserted_count: 0,
                updated_count: 0,
                skipped_count: 0,
                conflict_count: 0,
                executed: false,
                conflicts: Vec::new(),
            },
        ]
    }

    /// Pins the bytes of the sync and forget `--json` output, which is a CLI
    /// contract agents parse.
    #[test]
    fn renders_stable_sync_and_forget_json() {
        let push = SyncPushResult {
            run_id: None,
            dry_run: true,
            backend: "direct_libsql".to_string(),
            status: "dry_run".to_string(),
            tables: snapshot_tables(),
        };
        let pull = SyncPullResult {
            run_id: Some("sync_pull_7".to_string()),
            dry_run: false,
            backend: "hugr_api".to_string(),
            status: "executed".to_string(),
            tables: snapshot_tables(),
        };
        let history = vec![SyncRunHistory {
            id: "run_1".to_string(),
            operation: "push".to_string(),
            backend: "direct_libsql".to_string(),
            status: "executed".to_string(),
            started_at_ms: 10,
            ended_at_ms: 20,
            tables: snapshot_tables(),
        }];
        let forget = ForgetResult {
            query: "plugin \"hooks\"".to_string(),
            forgotten_count: 1,
            forgotten_at: "2026-01-01T00:00:00Z".to_string(),
            memories: snapshot_memories(),
        };

        assert_eq!(render_sync_push_json(&push), PUSH_SNAPSHOT);
        assert_eq!(render_sync_pull_json(&pull), PULL_SNAPSHOT);
        assert_eq!(render_sync_history_json(&history), HISTORY_SNAPSHOT);
        assert_eq!(render_forget_json(&forget), FORGET_SNAPSHOT);
    }

    const PUSH_SNAPSHOT: &str = r#"{"run_id":null,"dry_run":true,"backend":"direct_libsql","status":"dry_run","tables":[{"class":"memories","table":"memories","row_count":2,"inserted_count":1,"updated_count":0,"skipped_count":1,"conflict_count":1,"executed":true,"conflicts":[{"reason":"local_row_preserved","count":1}]},{"class":"embeddings","table":"memory_embeddings","row_count":0,"inserted_count":0,"updated_count":0,"skipped_count":0,"conflict_count":0,"executed":false,"conflicts":[]}]}"#;

    const PULL_SNAPSHOT: &str = r#"{"run_id":"sync_pull_7","dry_run":false,"backend":"hugr_api","status":"executed","tables":[{"class":"memories","table":"memories","row_count":2,"inserted_count":1,"updated_count":0,"skipped_count":1,"conflict_count":1,"executed":true,"conflicts":[{"reason":"local_row_preserved","count":1}]},{"class":"embeddings","table":"memory_embeddings","row_count":0,"inserted_count":0,"updated_count":0,"skipped_count":0,"conflict_count":0,"executed":false,"conflicts":[]}]}"#;

    const HISTORY_SNAPSHOT: &str = r#"{"runs":[{"id":"run_1","operation":"push","backend":"direct_libsql","status":"executed","started_at_ms":10,"ended_at_ms":20,"tables":[{"class":"memories","table":"memories","row_count":2,"inserted_count":1,"updated_count":0,"skipped_count":1,"conflict_count":1,"executed":true,"conflicts":[{"reason":"local_row_preserved","count":1}]},{"class":"embeddings","table":"memory_embeddings","row_count":0,"inserted_count":0,"updated_count":0,"skipped_count":0,"conflict_count":0,"executed":false,"conflicts":[]}]}]}"#;

    const FORGET_SNAPSHOT: &str = r#"{"query":"plugin \"hooks\"","forgotten_count":1,"forgotten_at":"2026-01-01T00:00:00Z","memories":[{"id":"mem_1","created_at_ms":10,"kind":"fact","text":"quote \" backslash \\ newline \n tab \t unicode ✓","structured_payload":{"source":{"type":"session"}}},{"id":"mem_2","created_at_ms":20,"kind":"note","text":"","structured_payload":null},{"id":"mem_3","created_at_ms":30,"kind":"fact","text":"plain","structured_payload":"not json at all"}]}"#;

    fn snapshot_duplicate_groups() -> Vec<DuplicateMemoryGroup> {
        vec![DuplicateMemoryGroup {
            normalized_text: "plugin hooks run before config".to_string(),
            memories: snapshot_memories(),
        }]
    }

    fn snapshot_stale_candidates() -> Vec<StaleMemoryCandidate> {
        vec![StaleMemoryCandidate {
            reason: "contradicted".to_string(),
            signal: "before/after".to_string(),
            shared_terms: vec!["plugin".to_string(), "hooks \"quoted\"".to_string()],
            newer_memory: snapshot_memories()[0].clone(),
            older_memory: snapshot_memories()[1].clone(),
        }]
    }

    fn snapshot_plan() -> SyncExecutionPlan {
        SyncExecutionPlan {
            storage_mode: "hybrid".to_string(),
            backend: "hugr_api".to_string(),
            local_writes_enabled: true,
            remote_configured: true,
            remote_auth_configured: false,
            remote_reads_enabled: true,
            remote_writes_enabled: false,
            remote_endpoint: Some("https://api.example".to_string()),
            api_contract_version: None,
            api_routes: vec!["GET /v1/sync/status".to_string()],
            sync_classes: vec!["memories".to_string(), "full_source".to_string()],
            explicit_opt_in_classes: Vec::new(),
            status: "ready".to_string(),
        }
    }

    /// Pins the `improve` and `sync status` `--json` output. Two of these
    /// shapes do not match their struct: the consolidation and retirement
    /// envelopes lead with a literal `action`, and `sync status` emits
    /// `status` third while the struct declares it last.
    #[test]
    fn renders_stable_improve_and_status_json() {
        let report = MemoryMaintenanceReport {
            active_count: 3,
            retired_count: 1,
            duplicate_groups: snapshot_duplicate_groups(),
            stale_candidates: snapshot_stale_candidates(),
        };
        let consolidation = MemoryConsolidationResult {
            executed_at: "2026-01-01T00:00:00Z".to_string(),
            duplicate_groups: snapshot_duplicate_groups(),
            kept_memories: snapshot_memories(),
            retired_memories: Vec::new(),
        };
        let retirement = StaleRetirementResult {
            executed_at: "2026-01-02T00:00:00Z".to_string(),
            stale_candidates: snapshot_stale_candidates(),
            kept_memories: Vec::new(),
            retired_memories: snapshot_memories(),
        };

        assert_eq!(render_improve_json(&report), IMPROVE_SNAPSHOT);
        assert_eq!(
            render_consolidation_json(&consolidation),
            CONSOLIDATION_SNAPSHOT
        );
        assert_eq!(
            render_stale_retirement_json(&retirement),
            RETIREMENT_SNAPSHOT
        );
        assert_eq!(render_sync_status_json(&snapshot_plan()), STATUS_SNAPSHOT);
    }

    const IMPROVE_SNAPSHOT: &str = r#"{"active_count":3,"retired_count":1,"duplicate_groups":[{"normalized_text":"plugin hooks run before config","memories":[{"id":"mem_1","created_at_ms":10,"kind":"fact","text":"quote \" backslash \\ newline \n tab \t unicode ✓","structured_payload":{"source":{"type":"session"}}},{"id":"mem_2","created_at_ms":20,"kind":"note","text":"","structured_payload":null},{"id":"mem_3","created_at_ms":30,"kind":"fact","text":"plain","structured_payload":"not json at all"}]}],"stale_candidates":[{"reason":"contradicted","signal":"before/after","shared_terms":["plugin","hooks \"quoted\""],"newer_memory":{"id":"mem_1","created_at_ms":10,"kind":"fact","text":"quote \" backslash \\ newline \n tab \t unicode ✓","structured_payload":{"source":{"type":"session"}}},"older_memory":{"id":"mem_2","created_at_ms":20,"kind":"note","text":"","structured_payload":null}}]}"#;

    const CONSOLIDATION_SNAPSHOT: &str = r#"{"action":"duplicates","executed_at":"2026-01-01T00:00:00Z","duplicate_groups":[{"normalized_text":"plugin hooks run before config","memories":[{"id":"mem_1","created_at_ms":10,"kind":"fact","text":"quote \" backslash \\ newline \n tab \t unicode ✓","structured_payload":{"source":{"type":"session"}}},{"id":"mem_2","created_at_ms":20,"kind":"note","text":"","structured_payload":null},{"id":"mem_3","created_at_ms":30,"kind":"fact","text":"plain","structured_payload":"not json at all"}]}],"kept_memories":[{"id":"mem_1","created_at_ms":10,"kind":"fact","text":"quote \" backslash \\ newline \n tab \t unicode ✓","structured_payload":{"source":{"type":"session"}}},{"id":"mem_2","created_at_ms":20,"kind":"note","text":"","structured_payload":null},{"id":"mem_3","created_at_ms":30,"kind":"fact","text":"plain","structured_payload":"not json at all"}],"retired_memories":[]}"#;

    const RETIREMENT_SNAPSHOT: &str = r#"{"action":"stale","executed_at":"2026-01-02T00:00:00Z","stale_candidates":[{"reason":"contradicted","signal":"before/after","shared_terms":["plugin","hooks \"quoted\""],"newer_memory":{"id":"mem_1","created_at_ms":10,"kind":"fact","text":"quote \" backslash \\ newline \n tab \t unicode ✓","structured_payload":{"source":{"type":"session"}}},"older_memory":{"id":"mem_2","created_at_ms":20,"kind":"note","text":"","structured_payload":null}}],"kept_memories":[],"retired_memories":[{"id":"mem_1","created_at_ms":10,"kind":"fact","text":"quote \" backslash \\ newline \n tab \t unicode ✓","structured_payload":{"source":{"type":"session"}}},{"id":"mem_2","created_at_ms":20,"kind":"note","text":"","structured_payload":null},{"id":"mem_3","created_at_ms":30,"kind":"fact","text":"plain","structured_payload":"not json at all"}]}"#;

    const STATUS_SNAPSHOT: &str = r#"{"storage_mode":"hybrid","backend":"hugr_api","status":"ready","local_writes_enabled":true,"remote_configured":true,"remote_auth_configured":false,"remote_reads_enabled":true,"remote_writes_enabled":false,"remote_endpoint":"https://api.example","api_contract_version":null,"api_routes":["GET /v1/sync/status"],"sync_classes":["memories","full_source"],"explicit_opt_in_classes":[]}"#;

    fn edit_temp_dir(name: &str) -> std::path::PathBuf {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system time should be after unix epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("hugr_write_{name}_{unique}"));
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn planned_writes_apply_every_file() {
        let root = edit_temp_dir("apply");
        let first = root.join("first.rs");
        let second = root.join("second.rs");
        std::fs::write(&first, "old first\n").unwrap();
        std::fs::write(&second, "old second\n").unwrap();

        write_planned_files(
            "rename-symbol",
            &[
                (first.to_str().unwrap(), "new first\n"),
                (second.to_str().unwrap(), "new second\n"),
            ],
        )
        .unwrap();

        assert_eq!(std::fs::read_to_string(&first).unwrap(), "new first\n");
        assert_eq!(std::fs::read_to_string(&second).unwrap(), "new second\n");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The regression this staging exists for. A multi-file refactor used to
    /// be written in a plain loop, so a failure partway through left earlier
    /// files rewritten against a stale rest of the tree — a state that does
    /// not compile, produced by a command that reported an error. Staging a
    /// directory that cannot be created fails before any destination is
    /// touched.
    #[test]
    fn a_failed_planned_write_leaves_every_file_untouched() {
        let root = edit_temp_dir("atomic");
        let first = root.join("first.rs");
        std::fs::write(&first, "old first\n").unwrap();
        // A path whose parent is a *file*, so staging beside it cannot work.
        let blocked = first.join("nested").join("second.rs");

        let error = write_planned_files(
            "rename-symbol",
            &[
                (first.to_str().unwrap(), "new first\n"),
                (blocked.to_str().unwrap(), "new second\n"),
            ],
        )
        .expect_err("staging beneath a file must fail");

        assert!(error.to_string().contains("rename-symbol"));
        assert_eq!(
            std::fs::read_to_string(&first).unwrap(),
            "old first\n",
            "the first file must keep its original contents"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Staging files must not survive a failure, or the next `hugr index`
    /// would pick up `.name.hugr-tmp` siblings as real source files.
    #[test]
    fn a_failed_planned_write_cleans_up_staged_files() {
        let root = edit_temp_dir("cleanup");
        let first = root.join("first.rs");
        std::fs::write(&first, "old first\n").unwrap();
        let blocked = first.join("nested").join("second.rs");

        let _ = write_planned_files(
            "move-symbol",
            &[
                (first.to_str().unwrap(), "new first\n"),
                (blocked.to_str().unwrap(), "new second\n"),
            ],
        );

        let leftovers = std::fs::read_dir(&root)
            .unwrap()
            .filter_map(std::result::Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().contains("hugr-tmp"))
            .count();
        assert_eq!(leftovers, 0, "staged files must be cleaned up on failure");
        let _ = std::fs::remove_dir_all(&root);
    }
}
