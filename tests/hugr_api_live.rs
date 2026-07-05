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
    assert!(
        edited.contains("let value = 42;"),
        "body not replaced: {edited}"
    );
    assert!(
        edited.contains("pub struct Registry;"),
        "struct clobbered: {edited}"
    );
    assert!(
        edited.contains("pub fn other() {}"),
        "sibling clobbered: {edited}"
    );

    // The index refresh baked into replace-symbol should surface the new line span.
    let symbols = run_local_hugr(hugr, &workspace, &["symbols", "--json", "greet"]);
    assert!(symbols.status.success(), "symbols failed: {symbols:?}");
    let symbols_json = String::from_utf8(symbols.stdout).unwrap();
    assert!(
        symbols_json.contains("\"line_end\":6"),
        "stale index: {symbols_json}"
    );

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
    assert!(
        rename_stderr.contains("does not define"),
        "unexpected error: {rename_stderr}"
    );
    let after_refusal = fs::read_to_string(&source).unwrap();
    assert_eq!(
        after_refusal, edited,
        "refused edit must not modify the file"
    );

    let _ = fs::remove_dir_all(&workspace);
}

#[test]
fn rename_symbol_updates_definition_references_and_refreshes_index() {
    let hugr = env!("CARGO_BIN_EXE_hugr");
    let workspace = temp_workspace("rename_symbol");
    let src_dir = workspace.join("src");
    fs::create_dir_all(&src_dir).expect("src dir should be created");
    let lib = src_dir.join("lib.rs");
    let main = src_dir.join("main.rs");
    fs::write(&lib, "pub fn run_after_config() -> u8 {\n    1\n}\n")
        .expect("lib source should be written");
    fs::write(
        &main,
        "use crate::lib::run_after_config;\n\nfn main() {\n    let _ = run_after_config();\n}\n",
    )
    .expect("main source should be written");

    let init = run_local_hugr(hugr, &workspace, &["init"]);
    assert!(init.status.success(), "init failed: {init:?}");

    let rename = run_local_hugr(
        hugr,
        &workspace,
        &[
            "rename-symbol",
            "--json",
            "--kind",
            "function",
            "src/lib.rs",
            "run_after_config",
            "run_before_config",
        ],
    );
    assert!(rename.status.success(), "rename failed: {rename:?}");
    let rename_json = String::from_utf8(rename.stdout).unwrap();
    assert!(rename_json.contains("\"old_name\":\"run_after_config\""));
    assert!(rename_json.contains("\"new_name\":\"run_before_config\""));
    assert!(rename_json.contains("\"reference_count\":2"));

    let edited_lib = fs::read_to_string(&lib).unwrap();
    let edited_main = fs::read_to_string(&main).unwrap();
    assert!(edited_lib.contains("run_before_config"));
    assert!(!edited_lib.contains("run_after_config"));
    assert!(edited_main.contains("use crate::lib::run_before_config;"));
    assert!(edited_main.contains("run_before_config();"));
    assert!(!edited_main.contains("run_after_config"));

    let symbols = run_local_hugr(
        hugr,
        &workspace,
        &["symbols", "--json", "run_before_config"],
    );
    assert!(symbols.status.success(), "symbols failed: {symbols:?}");
    let symbols_json = String::from_utf8(symbols.stdout).unwrap();
    assert!(symbols_json.contains("\"name\":\"run_before_config\""));

    let _ = fs::remove_dir_all(&workspace);
}

