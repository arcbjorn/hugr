use crate::daemon::DEFAULT_DAEMON_ADDR;

#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    Init,
    Status,
    Remember {
        text: String,
        options: MemoryWriteArgs,
    },
    Recall {
        query: String,
        format: OutputFormat,
    },
    Context {
        task: String,
        format: OutputFormat,
    },
    Index,
    Symbols {
        query: String,
        format: OutputFormat,
    },
    Impact {
        target: String,
        format: OutputFormat,
    },
    ProjectStatus,
    SessionStart {
        task: String,
    },
    SessionEvent {
        kind: String,
        detail: String,
    },
    SessionEnd {
        summary: Option<String>,
    },
    SessionPromote {
        format: OutputFormat,
    },
    SyncStatus {
        format: OutputFormat,
    },
    SyncPush {
        dry_run: bool,
        format: OutputFormat,
    },
    SyncPull {
        dry_run: bool,
        format: OutputFormat,
    },
    SyncHistory {
        format: OutputFormat,
    },
    Mcp,
    Daemon {
        addr: String,
    },
    Run {
        command: Vec<String>,
    },
    ObserveCommand {
        status: i32,
        command: Vec<String>,
    },
    ShellHook {
        shell: String,
    },
    Improve {
        execute: bool,
        duplicates: bool,
        stale: bool,
        format: OutputFormat,
    },
    Forget {
        query: String,
        format: OutputFormat,
    },
    Doctor,
    Help,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemorySourceArg {
    pub kind: String,
    pub locator: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MemoryWriteArgs {
    pub source: Option<MemorySourceArg>,
    pub confidence: Option<String>,
    pub sensitivity: Option<String>,
    pub valid_from: Option<String>,
    pub valid_to: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputFormat {
    Markdown,
    Json,
}

impl Command {
    pub fn parse(args: &[String]) -> Result<Self, String> {
        let Some(command) = args.get(1).map(String::as_str) else {
            return Ok(Self::Help);
        };

        match command {
            "init" => Ok(Self::Init),
            "status" => Ok(Self::Status),
            "remember" => parse_remember_command(args),
            "recall" => {
                let text = required_text_output(args, "recall")?;
                Ok(Self::Recall {
                    query: text.value,
                    format: text.format,
                })
            }
            "context" => {
                let text = required_text_output(args, "context")?;
                Ok(Self::Context {
                    task: text.value,
                    format: text.format,
                })
            }
            "index" => Ok(Self::Index),
            "symbols" => {
                let text = required_text_output(args, "symbols")?;
                Ok(Self::Symbols {
                    query: text.value,
                    format: text.format,
                })
            }
            "impact" => {
                let text = required_text_output(args, "impact")?;
                Ok(Self::Impact {
                    target: text.value,
                    format: text.format,
                })
            }
            "project" => parse_project_command(args),
            "session" => parse_session_command(args),
            "sync" => parse_sync_command(args),
            "mcp" => Ok(Self::Mcp),
            "daemon" => parse_daemon_command(args),
            "run" => parse_run_command(args),
            "observe" => parse_observe_command(args),
            "shell-hook" => parse_shell_hook_command(args),
            "improve" => {
                let options = improve_options_from(args, 2)?;
                Ok(Self::Improve {
                    execute: options.execute,
                    duplicates: options.duplicates,
                    stale: options.stale,
                    format: options.format,
                })
            }
            "forget" => {
                let text = required_text_output(args, "forget")?;
                Ok(Self::Forget {
                    query: text.value,
                    format: text.format,
                })
            }
            "doctor" => Ok(Self::Doctor),
            "help" | "--help" | "-h" => Ok(Self::Help),
            unknown => Err(format!("unknown command '{unknown}'")),
        }
    }
}

fn parse_remember_command(args: &[String]) -> Result<Command, String> {
    let mut options = MemoryWriteArgs::default();
    let mut words = Vec::new();
    let mut index = 2;

    while index < args.len() {
        let arg = &args[index];
        if arg == "--" {
            words.extend(args.iter().skip(index + 1).cloned());
            break;
        } else if arg == "--source" {
            index += 1;
            options.source = Some(parse_memory_source(
                args.get(index).map(String::as_str),
                "hugr remember --source",
            )?);
        } else if let Some(value) = arg.strip_prefix("--source=") {
            options.source = Some(parse_memory_source(Some(value), "hugr remember --source")?);
        } else if arg == "--confidence" {
            index += 1;
            options.confidence = Some(required_option_value(
                args.get(index).map(String::as_str),
                "hugr remember --confidence",
            )?);
        } else if let Some(value) = arg.strip_prefix("--confidence=") {
            options.confidence = Some(required_option_value(
                Some(value),
                "hugr remember --confidence",
            )?);
        } else if arg == "--sensitivity" {
            index += 1;
            options.sensitivity = Some(required_option_value(
                args.get(index).map(String::as_str),
                "hugr remember --sensitivity",
            )?);
        } else if let Some(value) = arg.strip_prefix("--sensitivity=") {
            options.sensitivity = Some(required_option_value(
                Some(value),
                "hugr remember --sensitivity",
            )?);
        } else if arg == "--valid-from" {
            index += 1;
            options.valid_from = Some(required_option_value(
                args.get(index).map(String::as_str),
                "hugr remember --valid-from",
            )?);
        } else if let Some(value) = arg.strip_prefix("--valid-from=") {
            options.valid_from = Some(required_option_value(
                Some(value),
                "hugr remember --valid-from",
            )?);
        } else if arg == "--valid-to" {
            index += 1;
            options.valid_to = Some(required_option_value(
                args.get(index).map(String::as_str),
                "hugr remember --valid-to",
            )?);
        } else if let Some(value) = arg.strip_prefix("--valid-to=") {
            options.valid_to = Some(required_option_value(
                Some(value),
                "hugr remember --valid-to",
            )?);
        } else if arg.starts_with("--") {
            return Err(format!("unknown option '{arg}'"));
        } else {
            words.push(arg.clone());
        }

        index += 1;
    }

    let text = words.join(" ");
    if text.trim().is_empty() {
        Err("hugr remember requires text".to_string())
    } else {
        Ok(Command::Remember { text, options })
    }
}

fn required_option_value(value: Option<&str>, command: &str) -> Result<String, String> {
    value
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string)
        .ok_or_else(|| format!("{command} requires a value"))
}

fn parse_memory_source(value: Option<&str>, command: &str) -> Result<MemorySourceArg, String> {
    let value = value
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| format!("{command} requires a value"))?;
    let Some((kind, locator)) = value.split_once(':') else {
        return Err(format!("{command} must use kind:locator"));
    };
    let kind = kind.trim();
    let locator = locator.trim();
    if kind.is_empty() || locator.is_empty() {
        Err(format!("{command} must use kind:locator"))
    } else {
        Ok(MemorySourceArg {
            kind: kind.to_string(),
            locator: locator.to_string(),
        })
    }
}

