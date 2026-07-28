use crate::error::{Error, Result};
use crate::indexer;
use crate::store::{
    HUGR_API_CONTRACT_VERSION, Store, SyncApiTablePayload, SyncConflictSummary, SyncExecutionPlan,
    SyncPullResult, SyncPushResult, SyncRunHistory, SyncTableResult,
};
use crate::worktree;
use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::env;
use std::net::SocketAddr;
use std::path::{Component, Path};
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};
use tokio::time::{Duration, Instant, MissedTickBehavior};

pub(crate) const DEFAULT_DAEMON_ADDR: &str = "127.0.0.1:5874";
const INDEX_DEBOUNCE: Duration = Duration::from_millis(750);
const IDLE_DEBOUNCE: Duration = Duration::from_secs(60 * 60 * 24 * 365);
const MEMORY_JOB_INTERVAL: Duration = Duration::from_secs(15 * 60);
const SESSION_PROMOTION_JOB_INTERVAL: Duration = Duration::from_secs(5 * 60);
const MAX_HTTP_REQUEST_BYTES: usize = 1024 * 1024;
/// Ceiling on how long one client may take to deliver a complete request.
/// Without it a connection that opens and then stalls pins a task forever,
/// so a handful of idle peers can accumulate until the daemon runs out of
/// descriptors.
const HTTP_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DaemonConfig {
    pub addr: String,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            addr: DEFAULT_DAEMON_ADDR.to_string(),
        }
    }
}

pub(crate) async fn serve(config: DaemonConfig) -> Result<()> {
    let listener = TcpListener::bind(&config.addr).await.map_err(|error| {
        Error::with_source(
            format!("failed to bind daemon to {}: {error}", config.addr),
            error,
        )
    })?;
    let local_addr = listener.local_addr()?;
    let state = Arc::new(DaemonState::default());
    let (mut watcher_events, _watcher) = start_file_watcher(Path::new("."), state.clone())?;
    let debounce = tokio::time::sleep(IDLE_DEBOUNCE);
    tokio::pin!(debounce);
    let mut memory_jobs = tokio::time::interval(MEMORY_JOB_INTERVAL);
    memory_jobs.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut session_promotion_jobs = tokio::time::interval(SESSION_PROMOTION_JOB_INTERVAL);
    session_promotion_jobs.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut index_pending = false;
    let mut pending_paths = BTreeSet::new();

    println!("Hugr daemon listening on http://{local_addr}");

    loop {
        tokio::select! {
            accepted = listener.accept() => {
                // A single failed accept (descriptor exhaustion, a client that
                // vanished mid-handshake) must not take the daemon down: file
                // watching, indexing, and the maintenance jobs keep running.
                let (stream, peer_addr) = match accepted {
                    Ok(accepted) => accepted,
                    Err(error) => {
                        eprintln!("daemon failed to accept connection: {error}");
                        continue;
                    }
                };
                let state = state.clone();
                tokio::spawn(async move {
                    if let Err(error) = handle_client(stream, peer_addr, state).await {
                        eprintln!("daemon request failed: {error}");
                    }
                });
            }
            Some(paths) = watcher_events.recv() => {
                index_pending = true;
                pending_paths.extend(paths);
                state.set_last_index_status("pending");
                debounce.as_mut().reset(Instant::now() + INDEX_DEBOUNCE);
            }
            _ = &mut debounce, if index_pending => {
                index_pending = false;
                debounce.as_mut().reset(Instant::now() + IDLE_DEBOUNCE);
                let state = state.clone();
                let observed_paths = pending_paths.iter().cloned().collect::<Vec<_>>();
                pending_paths.clear();
                tokio::spawn(async move {
                    run_background_index(state.clone(), observed_paths.clone()).await;
                    run_session_observation(state, observed_paths).await;
                });
            }
            _ = memory_jobs.tick() => {
                let state = state.clone();
                tokio::spawn(async move {
                    run_memory_maintenance_job(state).await;
                });
            }
            _ = session_promotion_jobs.tick() => {
                let state = state.clone();
                tokio::spawn(async move {
                    run_session_promotion_job(state).await;
                });
            }
            signal = tokio::signal::ctrl_c() => {
                signal?;
                println!("Hugr daemon shutting down");
                return Ok(());
            }
        }
    }
}

async fn handle_client(
    mut stream: TcpStream,
    peer_addr: SocketAddr,
    state: Arc<DaemonState>,
) -> Result<()> {
    let request = tokio::time::timeout(HTTP_REQUEST_TIMEOUT, read_http_request(&mut stream))
        .await
        .map_err(|_| Error::msg("timed out reading HTTP request"))??;
    if request.is_empty() {
        return Ok(());
    }

    let response = response_for_request(&request, peer_addr, &state).await;
    stream.write_all(response.as_bytes()).await?;
    stream.shutdown().await.map_err(Error::from)
}

async fn read_http_request(stream: &mut TcpStream) -> Result<String> {
    let mut buffer = Vec::new();
    let mut chunk = [0_u8; 8192];

    loop {
        let bytes_read = stream.read(&mut chunk).await?;
        if bytes_read == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..bytes_read]);
        if buffer.len() > MAX_HTTP_REQUEST_BYTES {
            return Err(Error::msg("HTTP request exceeded maximum size"));
        }
        if let Some(expected_len) = expected_http_request_len(&buffer)?
            && buffer.len() >= expected_len
        {
            break;
        }
    }

    String::from_utf8(buffer).map_err(Error::from)
}

fn expected_http_request_len(buffer: &[u8]) -> Result<Option<usize>> {
    let Some((header_end, separator_len)) = http_header_end(buffer) else {
        return Ok(None);
    };
    let headers = std::str::from_utf8(&buffer[..header_end])?;
    let content_length = headers
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>())
        })
        .transpose()?
        .unwrap_or(0);
    Ok(Some(header_end + separator_len + content_length))
}

fn http_header_end(buffer: &[u8]) -> Option<(usize, usize)> {
    buffer
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|index| (index, 4))
        .or_else(|| {
            buffer
                .windows(2)
                .position(|window| window == b"\n\n")
                .map(|index| (index, 2))
        })
}