#[test]
fn move_symbol_moves_unreferenced_definition_and_refreshes_index() {
    let hugr = env!("CARGO_BIN_EXE_hugr");
    let workspace = temp_workspace("move_symbol");
    let src_dir = workspace.join("src");
    fs::create_dir_all(&src_dir).expect("src dir should be created");
    let lib = src_dir.join("lib.rs");
    let helpers = src_dir.join("helpers.rs");
    fs::write(
        &lib,
        "pub fn helper() -> u8 {\n    1\n}\n\npub fn other() {}\n",
    )
    .expect("lib source should be written");
    fs::write(&helpers, "pub fn existing() {}\n").expect("helpers source should be written");

    let init = run_local_hugr(hugr, &workspace, &["init"]);
    assert!(init.status.success(), "init failed: {init:?}");

    let moved = run_local_hugr(
        hugr,
        &workspace,
        &[
            "move-symbol",
            "--json",
            "--kind",
            "function",
            "src/lib.rs",
            "helper",
            "src/helpers.rs",
        ],
    );
    assert!(moved.status.success(), "move failed: {moved:?}");
    let moved_json = String::from_utf8(moved.stdout).unwrap();
    assert!(moved_json.contains("\"source_path\":\"src/lib.rs\""));
    assert!(moved_json.contains("\"destination_path\":\"src/helpers.rs\""));
    assert!(moved_json.contains("\"name\":\"helper\""));

    let edited_lib = fs::read_to_string(&lib).unwrap();
    let edited_helpers = fs::read_to_string(&helpers).unwrap();
    assert!(!edited_lib.contains("pub fn helper"));
    assert!(edited_lib.contains("pub fn other() {}"));
    assert!(edited_helpers.contains("pub fn existing() {}"));
    assert!(edited_helpers.contains("pub fn helper() -> u8"));

    let symbols = run_local_hugr(hugr, &workspace, &["symbols", "--json", "helper"]);
    assert!(symbols.status.success(), "symbols failed: {symbols:?}");
    let symbols_json = String::from_utf8(symbols.stdout).unwrap();
    assert!(symbols_json.contains("\"path\":\"src/helpers.rs\""));

    let _ = fs::remove_dir_all(&workspace);
}

#[test]
fn move_symbol_rewrites_supported_references_and_refreshes_index() {
    let hugr = env!("CARGO_BIN_EXE_hugr");
    let workspace = temp_workspace("move_symbol_rewrite");
    let src_dir = workspace.join("src");
    fs::create_dir_all(&src_dir).expect("src dir should be created");
    let source = src_dir.join("plugin_hooks.rs");
    let destination = src_dir.join("helpers.rs");
    let main = src_dir.join("main.rs");
    fs::write(
        &source,
        "pub fn helper() -> u8 {\n    1\n}\n\npub fn other() {}\n",
    )
    .expect("source file should be written");
    fs::write(&destination, "pub fn existing() {}\n").expect("destination should be written");
    fs::write(
        &main,
        "use crate::plugin_hooks::{helper, other};\n\nfn main() {\n    let _ = helper();\n}\n",
    )
    .expect("main source should be written");

    let init = run_local_hugr(hugr, &workspace, &["init"]);
    assert!(init.status.success(), "init failed: {init:?}");

    let moved = run_local_hugr(
        hugr,
        &workspace,
        &[
            "move-symbol",
            "--json",
            "--rewrite-references",
            "--kind",
            "function",
            "src/plugin_hooks.rs",
            "helper",
            "src/helpers.rs",
        ],
    );
    assert!(moved.status.success(), "move failed: {moved:?}");
    let moved_json = String::from_utf8(moved.stdout).unwrap();
    assert!(moved_json.contains("\"rewritten_reference_count\":1"));

    let edited_source = fs::read_to_string(&source).unwrap();
    let edited_destination = fs::read_to_string(&destination).unwrap();
    let edited_main = fs::read_to_string(&main).unwrap();
    assert!(!edited_source.contains("pub fn helper"));
    assert!(edited_destination.contains("pub fn helper() -> u8"));
    assert!(edited_main.contains("use crate::plugin_hooks::{other};"));
    assert!(edited_main.contains("use crate::helpers::{helper};"));
    assert!(edited_main.contains("helper();"));
    assert!(!edited_main.contains("crate::plugin_hooks::helper"));

    let symbols = run_local_hugr(hugr, &workspace, &["symbols", "--json", "helper"]);
    assert!(symbols.status.success(), "symbols failed: {symbols:?}");
    let symbols_json = String::from_utf8(symbols.stdout).unwrap();
    assert!(symbols_json.contains("\"path\":\"src/helpers.rs\""));

    let _ = fs::remove_dir_all(&workspace);
}