fn parse_observe_command(args: &[String]) -> Result<Command, String> {
    match args.get(2).map(String::as_str) {
        Some("command") => parse_observe_shell_command(args),
        Some(unknown) => Err(format!("unknown observe command '{unknown}'")),
        None => Err("hugr observe requires a subcommand".to_string()),
    }
}

fn parse_observe_shell_command(args: &[String]) -> Result<Command, String> {
    let mut status = None;
    let mut command = Vec::new();
    let mut index = 3;

    while index < args.len() {
        let arg = &args[index];
        if arg == "--" {
            command.extend(args.iter().skip(index + 1).cloned());
            break;
        } else if arg == "--status" {
            index += 1;
            status = Some(parse_status_code(
                args.get(index).map(String::as_str),
                "hugr observe command --status",
            )?);
        } else if let Some(value) = arg.strip_prefix("--status=") {
            status = Some(parse_status_code(
                Some(value),
                "hugr observe command --status",
            )?);
        } else if arg.starts_with("--") {
            return Err(format!("unknown option '{arg}'"));
        } else {
            command.extend(args.iter().skip(index).cloned());
            break;
        }

        index += 1;
    }

    let status = status.ok_or_else(|| "hugr observe command requires --status".to_string())?;
    if command.is_empty() {
        Err("hugr observe command requires a command".to_string())
    } else {
        Ok(Command::ObserveCommand { status, command })
    }
}

