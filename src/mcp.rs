use crate::code::CodeSymbol;
use crate::context::ContextPack;
use crate::discovery;
use crate::edit;
use crate::impact;
use crate::indexer;
use crate::store::{
    Memory, MemorySource, MemoryWriteOptions, Project, Session, SessionEvent, Store,
};
use crate::worktree;
use serde_json::{Value, json};
use std::collections::HashSet;
use std::io::{self, BufRead, Write};
use std::path::Path;

const PROTOCOL_VERSION: &str = "2024-11-05";

pub async fn serve_stdio() -> Result<(), String> {
    let stdin = io::stdin();
    let mut stdout = io::stdout();

    for line in stdin.lock().lines() {
        let line = line.map_err(|error| error.to_string())?;
        if line.trim().is_empty() {
            continue;
        }

        let response = handle_line(&line).await;
        if let Some(response) = response {
            writeln!(stdout, "{response}").map_err(|error| error.to_string())?;
            stdout.flush().map_err(|error| error.to_string())?;
        }
    }

    Ok(())
}

async fn handle_line(line: &str) -> Option<String> {
    let request = match serde_json::from_str::<Value>(line) {
        Ok(request) => handle_request(request).await,
        Err(error) => Err(json_error(
            Value::Null,
            -32700,
            &format!("parse error: {error}"),
        )),
    };

    match request {
        Ok(Some(response)) => Some(response.to_string()),
        Ok(None) => None,
        Err(error) => Some(error.to_string()),
    }
}

async fn handle_request(request: Value) -> Result<Option<Value>, Value> {
    let id = request.get("id").cloned().unwrap_or(Value::Null);
    let method = request
        .get("method")
        .and_then(Value::as_str)
        .ok_or_else(|| json_error(id.clone(), -32600, "missing method"))?;

    match method {
        "initialize" => Ok(Some(json_response(
            id,
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {
                    "tools": {
                        "listChanged": false
                    }
                },
                "serverInfo": {
                    "name": "hugr",
                    "version": env!("CARGO_PKG_VERSION")
                }
            }),
        ))),
        "notifications/initialized" => Ok(None),
        "ping" => Ok(Some(json_response(id, json!({})))),
        "tools/list" => Ok(Some(json_response(id, json!({ "tools": tools() })))),
        "tools/call" => {
            let params = request.get("params").cloned().unwrap_or_else(|| json!({}));
            let result = handle_tool_call(params)
                .await
                .map_err(|error| json_error(id.clone(), -32603, &error))?;
            Ok(Some(json_response(id, result)))
        }
        unknown => Err(json_error(
            id,
            -32601,
            &format!("unknown method '{unknown}'"),
        )),
    }
}

async fn handle_tool_call(params: Value) -> Result<Value, String> {
    let name = required_string(&params, "name")?;
    let arguments = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));

    match name.as_str() {
        "hugr_context" => tool_context(&arguments).await,
        "hugr_remember" => tool_remember(&arguments).await,
        "hugr_recall" => tool_recall(&arguments).await,
        "hugr_project_status" => tool_project_status().await,
        "hugr_session_start" => tool_session_start(&arguments).await,
        "hugr_session_event" => tool_session_event(&arguments).await,
        "hugr_session_end" => tool_session_end(&arguments).await,
        "hugr_index" => tool_index(&arguments).await,
        "hugr_symbols" => tool_symbols(&arguments).await,
        "hugr_impact" => tool_impact(&arguments).await,
        "hugr_replace_symbol" => tool_replace_symbol(&arguments).await,
        "hugr_rename_symbol" => tool_rename_symbol(&arguments).await,
        "hugr_move_symbol" => tool_move_symbol(&arguments).await,
        "hugr_forget" => tool_forget(&arguments).await,
        unknown => Err(format!("unknown tool '{unknown}'")),
    }
}