#[test]
fn move_symbol_rewrites_nested_and_aliased_rust_references() {
    let hugr = env!("CARGO_BIN_EXE_hugr");
    let workspace = temp_workspace("move_symbol_nested_alias");
    let src_dir = workspace.join("src");
    fs::create_dir_all(&src_dir).expect("src dir should be created");
    let source = src_dir.join("plugin_hooks.rs");
    let destination = src_dir.join("helpers.rs");
    let main = src_dir.join("main.rs");
    fs::write(
        &source,
        "pub fn helper() -> u8 {\n    1\n}\n\npub fn other() {}\n",
    )
    .expect("source file should be written");
    fs::write(&destination, "pub fn existing() {}\n").expect("destination should be written");
    fs::write(
        &main,
        "use crate::{config::Settings, plugin_hooks::{helper as run_helper, other}, plugin_hooks as hooks};\n\nfn main() {\n    let _ = run_helper();\n    let _ = hooks::helper();\n}\n",
    )
    .expect("main source should be written");

    let init = run_local_hugr(hugr, &workspace, &["init"]);
    assert!(init.status.success(), "init failed: {init:?}");

    let moved = run_local_hugr(
        hugr,
        &workspace,
        &[
            "move-symbol",
            "--json",
            "--rewrite-references",
            "--kind",
            "function",
            "src/plugin_hooks.rs",
            "helper",
            "src/helpers.rs",
        ],
    );
    assert!(moved.status.success(), "move failed: {moved:?}");
    let moved_json = String::from_utf8(moved.stdout).unwrap();
    assert!(moved_json.contains("\"rewritten_reference_count\":2"));

    let edited_main = fs::read_to_string(&main).unwrap();
    assert!(
        edited_main.contains(
            "use crate::{config::Settings, plugin_hooks::{other}, plugin_hooks as hooks};"
        )
    );
    assert!(edited_main.contains("use crate::helpers::{helper as run_helper};"));
    assert!(edited_main.contains("run_helper();"));
    assert!(edited_main.contains("crate::helpers::helper();"));
    assert!(!edited_main.contains("hooks::helper"));

    let _ = fs::remove_dir_all(&workspace);
}

#[test]
fn move_symbol_rewrites_python_references_and_refreshes_index() {
    let hugr = env!("CARGO_BIN_EXE_hugr");
    let workspace = temp_workspace("move_symbol_python_rewrite");
    let source = workspace.join("plugin_hooks.py");
    let destination = workspace.join("helpers.py");
    let main = workspace.join("main.py");
    fs::write(
        &source,
        "def helper():\n    return 1\n\n\ndef other():\n    return 2\n",
    )
    .expect("source file should be written");
    fs::write(&destination, "def existing():\n    return 0\n")
        .expect("destination should be written");
    fs::write(
        &main,
        "from plugin_hooks import helper as run_helper, other\nimport plugin_hooks\n\nvalue = run_helper() + plugin_hooks.helper()\n",
    )
    .expect("main source should be written");

    let init = run_local_hugr(hugr, &workspace, &["init"]);
    assert!(init.status.success(), "init failed: {init:?}");

    let moved = run_local_hugr(
        hugr,
        &workspace,
        &[
            "move-symbol",
            "--json",
            "--rewrite-references",
            "--kind",
            "function",
            "plugin_hooks.py",
            "helper",
            "helpers.py",
        ],
    );
    assert!(moved.status.success(), "move failed: {moved:?}");
    let moved_json = String::from_utf8(moved.stdout).unwrap();
    assert!(moved_json.contains("\"rewritten_reference_count\":3"));

    let edited_source = fs::read_to_string(&source).unwrap();
    let edited_destination = fs::read_to_string(&destination).unwrap();
    let edited_main = fs::read_to_string(&main).unwrap();
    assert!(!edited_source.contains("def helper"));
    assert!(edited_source.contains("def other"));
    assert!(edited_destination.contains("def helper():"));
    assert!(edited_main.contains("from plugin_hooks import other"));
    assert!(edited_main.contains("from helpers import helper as run_helper"));
    assert!(edited_main.contains("import helpers"));
    assert!(edited_main.contains("run_helper() + helpers.helper()"));

    let symbols = run_local_hugr(hugr, &workspace, &["symbols", "--json", "helper"]);
    assert!(symbols.status.success(), "symbols failed: {symbols:?}");
    let symbols_json = String::from_utf8(symbols.stdout).unwrap();
    assert!(symbols_json.contains("\"path\":\"helpers.py\""));

    let _ = fs::remove_dir_all(&workspace);
}

