#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    Init,
    Status,
    Remember { text: String },
    Recall { query: String },
    Context { task: String },
    Improve,
    Forget { query: Option<String> },
    Doctor,
    Help,
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
            "recall" => Ok(Self::Recall {
                query: required_text(args, "recall")?,
            }),
            "context" => Ok(Self::Context {
                task: required_text(args, "context")?,
            }),
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

fn required_text(args: &[String], command: &str) -> Result<String, String> {
    optional_text(args).ok_or_else(|| format!("hugr {command} requires text"))
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
    "Hugr\n\nUsage:\n  hugr init\n  hugr status\n  hugr remember <text>\n  hugr recall <query>\n  hugr context <task>\n  hugr improve\n  hugr forget [query]\n  hugr doctor\n"
}

#[cfg(test)]
mod tests {
    use super::Command;

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
                task: "add hooks".into()
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