async fn tool_context(arguments: &Value) -> Result<Value, String> {
    let task = required_string(arguments, "task")?;
    let store = Store::open_current();
    let memories = store.recall(&task, 5).await?;
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
    let sessions = store.recent_session_facts(&task, 5).await?;
    let file_candidates = discovery::discover_candidate_files(Path::new("."), &task, 12)?;
    indexer::index_candidates(&store, Path::new("."), &file_candidates).await?;
    let symbols = store.recall_symbols(&task, 8).await?;
    let files = file_candidates
        .iter()
        .map(|candidate| candidate.path.clone())
        .collect::<Vec<_>>();
    let affected_tests = store.likely_tests_for_files(&files, 5).await?;
    let graph_neighbors = store
        .context_graph_neighbors(&task, &files, &symbols, 12)
        .await?;
    let freshness_signals = store
        .context_freshness_signals(&files, &symbols, 12)
        .await?;
    let diagnostics = store.recent_diagnostics(&task, &files, &symbols, 8).await?;
    let branch_state = worktree::inspect(Path::new("."));
    let pack =
        ContextPack::with_file_candidates_sessions_symbols_tests_branch_stale_risks_and_graph(
            &task,
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
        );
    let structured = context_pack_json(&pack);

    Ok(tool_result(pack.render_markdown(), structured))
}

async fn tool_remember(arguments: &Value) -> Result<Value, String> {
    let text = required_string(arguments, "text")?;
    let options = optional_memory_write_options(arguments)?;
    let memory = Store::open_current()
        .remember_with_options(&text, options)
        .await?;
    Ok(tool_result(
        format!("remembered {}", memory.id),
        json!({ "memory": memory_json(&memory) }),
    ))
}

async fn tool_recall(arguments: &Value) -> Result<Value, String> {
    let query = required_string(arguments, "query")?;
    let limit = optional_limit(arguments, 10)?;
    let memories = Store::open_current().recall(&query, limit).await?;
    let structured = json!({
        "query": query,
        "memories": memories.iter().map(memory_json).collect::<Vec<_>>()
    });
    Ok(tool_result(structured.to_string(), structured))
}

async fn tool_forget(arguments: &Value) -> Result<Value, String> {
    let query = required_string(arguments, "query")?;
    let limit = optional_bounded_usize(arguments, "limit", 25, 100)?;
    let result = Store::open_current().forget(&query, limit).await?;
    let structured = json!({
        "query": result.query,
        "forgotten_count": result.forgotten_count,
        "forgotten_at": result.forgotten_at,
        "memories": result.memories.iter().map(memory_json).collect::<Vec<_>>()
    });

    Ok(tool_result(
        format!("forgot {} memories", result.forgotten_count),
        structured,
    ))
}

async fn tool_project_status() -> Result<Value, String> {
    let store = Store::open_current();
    let project = store.sync_current_project().await?;
    Ok(tool_result(
        format!("project {} at {}", project.name, project.root_path),
        json!({ "project": project_json(&project), "storage": store.storage_summary() }),
    ))
}

async fn tool_session_start(arguments: &Value) -> Result<Value, String> {
    let task = required_string(arguments, "task")?;
    let session = Store::open_current().start_session(&task).await?;
    Ok(tool_result(
        format!("started session {}", session.id),
        json!({ "session": session_json(&session) }),
    ))
}

async fn tool_session_event(arguments: &Value) -> Result<Value, String> {
    let kind = required_string(arguments, "kind")?;
    let detail = required_string(arguments, "detail")?;
    let event = Store::open_current()
        .record_session_event(&kind, &detail)
        .await?;
    Ok(tool_result(
        format!("recorded event {}", event.id),
        json!({ "event": session_event_json(&event) }),
    ))
}

async fn tool_session_end(arguments: &Value) -> Result<Value, String> {
    let summary = arguments.get("summary").and_then(Value::as_str);
    let session = Store::open_current().end_session(summary).await?;
    Ok(tool_result(
        format!("ended session {}", session.id),
        json!({ "session": session_json(&session) }),
    ))
}