#[test]
fn move_symbol_rewrites_javascript_references_and_refreshes_index() {
    let hugr = env!("CARGO_BIN_EXE_hugr");
    let workspace = temp_workspace("move_symbol_javascript_rewrite");
    let source = workspace.join("pluginHooks.js");
    let destination = workspace.join("helpers.js");
    let main = workspace.join("main.js");
    fs::write(
        &source,
        "export function helper() {\n    return 1;\n}\n\nexport function other() {\n    return 2;\n}\n",
    )
    .expect("source file should be written");
    fs::write(
        &destination,
        "export function existing() {\n    return 0;\n}\n",
    )
    .expect("destination should be written");
    fs::write(
        &main,
        "import { helper as runHelper, other } from \"./pluginHooks.js\";\nimport * as hooks from \"./pluginHooks.js\";\n\nconst value = runHelper() + hooks.helper();\n",
    )
    .expect("main source should be written");

    let init = run_local_hugr(hugr, &workspace, &["init"]);
    assert!(init.status.success(), "init failed: {init:?}");

    let moved = run_local_hugr(
        hugr,
        &workspace,
        &[
            "move-symbol",
            "--json",
            "--rewrite-references",
            "--kind",
            "function",
            "pluginHooks.js",
            "helper",
            "helpers.js",
        ],
    );
    assert!(moved.status.success(), "move failed: {moved:?}");
    let moved_json = String::from_utf8(moved.stdout).unwrap();
    assert!(moved_json.contains("\"rewritten_reference_count\":2"));

    let edited_source = fs::read_to_string(&source).unwrap();
    let edited_destination = fs::read_to_string(&destination).unwrap();
    let edited_main = fs::read_to_string(&main).unwrap();
    assert!(!edited_source.contains("function helper"));
    assert!(edited_source.contains("function other"));
    assert!(edited_destination.contains("function helper()"));
    assert!(edited_main.contains("import { other } from \"./pluginHooks.js\";"));
    assert!(edited_main.contains("import { helper as runHelper } from \"./helpers.js\";"));
    assert!(edited_main.contains("import * as hooks from \"./helpers.js\";"));
    assert!(edited_main.contains("runHelper() + hooks.helper()"));

    let symbols = run_local_hugr(hugr, &workspace, &["symbols", "--json", "helper"]);
    assert!(symbols.status.success(), "symbols failed: {symbols:?}");
    let symbols_json = String::from_utf8(symbols.stdout).unwrap();
    assert!(symbols_json.contains("\"path\":\"helpers.js\""));

    let _ = fs::remove_dir_all(&workspace);
}