async fn run_background_index(state: Arc<DaemonState>, changed_paths: Vec<String>) {
    if state.indexing.swap(true, Ordering::SeqCst) {
        state.set_last_index_status("already_running");
        return;
    }

    state.set_last_index_status("running");
    let status = match indexer::refresh_paths(5000, &changed_paths).await {
        Ok(summary) => {
            let mut base_status = format!(
                "ok reparsed={} reference_files={} symbols={}",
                summary.reparsed_files, summary.reference_files, summary.symbol_count
            );
            if !summary.pruned.is_empty() {
                base_status.push_str(&format!(
                    " pruned_files={} pruned_symbols={} pruned_references={}",
                    summary.pruned.missing_paths, summary.pruned.symbols, summary.pruned.references
                ));
            }
            match record_refresh_capture(&summary).await {
                Ok(Some(event_id)) => format!("{base_status} discovery_event={event_id}"),
                Ok(None) => format!("{base_status} discovery=skipped"),
                Err(error) => format!("{base_status} discovery_error={error}"),
            }
        }
        Err(error) => format!("error: {error}"),
    };
    state.set_last_index_status(&status);
    state.indexing.store(false, Ordering::SeqCst);
}

async fn record_refresh_capture(summary: &indexer::RefreshSummary) -> Result<Option<String>> {
    Store::open_current()
        .record_session_event_if_active("discovery", &render_refresh_capture_detail(summary))
        .await
        .map(|event| event.map(|event| event.id))
}

fn render_refresh_capture_detail(summary: &indexer::RefreshSummary) -> String {
    let mut detail = format!(
        "incremental index reparsed_files={} reference_files={} symbols={}",
        summary.reparsed_files, summary.reference_files, summary.symbol_count
    );
    if !summary.pruned.is_empty() {
        detail.push_str(&format!(
            "; pruned_files={} pruned_symbols={} pruned_references={}",
            summary.pruned.missing_paths, summary.pruned.symbols, summary.pruned.references
        ));
    }
    detail
}

async fn run_memory_maintenance_job(state: Arc<DaemonState>) {
    if state.memory_job_running.swap(true, Ordering::SeqCst) {
        state.set_last_memory_job_status("already_running");
        return;
    }

    state.set_last_memory_job_status("running");
    let store = Store::open_current();
    let status = if store.exists() {
        match store.memory_maintenance_report().await {
            Ok(report) => format!(
                "ok active={} retired={} duplicate_groups={} stale_candidates={}",
                report.active_count,
                report.retired_count,
                report.duplicate_groups.len(),
                report.stale_candidates.len()
            ),
            Err(error) => format!("error: {error}"),
        }
    } else {
        "skipped store_missing".to_string()
    };
    state.set_last_memory_job_status(&status);
    state.memory_job_running.store(false, Ordering::SeqCst);
}

async fn run_session_promotion_job(state: Arc<DaemonState>) {
    if state.session_promotion_running.swap(true, Ordering::SeqCst) {
        state.set_last_session_promotion_status("already_running");
        return;
    }

    state.set_last_session_promotion_status("running");
    let store = Store::open_current();
    let status = if store.exists() {
        match store.promote_next_unpromoted_session().await {
            Ok(Some(result)) => format!(
                "ok session={} memory={} facts={}",
                result.session_id, result.memory.id, result.fact_count
            ),
            Ok(None) => "skipped no_unpromoted_session".to_string(),
            Err(error) => format!("error: {error}"),
        }
    } else {
        "skipped store_missing".to_string()
    };
    state.set_last_session_promotion_status(&status);
    state
        .session_promotion_running
        .store(false, Ordering::SeqCst);
}

async fn run_session_observation(state: Arc<DaemonState>, changed_paths: Vec<String>) {
    if state
        .session_observation_running
        .swap(true, Ordering::SeqCst)
    {
        state.set_last_session_observation_status("already_running");
        return;
    }

    state.set_last_session_observation_status("running");
    let worktree = worktree::inspect(Path::new("."));
    let detail = render_session_observation_detail(&changed_paths, &worktree);
    let store = Store::open_current();
    let status = match store
        .record_session_event_if_active("daemon_observation", &detail)
        .await
    {
        Ok(Some(event)) => format!("ok event={}", event.id),
        Ok(None) => "skipped no_active_session".to_string(),
        Err(error) => format!("error: {error}"),
    };
    state.set_last_session_observation_status(&status);
    state
        .session_observation_running
        .store(false, Ordering::SeqCst);
}

fn render_session_observation_detail(
    changed_paths: &[String],
    worktree: &worktree::WorktreeState,
) -> String {
    let paths = if changed_paths.is_empty() {
        "none".to_string()
    } else {
        changed_paths
            .iter()
            .take(12)
            .cloned()
            .collect::<Vec<_>>()
            .join(", ")
    };
    let extra_paths = changed_paths.len().saturating_sub(12);
    let paths = if extra_paths == 0 {
        paths
    } else {
        format!("{paths}, +{extra_paths} more")
    };
    let changed_file_count = worktree.changed_files.len();
    let branch = worktree.branch.as_deref().unwrap_or("unknown");

    format!(
        "files changed: {paths}; git branch={branch} ahead={} behind={} changed_files={changed_file_count}",
        worktree.ahead, worktree.behind
    )
}

fn start_file_watcher(
    root: &Path,
    state: Arc<DaemonState>,
) -> Result<(UnboundedReceiver<Vec<String>>, RecommendedWatcher)> {
    let (sender, receiver) = unbounded_channel();
    let callback_state = state.clone();
    let mut watcher =
        notify::recommended_watcher(move |event: notify::Result<Event>| match event {
            Ok(event) => {
                let paths = relevant_watch_paths(&event);
                if !paths.is_empty() {
                    let _ = sender.send(paths);
                }
            }
            Err(error) => {
                callback_state.set_last_index_status(&format!("watch_error: {error}"));
            }
        })?;

    watcher.watch(root, RecursiveMode::Recursive)?;
    state.watcher_enabled.store(true, Ordering::SeqCst);
    state.set_last_index_status("watching");
    Ok((receiver, watcher))
}

fn relevant_watch_paths(event: &Event) -> Vec<String> {
    let mut paths = event
        .paths
        .iter()
        .filter(|path| !is_ignored_watch_path(path))
        .map(|path| display_watch_path(path))
        .collect::<Vec<_>>();
    paths.sort();
    paths.dedup();
    paths
}

fn display_watch_path(path: &Path) -> String {
    if let Ok(current_dir) = std::env::current_dir()
        && let Ok(relative) = path.strip_prefix(current_dir)
    {
        return relative.display().to_string();
    }
    path.display().to_string()
}

fn is_ignored_watch_path(path: &Path) -> bool {
    path.components().any(|component| {
        let Component::Normal(name) = component else {
            return false;
        };
        matches!(
            name.to_str(),
            Some(
                ".git"
                    | ".hugr"
                    | "target"
                    | "node_modules"
                    | ".next"
                    | "dist"
                    | "build"
                    | ".DS_Store"
            )
        )
    })
}