async fn tool_index(arguments: &Value) -> Result<Value, String> {
    let limit = optional_bounded_usize(arguments, "limit", 5000, 50000)?;
    let summary = indexer::index_project(limit).await?;
    Ok(tool_result(
        format!(
            "indexed {} files and {} symbols",
            summary.file_count, summary.symbol_count
        ),
        json!({
            "files": summary.file_count,
            "symbols": summary.symbol_count,
            "file_roles": summary.file_roles.iter().map(index_classification_json).collect::<Vec<_>>(),
            "languages": summary.languages.iter().map(index_classification_json).collect::<Vec<_>>(),
            "symbol_kinds": summary.symbol_kinds.iter().map(index_classification_json).collect::<Vec<_>>()
        }),
    ))
}

async fn tool_symbols(arguments: &Value) -> Result<Value, String> {
    let query = required_string(arguments, "query")?;
    let limit = optional_bounded_usize(arguments, "limit", 25, 100)?;
    indexer::index_project(5000).await?;
    let symbols = Store::open_current()
        .symbols_for_target(&query, limit)
        .await?;
    let structured = json!({
        "query": query,
        "symbols": symbols.iter().map(symbol_json).collect::<Vec<_>>()
    });

    Ok(tool_result(structured.to_string(), structured))
}

async fn tool_impact(arguments: &Value) -> Result<Value, String> {
    let target = required_string(arguments, "target")?;
    let limit = optional_bounded_usize(arguments, "limit", 50, 500)?;
    indexer::index_project(5000).await?;
    let store = Store::open_current();
    let report = impact::analyze(&store, &target, limit).await?;
    let structured = serde_json::from_str(&report.render_json()).unwrap_or_else(|_| json!({}));

    Ok(tool_result(report.render_markdown(), structured))
}

async fn tool_replace_symbol(arguments: &Value) -> Result<Value, String> {
    let path = required_string(arguments, "path")?;
    let name = required_string(arguments, "name")?;
    let kind = optional_string(arguments, "kind")?;
    let body = required_string(arguments, "body")?;

    let store = Store::open_current();
    if !store.supports_local_source_edits()? {
        return Err(
            "hugr_replace_symbol edits the local working tree and is not available in remote Hugr API mode"
                .to_string(),
        );
    }

    let contents = std::fs::read_to_string(&path)
        .map_err(|error| format!("hugr_replace_symbol cannot read {path}: {error}"))?;
    let planned = edit::plan_replacement(&path, &contents, &name, kind.as_deref(), &body)?;

    std::fs::write(&path, &planned.contents)
        .map_err(|error| format!("hugr_replace_symbol cannot write {path}: {error}"))?;
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

    let structured = serde_json::from_str(&summary.render_json()).unwrap_or_else(|_| json!({}));
    Ok(tool_result(summary.render_markdown(), structured))
}

async fn tool_rename_symbol(arguments: &Value) -> Result<Value, String> {
    let path = required_string(arguments, "path")?;
    let name = required_string(arguments, "name")?;
    let new_name = required_string(arguments, "new_name")?;
    let kind = optional_string(arguments, "kind")?;

    let store = Store::open_current();
    if !store.supports_local_source_edits()? {
        return Err(
            "hugr_rename_symbol edits the local working tree and is not available in remote Hugr API mode"
                .to_string(),
        );
    }

    indexer::index_project(5000).await?;

    let contents = std::fs::read_to_string(&path)
        .map_err(|error| format!("hugr_rename_symbol cannot read {path}: {error}"))?;
    let target =
        edit::resolve_symbol_in_source(&path, &contents, &name, kind.as_deref(), "rename")?;
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
    for path in paths {
        let contents = std::fs::read_to_string(&path)
            .map_err(|error| format!("hugr_rename_symbol cannot read {path}: {error}"))?;
        files.push((path, contents));
    }

    let planned = edit::plan_rename(&target, &references, files, &new_name)?;
    for file in &planned.files {
        std::fs::write(&file.path, &file.contents)
            .map_err(|error| format!("hugr_rename_symbol cannot write {}: {error}", file.path))?;
    }

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

    let structured = serde_json::from_str(&summary.render_json()).unwrap_or_else(|_| json!({}));
    Ok(tool_result(summary.render_markdown(), structured))
}

