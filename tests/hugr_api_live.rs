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
        &["forget", "--json", "live remote memory"],
    );
    assert!(forget.status.success(), "forget failed: {forget:?}");
    let forget_json = String::from_utf8(forget.stdout).unwrap();
    assert!(forget_json.contains(r#""forgotten_count":1"#));

    let recall_after_forget = run_remote_hugr(
        hugr,
        &client_dir,
        &api_url,
        &["recall", "--json", "live remote memory"],
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