async fn response_for_request(request: &str, peer_addr: SocketAddr, state: &DaemonState) -> String {
    response_for_request_with_api_token(request, peer_addr, state, configured_hugr_api_token())
        .await
}

async fn response_for_request_with_api_token(
    request: &str,
    peer_addr: SocketAddr,
    state: &DaemonState,
    api_token: Option<String>,
) -> String {
    let Some(parsed) = parse_http_request(request) else {
        return http_response(400, "application/json", r#"{"error":"bad_request"}"#);
    };

    if parsed.path.starts_with("/v1/memories") {
        return response_for_memory_api_request(&parsed, api_token.as_deref()).await;
    }

    if parsed.path.starts_with("/v1/storage") {
        return response_for_storage_api_request(&parsed, api_token.as_deref()).await;
    }

    if parsed.path.starts_with("/v1/sync/") {
        return response_for_sync_api_request(&parsed, state, api_token.as_deref()).await;
    }

    if parsed.method != "GET" {
        return http_response(405, "application/json", r#"{"error":"method_not_allowed"}"#);
    }

    match parsed.path.as_str() {
        "/health" => http_response(200, "application/json", &render_health_json()),
        "/status" => http_response(
            200,
            "application/json",
            &render_status_json(peer_addr, state),
        ),
        _ => http_response(404, "application/json", r#"{"error":"not_found"}"#),
    }
}

async fn response_for_sync_api_request(
    request: &HttpRequest,
    state: &DaemonState,
    api_token: Option<&str>,
) -> String {
    if let Some(response) = sync_api_auth_failure_response(request, api_token) {
        return response;
    }

    let route = request.path.split('?').next().unwrap_or(&request.path);
    match (request.method.as_str(), route) {
        ("GET", "/v1/sync/status") => match Store::open_current().sync_execution_plan() {
            Ok(plan) => http_response(
                200,
                "application/json",
                &render_sync_api_status_json(&plan, state),
            ),
            Err(error) => http_response(
                500,
                "application/json",
                &render_error_json(&error.to_string()),
            ),
        },
        ("GET", "/v1/sync/history") => {
            let limit = sync_history_limit(&request.path);
            match Store::open_current().sync_history(limit).await {
                Ok(history) => http_response(
                    200,
                    "application/json",
                    &render_sync_history_response_json(&history),
                ),
                Err(error) => http_response(
                    500,
                    "application/json",
                    &render_error_json(&error.to_string()),
                ),
            }
        }
        ("POST", "/v1/sync/push") => response_for_sync_api_operation("push", &request.body).await,
        ("POST", "/v1/sync/pull") => response_for_sync_api_operation("pull", &request.body).await,
        (_, "/v1/sync/status" | "/v1/sync/history" | "/v1/sync/push" | "/v1/sync/pull") => {
            http_response(405, "application/json", r#"{"error":"method_not_allowed"}"#)
        }
        _ => http_response(404, "application/json", r#"{"error":"not_found"}"#),
    }
}

async fn response_for_memory_api_request(request: &HttpRequest, api_token: Option<&str>) -> String {
    if let Some(response) = sync_api_auth_failure_response(request, api_token) {
        return response;
    }

    let route = request.path.split('?').next().unwrap_or(&request.path);
    match (request.method.as_str(), route) {
        ("GET", "/v1/memories") => match Store::open_current().api_memory_records().await {
            Ok(records) => http_response(
                200,
                "application/json",
                &render_memory_records_response_json(&records),
            ),
            Err(error) => http_response(
                500,
                "application/json",
                &render_error_json(&error.to_string()),
            ),
        },
        ("POST", "/v1/memories") => response_for_memory_api_apply(&request.body).await,
        (_, "/v1/memories") => {
            http_response(405, "application/json", r#"{"error":"method_not_allowed"}"#)
        }
        _ => http_response(404, "application/json", r#"{"error":"not_found"}"#),
    }
}

async fn response_for_memory_api_apply(body: &str) -> String {
    let request = match parse_memory_api_apply_request(body) {
        Ok(request) => request,
        Err(error) => {
            return http_response(
                400,
                "application/json",
                &render_error_json(&error.to_string()),
            );
        }
    };

    match Store::open_current()
        .apply_api_memory_storage_payloads(&request.table_payloads)
        .await
    {
        Ok((status, payloads)) => http_response(
            200,
            "application/json",
            &render_memory_apply_response_json(&status, &payloads),
        ),
        Err(error) => http_response(
            500,
            "application/json",
            &render_error_json(&error.to_string()),
        ),
    }
}

async fn response_for_storage_api_request(
    request: &HttpRequest,
    api_token: Option<&str>,
) -> String {
    if let Some(response) = sync_api_auth_failure_response(request, api_token) {
        return response;
    }

    let route = request.path.split('?').next().unwrap_or(&request.path);
    match (request.method.as_str(), route) {
        ("GET", "/v1/storage") => match Store::open_current().api_storage_records().await {
            Ok((payloads, session_events, session_promotions)) => http_response(
                200,
                "application/json",
                &render_storage_records_response_json(
                    &payloads,
                    &session_events,
                    &session_promotions,
                ),
            ),
            Err(error) => http_response(
                500,
                "application/json",
                &render_error_json(&error.to_string()),
            ),
        },
        ("POST", "/v1/storage") => response_for_storage_api_apply(&request.body).await,
        (_, "/v1/storage") => {
            http_response(405, "application/json", r#"{"error":"method_not_allowed"}"#)
        }
        _ => http_response(404, "application/json", r#"{"error":"not_found"}"#),
    }
}

async fn response_for_storage_api_apply(body: &str) -> String {
    let request = match parse_storage_api_apply_request(body) {
        Ok(request) => request,
        Err(error) => {
            return http_response(
                400,
                "application/json",
                &render_error_json(&error.to_string()),
            );
        }
    };

    match Store::open_current()
        .apply_api_storage_payloads(
            &request.table_payloads,
            &request.session_events,
            &request.session_promotions,
            &request.replace_code_index_paths,
        )
        .await
    {
        Ok((status, payloads, session_events_table, session_promotions_table)) => http_response(
            200,
            "application/json",
            &render_storage_apply_response_json(
                &status,
                &payloads,
                &session_events_table,
                &session_promotions_table,
            ),
        ),
        Err(error) => http_response(
            500,
            "application/json",
            &render_error_json(&error.to_string()),
        ),
    }
}

async fn response_for_sync_api_operation(operation: &str, body: &str) -> String {
    let request = match parse_sync_api_operation_request(operation, body) {
        Ok(request) => request,
        Err(error) => {
            return http_response(
                400,
                "application/json",
                &render_error_json(&error.to_string()),
            );
        }
    };

    match operation {
        "push" => match Store::open_current()
            .apply_api_sync_push_payloads(&request.table_payloads, request.dry_run)
            .await
        {
            Ok((run_id, status, payloads)) => {
                let tables = payloads
                    .iter()
                    .map(|payload| payload.result.clone())
                    .collect();
                let result = SyncPushResult {
                    run_id,
                    dry_run: request.dry_run,
                    backend: "hugr_api".to_string(),
                    status,
                    tables,
                };
                http_response(
                    200,
                    "application/json",
                    &render_sync_push_response_json(&result, &payloads),
                )
            }
            Err(error) => http_response(
                500,
                "application/json",
                &render_error_json(&error.to_string()),
            ),
        },
        "pull" => match Store::open_current()
            .api_sync_pull_payloads(&request.table_payloads, request.dry_run)
            .await
        {
            Ok((run_id, status, payloads)) => {
                let tables = payloads
                    .iter()
                    .map(|payload| payload.result.clone())
                    .collect();
                let result = SyncPullResult {
                    run_id,
                    dry_run: request.dry_run,
                    backend: "hugr_api".to_string(),
                    status,
                    tables,
                };
                http_response(
                    200,
                    "application/json",
                    &render_sync_pull_response_json(&result, &payloads),
                )
            }
            Err(error) => http_response(
                500,
                "application/json",
                &render_error_json(&error.to_string()),
            ),
        },
        _ => http_response(400, "application/json", r#"{"error":"bad_request"}"#),
    }
}

#[cfg(test)]
fn request_line_parts(request: &str) -> Option<(&str, &str)> {
    let mut parts = request.lines().next()?.split_whitespace();
    let method = parts.next()?;
    let path = parts.next()?;
    Some((method, path))
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct HttpRequest {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
    body: String,
}

fn parse_http_request(request: &str) -> Option<HttpRequest> {
    let (head, body) = request
        .split_once("\r\n\r\n")
        .or_else(|| request.split_once("\n\n"))
        .unwrap_or((request, ""));
    let mut lines = head.lines();
    let mut request_line = lines.next()?.split_whitespace();
    let method = request_line.next()?.to_string();
    let path = request_line.next()?.to_string();
    let headers = lines
        .filter_map(|line| {
            let (name, value) = line.split_once(':')?;
            Some((name.trim().to_ascii_lowercase(), value.trim().to_string()))
        })
        .collect();

    Some(HttpRequest {
        method,
        path,
        headers,
        body: body.to_string(),
    })
}

fn configured_hugr_api_token() -> Option<String> {
    env::var("HUGR_API_TOKEN")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            env::var("HUGR_REMOTE_AUTH_TOKEN")
                .ok()
                .filter(|value| !value.trim().is_empty())
        })
}

fn sync_api_auth_failure_response(
    request: &HttpRequest,
    api_token: Option<&str>,
) -> Option<String> {
    let Some(api_token) = api_token else {
        return Some(http_response(
            503,
            "application/json",
            &render_error_json("hugr API auth token is not configured"),
        ));
    };
    let authorization = request_header(request, "authorization").unwrap_or_default();
    if !constant_time_eq(&authorization, &format!("Bearer {api_token}")) {
        return Some(http_response(
            401,
            "application/json",
            &render_error_json("invalid Hugr API bearer token"),
        ));
    }
    None
}

/// Compares two secrets without short-circuiting on the first differing byte.
/// The daemon is reachable over TCP, so a length-independent early exit would
/// let a caller recover the token one byte at a time from response timing.
fn constant_time_eq(left: &str, right: &str) -> bool {
    let left = left.as_bytes();
    let right = right.as_bytes();
    let mut difference = u8::from(left.len() != right.len());
    for index in 0..left.len().max(right.len()) {
        let left_byte = left.get(index).copied().unwrap_or(0);
        let right_byte = right.get(index).copied().unwrap_or(0);
        difference |= left_byte ^ right_byte;
    }
    difference == 0
}

fn request_header(request: &HttpRequest, name: &str) -> Option<String> {
    request
        .headers
        .iter()
        .find(|(header, _)| header == &name.to_ascii_lowercase())
        .map(|(_, value)| value.clone())
}

#[derive(Debug, Clone, PartialEq)]
struct SyncApiOperationRequest {
    dry_run: bool,
    table_payloads: Vec<SyncApiTablePayload>,
}

#[derive(Debug, Clone, PartialEq)]
struct MemoryApiApplyRequest {
    table_payloads: Vec<SyncApiTablePayload>,
}

#[derive(Debug, Clone, PartialEq)]
struct StorageApiApplyRequest {
    table_payloads: Vec<SyncApiTablePayload>,
    session_events: Vec<Value>,
    session_promotions: Vec<Value>,
    replace_code_index_paths: Vec<String>,
}

fn parse_memory_api_apply_request(body: &str) -> Result<MemoryApiApplyRequest> {
    let value = serde_json::from_str::<Value>(body)
        .map_err(|error| Error::with_source(format!("invalid JSON: {error}"), error))?;
    let contract_version = json_string_field(&value, "contract_version")?;
    if contract_version != HUGR_API_CONTRACT_VERSION {
        return Err(Error::msg(format!(
            "unsupported Hugr API contract version '{contract_version}'"
        )));
    }

    Ok(MemoryApiApplyRequest {
        table_payloads: json_array_field(&value, "tables")?
            .iter()
            .map(parse_sync_api_table_payload)
            .collect::<Result<Vec<_>, _>>()?,
    })
}

fn parse_storage_api_apply_request(body: &str) -> Result<StorageApiApplyRequest> {
    let value = serde_json::from_str::<Value>(body)
        .map_err(|error| Error::with_source(format!("invalid JSON: {error}"), error))?;
    let contract_version = json_string_field(&value, "contract_version")?;
    if contract_version != HUGR_API_CONTRACT_VERSION {
        return Err(Error::msg(format!(
            "unsupported Hugr API contract version '{contract_version}'"
        )));
    }

    Ok(StorageApiApplyRequest {
        table_payloads: json_array_field(&value, "tables")?
            .iter()
            .map(parse_sync_api_table_payload)
            .collect::<Result<Vec<_>, _>>()?,
        session_events: optional_json_array_field(&value, "session_events"),
        session_promotions: optional_json_array_field(&value, "session_promotions"),
        replace_code_index_paths: optional_string_array_field(&value, "replace_code_index_paths")?,
    })
}

fn parse_sync_api_operation_request(
    expected_operation: &str,
    body: &str,
) -> Result<SyncApiOperationRequest> {
    let value = serde_json::from_str::<Value>(body)
        .map_err(|error| Error::with_source(format!("invalid JSON: {error}"), error))?;
    let contract_version = json_string_field(&value, "contract_version")?;
    if contract_version != HUGR_API_CONTRACT_VERSION {
        return Err(Error::msg(format!(
            "unsupported Hugr API contract version '{contract_version}'"
        )));
    }
    let operation = json_string_field(&value, "operation")?;
    if operation != expected_operation {
        return Err(Error::msg(format!(
            "operation '{operation}' does not match route '{expected_operation}'"
        )));
    }

    Ok(SyncApiOperationRequest {
        dry_run: json_bool_field(&value, "dry_run")?,
        table_payloads: json_array_field(&value, "tables")?
            .iter()
            .map(parse_sync_api_table_payload)
            .collect::<Result<Vec<_>, _>>()?,
    })
}

fn parse_sync_api_table_payload(value: &Value) -> Result<SyncApiTablePayload> {
    let records = value
        .get("records")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    Ok(SyncApiTablePayload {
        result: parse_sync_table_result(value)?,
        records,
    })
}

fn parse_sync_table_result(value: &Value) -> Result<SyncTableResult> {
    Ok(SyncTableResult {
        class: json_string_field(value, "class")?,
        table: json_string_field(value, "table")?,
        row_count: json_usize_field(value, "row_count")?,
        inserted_count: json_usize_field(value, "inserted_count")?,
        updated_count: json_usize_field(value, "updated_count")?,
        skipped_count: json_usize_field(value, "skipped_count")?,
        conflict_count: json_usize_field(value, "conflict_count")?,
        executed: json_bool_field(value, "executed")?,
        conflicts: json_array_field(value, "conflicts")?
            .iter()
            .map(parse_sync_conflict_summary)
            .collect::<Result<Vec<_>, _>>()?,
    })
}

fn parse_sync_conflict_summary(value: &Value) -> Result<SyncConflictSummary> {
    Ok(SyncConflictSummary {
        reason: json_string_field(value, "reason")?,
        count: json_usize_field(value, "count")?,
    })
}

fn json_array_field<'a>(value: &'a Value, field: &str) -> Result<&'a Vec<Value>> {
    value
        .get(field)
        .and_then(Value::as_array)
        .ok_or_else(|| Error::msg(format!("Hugr API request missing array field '{field}'")))
}

fn optional_json_array_field(value: &Value, field: &str) -> Vec<Value> {
    value
        .get(field)
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

fn optional_string_array_field(value: &Value, field: &str) -> Result<Vec<String>> {
    let Some(values) = value.get(field) else {
        return Ok(Vec::new());
    };
    let Some(values) = values.as_array() else {
        return Err(Error::msg(format!(
            "Hugr API request field '{field}' must be an array"
        )));
    };
    values
        .iter()
        .map(|value| {
            value.as_str().map(str::to_string).ok_or_else(|| {
                Error::msg(format!(
                    "Hugr API request field '{field}' must contain strings"
                ))
            })
        })
        .collect()
}

fn json_string_field(value: &Value, field: &str) -> Result<String> {
    value
        .get(field)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| Error::msg(format!("Hugr API request missing string field '{field}'")))
}

fn json_usize_field(value: &Value, field: &str) -> Result<usize> {
    let raw = value.get(field).and_then(Value::as_u64).ok_or_else(|| {
        Error::msg(format!(
            "Hugr API request missing unsigned integer field '{field}'"
        ))
    })?;
    usize::try_from(raw).map_err(Error::from)
}

fn json_bool_field(value: &Value, field: &str) -> Result<bool> {
    value
        .get(field)
        .and_then(Value::as_bool)
        .ok_or_else(|| Error::msg(format!("Hugr API request missing boolean field '{field}'")))
}

fn sync_history_limit(path: &str) -> usize {
    path.split_once('?')
        .map(|(_, query)| query)
        .and_then(|query| {
            query.split('&').find_map(|pair| {
                let (key, value) = pair.split_once('=')?;
                (key == "limit")
                    .then(|| value.parse::<usize>().ok())
                    .flatten()
            })
        })
        .unwrap_or(10)
        .max(1)
}

fn render_health_json() -> String {
    r#"{"status":"ok","service":"hugr-daemon"}"#.to_string()
}

fn render_sync_api_status_json(plan: &SyncExecutionPlan, state: &DaemonState) -> String {
    json!({
        "status": "ok",
        "service": "hugr-api",
        "contract_version": HUGR_API_CONTRACT_VERSION,
        "sync": sync_execution_plan_value(plan),
        "daemon": {
            "watcher_enabled": state.watcher_enabled.load(Ordering::SeqCst),
            "indexing": state.indexing.load(Ordering::SeqCst),
            "last_index_status": state.last_index_status(),
            "memory_job_running": state.memory_job_running.load(Ordering::SeqCst),
            "last_memory_job_status": state.last_memory_job_status(),
            "session_observation_running": state.session_observation_running.load(Ordering::SeqCst),
            "last_session_observation_status": state.last_session_observation_status(),
            "session_promotion_running": state.session_promotion_running.load(Ordering::SeqCst),
            "last_session_promotion_status": state.last_session_promotion_status()
        }
    })
    .to_string()
}

fn sync_execution_plan_value(plan: &SyncExecutionPlan) -> Value {
    json!({
        "storage_mode": plan.storage_mode,
        "backend": plan.backend,
        "status": plan.status,
        "local_writes_enabled": plan.local_writes_enabled,
        "remote_configured": plan.remote_configured,
        "remote_auth_configured": plan.remote_auth_configured,
        "remote_reads_enabled": plan.remote_reads_enabled,
        "remote_writes_enabled": plan.remote_writes_enabled,
        "remote_endpoint": plan.remote_endpoint,
        "api_contract_version": plan.api_contract_version,
        "api_routes": plan.api_routes,
        "sync_classes": plan.sync_classes,
        "explicit_opt_in_classes": plan.explicit_opt_in_classes
    })
}

fn render_sync_push_response_json(
    result: &SyncPushResult,
    payloads: &[SyncApiTablePayload],
) -> String {
    json!({
        "run_id": result.run_id,
        "dry_run": result.dry_run,
        "backend": result.backend,
        "status": result.status,
        "tables": payloads.iter().map(sync_api_table_payload_value).collect::<Vec<_>>()
    })
    .to_string()
}

fn render_sync_pull_response_json(
    result: &SyncPullResult,
    payloads: &[SyncApiTablePayload],
) -> String {
    json!({
        "run_id": result.run_id,
        "dry_run": result.dry_run,
        "backend": result.backend,
        "status": result.status,
        "tables": payloads.iter().map(sync_api_table_payload_value).collect::<Vec<_>>()
    })
    .to_string()
}

fn render_sync_history_response_json(history: &[SyncRunHistory]) -> String {
    json!({
        "runs": history.iter().map(sync_run_history_value).collect::<Vec<_>>()
    })
    .to_string()
}

fn render_memory_records_response_json(records: &[Value]) -> String {
    json!({
        "status": "ok",
        "contract_version": HUGR_API_CONTRACT_VERSION,
        "records": records
    })
    .to_string()
}

fn render_memory_apply_response_json(status: &str, payloads: &[SyncApiTablePayload]) -> String {
    json!({
        "status": status,
        "contract_version": HUGR_API_CONTRACT_VERSION,
        "tables": payloads.iter().map(sync_api_table_payload_value).collect::<Vec<_>>()
    })
    .to_string()
}

fn render_storage_records_response_json(
    payloads: &[SyncApiTablePayload],
    session_events: &[Value],
    session_promotions: &[Value],
) -> String {
    json!({
        "status": "ok",
        "contract_version": HUGR_API_CONTRACT_VERSION,
        "tables": payloads.iter().map(sync_api_table_payload_value).collect::<Vec<_>>(),
        "session_events": session_events,
        "session_promotions": session_promotions
    })
    .to_string()
}

fn render_storage_apply_response_json(
    status: &str,
    payloads: &[SyncApiTablePayload],
    session_events_table: &SyncTableResult,
    session_promotions_table: &SyncTableResult,
) -> String {
    json!({
        "status": status,
        "contract_version": HUGR_API_CONTRACT_VERSION,
        "tables": payloads.iter().map(sync_api_table_payload_value).collect::<Vec<_>>(),
        "session_events_table": sync_table_result_value(session_events_table),
        "session_promotions_table": sync_table_result_value(session_promotions_table)
    })
    .to_string()
}

fn sync_run_history_value(run: &SyncRunHistory) -> Value {
    json!({
        "id": run.id,
        "operation": run.operation,
        "backend": run.backend,
        "status": run.status,
        "started_at_ms": run.started_at_ms,
        "ended_at_ms": run.ended_at_ms,
        "tables": run.tables.iter().map(sync_table_result_value).collect::<Vec<_>>()
    })
}

fn sync_api_table_payload_value(payload: &SyncApiTablePayload) -> Value {
    let mut value = sync_table_result_value(&payload.result);
    if let Some(object) = value.as_object_mut() {
        object.insert("records".to_string(), Value::Array(payload.records.clone()));
    }
    value
}

fn sync_table_result_value(table: &SyncTableResult) -> Value {
    json!({
        "class": table.class,
        "table": table.table,
        "row_count": table.row_count,
        "inserted_count": table.inserted_count,
        "updated_count": table.updated_count,
        "skipped_count": table.skipped_count,
        "conflict_count": table.conflict_count,
        "executed": table.executed,
        "conflicts": table.conflicts.iter().map(sync_conflict_summary_value).collect::<Vec<_>>()
    })
}

fn sync_conflict_summary_value(conflict: &SyncConflictSummary) -> Value {
    json!({
        "reason": conflict.reason,
        "count": conflict.count
    })
}

fn render_error_json(message: &str) -> String {
    json!({
        "error": {
            "message": message
        }
    })
    .to_string()
}

/// The `/status` payload. A struct rather than `json!` because
/// `serde_json::Map` sorts its keys and this response has a deliberate
/// order, and because the fields come from several sources rather than one
/// value that could be serialised directly.
#[derive(Serialize)]
struct StatusJson {
    status: &'static str,
    service: &'static str,
    peer_addr: String,
    current_dir: String,
    store_exists: bool,
    store_root: String,
    storage: String,
    watcher_enabled: bool,
    indexing: bool,
    last_index_status: String,
    memory_job_running: bool,
    last_memory_job_status: String,
    session_observation_running: bool,
    last_session_observation_status: String,
    session_promotion_running: bool,
    last_session_promotion_status: String,
}

fn render_status_json(peer_addr: SocketAddr, state: &DaemonState) -> String {
    let store = Store::open_current();
    let current_dir = std::env::current_dir()
        .map_or_else(|_| "unknown".to_string(), |path| path.display().to_string());

    crate::json::render(&StatusJson {
        status: "running",
        service: "hugr-daemon",
        peer_addr: peer_addr.to_string(),
        current_dir,
        store_exists: store.exists(),
        store_root: store.root().display().to_string(),
        storage: store.storage_summary(),
        watcher_enabled: state.watcher_enabled.load(Ordering::SeqCst),
        indexing: state.indexing.load(Ordering::SeqCst),
        last_index_status: state.last_index_status(),
        memory_job_running: state.memory_job_running.load(Ordering::SeqCst),
        last_memory_job_status: state.last_memory_job_status(),
        session_observation_running: state.session_observation_running.load(Ordering::SeqCst),
        last_session_observation_status: state.last_session_observation_status(),
        session_promotion_running: state.session_promotion_running.load(Ordering::SeqCst),
        last_session_promotion_status: state.last_session_promotion_status(),
    })
}

fn http_response(status_code: u16, content_type: &str, body: &str) -> String {
    let reason = match status_code {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        405 => "Method Not Allowed",
        503 => "Service Unavailable",
        _ => "Internal Server Error",
    };

    format!(
        "HTTP/1.1 {status_code} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

#[derive(Debug)]
struct DaemonState {
    watcher_enabled: AtomicBool,
    indexing: AtomicBool,
    memory_job_running: AtomicBool,
    session_observation_running: AtomicBool,
    session_promotion_running: AtomicBool,
    last_index_status: Mutex<String>,
    last_memory_job_status: Mutex<String>,
    last_session_observation_status: Mutex<String>,
    last_session_promotion_status: Mutex<String>,
}

impl Default for DaemonState {
    fn default() -> Self {
        Self {
            watcher_enabled: AtomicBool::new(false),
            indexing: AtomicBool::new(false),
            memory_job_running: AtomicBool::new(false),
            session_observation_running: AtomicBool::new(false),
            session_promotion_running: AtomicBool::new(false),
            last_index_status: Mutex::new("not_started".to_string()),
            last_memory_job_status: Mutex::new("not_started".to_string()),
            last_session_observation_status: Mutex::new("not_started".to_string()),
            last_session_promotion_status: Mutex::new("not_started".to_string()),
        }
    }
}

impl DaemonState {
    fn set_last_index_status(&self, status: &str) {
        if let Ok(mut value) = self.last_index_status.lock() {
            *value = status.to_string();
        }
    }

    fn last_index_status(&self) -> String {
        self.last_index_status
            .lock()
            .map_or_else(|_| "unavailable".to_string(), |status| status.clone())
    }

    fn set_last_memory_job_status(&self, status: &str) {
        if let Ok(mut value) = self.last_memory_job_status.lock() {
            *value = status.to_string();
        }
    }

    fn last_memory_job_status(&self) -> String {
        self.last_memory_job_status
            .lock()
            .map_or_else(|_| "unavailable".to_string(), |status| status.clone())
    }

    fn set_last_session_observation_status(&self, status: &str) {
        if let Ok(mut value) = self.last_session_observation_status.lock() {
            *value = status.to_string();
        }
    }

    fn last_session_observation_status(&self) -> String {
        self.last_session_observation_status
            .lock()
            .map_or_else(|_| "unavailable".to_string(), |status| status.clone())
    }

    fn set_last_session_promotion_status(&self, status: &str) {
        if let Ok(mut value) = self.last_session_promotion_status.lock() {
            *value = status.to_string();
        }
    }

    fn last_session_promotion_status(&self) -> String {
        self.last_session_promotion_status
            .lock()
            .map_or_else(|_| "unavailable".to_string(), |status| status.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::{
        DaemonState, HUGR_API_CONTRACT_VERSION, constant_time_eq, is_ignored_watch_path,
        parse_memory_api_apply_request, parse_storage_api_apply_request,
        render_refresh_capture_detail, render_session_observation_detail, request_line_parts,
        response_for_request_with_api_token,
    };
    use crate::indexer::RefreshSummary;
    use crate::store::PruneSummary;
    use crate::worktree::{ChangedFile, WorktreeState};
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};
    use std::path::Path;
    use std::sync::atomic::Ordering;

    #[test]
    fn parses_http_request_line() {
        let request = "GET /health HTTP/1.1\r\nHost: localhost\r\n\r\n";

        assert_eq!(request_line_parts(request), Some(("GET", "/health")));
    }

    #[test]
    fn constant_time_eq_matches_string_equality() {
        assert!(constant_time_eq("Bearer secret", "Bearer secret"));
        assert!(constant_time_eq("", ""));
        assert!(!constant_time_eq("Bearer secret", "Bearer secreT"));
        assert!(!constant_time_eq("Bearer secret", "Bearer secret "));
        assert!(!constant_time_eq("Bearer secret", ""));
        assert!(!constant_time_eq("", "Bearer secret"));
    }

    #[tokio::test]
    async fn health_response_is_json() {
        let response = response_for_request_with_api_token(
            "GET /health HTTP/1.1\r\nHost: localhost\r\n\r\n",
            local_peer_addr(),
            &DaemonState::default(),
            None,
        )
        .await;

        assert!(response.starts_with("HTTP/1.1 200 OK"));
        assert!(response.contains("Content-Type: application/json"));
        assert!(response.ends_with(r#"{"status":"ok","service":"hugr-daemon"}"#));
    }

    #[tokio::test]
    async fn unknown_route_returns_not_found() {
        let response = response_for_request_with_api_token(
            "GET /missing HTTP/1.1\r\nHost: localhost\r\n\r\n",
            local_peer_addr(),
            &DaemonState::default(),
            None,
        )
        .await;

        assert!(response.starts_with("HTTP/1.1 404 Not Found"));
        assert!(response.ends_with(r#"{"error":"not_found"}"#));
    }

    #[tokio::test]
    async fn status_response_includes_watcher_state() {
        let state = DaemonState::default();
        state.watcher_enabled.store(true, Ordering::SeqCst);
        state.set_last_index_status("watching");
        state.set_last_memory_job_status(
            "ok active=1 retired=0 duplicate_groups=0 stale_candidates=0",
        );
        state.set_last_session_observation_status("ok event=evt_1_0");
        state.set_last_session_promotion_status("ok session=ses_1 memory=mem_1 facts=2");

        let response = response_for_request_with_api_token(
            "GET /status HTTP/1.1\r\nHost: localhost\r\n\r\n",
            local_peer_addr(),
            &state,
            None,
        )
        .await;

        assert!(response.starts_with("HTTP/1.1 200 OK"));
        assert!(response.contains(r#""watcher_enabled":true"#));
        assert!(response.contains(r#""last_index_status":"watching""#));
        assert!(response.contains(r#""memory_job_running":false"#));
        assert!(response.contains(r#""last_memory_job_status":"ok active=1 retired=0 duplicate_groups=0 stale_candidates=0""#));
        assert!(response.contains(r#""session_observation_running":false"#));
        assert!(response.contains(r#""last_session_observation_status":"ok event=evt_1_0""#));
        assert!(response.contains(r#""session_promotion_running":false"#));
        assert!(response.contains(
            r#""last_session_promotion_status":"ok session=ses_1 memory=mem_1 facts=2""#
        ));
    }

    #[tokio::test]
    async fn sync_api_requires_configured_token() {
        let response = response_for_request_with_api_token(
            "GET /v1/sync/status HTTP/1.1\r\nHost: localhost\r\n\r\n",
            local_peer_addr(),
            &DaemonState::default(),
            None,
        )
        .await;

        assert!(response.starts_with("HTTP/1.1 503 Service Unavailable"));
        assert!(response.contains("hugr API auth token is not configured"));
    }

    #[tokio::test]
    async fn memory_api_requires_configured_token() {
        let response = response_for_request_with_api_token(
            "GET /v1/memories HTTP/1.1\r\nHost: localhost\r\n\r\n",
            local_peer_addr(),
            &DaemonState::default(),
            None,
        )
        .await;

        assert!(response.starts_with("HTTP/1.1 503 Service Unavailable"));
        assert!(response.contains("hugr API auth token is not configured"));
    }

    #[test]
    fn parses_memory_api_apply_request() {
        let body = format!(
            r#"{{
                "contract_version": "{HUGR_API_CONTRACT_VERSION}",
                "tables": [
                    {{
                        "class": "memories",
                        "table": "memories",
                        "row_count": 1,
                        "inserted_count": 0,
                        "updated_count": 0,
                        "skipped_count": 0,
                        "conflict_count": 0,
                        "executed": false,
                        "conflicts": [],
                        "records": []
                    }}
                ]
            }}"#
        );

        let parsed = parse_memory_api_apply_request(&body).unwrap();

        assert_eq!(parsed.table_payloads.len(), 1);
        assert_eq!(parsed.table_payloads[0].result.table, "memories");
    }

    #[test]
    fn parses_storage_api_apply_request() {
        let body = format!(
            r#"{{
                "contract_version": "{HUGR_API_CONTRACT_VERSION}",
                "replace_code_index_paths": ["src/lib.rs"],
                "session_events": [
                    {{
                        "id": "evt_1_0",
                        "session_id": "ses_1",
                        "kind": "note",
                        "detail": "indexed",
                        "created_at_ms": 2
                    }}
                ],
                "session_promotions": [
                    {{
                        "session_id": "ses_1",
                        "memory_id": "mem_1",
                        "promoted_at_ms": 3
                    }}
                ],
                "tables": [
                    {{
                        "class": "entities",
                        "table": "code_symbols",
                        "row_count": 1,
                        "inserted_count": 0,
                        "updated_count": 0,
                        "skipped_count": 0,
                        "conflict_count": 0,
                        "executed": false,
                        "conflicts": [],
                        "records": []
                    }}
                ]
            }}"#
        );

        let parsed = parse_storage_api_apply_request(&body).unwrap();

        assert_eq!(parsed.table_payloads.len(), 1);
        assert_eq!(parsed.table_payloads[0].result.table, "code_symbols");
        assert_eq!(parsed.session_events.len(), 1);
        assert_eq!(parsed.session_promotions.len(), 1);
        assert_eq!(parsed.replace_code_index_paths, vec!["src/lib.rs"]);
    }

    #[tokio::test]
    async fn sync_api_rejects_invalid_bearer_token() {
        let response = response_for_request_with_api_token(
            "GET /v1/sync/status HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer wrong\r\n\r\n",
            local_peer_addr(),
            &DaemonState::default(),
            Some("secret-token".to_string()),
        )
        .await;

        assert!(response.starts_with("HTTP/1.1 401 Unauthorized"));
        assert!(response.contains("invalid Hugr API bearer token"));
    }

    #[tokio::test]
    async fn sync_api_status_returns_contract_when_authorized() {
        let response = response_for_request_with_api_token(
            "GET /v1/sync/status HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer secret-token\r\n\r\n",
            local_peer_addr(),
            &DaemonState::default(),
            Some("secret-token".to_string()),
        )
        .await;

        assert!(response.starts_with("HTTP/1.1 200 OK"));
        assert!(response.contains(r#""service":"hugr-api""#));
        assert!(response.contains(&format!(
            r#""contract_version":"{HUGR_API_CONTRACT_VERSION}""#
        )));
        assert!(response.contains(r#""sync":{"#));
    }

    #[tokio::test]
    async fn sync_api_push_accepts_contract_tables() {
        let body = format!(
            r#"{{
                "contract_version": "{HUGR_API_CONTRACT_VERSION}",
                "operation": "push",
                "dry_run": true,
                "tables": [
                    {{
                        "class": "memories",
                        "table": "memories",
                        "row_count": 2,
                        "inserted_count": 0,
                        "updated_count": 0,
                        "skipped_count": 0,
                        "conflict_count": 0,
                        "executed": false,
                        "conflicts": []
                    }}
                ]
            }}"#
        );
        let request = format!(
            "POST /v1/sync/push HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer secret-token\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );

        let response = response_for_request_with_api_token(
            &request,
            local_peer_addr(),
            &DaemonState::default(),
            Some("secret-token".to_string()),
        )
        .await;

        assert!(response.starts_with("HTTP/1.1 200 OK"));
        assert!(response.contains(r#""backend":"hugr_api""#));
        assert!(response.contains(r#""status":"dry_run""#));
        assert!(response.contains(r#""table":"memories""#));
        assert!(response.contains(r#""executed":false"#));
    }

    #[test]
    fn session_observation_detail_includes_files_and_git_state() {
        let worktree = WorktreeState {
            inside_worktree: true,
            root_path: Some("/repo".to_string()),
            branch: Some("feature".to_string()),
            upstream: Some("origin/feature".to_string()),
            ahead: 2,
            behind: 1,
            changed_files: vec![ChangedFile {
                path: "src/lib.rs".to_string(),
                original_path: None,
                staged_status: None,
                unstaged_status: Some("modified".to_string()),
            }],
        };

        let detail = render_session_observation_detail(
            &[
                "src/lib.rs".to_string(),
                "Sources/Plugin/PluginRegistry.swift".to_string(),
            ],
            &worktree,
        );

        assert!(detail.contains("files changed: src/lib.rs"));
        assert!(detail.contains("Sources/Plugin/PluginRegistry.swift"));
        assert!(detail.contains("git branch=feature ahead=2 behind=1 changed_files=1"));
    }

    #[test]
    fn refresh_capture_detail_includes_incremental_counts() {
        let detail = render_refresh_capture_detail(&RefreshSummary {
            reparsed_files: 2,
            reference_files: 3,
            symbol_count: 5,
            pruned: Default::default(),
        });
        assert!(detail.contains("incremental index reparsed_files=2 reference_files=3 symbols=5"));
        assert!(!detail.contains("pruned_files"));
    }

    #[test]
    fn refresh_capture_detail_reports_pruned_rows() {
        let detail = render_refresh_capture_detail(&RefreshSummary {
            reparsed_files: 1,
            reference_files: 1,
            symbol_count: 1,
            pruned: PruneSummary {
                missing_paths: 2,
                discovered_files: 2,
                symbols: 3,
                references: 4,
                test_mappings: 0,
                source_embeddings: 0,
            },
        });
        assert!(detail.contains("pruned_files=2 pruned_symbols=3 pruned_references=4"));
    }

    #[test]
    fn watch_filter_ignores_generated_and_internal_paths() {
        assert!(is_ignored_watch_path(Path::new(".hugr/hugr.db")));
        assert!(is_ignored_watch_path(Path::new("target/debug/hugr")));
        assert!(is_ignored_watch_path(Path::new(".git/index")));
        assert!(!is_ignored_watch_path(Path::new("src/lib.rs")));
    }

    fn local_peer_addr() -> SocketAddr {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 49152)
    }
}
