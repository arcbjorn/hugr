#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    Init,
    Status,
    Remember { text: String },
    Recall { query: String, format: OutputFormat },
    Context { task: String, format: OutputFormat },
    ProjectStatus,
    SessionStart { task: String },
    SessionEvent { kind: String, detail: String },
    SessionEnd { summary: Option<String> },
    Improve,
    Forget { query: Option<String> },
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
            "project" => parse_project_command(args),
            "session" => parse_session_command(args),
            "improve" => Ok(Self::Improve),
            "forget" => Ok(Self::Forget {
                query: optional_text(args),
            }),
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

pub fn help_text() -> &'static str {
    "Hugr\n\nUsage:\n  hugr init\n  hugr status\n  hugr remember <text>\n  hugr recall [--json] <query>\n  hugr context [--json] <task>\n  hugr project status\n  hugr session start <task>\n  hugr session event <kind> <detail>\n  hugr session end [summary]\n  hugr improve\n  hugr forget [query]\n  hugr doctor\n"
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
    fn rejects_missing_text() {
        let args = vec!["hugr".into(), "remember".into()];
        assert_eq!(
            Command::parse(&args),
            Err("hugr remember requires text".into())
        );
    }
}
