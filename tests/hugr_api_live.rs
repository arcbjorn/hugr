use std::fs;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const TOKEN: &str = "secret-token";

struct DaemonProcess {
    child: Child,
}

impl Drop for DaemonProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
#[ignore = "binds localhost and spawns a daemon process"]
fn remote_memory_commands_use_hosted_api_without_local_store() {
    let hugr = env!("CARGO_BIN_EXE_hugr");
    let server_dir = temp_workspace("server");
    let client_dir = temp_workspace("client");
    let addr = unused_loopback_addr();
    let api_url = format!("http://{addr}");
    let _daemon = spawn_daemon(hugr, &server_dir, addr);

    let remember = run_remote_hugr(
        hugr,
        &client_dir,
        &api_url,
        &[
            "remember",
            "--source",
            "url:https://example.test/live-memory",
            "live remote memory contract fact",
        ],
    );
    assert!(remember.status.success(), "remember failed: {remember:?}");
    assert!(
        !client_dir.join(".hugr").exists(),
        "remote API memory command should not create a local store"
    );
    write_client_source(&client_dir);

    let project_status = run_remote_hugr(hugr, &client_dir, &api_url, &["project", "status"]);
    assert!(
        project_status.status.success(),
        "project status failed: {project_status:?}"
    );
    let project_status_text = String::from_utf8(project_status.stdout).unwrap();
    assert!(project_status_text.contains("hugr_api_live_client"));

    let index = run_remote_hugr(hugr, &client_dir, &api_url, &["index"]);
    assert!(index.status.success(), "index failed: {index:?}");
    let index_text = String::from_utf8(index.stdout).unwrap();
    assert!(index_text.contains("indexed 1 files"));

    let symbols = run_remote_hugr(
        hugr,
        &client_dir,
        &api_url,
        &["symbols", "--json", "LiveRemoteThing"],
    );
    assert!(symbols.status.success(), "symbols failed: {symbols:?}");
    let symbols_json = String::from_utf8(symbols.stdout).unwrap();
    assert!(symbols_json.contains("LiveRemoteThing"));
    assert!(symbols_json.contains("src/lib.rs"));

    let diagnostic_run = run_remote_hugr(
        hugr,
        &client_dir,
        &api_url,
        &[
            "run",
            "sh",
            "-c",
            "printf 'error[E0425]: cannot find value LiveRemoteThing in this scope\n  --> src/lib.rs:2:5\n' >&2; exit 1",
        ],
    );
    assert!(
        !diagnostic_run.status.success(),
        "diagnostic command should fail: {diagnostic_run:?}"
    );
    let diagnostic_stderr = String::from_utf8(diagnostic_run.stderr).unwrap();
    assert!(diagnostic_stderr.contains("error[E0425]"));

    let session_start = run_remote_hugr(
        hugr,
        &client_dir,
        &api_url,
        &["session", "start", "live remote session"],
    );
    assert!(
        session_start.status.success(),
        "session start failed: {session_start:?}"
    );
    let session_event = run_remote_hugr(
        hugr,
        &client_dir,
        &api_url,
        &[
            "session",
            "event",
            "note",
            "LiveRemoteThing was indexed remotely",
        ],
    );
    assert!(
        session_event.status.success(),
        "session event failed: {session_event:?}"
    );
    let session_end = run_remote_hugr(
        hugr,
        &client_dir,
        &api_url,
        &["session", "end", "LiveRemoteThing session complete"],
    );
    assert!(
        session_end.status.success(),
        "session end failed: {session_end:?}"
    );

    let session_promote = run_remote_hugr(
        hugr,
        &client_dir,
        &api_url,
        &["session", "promote", "--json"],
    );
    assert!(
        session_promote.status.success(),
        "session promote failed: {session_promote:?}"
    );
    let session_promote_json = String::from_utf8(session_promote.stdout).unwrap();
    assert!(session_promote_json.contains("LiveRemoteThing session complete"));
    assert!(session_promote_json.contains("session_promotion"));
    assert!(
        !client_dir.join(".hugr").exists(),
        "remote API session promotion should not create a local store"
    );

    let recall_promoted = run_remote_hugr(
        hugr,
        &client_dir,
        &api_url,
        &["recall", "--json", "LiveRemoteThing session complete"],
    );
    assert!(
        recall_promoted.status.success(),
        "promoted recall failed: {recall_promoted:?}"
    );
    let recall_promoted_json = String::from_utf8(recall_promoted.stdout).unwrap();
    assert!(recall_promoted_json.contains("LiveRemoteThing session complete"));
    assert!(recall_promoted_json.contains("session_promotion"));

    let context = run_remote_hugr(
        hugr,
        &client_dir,
        &api_url,
        &["context", "--json", "LiveRemoteThing"],
    );
    assert!(context.status.success(), "context failed: {context:?}");
    let context_json = String::from_utf8(context.stdout).unwrap();
    assert!(context_json.contains("LiveRemoteThing"));
    assert!(context_json.contains(r#""diagnostics""#));
    assert!(context_json.contains("E0425"));
    assert!(context_json.contains("src/lib.rs"));
    assert!(
        context_json.contains("indexed remotely") || context_json.contains("session complete"),
        "context should include remote session facts: {context_json}"
    );

    let impact = run_remote_hugr(
        hugr,
        &client_dir,
        &api_url,
        &["impact", "--json", "LiveRemoteThing"],
    );
    assert!(impact.status.success(), "impact failed: {impact:?}");
    let impact_json = String::from_utf8(impact.stdout).unwrap();
    assert!(impact_json.contains("LiveRemoteThing"));
    assert!(
        !client_dir.join(".hugr").exists(),
        "remote API non-memory commands should not create a local store"
    );

    let recall = run_remote_hugr(
        hugr,
        &client_dir,
        &api_url,
        &["recall", "--json", "live remote memory"],
    );
    assert!(recall.status.success(), "recall failed: {recall:?}");
    let recall_json = String::from_utf8(recall.stdout).unwrap();
    assert!(recall_json.contains("live remote memory contract fact"));

    let forget = run_remote_hugr(
        hugr,
        &client_dir,
        &api_url,
        &["forget", "--json", "memory contract fact"],
    );
    assert!(forget.status.success(), "forget failed: {forget:?}");
    let forget_json = String::from_utf8(forget.stdout).unwrap();
    assert!(forget_json.contains(r#""forgotten_count":1"#));

    let recall_after_forget = run_remote_hugr(
        hugr,
        &client_dir,
        &api_url,
        &["recall", "--json", "memory contract fact"],
    );
    assert!(
        recall_after_forget.status.success(),
        "recall after forget failed: {recall_after_forget:?}"
    );
    let recall_after_forget_json = String::from_utf8(recall_after_forget.stdout).unwrap();
    assert!(recall_after_forget_json.contains(r#""memories":[]"#));

    let history = Command::new(hugr)
        .args(["sync", "history", "--json"])
        .current_dir(&server_dir)
        .output()
        .expect("sync history should run");
    assert!(history.status.success(), "history failed: {history:?}");
    assert_eq!(
        String::from_utf8(history.stdout).unwrap().trim(),
        r#"{"runs":[]}"#
    );
}

fn spawn_daemon(hugr: &str, server_dir: &Path, addr: SocketAddr) -> DaemonProcess {
    let child = Command::new(hugr)
        .args(["daemon", "--addr", &addr.to_string()])
        .current_dir(server_dir)
        .env("HUGR_API_TOKEN", TOKEN)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("daemon should spawn");
    wait_for_health(addr);
    DaemonProcess { child }
}

fn run_remote_hugr(hugr: &str, dir: &Path, api_url: &str, args: &[&str]) -> std::process::Output {
    Command::new(hugr)
        .args(args)
        .current_dir(dir)
        .env("HUGR_STORAGE_MODE", "remote")
        .env("HUGR_SYNC_BACKEND", "hugr_api")
        .env("HUGR_API_URL", api_url)
        .env("HUGR_API_TOKEN", TOKEN)
        .output()
        .expect("hugr command should run")
}

fn wait_for_health(addr: SocketAddr) {
    for _ in 0..50 {
        if health_ok(addr) {
            return;
        }
        thread::sleep(Duration::from_millis(100));
    }
    panic!("daemon did not become healthy at {addr}");
}

fn health_ok(addr: SocketAddr) -> bool {
    let Ok(mut stream) = TcpStream::connect_timeout(&addr, Duration::from_millis(100)) else {
        return false;
    };
    if stream
        .write_all(b"GET /health HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .is_err()
    {
        return false;
    }
    let mut response = String::new();
    stream.read_to_string(&mut response).is_ok() && response.contains(r#""status":"ok""#)
}

fn unused_loopback_addr() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("loopback should bind");
    let addr = listener
        .local_addr()
        .expect("loopback addr should be readable");
    drop(listener);
    addr
}

fn temp_workspace(label: &str) -> PathBuf {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time should be after unix epoch")
        .as_nanos();
    let path = std::env::temp_dir().join(format!("hugr_api_live_{label}_{unique}"));
    fs::create_dir_all(&path).expect("temp workspace should be created");
    path
}

fn write_client_source(client_dir: &Path) {
    let src_dir = client_dir.join("src");
    fs::create_dir_all(&src_dir).expect("src dir should be created");
    fs::write(
        src_dir.join("lib.rs"),
        "pub struct LiveRemoteThing;\n\npub fn make_live_remote_thing() -> LiveRemoteThing {\n    LiveRemoteThing\n}\n",
    )
    .expect("source file should be written");
}

#[test]
fn replace_symbol_edits_local_source_and_refreshes_index() {
    let hugr = env!("CARGO_BIN_EXE_hugr");
    let workspace = temp_workspace("replace_symbol");
    let src_dir = workspace.join("src");
    fs::create_dir_all(&src_dir).expect("src dir should be created");
    let source = src_dir.join("lib.rs");
    fs::write(
        &source,
        "pub struct Registry;\n\npub fn greet() -> u8 {\n    1\n}\n\npub fn other() {}\n",
    )
    .expect("source file should be written");

    let init = run_local_hugr(hugr, &workspace, &["init"]);
    assert!(init.status.success(), "init failed: {init:?}");

    let replace = run_local_hugr(
        hugr,
        &workspace,
        &[
            "replace-symbol",
            "--json",
            "src/lib.rs",
            "greet",
            "--body",
            "pub fn greet() -> u8 {\n    let value = 42;\n    value\n}",
        ],
    );
    assert!(replace.status.success(), "replace failed: {replace:?}");
    let replace_json = String::from_utf8(replace.stdout).unwrap();
    assert!(replace_json.contains("\"name\":\"greet\""));
    assert!(replace_json.contains("\"old_line_end\":5"));
    assert!(replace_json.contains("\"new_line_end\":6"));

    let edited = fs::read_to_string(&source).unwrap();
    assert!(edited.contains("let value = 42;"), "body not replaced: {edited}");
    assert!(edited.contains("pub struct Registry;"), "struct clobbered: {edited}");
    assert!(edited.contains("pub fn other() {}"), "sibling clobbered: {edited}");

    // The index refresh baked into replace-symbol should surface the new line span.
    let symbols = run_local_hugr(hugr, &workspace, &["symbols", "--json", "greet"]);
    assert!(symbols.status.success(), "symbols failed: {symbols:?}");
    let symbols_json = String::from_utf8(symbols.stdout).unwrap();
    assert!(symbols_json.contains("\"line_end\":6"), "stale index: {symbols_json}");

    // A rename attempt is refused and leaves the file untouched.
    let rename = run_local_hugr(
        hugr,
        &workspace,
        &[
            "replace-symbol",
            "src/lib.rs",
            "greet",
            "--body",
            "pub fn renamed() {}",
        ],
    );
    assert!(!rename.status.success(), "rename should fail: {rename:?}");
    let rename_stderr = String::from_utf8(rename.stderr).unwrap();
    assert!(rename_stderr.contains("does not define"), "unexpected error: {rename_stderr}");
    let after_refusal = fs::read_to_string(&source).unwrap();
    assert_eq!(after_refusal, edited, "refused edit must not modify the file");

    let _ = fs::remove_dir_all(&workspace);
}

fn run_local_hugr(hugr: &str, dir: &Path, args: &[&str]) -> std::process::Output {
    Command::new(hugr)
        .args(args)
        .current_dir(dir)
        .output()
        .expect("hugr command should run")
}