fn parse_status_code(value: Option<&str>, command: &str) -> Result<i32, String> {
    value
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| format!("{command} requires a value"))?
        .parse::<i32>()
        .map_err(|_| format!("{command} must be an integer"))
}

fn parse_shell_hook_command(args: &[String]) -> Result<Command, String> {
    let shell = args
        .get(2)
        .filter(|value| !value.trim().is_empty())
        .cloned()
        .ok_or_else(|| "hugr shell-hook requires a shell".to_string())?;

    if args.len() > 3 {
        return Err(format!("unknown option '{}'", args[3]));
    }

    match shell.as_str() {
        "bash" | "zsh" => Ok(Command::ShellHook { shell }),
        _ => Err("hugr shell-hook supports bash or zsh".to_string()),
    }
}

fn parse_daemon_command(args: &[String]) -> Result<Command, String> {
    let mut addr = DEFAULT_DAEMON_ADDR.to_string();
    let mut index = 2;

    while index < args.len() {
        let arg = &args[index];
        if arg == "--addr" {
            index += 1;
            addr = args
                .get(index)
                .filter(|value| !value.trim().is_empty())
                .cloned()
                .ok_or_else(|| "hugr daemon --addr requires a value".to_string())?;
        } else if let Some(value) = arg.strip_prefix("--addr=") {
            if value.trim().is_empty() {
                return Err("hugr daemon --addr requires a value".to_string());
            }
            addr = value.to_string();
        } else {
            return Err(format!("unknown option '{arg}'"));
        }

        index += 1;
    }

    Ok(Command::Daemon { addr })
}

fn parse_run_command(args: &[String]) -> Result<Command, String> {
    let mut command = args.iter().skip(2);
    if command.clone().next().is_some_and(|arg| arg == "--") {
        command.next();
    }

    let command = command.cloned().collect::<Vec<_>>();
    if command.is_empty() {
        Err("hugr run requires a command".to_string())
    } else {
        Ok(Command::Run { command })
    }
}

fn parse_project_command(args: &[String]) -> Result<Command, String> {
    match args.get(2).map(String::as_str) {
        Some("status") => Ok(Command::ProjectStatus),
        Some(unknown) => Err(format!("unknown project command '{unknown}'")),
        None => Err("hugr project requires a subcommand".to_string()),
    }
}

fn parse_session_command(args: &[String]) -> Result<Command, String> {
    match args.get(2).map(String::as_str) {
        Some("start") => Ok(Command::SessionStart {
            task: required_text_from(args, 3, "session start")?,
        }),
        Some("event") => {
            let kind = args
                .get(3)
                .filter(|kind| !kind.trim().is_empty())
                .cloned()
                .ok_or_else(|| "hugr session event requires kind".to_string())?;
            Ok(Command::SessionEvent {
                kind,
                detail: required_text_from(args, 4, "session event")?,
            })
        }
        Some("end") => Ok(Command::SessionEnd {
            summary: optional_text_from(args, 3),
        }),
        Some("promote") => Ok(Command::SessionPromote {
            format: output_format_from(args, 3)?,
        }),
        Some(unknown) => Err(format!("unknown session command '{unknown}'")),
        None => Err("hugr session requires a subcommand".to_string()),
    }
}

