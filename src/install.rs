//! One-command agent integration. `hugr install` writes the MCP registration
//! and session hooks an agent client needs to reach `hugr context`, and
//! `hugr hook` is the stdin adapter those hooks call back into, keeping the
//! JSON schema of each agent client isolated to this module.
//!
//! Config writes merge into existing JSON and never clobber unrelated keys;
//! malformed existing files abort the install instead of being overwritten.
//! Re-running an install is a no-op.

use crate::error::{Error, Result};
use crate::store::Store;
use serde_json::{Map, Value, json};
use std::fs;
use std::io::Read;
use std::path::Path;

pub(crate) fn install(agent: &str, shared: bool) -> Result<()> {
    install_at(Path::new("."), agent, shared, &hugr_executable())
}

fn install_at(root: &Path, agent: &str, shared: bool, executable: &str) -> Result<()> {
    match agent {
        "claude-code" => install_claude_code(root, shared, executable),
        "cursor" => install_cursor(root, executable),
        unknown => Err(Error::msg(format!(
            "unsupported agent '{unknown}'; supported agents: claude-code, cursor"
        ))),
    }
}

fn install_claude_code(root: &Path, shared: bool, executable: &str) -> Result<()> {
    let mcp_path = root.join(".mcp.json");
    let mcp_changed = merge_mcp_server(&mcp_path, executable)?;

    let settings_name = if shared {
        "settings.json"
    } else {
        "settings.local.json"
    };
    let settings_path = root.join(".claude").join(settings_name);
    let hooks_changed = merge_claude_hooks(&settings_path, executable)?;

    report_file(&mcp_path, mcp_changed);
    report_file(&settings_path, hooks_changed);
    println!("restart your Claude Code session to pick up the hugr MCP server and hooks");
    Ok(())
}

fn install_cursor(root: &Path, executable: &str) -> Result<()> {
    let mcp_path = root.join(".cursor").join("mcp.json");
    let changed = merge_mcp_server(&mcp_path, executable)?;

    report_file(&mcp_path, changed);
    println!("restart Cursor to pick up the hugr MCP server");
    Ok(())
}

fn report_file(path: &Path, changed: bool) {
    if changed {
        println!("updated {}", path.display());
    } else {
        println!("unchanged {}", path.display());
    }
}

fn merge_mcp_server(path: &Path, executable: &str) -> Result<bool> {
    let mut config = read_json_object(path)?;
    let servers = config
        .entry("mcpServers".to_string())
        .or_insert_with(|| json!({}));
    let servers = servers
        .as_object_mut()
        .ok_or_else(|| refusal(path, "mcpServers is not a JSON object"))?;

    let desired = json!({
        "command": executable,
        "args": ["mcp"],
    });
    if servers.get("hugr") == Some(&desired) {
        return Ok(false);
    }

    servers.insert("hugr".to_string(), desired);
    write_json_object(path, &config)?;
    Ok(true)
}

fn merge_claude_hooks(path: &Path, executable: &str) -> Result<bool> {
    let mut settings = read_json_object(path)?;
    let hooks = settings
        .entry("hooks".to_string())
        .or_insert_with(|| json!({}));
    let hooks = hooks
        .as_object_mut()
        .ok_or_else(|| refusal(path, "hooks is not a JSON object"))?;

    let mut changed = false;
    for entry in claude_hook_entries(executable) {
        let event_entries = hooks
            .entry(entry.event.to_string())
            .or_insert_with(|| json!([]));
        let event_entries = event_entries
            .as_array_mut()
            .ok_or_else(|| refusal(path, "a hook event entry is not a JSON array"))?;
        if event_entries
            .iter()
            .any(|existing| entry_contains_command(existing, &entry.command))
        {
            continue;
        }

        let mut group = Map::new();
        if let Some(matcher) = entry.matcher {
            group.insert("matcher".to_string(), json!(matcher));
        }
        group.insert(
            "hooks".to_string(),
            json!([{ "type": "command", "command": entry.command }]),
        );
        event_entries.push(Value::Object(group));
        changed = true;
    }

    if changed {
        write_json_object(path, &settings)?;
    }
    Ok(changed)
}

struct ClaudeHookEntry {
    event: &'static str,
    matcher: Option<&'static str>,
    command: String,
}

/// The hook surface follows the session lifecycle in VISION.md: start
/// sessions, observe edits, promote learnings at session end. Fresh sessions
/// only (`startup|clear`); resumed sessions continue their earlier history.
fn claude_hook_entries(executable: &str) -> Vec<ClaudeHookEntry> {
    let executable = shell_quoted(executable);
    vec![
        ClaudeHookEntry {
            event: "SessionStart",
            matcher: Some("startup|clear"),
            command: format!("{executable} hook claude-code session-start"),
        },
        ClaudeHookEntry {
            event: "PostToolUse",
            matcher: Some("Edit|Write|NotebookEdit"),
            command: format!("{executable} hook claude-code post-tool-use"),
        },
        ClaudeHookEntry {
            event: "SessionEnd",
            matcher: None,
            command: format!("{executable} hook claude-code session-end"),
        },
    ]
}

