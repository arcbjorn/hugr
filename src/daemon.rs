use crate::context::json_string;
use crate::indexer;
use crate::store::Store;
use crate::worktree;
use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};
use std::collections::BTreeSet;
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

pub(crate) async fn serve(config: DaemonConfig) -> Result<(), String> {
    let listener = TcpListener::bind(&config.addr)
        .await
        .map_err(|error| format!("failed to bind daemon to {}: {error}", config.addr))?;
    let local_addr = listener.local_addr().map_err(|error| error.to_string())?;
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
                let (stream, peer_addr) = accepted.map_err(|error| error.to_string())?;
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
                    run_background_index(state.clone()).await;
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
                signal.map_err(|error| error.to_string())?;
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
) -> Result<(), String> {
    let mut buffer = [0_u8; 8192];
    let bytes_read = stream
        .read(&mut buffer)
        .await
        .map_err(|error| error.to_string())?;
    if bytes_read == 0 {
        return Ok(());
    }

    let request = String::from_utf8_lossy(&buffer[..bytes_read]);
    let response = response_for_request(&request, peer_addr, &state);
    stream
        .write_all(response.as_bytes())
        .await
        .map_err(|error| error.to_string())?;
    stream.shutdown().await.map_err(|error| error.to_string())
}

async fn run_background_index(state: Arc<DaemonState>) {
    if state.indexing.swap(true, Ordering::SeqCst) {
        state.set_last_index_status("already_running");
        return;
    }

    state.set_last_index_status("running");
    let status = match indexer::index_project(5000).await {
        Ok(summary) => {
            let base_status = format!(
                "ok files={} symbols={}",
                summary.file_count, summary.symbol_count
            );
            match record_discovery_capture(&summary).await {
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

async fn record_discovery_capture(
    summary: &indexer::IndexSummary,
) -> Result<Option<String>, String> {
    Store::open_current()
        .record_session_event_if_active("discovery", &render_discovery_capture_detail(summary))
        .await
        .map(|event| event.map(|event| event.id))
}

fn render_discovery_capture_detail(summary: &indexer::IndexSummary) -> String {
    let sample_files = if summary.sample_files.is_empty() {
        "none".to_string()
    } else {
        summary.sample_files.join(", ")
    };
    let extra_files = summary
        .file_count
        .saturating_sub(summary.sample_files.len());
    let sample_files = if extra_files == 0 {
        sample_files
    } else {
        format!("{sample_files}, +{extra_files} more")
    };

    format!(
        "indexed files={} symbols={}; sample_files={sample_files}",
        summary.file_count, summary.symbol_count
    )
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
) -> Result<(UnboundedReceiver<Vec<String>>, RecommendedWatcher), String> {
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
        })
        .map_err(|error| error.to_string())?;

    watcher
        .watch(root, RecursiveMode::Recursive)
        .map_err(|error| error.to_string())?;
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
    if let Ok(current_dir) = std::env::current_dir() {
        if let Ok(relative) = path.strip_prefix(current_dir) {
            return relative.display().to_string();
        }
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

fn response_for_request(request: &str, peer_addr: SocketAddr, state: &DaemonState) -> String {
    let Some((method, path)) = request_line_parts(request) else {
        return http_response(400, "application/json", r#"{"error":"bad_request"}"#);
    };

    if method != "GET" {
        return http_response(405, "application/json", r#"{"error":"method_not_allowed"}"#);
    }

    match path {
        "/health" => http_response(200, "application/json", &render_health_json()),
        "/status" => http_response(
            200,
            "application/json",
            &render_status_json(peer_addr, state),
        ),
        _ => http_response(404, "application/json", r#"{"error":"not_found"}"#),
    }
}

fn request_line_parts(request: &str) -> Option<(&str, &str)> {
    let mut parts = request.lines().next()?.split_whitespace();
    let method = parts.next()?;
    let path = parts.next()?;
    Some((method, path))
}

fn render_health_json() -> String {
    r#"{"status":"ok","service":"hugr-daemon"}"#.to_string()
}

fn render_status_json(peer_addr: SocketAddr, state: &DaemonState) -> String {
    let store = Store::open_current();
    let current_dir = std::env::current_dir()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|_| "unknown".to_string());

    format!(
        "{{\"status\":\"running\",\"service\":\"hugr-daemon\",\"peer_addr\":{},\"current_dir\":{},\"store_exists\":{},\"store_root\":{},\"storage\":{},\"watcher_enabled\":{},\"indexing\":{},\"last_index_status\":{},\"memory_job_running\":{},\"last_memory_job_status\":{},\"session_observation_running\":{},\"last_session_observation_status\":{},\"session_promotion_running\":{},\"last_session_promotion_status\":{}}}",
        json_string(&peer_addr.to_string()),
        json_string(&current_dir),
        store.exists(),
        json_string(&store.root().display().to_string()),
        json_string(&store.storage_summary()),
        state.watcher_enabled.load(Ordering::SeqCst),
        state.indexing.load(Ordering::SeqCst),
        json_string(&state.last_index_status()),
        state.memory_job_running.load(Ordering::SeqCst),
        json_string(&state.last_memory_job_status()),
        state.session_observation_running.load(Ordering::SeqCst),
        json_string(&state.last_session_observation_status()),
        state.session_promotion_running.load(Ordering::SeqCst),
        json_string(&state.last_session_promotion_status())
    )
}

fn http_response(status_code: u16, content_type: &str, body: &str) -> String {
    let reason = match status_code {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        405 => "Method Not Allowed",
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
            .map(|status| status.clone())
            .unwrap_or_else(|_| "unavailable".to_string())
    }

    fn set_last_memory_job_status(&self, status: &str) {
        if let Ok(mut value) = self.last_memory_job_status.lock() {
            *value = status.to_string();
        }
    }

    fn last_memory_job_status(&self) -> String {
        self.last_memory_job_status
            .lock()
            .map(|status| status.clone())
            .unwrap_or_else(|_| "unavailable".to_string())
    }

    fn set_last_session_observation_status(&self, status: &str) {
        if let Ok(mut value) = self.last_session_observation_status.lock() {
            *value = status.to_string();
        }
    }

    fn last_session_observation_status(&self) -> String {
        self.last_session_observation_status
            .lock()
            .map(|status| status.clone())
            .unwrap_or_else(|_| "unavailable".to_string())
    }

    fn set_last_session_promotion_status(&self, status: &str) {
        if let Ok(mut value) = self.last_session_promotion_status.lock() {
            *value = status.to_string();
        }
    }

    fn last_session_promotion_status(&self) -> String {
        self.last_session_promotion_status
            .lock()
            .map(|status| status.clone())
            .unwrap_or_else(|_| "unavailable".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::{
        DaemonState, is_ignored_watch_path, render_discovery_capture_detail,
        render_session_observation_detail, request_line_parts, response_for_request,
    };
    use crate::indexer::IndexSummary;
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
    fn health_response_is_json() {
        let response = response_for_request(
            "GET /health HTTP/1.1\r\nHost: localhost\r\n\r\n",
            local_peer_addr(),
            &DaemonState::default(),
        );

        assert!(response.starts_with("HTTP/1.1 200 OK"));
        assert!(response.contains("Content-Type: application/json"));
        assert!(response.ends_with(r#"{"status":"ok","service":"hugr-daemon"}"#));
    }

    #[test]
    fn unknown_route_returns_not_found() {
        let response = response_for_request(
            "GET /missing HTTP/1.1\r\nHost: localhost\r\n\r\n",
            local_peer_addr(),
            &DaemonState::default(),
        );

        assert!(response.starts_with("HTTP/1.1 404 Not Found"));
        assert!(response.ends_with(r#"{"error":"not_found"}"#));
    }

    #[test]
    fn status_response_includes_watcher_state() {
        let state = DaemonState::default();
        state.watcher_enabled.store(true, Ordering::SeqCst);
        state.set_last_index_status("watching");
        state.set_last_memory_job_status(
            "ok active=1 retired=0 duplicate_groups=0 stale_candidates=0",
        );
        state.set_last_session_observation_status("ok event=evt_1_0");
        state.set_last_session_promotion_status("ok session=ses_1 memory=mem_1 facts=2");

        let response = response_for_request(
            "GET /status HTTP/1.1\r\nHost: localhost\r\n\r\n",
            local_peer_addr(),
            &state,
        );

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
    fn discovery_capture_detail_includes_index_summary() {
        let detail = render_discovery_capture_detail(&IndexSummary {
            file_count: 14,
            symbol_count: 7,
            sample_files: vec!["src/lib.rs".to_string(), "src/store.rs".to_string()],
        });

        assert!(detail.contains("indexed files=14 symbols=7"));
        assert!(detail.contains("sample_files=src/lib.rs, src/store.rs, +12 more"));
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