async fn tool_move_symbol(arguments: &Value) -> Result<Value, String> {
    let source_path = required_string(arguments, "source_path")?;
    let name = required_string(arguments, "name")?;
    let destination_path = required_string(arguments, "destination_path")?;
    let kind = optional_string(arguments, "kind")?;
    let rewrite_references = optional_bool(arguments, "rewrite_references")?;

    let store = Store::open_current();
    if !store.supports_local_source_edits()? {
        return Err(
            "hugr_move_symbol edits the local working tree and is not available in remote Hugr API mode"
                .to_string(),
        );
    }

    indexer::index_project(5000).await?;

    let source_contents = std::fs::read_to_string(&source_path)
        .map_err(|error| format!("hugr_move_symbol cannot read {source_path}: {error}"))?;
    let destination_contents = read_optional_destination(&destination_path, "hugr_move_symbol")?;
    let target = edit::resolve_symbol_in_source(
        &source_path,
        &source_contents,
        &name,
        kind.as_deref(),
        "move",
    )?;
    let references = store
        .references_to_symbols(std::slice::from_ref(&target), 2000)
        .await?;
    let reference_files = if rewrite_references {
        read_reference_files(
            &references,
            &source_path,
            &destination_path,
            "hugr_move_symbol",
        )?
    } else {
        Vec::new()
    };
    let planned = edit::plan_move(
        &target,
        &references,
        &source_contents,
        &destination_path,
        &destination_contents,
        reference_files,
        rewrite_references,
    )?;

    for file in &planned.files {
        std::fs::write(&file.path, &file.contents)
            .map_err(|error| format!("hugr_move_symbol cannot write {}: {error}", file.path))?;
    }

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

    let structured = serde_json::from_str(&summary.render_json()).unwrap_or_else(|_| json!({}));
    Ok(tool_result(summary.render_markdown(), structured))
}

fn read_optional_destination(path: &str, command: &str) -> Result<String, String> {
    match std::fs::read_to_string(path) {
        Ok(contents) => Ok(contents),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(String::new()),
        Err(error) => Err(format!("{command} cannot read {path}: {error}")),
    }
}

fn read_reference_files(
    references: &[crate::code::CodeReference],
    source_path: &str,
    destination_path: &str,
    command: &str,
) -> Result<Vec<(String, String)>, String> {
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
                .map(|contents| (path.clone(), contents))
                .map_err(|error| format!("{command} cannot read {path}: {error}"))
        })
        .collect()
}

fn tools() -> Vec<Value> {
    vec![
        tool_schema(
            "hugr_context",
            "Compile a Hugr context pack for a task.",
            &[("task", "string")],
        ),
        tool_schema(
            "hugr_remember",
            "Store a durable memory.",
            &[("text", "string")],
        ),
        tool_schema(
            "hugr_recall",
            "Recall memories for a query.",
            &[("query", "string")],
        ),
        tool_schema(
            "hugr_project_status",
            "Return current project metadata.",
            &[],
        ),
        tool_schema(
            "hugr_session_start",
            "Start an agent work session.",
            &[("task", "string")],
        ),
        tool_schema(
            "hugr_session_event",
            "Record an event for the active session.",
            &[("kind", "string"), ("detail", "string")],
        ),
        tool_schema(
            "hugr_session_end",
            "End the active session with an optional summary.",
            &[],
        ),
        tool_schema(
            "hugr_forget",
            "Retire memories matching a query.",
            &[("query", "string")],
        ),
        tool_schema("hugr_index", "Index project files and symbols.", &[]),
        tool_schema(
            "hugr_symbols",
            "Lookup indexed code symbols by file path, exact name, or query.",
            &[("query", "string")],
        ),
        tool_schema(
            "hugr_impact",
            "Trace direct indexed references for a file or symbol.",
            &[("target", "string")],
        ),
        tool_schema(
            "hugr_replace_symbol",
            "Safely replace one top-level symbol's source in a local file. Refuses ambiguous targets, renames, kind changes, and bodies that fail to parse.",
            &[("path", "string"), ("name", "string"), ("body", "string")],
        ),
        tool_schema(
            "hugr_rename_symbol",
            "Safely rename one local symbol and its indexed inbound references. Refuses ambiguous targets, stale reference lines, invalid identifiers, and files that fail to parse after the refactor.",
            &[
                ("path", "string"),
                ("name", "string"),
                ("new_name", "string"),
            ],
        ),
        tool_schema(
            "hugr_move_symbol",
            "Safely move one local symbol from a source file to a destination file. With rewrite_references, rewrites or validates supported inbound references; otherwise refuses referenced symbols. Refuses language mismatches, destination collisions, and files that fail to parse after the move.",
            &[
                ("source_path", "string"),
                ("name", "string"),
                ("destination_path", "string"),
            ],
        ),
    ]
}