fn entry_contains_command(entry: &Value, command: &str) -> bool {
    entry
        .get("hooks")
        .and_then(Value::as_array)
        .is_some_and(|hooks| {
            hooks
                .iter()
                .any(|hook| hook.get("command").and_then(Value::as_str) == Some(command))
        })
}

fn read_json_object(path: &Path) -> Result<Map<String, Value>> {
    if !path.exists() {
        return Ok(Map::new());
    }
    let text = fs::read_to_string(path)?;
    if text.trim().is_empty() {
        return Ok(Map::new());
    }
    let value = serde_json::from_str::<Value>(&text)
        .map_err(|error| refusal(path, &format!("existing JSON failed to parse: {error}")))?;
    match value {
        Value::Object(map) => Ok(map),
        _ => Err(refusal(path, "top level is not a JSON object")),
    }
}

fn write_json_object(path: &Path, map: &Map<String, Value>) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut rendered = serde_json::to_string_pretty(&Value::Object(map.clone()))?;
    rendered.push('\n');
    fs::write(path, rendered).map_err(Error::from)
}

fn refusal(path: &Path, reason: &str) -> Error {
    Error::msg(format!("refusing to modify {}: {reason}", path.display()))
}

fn hugr_executable() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|path| path.canonicalize().ok())
        .map_or_else(
            || "hugr".to_string(),
            |path| path.to_string_lossy().into_owned(),
        )
}

fn shell_quoted(value: &str) -> String {
    if value
        .chars()
        .all(|char| char.is_alphanumeric() || matches!(char, '/' | '.' | '_' | '-' | '+'))
    {
        return value.to_string();
    }
    format!("'{}'", value.replace('\'', "'\\''"))
}

/// Entry point for agent hooks. Hooks must never break the agent session, so
/// every failure is reported on stderr and the process still exits 0; stdout
/// stays silent because Claude Code parses hook stdout as JSON.
pub(crate) async fn hook(agent: &str, event: &str) -> Result<()> {
    if agent != "claude-code" {
        eprintln!("hugr hook: unsupported agent '{agent}'");
        return Ok(());
    }

    let mut payload = String::new();
    let _ = std::io::stdin().read_to_string(&mut payload);
    let payload = serde_json::from_str::<Value>(&payload).unwrap_or(Value::Null);

    if let Err(error) = apply_hook_event(event, &payload).await {
        eprintln!("hugr hook: {error}");
    }
    Ok(())
}

async fn apply_hook_event(event: &str, payload: &Value) -> Result<()> {
    match event {
        "session-start" => {
            let source = payload
                .get("source")
                .and_then(Value::as_str)
                .unwrap_or("startup");
            Store::open_current()
                .start_session(&format!("agent session ({source})"))
                .await?;
            Ok(())
        }
        "post-tool-use" => {
            let Some(path) = edited_file_path(payload) else {
                return Ok(());
            };
            Store::open_current()
                .record_session_event_if_active("edit", &path)
                .await?;
            Ok(())
        }
        "session-end" => {
            let store = Store::open_current();
            store.end_session(None).await?;
            store.promote_latest_session().await?;
            Ok(())
        }
        unknown => Err(Error::msg(format!("unsupported hook event '{unknown}'"))),
    }
}