#[test]
fn move_symbol_allows_go_same_package_references_and_refreshes_index() {
    let hugr = env!("CARGO_BIN_EXE_hugr");
    let workspace = temp_workspace("move_symbol_go_same_package");
    let pkg_dir = workspace.join("plugin");
    fs::create_dir_all(&pkg_dir).expect("package dir should be created");
    let source = pkg_dir.join("hooks.go");
    let destination = pkg_dir.join("helpers.go");
    let caller = pkg_dir.join("caller.go");
    fs::write(
        &source,
        "package plugin\n\nfunc helper() int {\n    return 1\n}\n\nfunc other() int {\n    return 2\n}\n",
    )
    .expect("source file should be written");
    fs::write(
        &destination,
        "package plugin\n\nfunc existing() int {\n    return 0\n}\n",
    )
    .expect("destination should be written");
    fs::write(
        &caller,
        "package plugin\n\nfunc useHelper() int {\n    return helper()\n}\n",
    )
    .expect("caller should be written");

    let init = run_local_hugr(hugr, &workspace, &["init"]);
    assert!(init.status.success(), "init failed: {init:?}");

    let moved = run_local_hugr(
        hugr,
        &workspace,
        &[
            "move-symbol",
            "--json",
            "--rewrite-references",
            "--kind",
            "function",
            "plugin/hooks.go",
            "helper",
            "plugin/helpers.go",
        ],
    );
    assert!(moved.status.success(), "move failed: {moved:?}");
    let moved_json = String::from_utf8(moved.stdout).unwrap();
    assert!(moved_json.contains("\"rewritten_reference_count\":0"));

    let edited_source = fs::read_to_string(&source).unwrap();
    let edited_destination = fs::read_to_string(&destination).unwrap();
    let edited_caller = fs::read_to_string(&caller).unwrap();
    assert!(!edited_source.contains("func helper"));
    assert!(edited_source.contains("func other"));
    assert!(edited_destination.contains("func helper() int"));
    assert!(edited_caller.contains("return helper()"));

    let symbols = run_local_hugr(hugr, &workspace, &["symbols", "--json", "helper"]);
    assert!(symbols.status.success(), "symbols failed: {symbols:?}");
    let symbols_json = String::from_utf8(symbols.stdout).unwrap();
    assert!(symbols_json.contains("\"path\":\"plugin/helpers.go\""));

    let _ = fs::remove_dir_all(&workspace);
}

#[test]
fn move_symbol_allows_java_same_package_type_references_and_refreshes_index() {
    let hugr = env!("CARGO_BIN_EXE_hugr");
    let workspace = temp_workspace("move_symbol_java_same_package");
    let pkg_dir = workspace.join("src/plugin");
    fs::create_dir_all(&pkg_dir).expect("package dir should be created");
    let source = pkg_dir.join("PluginHooks.java");
    let destination = pkg_dir.join("Helper.java");
    let caller = pkg_dir.join("Caller.java");
    fs::write(
        &source,
        "package plugin;\n\nclass Helper {\n    int value() { return 1; }\n}\n\nclass Other {}\n",
    )
    .expect("source file should be written");
    fs::write(&destination, "package plugin;\n\nclass Existing {}\n")
        .expect("destination should be written");
    fs::write(
        &caller,
        "package plugin;\n\nclass Caller {\n    Helper helper = new Helper();\n}\n",
    )
    .expect("caller should be written");

    let init = run_local_hugr(hugr, &workspace, &["init"]);
    assert!(init.status.success(), "init failed: {init:?}");

    let moved = run_local_hugr(
        hugr,
        &workspace,
        &[
            "move-symbol",
            "--json",
            "--rewrite-references",
            "--kind",
            "class",
            "src/plugin/PluginHooks.java",
            "Helper",
            "src/plugin/Helper.java",
        ],
    );
    assert!(moved.status.success(), "move failed: {moved:?}");
    let moved_json = String::from_utf8(moved.stdout).unwrap();
    assert!(moved_json.contains("\"rewritten_reference_count\":0"));

    let edited_source = fs::read_to_string(&source).unwrap();
    let edited_destination = fs::read_to_string(&destination).unwrap();
    let edited_caller = fs::read_to_string(&caller).unwrap();
    assert!(!edited_source.contains("class Helper"));
    assert!(edited_source.contains("class Other"));
    assert!(edited_destination.contains("class Helper"));
    assert!(edited_caller.contains("Helper helper = new Helper();"));

    let symbols = run_local_hugr(hugr, &workspace, &["symbols", "--json", "Helper"]);
    assert!(symbols.status.success(), "symbols failed: {symbols:?}");
    let symbols_json = String::from_utf8(symbols.stdout).unwrap();
    assert!(symbols_json.contains("\"path\":\"src/plugin/Helper.java\""));

    let _ = fs::remove_dir_all(&workspace);
}