fn tool_schema(name: &str, description: &str, properties: &[(&str, &str)]) -> Value {
    let mut props = serde_json::Map::new();
    let mut required = Vec::new();

    for (property, kind) in properties {
        props.insert(
            (*property).to_string(),
            json!({
                "type": kind,
            }),
        );
        required.push(json!(property));
    }

    if name == "hugr_recall" || name == "hugr_forget" {
        props.insert(
            "limit".to_string(),
            json!({
                "type": "integer",
                "minimum": 1,
                "maximum": if name == "hugr_forget" { 100 } else { 50 }
            }),
        );
    }

    if name == "hugr_impact" || name == "hugr_symbols" {
        props.insert(
            "limit".to_string(),
            json!({
                "type": "integer",
                "minimum": 1,
                "maximum": if name == "hugr_impact" { 500 } else { 100 }
            }),
        );
    }

    if name == "hugr_index" {
        props.insert(
            "limit".to_string(),
            json!({
                "type": "integer",
                "minimum": 1,
                "maximum": 50000
            }),
        );
    }

    if name == "hugr_replace_symbol" || name == "hugr_rename_symbol" || name == "hugr_move_symbol" {
        props.insert(
            "kind".to_string(),
            json!({
                "type": "string"
            }),
        );
    }

    if name == "hugr_move_symbol" {
        props.insert(
            "rewrite_references".to_string(),
            json!({
                "type": "boolean"
            }),
        );
    }

    if name == "hugr_remember" {
        props.insert(
            "confidence".to_string(),
            json!({
                "type": "number",
                "minimum": 0.0,
                "maximum": 1.0
            }),
        );
        props.insert(
            "sensitivity".to_string(),
            json!({
                "type": "string"
            }),
        );
        props.insert(
            "valid_from".to_string(),
            json!({
                "type": "string"
            }),
        );
        props.insert(
            "valid_to".to_string(),
            json!({
                "type": "string"
            }),
        );
        props.insert(
            "source".to_string(),
            json!({
                "type": "object",
                "properties": {
                    "kind": { "type": "string" },
                    "locator": { "type": "string" }
                },
                "required": ["kind", "locator"]
            }),
        );
    }

    if name == "hugr_session_end" {
        props.insert(
            "summary".to_string(),
            json!({
                "type": "string"
            }),
        );
    }

    json!({
        "name": name,
        "description": description,
        "inputSchema": {
            "type": "object",
            "properties": props,
            "required": required
        }
    })
}

fn required_string(arguments: &Value, key: &str) -> Result<String, String> {
    arguments
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| format!("missing required string argument '{key}'"))
}

fn optional_limit(arguments: &Value, default: usize) -> Result<usize, String> {
    optional_bounded_usize(arguments, "limit", default, 50)
}

fn optional_bounded_usize(
    arguments: &Value,
    key: &str,
    default: usize,
    maximum: usize,
) -> Result<usize, String> {
    let Some(value) = arguments.get(key) else {
        return Ok(default);
    };
    let Some(limit) = value.as_u64() else {
        return Err(format!("{key} must be an integer"));
    };
    usize::try_from(limit)
        .map(|limit| limit.clamp(1, maximum))
        .map_err(|error| error.to_string())
}