fn edited_file_path(payload: &Value) -> Option<String> {
    let input = payload.get("tool_input")?;
    input
        .get("file_path")
        .or_else(|| input.get("notebook_path"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::{edited_file_path, install_at, shell_quoted};
    use serde_json::{Value, json};
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct TempRoot {
        root: PathBuf,
    }

    impl TempRoot {
        fn new(name: &str) -> Self {
            let unique = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system time should be after unix epoch")
                .as_nanos();
            let root = std::env::temp_dir().join(format!("hugr_install_{name}_{unique}"));
            fs::create_dir_all(&root).expect("temp root should be created");
            Self { root }
        }

        fn read_json(&self, relative: &str) -> Value {
            let text =
                fs::read_to_string(self.root.join(relative)).expect("file should be readable");
            serde_json::from_str(&text).expect("file should be JSON")
        }
    }

    impl Drop for TempRoot {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn installs_claude_code_mcp_and_hooks() {
        let temp = TempRoot::new("fresh");

        install_at(&temp.root, "claude-code", false, "hugr").expect("install should succeed");

        let mcp = temp.read_json(".mcp.json");
        assert_eq!(mcp["mcpServers"]["hugr"]["command"], "hugr");
        assert_eq!(mcp["mcpServers"]["hugr"]["args"][0], "mcp");

        let settings = temp.read_json(".claude/settings.local.json");
        assert_eq!(
            settings["hooks"]["SessionStart"][0]["matcher"],
            "startup|clear"
        );
        assert_eq!(
            settings["hooks"]["PostToolUse"][0]["matcher"],
            "Edit|Write|NotebookEdit"
        );
        assert_eq!(
            settings["hooks"]["SessionEnd"][0]["hooks"][0]["command"],
            "hugr hook claude-code session-end"
        );
    }

    #[test]
    fn reruns_are_idempotent() {
        let temp = TempRoot::new("idempotent");

        install_at(&temp.root, "claude-code", false, "hugr").expect("first install");
        let mcp_before = temp.read_json(".mcp.json");
        let settings_before = temp.read_json(".claude/settings.local.json");

        install_at(&temp.root, "claude-code", false, "hugr").expect("second install");

        assert_eq!(temp.read_json(".mcp.json"), mcp_before);
        assert_eq!(
            temp.read_json(".claude/settings.local.json"),
            settings_before
        );
        assert_eq!(
            settings_before["hooks"]["SessionStart"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn preserves_unrelated_configuration() {
        let temp = TempRoot::new("merge");
        fs::write(
            temp.root.join(".mcp.json"),
            json!({"mcpServers": {"other": {"command": "other-tool"}}}).to_string(),
        )
        .unwrap();
        fs::create_dir_all(temp.root.join(".claude")).unwrap();
        fs::write(
            temp.root.join(".claude/settings.local.json"),
            json!({
                "permissions": {"allow": ["Bash(ls:*)"]},
                "hooks": {"SessionStart": [{"hooks": [{"type": "command", "command": "echo hi"}]}]}
            })
            .to_string(),
        )
        .unwrap();

        install_at(&temp.root, "claude-code", false, "hugr").expect("install should merge");

        let mcp = temp.read_json(".mcp.json");
        assert_eq!(mcp["mcpServers"]["other"]["command"], "other-tool");
        assert_eq!(mcp["mcpServers"]["hugr"]["args"][0], "mcp");

        let settings = temp.read_json(".claude/settings.local.json");
        assert_eq!(settings["permissions"]["allow"][0], "Bash(ls:*)");
        let session_start = settings["hooks"]["SessionStart"].as_array().unwrap();
        assert_eq!(session_start.len(), 2, "existing hook entry should remain");
        assert_eq!(session_start[0]["hooks"][0]["command"], "echo hi");
    }

    #[test]
    fn refuses_malformed_existing_json() {
        let temp = TempRoot::new("malformed");
        fs::write(temp.root.join(".mcp.json"), "{not json").unwrap();

        let error = install_at(&temp.root, "claude-code", false, "hugr")
            .expect_err("malformed JSON must refuse")
            .to_string();

        assert!(
            error.contains(".mcp.json"),
            "error should name the file: {error}"
        );
        assert_eq!(
            fs::read_to_string(temp.root.join(".mcp.json")).unwrap(),
            "{not json",
            "the malformed file must not be overwritten"
        );
    }

    #[test]
    fn shared_flag_targets_committed_settings() {
        let temp = TempRoot::new("shared");

        install_at(&temp.root, "claude-code", true, "hugr").expect("shared install");

        assert!(temp.root.join(".claude/settings.json").exists());
        assert!(!temp.root.join(".claude/settings.local.json").exists());
    }

    #[test]
    fn installs_cursor_mcp_only() {
        let temp = TempRoot::new("cursor");

        install_at(&temp.root, "cursor", false, "hugr").expect("cursor install");

        let mcp = temp.read_json(".cursor/mcp.json");
        assert_eq!(mcp["mcpServers"]["hugr"]["command"], "hugr");
        assert!(!temp.root.join(".claude").exists());
    }

    #[test]
    fn rejects_unknown_agents() {
        let temp = TempRoot::new("unknown");

        let error = install_at(&temp.root, "windsurf", false, "hugr")
            .expect_err("unknown agent")
            .to_string();

        assert!(error.contains("unsupported agent"));
    }

    #[test]
    fn extracts_edited_file_paths_from_payloads() {
        assert_eq!(
            edited_file_path(&json!({"tool_input": {"file_path": "src/a.rs"}})),
            Some("src/a.rs".to_string())
        );
        assert_eq!(
            edited_file_path(&json!({"tool_input": {"notebook_path": "nb.ipynb"}})),
            Some("nb.ipynb".to_string())
        );
        assert_eq!(edited_file_path(&json!({"tool_input": {}})), None);
        assert_eq!(edited_file_path(&Value::Null), None);
    }

    #[test]
    fn quotes_executables_with_spaces() {
        assert_eq!(shell_quoted("/usr/local/bin/hugr"), "/usr/local/bin/hugr");
        assert_eq!(shell_quoted("/Users/a b/hugr"), "'/Users/a b/hugr'");
    }
}