#[test]
fn move_symbol_allows_kotlin_same_package_type_references_and_refreshes_index() {
    let hugr = env!("CARGO_BIN_EXE_hugr");
    let workspace = temp_workspace("move_symbol_kotlin_same_package");
    let pkg_dir = workspace.join("src/plugin");
    fs::create_dir_all(&pkg_dir).expect("package dir should be created");
    let source = pkg_dir.join("Hooks.kt");
    let destination = pkg_dir.join("Helper.kt");
    let caller = pkg_dir.join("Caller.kt");
    fs::write(
        &source,
        "package plugin\n\nclass Helper {\n    fun value(): Int = 1\n}\n\nclass Other\n",
    )
    .expect("source file should be written");
    fs::write(&destination, "package plugin\n\nclass Existing\n")
        .expect("destination should be written");
    fs::write(
        &caller,
        "package plugin\n\nclass Caller {\n    val helper = Helper()\n}\n",
    )
    .expect("caller should be written");

    let init = run_local_hugr(hugr, &workspace, &["init"]);
    assert!(init.status.success(), "init failed: {init:?}");

    let moved = run_local_hugr(
        hugr,
        &workspace,
        &[
            "move-symbol",
            "--json",
            "--rewrite-references",
            "--kind",
            "class",
            "src/plugin/Hooks.kt",
            "Helper",
            "src/plugin/Helper.kt",
        ],
    );
    assert!(moved.status.success(), "move failed: {moved:?}");
    let moved_json = String::from_utf8(moved.stdout).unwrap();
    assert!(moved_json.contains("\"rewritten_reference_count\":0"));

    let edited_source = fs::read_to_string(&source).unwrap();
    let edited_destination = fs::read_to_string(&destination).unwrap();
    let edited_caller = fs::read_to_string(&caller).unwrap();
    assert!(!edited_source.contains("class Helper"));
    assert!(edited_source.contains("class Other"));
    assert!(edited_destination.contains("class Helper"));
    assert!(edited_caller.contains("val helper = Helper()"));

    let symbols = run_local_hugr(hugr, &workspace, &["symbols", "--json", "Helper"]);
    assert!(symbols.status.success(), "symbols failed: {symbols:?}");
    let symbols_json = String::from_utf8(symbols.stdout).unwrap();
    assert!(symbols_json.contains("\"path\":\"src/plugin/Helper.kt\""));

    let _ = fs::remove_dir_all(&workspace);
}

#[test]
fn move_symbol_allows_swift_same_module_type_references_and_refreshes_index() {
    let hugr = env!("CARGO_BIN_EXE_hugr");
    let workspace = temp_workspace("move_symbol_swift_same_module");
    let module_dir = workspace.join("Sources/App");
    fs::create_dir_all(&module_dir).expect("module dir should be created");
    let source = module_dir.join("Hooks.swift");
    let destination = module_dir.join("Helper.swift");
    let caller = module_dir.join("Caller.swift");
    fs::write(
        &source,
        "struct Helper {\n    let value = 1\n}\n\nstruct Other {}\n",
    )
    .expect("source file should be written");
    fs::write(&destination, "struct Existing {}\n").expect("destination should be written");
    fs::write(&caller, "struct Caller {\n    let helper = Helper()\n}\n")
        .expect("caller should be written");

    let init = run_local_hugr(hugr, &workspace, &["init"]);
    assert!(init.status.success(), "init failed: {init:?}");

    let moved = run_local_hugr(
        hugr,
        &workspace,
        &[
            "move-symbol",
            "--json",
            "--rewrite-references",
            "--kind",
            "struct",
            "Sources/App/Hooks.swift",
            "Helper",
            "Sources/App/Helper.swift",
        ],
    );
    assert!(moved.status.success(), "move failed: {moved:?}");
    let moved_json = String::from_utf8(moved.stdout).unwrap();
    assert!(moved_json.contains("\"rewritten_reference_count\":0"));

    let edited_source = fs::read_to_string(&source).unwrap();
    let edited_destination = fs::read_to_string(&destination).unwrap();
    let edited_caller = fs::read_to_string(&caller).unwrap();
    assert!(!edited_source.contains("struct Helper"));
    assert!(edited_source.contains("struct Other"));
    assert!(edited_destination.contains("struct Helper"));
    assert!(edited_caller.contains("let helper = Helper()"));

    let symbols = run_local_hugr(hugr, &workspace, &["symbols", "--json", "Helper"]);
    assert!(symbols.status.success(), "symbols failed: {symbols:?}");
    let symbols_json = String::from_utf8(symbols.stdout).unwrap();
    assert!(symbols_json.contains("\"path\":\"Sources/App/Helper.swift\""));

    let _ = fs::remove_dir_all(&workspace);
}