fn parse_sync_command(args: &[String]) -> Result<Command, String> {
    match args.get(2).map(String::as_str) {
        Some("status") => Ok(Command::SyncStatus {
            format: output_format_from(args, 3)?,
        }),
        Some("push") => {
            let options = sync_push_options_from(args, 3)?;
            Ok(Command::SyncPush {
                dry_run: options.dry_run,
                format: options.format,
            })
        }
        Some("pull") => {
            let options = sync_push_options_from(args, 3)?;
            Ok(Command::SyncPull {
                dry_run: options.dry_run,
                format: options.format,
            })
        }
        Some("history") => Ok(Command::SyncHistory {
            format: output_format_from(args, 3)?,
        }),
        Some(unknown) => Err(format!("unknown sync command '{unknown}'")),
        None => Err("hugr sync requires a subcommand".to_string()),
    }
}

struct SyncPushOptions {
    dry_run: bool,
    format: OutputFormat,
}

struct ImproveOptions {
    execute: bool,
    duplicates: bool,
    stale: bool,
    format: OutputFormat,
}

struct TextOutput {
    value: String,
    format: OutputFormat,
}

fn required_text_from(args: &[String], start: usize, command: &str) -> Result<String, String> {
    optional_text_from(args, start).ok_or_else(|| format!("hugr {command} requires text"))
}

fn required_text_output(args: &[String], command: &str) -> Result<TextOutput, String> {
    let mut format = OutputFormat::Markdown;
    let mut words = Vec::new();

    for arg in args.iter().skip(2) {
        if arg == "--json" {
            format = OutputFormat::Json;
        } else {
            words.push(arg.clone());
        }
    }

    let value = words.join(" ");
    if value.trim().is_empty() {
        Err(format!("hugr {command} requires text"))
    } else {
        Ok(TextOutput { value, format })
    }
}

fn optional_text_from(args: &[String], start: usize) -> Option<String> {
    let text = args
        .iter()
        .skip(start)
        .cloned()
        .collect::<Vec<_>>()
        .join(" ");
    if text.trim().is_empty() {
        None
    } else {
        Some(text)
    }
}

fn output_format_from(args: &[String], start: usize) -> Result<OutputFormat, String> {
    let mut format = OutputFormat::Markdown;
    for arg in args.iter().skip(start) {
        if arg == "--json" {
            format = OutputFormat::Json;
        } else {
            return Err(format!("unknown option '{arg}'"));
        }
    }
    Ok(format)
}

fn sync_push_options_from(args: &[String], start: usize) -> Result<SyncPushOptions, String> {
    let mut dry_run = true;
    let mut format = OutputFormat::Markdown;

    for arg in args.iter().skip(start) {
        match arg.as_str() {
            "--dry-run" => dry_run = true,
            "--execute" => dry_run = false,
            "--json" => format = OutputFormat::Json,
            unknown => return Err(format!("unknown option '{unknown}'")),
        }
    }

    Ok(SyncPushOptions { dry_run, format })
}

fn improve_options_from(args: &[String], start: usize) -> Result<ImproveOptions, String> {
    let mut execute = false;
    let mut duplicates = false;
    let mut stale = false;
    let mut format = OutputFormat::Markdown;

    for arg in args.iter().skip(start) {
        match arg.as_str() {
            "--execute" => execute = true,
            "--duplicates" => duplicates = true,
            "--stale" => stale = true,
            "--json" => format = OutputFormat::Json,
            unknown => return Err(format!("unknown option '{unknown}'")),
        }
    }

    Ok(ImproveOptions {
        execute,
        duplicates,
        stale,
        format,
    })
}

pub fn help_text() -> &'static str {
    "Hugr\n\nUsage:\n  hugr init\n  hugr status\n  hugr remember [--source <kind:locator>] [--confidence <0.0-1.0>] [--sensitivity <label>] [--valid-from <value>] [--valid-to <value>] <text>\n  hugr recall [--json] <query>\n  hugr context [--json] <task>\n  hugr index\n  hugr symbols [--json] <query>\n  hugr impact [--json] <file-or-symbol>\n  hugr project status\n  hugr sync status [--json]\n  hugr sync push [--dry-run|--execute] [--json]\n  hugr sync pull [--dry-run|--execute] [--json]\n  hugr sync history [--json]\n  hugr session start <task>\n  hugr session event <kind> <detail>\n  hugr session end [summary]\n  hugr session promote [--json]\n  hugr mcp\n  hugr daemon [--addr <host:port>]\n  hugr run [--] <command> [args...]\n  hugr observe command --status <code> -- <command> [args...]\n  hugr shell-hook <bash|zsh>\n  hugr improve [--execute] [--duplicates|--stale] [--json]\n  hugr forget [--json] <query>\n  hugr doctor\n"
}

