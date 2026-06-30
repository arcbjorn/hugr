#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    Init,
    Status,
    Remember { text: String },
    Recall { query: String, format: OutputFormat },
    Context { task: String, format: OutputFormat },
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

struct TextOutput {
    value: String,
    format: OutputFormat,
}

fn required_text(args: &[String], command: &str) -> Result<String, String> {
    optional_text(args).ok_or_else(|| format!("hugr {command} requires text"))
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
    let text = args.iter().skip(2).cloned().collect::<Vec<_>>().join(" ");
    if text.trim().is_empty() {
        None
    } else {
        Some(text)
    }
}

pub fn help_text() -> &'static str {
    "Hugr\n\nUsage:\n  hugr init\n  hugr status\n  hugr remember <text>\n  hugr recall [--json] <query>\n  hugr context [--json] <task>\n  hugr improve\n  hugr forget [query]\n  hugr doctor\n"
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
    fn rejects_missing_text() {
        let args = vec!["hugr".into(), "remember".into()];
        assert_eq!(
            Command::parse(&args),
            Err("hugr remember requires text".into())
        );
    }
}