#[test]
fn index_prunes_symbols_for_deleted_files() {
    let hugr = env!("CARGO_BIN_EXE_hugr");
    let workspace = temp_workspace("index_prune_deleted");
    let src_dir = workspace.join("src");
    fs::create_dir_all(&src_dir).expect("src dir should be created");
    let kept = src_dir.join("kept.rs");
    let doomed = src_dir.join("doomed.rs");
    fs::write(&kept, "pub fn kept_symbol() -> i32 {\n    1\n}\n").expect("kept should be written");
    fs::write(
        &doomed,
        "pub fn doomed_symbol() -> i32 {\n    kept_symbol()\n}\n",
    )
    .expect("doomed should be written");

    let init = run_local_hugr(hugr, &workspace, &["init"]);
    assert!(init.status.success(), "init failed: {init:?}");

    let first_index = run_local_hugr(hugr, &workspace, &["index"]);
    assert!(
        first_index.status.success(),
        "first index failed: {first_index:?}"
    );

    let symbols_before = run_local_hugr(hugr, &workspace, &["symbols", "--json", "doomed_symbol"]);
    assert!(symbols_before.status.success());
    let symbols_before_json = String::from_utf8(symbols_before.stdout).unwrap();
    assert!(
        symbols_before_json.contains("doomed_symbol"),
        "expected doomed_symbol indexed: {symbols_before_json}"
    );

    fs::remove_file(&doomed).expect("doomed file should be removed");

    let second_index = run_local_hugr(hugr, &workspace, &["index"]);
    assert!(
        second_index.status.success(),
        "second index failed: {second_index:?}"
    );
    let second_index_text = String::from_utf8(second_index.stdout).unwrap();
    assert!(
        second_index_text.contains("pruned:"),
        "expected prune report after deletion: {second_index_text}"
    );

    let symbols_after = run_local_hugr(hugr, &workspace, &["symbols", "--json", "doomed_symbol"]);
    assert!(symbols_after.status.success());
    let symbols_after_json = String::from_utf8(symbols_after.stdout).unwrap();
    assert!(
        !symbols_after_json.contains("src/doomed.rs"),
        "deleted file symbols should be pruned: {symbols_after_json}"
    );

    // The surviving file's symbol is still indexed.
    let kept_symbols = run_local_hugr(hugr, &workspace, &["symbols", "--json", "kept_symbol"]);
    assert!(kept_symbols.status.success());
    let kept_symbols_json = String::from_utf8(kept_symbols.stdout).unwrap();
    assert!(
        kept_symbols_json.contains("src/kept.rs"),
        "surviving symbol should remain: {kept_symbols_json}"
    );

    let _ = fs::remove_dir_all(&workspace);
}