#[cfg(test)]
mod tests {
    use super::{Command, MemorySourceArg, MemoryWriteArgs, OutputFormat};

    #[test]
    fn parses_context_text() {
        let args = vec![
            "hugr".into(),
            "context".into(),
            "add".into(),
            "hooks".into(),
        ];
        assert_eq!(
            Command::parse(&args),
            Ok(Command::Context {
                task: "add hooks".into(),
                format: OutputFormat::Markdown
            })
        );
    }

    #[test]
    fn parses_json_output_format() {
        let args = vec![
            "hugr".into(),
            "recall".into(),
            "--json".into(),
            "plugin".into(),
            "hooks".into(),
        ];
        assert_eq!(
            Command::parse(&args),
            Ok(Command::Recall {
                query: "plugin hooks".into(),
                format: OutputFormat::Json
            })
        );
    }

    #[test]
    fn parses_project_status() {
        let args = vec!["hugr".into(), "project".into(), "status".into()];
        assert_eq!(Command::parse(&args), Ok(Command::ProjectStatus));
    }

    #[test]
    fn parses_index_command() {
        let args = vec!["hugr".into(), "index".into()];
        assert_eq!(Command::parse(&args), Ok(Command::Index));
    }

    #[test]
    fn parses_symbols_command() {
        let args = vec![
            "hugr".into(),
            "symbols".into(),
            "--json".into(),
            "PluginHooks".into(),
        ];
        assert_eq!(
            Command::parse(&args),
            Ok(Command::Symbols {
                query: "PluginHooks".into(),
                format: OutputFormat::Json
            })
        );
    }

    #[test]
    fn parses_impact_command() {
        let args = vec![
            "hugr".into(),
            "impact".into(),
            "--json".into(),
            "PluginHooks".into(),
        ];
        assert_eq!(
            Command::parse(&args),
            Ok(Command::Impact {
                target: "PluginHooks".into(),
                format: OutputFormat::Json
            })
        );
    }

    #[test]
    fn parses_session_commands() {
        assert_eq!(
            Command::parse(&[
                "hugr".into(),
                "session".into(),
                "start".into(),
                "add".into(),
                "hooks".into()
            ]),
            Ok(Command::SessionStart {
                task: "add hooks".into()
            })
        );
        assert_eq!(
            Command::parse(&[
                "hugr".into(),
                "session".into(),
                "event".into(),
                "test".into(),
                "cargo".into(),
                "test".into()
            ]),
            Ok(Command::SessionEvent {
                kind: "test".into(),
                detail: "cargo test".into()
            })
        );
        assert_eq!(
            Command::parse(&["hugr".into(), "session".into(), "end".into(), "done".into()]),
            Ok(Command::SessionEnd {
                summary: Some("done".into())
            })
        );
        assert_eq!(
            Command::parse(&[
                "hugr".into(),
                "session".into(),
                "promote".into(),
                "--json".into()
            ]),
            Ok(Command::SessionPromote {
                format: OutputFormat::Json
            })
        );
    }

    #[test]
    fn parses_sync_status() {
        assert_eq!(
            Command::parse(&["hugr".into(), "sync".into(), "status".into()]),
            Ok(Command::SyncStatus {
                format: OutputFormat::Markdown
            })
        );
        assert_eq!(
            Command::parse(&[
                "hugr".into(),
                "sync".into(),
                "status".into(),
                "--json".into()
            ]),
            Ok(Command::SyncStatus {
                format: OutputFormat::Json
            })
        );
    }

