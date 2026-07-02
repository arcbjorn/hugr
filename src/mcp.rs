use crate::context::ContextPack;
use crate::discovery;
use crate::impact;
use crate::indexer;
use crate::store::{Memory, Project, Session, SessionEvent, Store};
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
        "hugr_impact" => tool_impact(&arguments).await,
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
    let branch_state = worktree::inspect(Path::new("."));
    let pack = ContextPack::with_file_candidates_sessions_symbols_tests_branch_and_stale_risks(
        &task,
        file_candidates,
        memories,
        sessions,
        symbols,
        affected_tests,
        Some(branch_state),
        stale_candidates,
    );
    let structured = context_pack_json(&pack);

    Ok(tool_result(pack.render_markdown(), structured))
}

async fn tool_remember(arguments: &Value) -> Result<Value, String> {
    let text = required_string(arguments, "text")?;
    let memory = Store::open_current().remember(&text).await?;
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
            "symbols": summary.symbol_count
        }),
    ))
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
            "hugr_impact",
            "Trace direct indexed references for a file or symbol.",
            &[("target", "string")],
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

    if name == "hugr_impact" {
        props.insert(
            "limit".to_string(),
            json!({
                "type": "integer",
                "minimum": 1,
                "maximum": 500
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
        "text": &memory.text
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
    use super::{handle_line, tools};
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
    }
}