fn optional_memory_write_options(arguments: &Value) -> Result<MemoryWriteOptions, String> {
    Ok(MemoryWriteOptions {
        source: optional_memory_source(arguments)?,
        confidence: optional_f64(arguments, "confidence")?,
        sensitivity: optional_string(arguments, "sensitivity")?,
        valid_from: optional_string(arguments, "valid_from")?,
        valid_to: optional_string(arguments, "valid_to")?,
    })
}

fn optional_memory_source(arguments: &Value) -> Result<Option<MemorySource>, String> {
    match arguments.get("source") {
        Some(source) if source.is_object() => Ok(Some(MemorySource {
            kind: required_string(source, "kind")?,
            locator: required_string(source, "locator")?,
        })),
        Some(_) => Err("source must be an object".to_string()),
        None => Ok(None),
    }
}

fn optional_f64(arguments: &Value, key: &str) -> Result<Option<f64>, String> {
    let Some(value) = arguments.get(key) else {
        return Ok(None);
    };
    value
        .as_f64()
        .map(Some)
        .ok_or_else(|| format!("{key} must be a number"))
}

fn optional_string(arguments: &Value, key: &str) -> Result<Option<String>, String> {
    let Some(value) = arguments.get(key) else {
        return Ok(None);
    };
    value
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .map(Some)
        .ok_or_else(|| format!("{key} must be a non-empty string"))
}

fn optional_bool(arguments: &Value, key: &str) -> Result<bool, String> {
    let Some(value) = arguments.get(key) else {
        return Ok(false);
    };
    value
        .as_bool()
        .ok_or_else(|| format!("{key} must be a boolean"))
}

fn tool_result(text: String, structured: Value) -> Value {
    json!({
        "content": [
            {
                "type": "text",
                "text": text
            }
        ],
        "structuredContent": structured
    })
}

fn json_response(id: Value, result: Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result
    })
}

fn json_error(id: Value, code: i64, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": code,
            "message": message
        }
    })
}

fn memory_json(memory: &Memory) -> Value {
    json!({
        "id": &memory.id,
        "created_at_ms": memory.created_at_ms,
        "kind": &memory.kind,
        "text": &memory.text,
        "structured_payload": memory_payload_json(memory.structured_payload.as_deref())
    })
}

fn symbol_json(symbol: &CodeSymbol) -> Value {
    json!({
        "path": &symbol.path,
        "language": &symbol.language,
        "name": &symbol.name,
        "kind": &symbol.kind,
        "line_start": symbol.line_start,
        "line_end": symbol.line_end,
        "signature": &symbol.signature
    })
}

fn memory_payload_json(payload: Option<&str>) -> Value {
    match payload {
        Some(payload) => serde_json::from_str(payload).unwrap_or_else(|_| json!(payload)),
        None => Value::Null,
    }
}

fn index_classification_json(classification: &indexer::IndexClassification) -> Value {
    json!({
        "name": &classification.name,
        "count": classification.count
    })
}

fn project_json(project: &Project) -> Value {
    json!({
        "id": &project.id,
        "name": &project.name,
        "root_path": &project.root_path,
        "git_remote": &project.git_remote,
        "default_branch": &project.default_branch,
        "created_at_ms": project.created_at_ms,
        "updated_at_ms": project.updated_at_ms
    })
}

fn session_json(session: &Session) -> Value {
    json!({
        "id": &session.id,
        "task": &session.task,
        "branch": &session.branch,
        "started_at_ms": session.started_at_ms,
        "ended_at_ms": session.ended_at_ms,
        "final_summary": &session.final_summary
    })
}

fn session_event_json(event: &SessionEvent) -> Value {
    json!({
        "id": &event.id,
        "session_id": &event.session_id,
        "kind": &event.kind,
        "detail": &event.detail,
        "created_at_ms": event.created_at_ms
    })
}

fn context_pack_json(pack: &ContextPack) -> Value {
    serde_json::from_str(&pack.render_json()).unwrap_or_else(|_| json!({}))
}

#[cfg(test)]
mod tests {
    use super::{handle_line, index_classification_json, memory_json, symbol_json, tools};
    use crate::code::CodeSymbol;
    use crate::indexer::IndexClassification;
    use crate::store::Memory;
    use serde_json::Value;