    #[test]
    fn parses_sync_push() {
        assert_eq!(
            Command::parse(&["hugr".into(), "sync".into(), "push".into()]),
            Ok(Command::SyncPush {
                dry_run: true,
                format: OutputFormat::Markdown
            })
        );
        assert_eq!(
            Command::parse(&[
                "hugr".into(),
                "sync".into(),
                "push".into(),
                "--execute".into(),
                "--json".into()
            ]),
            Ok(Command::SyncPush {
                dry_run: false,
                format: OutputFormat::Json
            })
        );
    }

    #[test]
    fn parses_sync_pull() {
        assert_eq!(
            Command::parse(&["hugr".into(), "sync".into(), "pull".into()]),
            Ok(Command::SyncPull {
                dry_run: true,
                format: OutputFormat::Markdown
            })
        );
        assert_eq!(
            Command::parse(&[
                "hugr".into(),
                "sync".into(),
                "pull".into(),
                "--execute".into(),
                "--json".into()
            ]),
            Ok(Command::SyncPull {
                dry_run: false,
                format: OutputFormat::Json
            })
        );
    }

    #[test]
    fn parses_sync_history() {
        assert_eq!(
            Command::parse(&["hugr".into(), "sync".into(), "history".into()]),
            Ok(Command::SyncHistory {
                format: OutputFormat::Markdown
            })
        );
        assert_eq!(
            Command::parse(&[
                "hugr".into(),
                "sync".into(),
                "history".into(),
                "--json".into()
            ]),
            Ok(Command::SyncHistory {
                format: OutputFormat::Json
            })
        );
    }

    #[test]
    fn parses_mcp_command() {
        let args = vec!["hugr".into(), "mcp".into()];
        assert_eq!(Command::parse(&args), Ok(Command::Mcp));
    }

    #[test]
    fn parses_daemon_command() {
        assert_eq!(
            Command::parse(&["hugr".into(), "daemon".into()]),
            Ok(Command::Daemon {
                addr: "127.0.0.1:5874".into()
            })
        );
        assert_eq!(
            Command::parse(&[
                "hugr".into(),
                "daemon".into(),
                "--addr".into(),
                "127.0.0.1:0".into()
            ]),
            Ok(Command::Daemon {
                addr: "127.0.0.1:0".into()
            })
        );
        assert_eq!(
            Command::parse(&[
                "hugr".into(),
                "daemon".into(),
                "--addr=127.0.0.1:9999".into()
            ]),
            Ok(Command::Daemon {
                addr: "127.0.0.1:9999".into()
            })
        );
    }

    #[test]
    fn parses_run_command() {
        assert_eq!(
            Command::parse(&["hugr".into(), "run".into(), "cargo".into(), "test".into()]),
            Ok(Command::Run {
                command: vec!["cargo".into(), "test".into()]
            })
        );
        assert_eq!(
            Command::parse(&[
                "hugr".into(),
                "run".into(),
                "--".into(),
                "cargo".into(),
                "test".into()
            ]),
            Ok(Command::Run {
                command: vec!["cargo".into(), "test".into()]
            })
        );
    }

    #[test]
    fn parses_observe_command() {
        assert_eq!(
            Command::parse(&[
                "hugr".into(),
                "observe".into(),
                "command".into(),
                "--status".into(),
                "0".into(),
                "--".into(),
                "cargo".into(),
                "test".into()
            ]),
            Ok(Command::ObserveCommand {
                status: 0,
                command: vec!["cargo".into(), "test".into()]
            })
        );
        assert_eq!(
            Command::parse(&[
                "hugr".into(),
                "observe".into(),
                "command".into(),
                "--status=1".into(),
                "cargo".into(),
                "test".into()
            ]),
            Ok(Command::ObserveCommand {
                status: 1,
                command: vec!["cargo".into(), "test".into()]
            })
        );
    }

    #[test]
    fn parses_remember_command() {
        assert_eq!(
            Command::parse(&[
                "hugr".into(),
                "remember".into(),
                "plugin".into(),
                "hooks".into()
            ]),
            Ok(Command::Remember {
                text: "plugin hooks".into(),
                options: MemoryWriteArgs::default(),
            })
        );
    }

