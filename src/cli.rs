#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    Init,
    Status,
    Remember {
        text: String,
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
            "remember" => Ok(Self::Remember {
                text: required_text(args, "remember")?,
            }),
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

fn required_text(args: &[String], command: &str) -> Result<String, String> {
    optional_text(args).ok_or_else(|| format!("hugr {command} requires text"))
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

fn optional_text(args: &[String]) -> Option<String> {
    optional_text_from(args, 2)
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
    "Hugr\n\nUsage:\n  hugr init\n  hugr status\n  hugr remember <text>\n  hugr recall [--json] <query>\n  hugr context [--json] <task>\n  hugr index\n  hugr impact [--json] <file-or-symbol>\n  hugr project status\n  hugr sync status [--json]\n  hugr sync push [--dry-run|--execute] [--json]\n  hugr sync pull [--dry-run|--execute] [--json]\n  hugr sync history [--json]\n  hugr session start <task>\n  hugr session event <kind> <detail>\n  hugr session end [summary]\n  hugr mcp\n  hugr improve [--execute] [--duplicates|--stale] [--json]\n  hugr forget [--json] <query>\n  hugr doctor\n"
}

#[cfg(test)]
mod tests {
    use super::{Command, OutputFormat};

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