    #[tokio::test]
    async fn initializes_mcp_server() {
        let response = handle_line(r#"{"jsonrpc":"2.0","id":1,"method":"initialize"}"#)
            .await
            .unwrap();
        let value = serde_json::from_str::<Value>(&response).unwrap();

        assert_eq!(value["id"], 1);
        assert_eq!(value["result"]["serverInfo"]["name"], "hugr");
    }

    #[test]
    fn lists_expected_tools() {
        let tools = tools();
        let names = tools
            .iter()
            .filter_map(|tool| tool.get("name").and_then(Value::as_str))
            .collect::<Vec<_>>();

        assert!(names.contains(&"hugr_context"));
        assert!(names.contains(&"hugr_remember"));
        assert!(names.contains(&"hugr_recall"));
        assert!(names.contains(&"hugr_project_status"));
        assert!(names.contains(&"hugr_session_start"));
        assert!(names.contains(&"hugr_session_event"));
        assert!(names.contains(&"hugr_session_end"));
        assert!(names.contains(&"hugr_forget"));
        assert!(names.contains(&"hugr_symbols"));
        assert!(names.contains(&"hugr_replace_symbol"));
        assert!(names.contains(&"hugr_rename_symbol"));
        assert!(names.contains(&"hugr_move_symbol"));
    }

    #[test]
    fn remember_tool_schema_allows_source_and_metadata() {
        let tools = tools();
        let remember = tools
            .iter()
            .find(|tool| tool.get("name").and_then(Value::as_str) == Some("hugr_remember"))
            .unwrap();

        assert_eq!(
            remember["inputSchema"]["properties"]["source"]["properties"]["kind"]["type"],
            "string"
        );
        assert_eq!(
            remember["inputSchema"]["properties"]["source"]["properties"]["locator"]["type"],
            "string"
        );
        assert_eq!(
            remember["inputSchema"]["properties"]["source"]["required"][0],
            "kind"
        );
        assert_eq!(
            remember["inputSchema"]["properties"]["confidence"]["type"],
            "number"
        );
        assert_eq!(
            remember["inputSchema"]["properties"]["confidence"]["minimum"],
            0.0
        );
        assert_eq!(
            remember["inputSchema"]["properties"]["sensitivity"]["type"],
            "string"
        );
        assert_eq!(
            remember["inputSchema"]["properties"]["valid_from"]["type"],
            "string"
        );
        assert_eq!(
            remember["inputSchema"]["properties"]["valid_to"]["type"],
            "string"
        );
    }

    #[test]
    fn memory_json_preserves_structured_payload() {
        let memory = Memory {
            id: "mem_1".to_string(),
            created_at_ms: 7,
            kind: "fact".to_string(),
            text: "Session promoted finding".to_string(),
            structured_payload: Some(
                r#"{"source":{"type":"session_promotion","session_id":"ses_1"}}"#.to_string(),
            ),
        };

        let value = memory_json(&memory);

        assert_eq!(
            value["structured_payload"]["source"]["type"],
            "session_promotion"
        );
        assert_eq!(value["structured_payload"]["source"]["session_id"], "ses_1");
    }

    #[test]
    fn symbol_json_includes_location_fields() {
        let value = symbol_json(&CodeSymbol {
            path: "src/plugin_hooks.rs".to_string(),
            language: Some("rust".to_string()),
            name: "PluginHooks".to_string(),
            kind: "struct".to_string(),
            line_start: 3,
            line_end: Some(8),
            signature: "pub struct PluginHooks".to_string(),
        });

        assert_eq!(value["path"], "src/plugin_hooks.rs");
        assert_eq!(value["name"], "PluginHooks");
        assert_eq!(value["line_start"], 3);
        assert_eq!(value["line_end"], 8);
    }

    #[test]
    fn index_classification_json_includes_name_and_count() {
        let value = index_classification_json(&IndexClassification {
            name: "source".to_string(),
            count: 3,
        });

        assert_eq!(value["name"], "source");
        assert_eq!(value["count"], 3);
    }
}