    #[test]
    fn parses_remember_source_attachment() {
        assert_eq!(
            Command::parse(&[
                "hugr".into(),
                "remember".into(),
                "--source".into(),
                "file:src/lib.rs".into(),
                "plugin".into(),
                "hooks".into(),
            ]),
            Ok(Command::Remember {
                text: "plugin hooks".into(),
                options: MemoryWriteArgs {
                    source: Some(MemorySourceArg {
                        kind: "file".into(),
                        locator: "src/lib.rs".into(),
                    }),
                    ..MemoryWriteArgs::default()
                },
            })
        );
        assert_eq!(
            Command::parse(&[
                "hugr".into(),
                "remember".into(),
                "--source=url:https://example.test/docs".into(),
                "remote".into(),
                "docs".into(),
            ]),
            Ok(Command::Remember {
                text: "remote docs".into(),
                options: MemoryWriteArgs {
                    source: Some(MemorySourceArg {
                        kind: "url".into(),
                        locator: "https://example.test/docs".into(),
                    }),
                    ..MemoryWriteArgs::default()
                },
            })
        );
    }

    #[test]
    fn parses_remember_metadata() {
        assert_eq!(
            Command::parse(&[
                "hugr".into(),
                "remember".into(),
                "--confidence=0.75".into(),
                "--sensitivity".into(),
                "private".into(),
                "--valid-from".into(),
                "2026-01-01".into(),
                "--valid-to=2026-12-31".into(),
                "plugin".into(),
                "hooks".into(),
            ]),
            Ok(Command::Remember {
                text: "plugin hooks".into(),
                options: MemoryWriteArgs {
                    confidence: Some("0.75".into()),
                    sensitivity: Some("private".into()),
                    valid_from: Some("2026-01-01".into()),
                    valid_to: Some("2026-12-31".into()),
                    ..MemoryWriteArgs::default()
                },
            })
        );
    }

    #[test]
    fn parses_shell_hook_command() {
        assert_eq!(
            Command::parse(&["hugr".into(), "shell-hook".into(), "zsh".into()]),
            Ok(Command::ShellHook {
                shell: "zsh".into()
            })
        );
        assert_eq!(
            Command::parse(&["hugr".into(), "shell-hook".into(), "bash".into()]),
            Ok(Command::ShellHook {
                shell: "bash".into()
            })
        );
    }

    #[test]
    fn parses_memory_maintenance_commands() {
        assert_eq!(
            Command::parse(&["hugr".into(), "improve".into(), "--json".into()]),
            Ok(Command::Improve {
                execute: false,
                duplicates: false,
                stale: false,
                format: OutputFormat::Json
            })
        );
        assert_eq!(
            Command::parse(&[
                "hugr".into(),
                "improve".into(),
                "--execute".into(),
                "--duplicates".into(),
                "--json".into()
            ]),
            Ok(Command::Improve {
                execute: true,
                duplicates: true,
                stale: false,
                format: OutputFormat::Json
            })
        );
        assert_eq!(
            Command::parse(&[
                "hugr".into(),
                "improve".into(),
                "--execute".into(),
                "--stale".into(),
                "--json".into()
            ]),
            Ok(Command::Improve {
                execute: true,
                duplicates: false,
                stale: true,
                format: OutputFormat::Json
            })
        );
        assert_eq!(
            Command::parse(&[
                "hugr".into(),
                "forget".into(),
                "--json".into(),
                "plugin".into(),
                "hooks".into()
            ]),
            Ok(Command::Forget {
                query: "plugin hooks".into(),
                format: OutputFormat::Json
            })
        );
    }

    #[test]
    fn rejects_missing_text() {
        let args = vec!["hugr".into(), "remember".into()];
        assert_eq!(
            Command::parse(&args),
            Err("hugr remember requires text".into())
        );

        let args = vec!["hugr".into(), "forget".into()];
        assert_eq!(
            Command::parse(&args),
            Err("hugr forget requires text".into())
        );
    }
}