#[test]
fn incremental_index_refreshes_only_changed_paths() {
    let hugr = env!("CARGO_BIN_EXE_hugr");
    let workspace = temp_workspace("incremental_index");
    let src_dir = workspace.join("src");
    fs::create_dir_all(&src_dir).expect("src dir should be created");
    let defs = src_dir.join("defs.rs");
    let caller = src_dir.join("caller.rs");
    // defs defines `provider_symbol`; caller references it. Only defs will change.
    fs::write(&defs, "pub fn provider_symbol() -> i32 {\n    1\n}\n").expect("defs written");
    fs::write(
        &caller,
        "pub fn use_provider() -> i32 {\n    provider_symbol()\n}\n",
    )
    .expect("caller written");

    let init = run_local_hugr(hugr, &workspace, &["init"]);
    assert!(init.status.success(), "init failed: {init:?}");
    let first_index = run_local_hugr(hugr, &workspace, &["index"]);
    assert!(
        first_index.status.success(),
        "full index failed: {first_index:?}"
    );

    // Sanity: the inbound reference from caller to provider_symbol is indexed.
    let impact_before = run_local_hugr(hugr, &workspace, &["impact", "--json", "provider_symbol"]);
    assert!(impact_before.status.success());
    let impact_before_json = String::from_utf8(impact_before.stdout).unwrap();
    assert!(
        impact_before_json.contains("src/caller.rs"),
        "expected inbound reference indexed: {impact_before_json}"
    );

    // Rename the provider symbol in defs.rs only. caller.rs is untouched, so its
    // reference to the old name becomes dangling and must be re-extracted away
    // by the incremental refresh (caller is an inbound-reference source).
    fs::write(&defs, "pub fn provider_symbol_v2() -> i32 {\n    2\n}\n").expect("defs rewritten");

    let incremental = run_local_hugr(hugr, &workspace, &["index", "--paths", "src/defs.rs"]);
    assert!(
        incremental.status.success(),
        "incremental index failed: {incremental:?}"
    );
    let incremental_text = String::from_utf8(incremental.stdout).unwrap();
    assert!(
        incremental_text.contains("reparsed 1 files"),
        "expected single-file reparse: {incremental_text}"
    );
    // caller is re-scanned as an inbound source, so reference_files > 1.
    assert!(
        incremental_text.contains("rescanned 2 reference files"),
        "expected inbound source rescanned: {incremental_text}"
    );

    // New symbol name is indexed.
    let new_symbols = run_local_hugr(
        hugr,
        &workspace,
        &["symbols", "--json", "provider_symbol_v2"],
    );
    assert!(new_symbols.status.success());
    let new_symbols_json = String::from_utf8(new_symbols.stdout).unwrap();
    assert!(
        new_symbols_json.contains("src/defs.rs"),
        "renamed symbol should be indexed: {new_symbols_json}"
    );

    // Old symbol name is gone from the symbol index.
    let old_symbols = run_local_hugr(hugr, &workspace, &["symbols", "--json", "provider_symbol"]);
    assert!(old_symbols.status.success());
    let old_symbols_json = String::from_utf8(old_symbols.stdout).unwrap();
    assert!(
        !old_symbols_json.contains("\"name\":\"provider_symbol\""),
        "old symbol should be pruned from index: {old_symbols_json}"
    );

    // The dangling inbound reference from caller.rs to the old name is gone,
    // proving the inbound-reference source was correctly re-scanned.
    let impact_after = run_local_hugr(hugr, &workspace, &["impact", "--json", "provider_symbol"]);
    assert!(impact_after.status.success());
    let impact_after_json = String::from_utf8(impact_after.stdout).unwrap();
    assert!(
        !impact_after_json.contains("src/caller.rs"),
        "dangling inbound reference should be re-extracted away: {impact_after_json}"
    );

    let _ = fs::remove_dir_all(&workspace);
}

fn run_local_hugr(hugr: &str, dir: &Path, args: &[&str]) -> std::process::Output {
    Command::new(hugr)
        .args(args)
        .current_dir(dir)
        .output()
        .expect("hugr command should run")
}
