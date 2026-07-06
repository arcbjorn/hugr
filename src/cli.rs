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
        budget: Option<usize>,
    },
    Index {
        paths: Vec<String>,
    },
    Symbols {
        query: String,
        format: OutputFormat,
    },
    Impact {
        target: String,
        format: OutputFormat,
    },
    ReplaceSymbol {
        path: String,
        name: String,
        kind: Option<String>,
        body: String,
        format: OutputFormat,
    },
    RenameSymbol {
        path: String,
        name: String,
        new_name: String,
        kind: Option<String>,
        format: OutputFormat,
    },
    MoveSymbol {
        source_path: String,
        name: String,
        destination_path: String,
        kind: Option<String>,
        rewrite_references: bool,
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
        llm: bool,
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
    Eval {
        from_git: usize,
        max_files: usize,
        min_hit_rate: Option<String>,
        format: OutputFormat,
    },
    Install {
        agent: String,
        shared: bool,
    },
    Hook {
        agent: String,
        event: String,
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
            "context" => parse_context_command(args),
            "index" => parse_index_command(args),
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
            "replace-symbol" => parse_replace_symbol_command(args),
            "rename-symbol" => parse_rename_symbol_command(args),
            "move-symbol" => parse_move_symbol_command(args),
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
            "eval" => parse_eval_command(args),
            "install" => parse_install_command(args),
            "hook" => parse_hook_command(args),
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

fn parse_index_command(args: &[String]) -> Result<Command, String> {
    let mut paths = Vec::new();
    let mut index = 2;

    while index < args.len() {
        let arg = &args[index];
        let raw = if arg == "--paths" {
            index += 1;
            required_option_value(args.get(index).map(String::as_str), "hugr index --paths")?
        } else if let Some(value) = arg.strip_prefix("--paths=") {
            required_option_value(Some(value), "hugr index --paths")?
        } else if arg.starts_with("--") {
            return Err(format!("unknown option '{arg}'"));
        } else {
            return Err(format!(
                "hugr index does not take positional argument '{arg}'"
            ));
        };
        for path in raw.split(',') {
            let trimmed = path.trim();
            if !trimmed.is_empty() {
                paths.push(trimmed.to_string());
            }
        }
        index += 1;
    }

    Ok(Command::Index { paths })
}

fn parse_move_symbol_command(args: &[String]) -> Result<Command, String> {
    let mut positional = Vec::new();
    let mut kind = None;
    let mut rewrite_references = false;
    let mut format = OutputFormat::Markdown;
    let mut index = 2;

    while index < args.len() {
        let arg = &args[index];
        if arg == "--json" {
            format = OutputFormat::Json;
        } else if arg == "--rewrite-references" {
            rewrite_references = true;
        } else if arg == "--kind" {
            index += 1;
            kind = Some(required_option_value(
                args.get(index).map(String::as_str),
                "hugr move-symbol --kind",
            )?);
        } else if let Some(value) = arg.strip_prefix("--kind=") {
            kind = Some(required_option_value(
                Some(value),
                "hugr move-symbol --kind",
            )?);
        } else if arg.starts_with("--") {
            return Err(format!("unknown option '{arg}'"));
        } else {
            positional.push(arg.clone());
        }

        index += 1;
    }

    let [source_path, name, destination_path] = positional.as_slice() else {
        return Err(
            "hugr move-symbol requires <source-path> <symbol> <destination-path>".to_string(),
        );
    };

    Ok(Command::MoveSymbol {
        source_path: source_path.clone(),
        name: name.clone(),
        destination_path: destination_path.clone(),
        kind,
        rewrite_references,
        format,
    })
}

fn parse_rename_symbol_command(args: &[String]) -> Result<Command, String> {
    let mut positional = Vec::new();
    let mut kind = None;
    let mut format = OutputFormat::Markdown;
    let mut index = 2;

    while index < args.len() {
        let arg = &args[index];
        if arg == "--json" {
            format = OutputFormat::Json;
        } else if arg == "--kind" {
            index += 1;
            kind = Some(required_option_value(
                args.get(index).map(String::as_str),
                "hugr rename-symbol --kind",
            )?);
        } else if let Some(value) = arg.strip_prefix("--kind=") {
            kind = Some(required_option_value(
                Some(value),
                "hugr rename-symbol --kind",
            )?);
        } else if arg.starts_with("--") {
            return Err(format!("unknown option '{arg}'"));
        } else {
            positional.push(arg.clone());
        }

        index += 1;
    }

    let [path, name, new_name] = positional.as_slice() else {
        return Err("hugr rename-symbol requires <path> <symbol> <new-symbol>".to_string());
    };

    Ok(Command::RenameSymbol {
        path: path.clone(),
        name: name.clone(),
        new_name: new_name.clone(),
        kind,
        format,
    })
}

fn parse_replace_symbol_command(args: &[String]) -> Result<Command, String> {
    let mut positional = Vec::new();
    let mut kind = None;
    let mut body = None;
    let mut body_file = None;
    let mut format = OutputFormat::Markdown;
    let mut index = 2;

    while index < args.len() {
        let arg = &args[index];
        if arg == "--json" {
            format = OutputFormat::Json;
        } else if arg == "--kind" {
            index += 1;
            kind = Some(required_option_value(
                args.get(index).map(String::as_str),
                "hugr replace-symbol --kind",
            )?);
        } else if let Some(value) = arg.strip_prefix("--kind=") {
            kind = Some(required_option_value(
                Some(value),
                "hugr replace-symbol --kind",
            )?);
        } else if arg == "--body" {
            index += 1;
            body = Some(
                args.get(index)
                    .cloned()
                    .ok_or_else(|| "hugr replace-symbol --body requires a value".to_string())?,
            );
        } else if let Some(value) = arg.strip_prefix("--body=") {
            body = Some(value.to_string());
        } else if arg == "--body-file" {
            index += 1;
            body_file = Some(required_option_value(
                args.get(index).map(String::as_str),
                "hugr replace-symbol --body-file",
            )?);
        } else if let Some(value) = arg.strip_prefix("--body-file=") {
            body_file = Some(required_option_value(
                Some(value),
                "hugr replace-symbol --body-file",
            )?);
        } else if arg.starts_with("--") {
            return Err(format!("unknown option '{arg}'"));
        } else {
            positional.push(arg.clone());
        }

        index += 1;
    }

    let [path, name] = positional.as_slice() else {
        return Err("hugr replace-symbol requires <path> <symbol>".to_string());
    };

    let body = match (body, body_file) {
        (Some(_), Some(_)) => {
            return Err("hugr replace-symbol accepts --body or --body-file, not both".to_string());
        }
        (Some(body), None) => body,
        (None, Some(file)) => std::fs::read_to_string(&file)
            .map_err(|error| format!("hugr replace-symbol --body-file: {error}"))?,
        (None, None) => {
            return Err("hugr replace-symbol requires --body or --body-file".to_string());
        }
    };

    if body.trim().is_empty() {
        return Err("hugr replace-symbol requires a non-empty body".to_string());
    }

    Ok(Command::ReplaceSymbol {
        path: path.clone(),
        name: name.clone(),
        kind,
        body,
        format,
    })
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
        Some("promote") => {
            let mut format = OutputFormat::Markdown;
            let mut llm = false;
            for arg in args.iter().skip(3) {
                match arg.as_str() {
                    "--json" => format = OutputFormat::Json,
                    "--llm" => llm = true,
                    unknown => return Err(format!("unknown option '{unknown}'")),
                }
            }
            Ok(Command::SessionPromote { format, llm })
        }
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

fn parse_context_command(args: &[String]) -> Result<Command, String> {
    let mut format = OutputFormat::Markdown;
    let mut budget = None;
    let mut words = Vec::new();
    let mut index = 2;

    while index < args.len() {
        match args[index].as_str() {
            "--json" => format = OutputFormat::Json,
            "--budget" => {
                index += 1;
                budget = Some(parse_positive_usize(args.get(index), "--budget")?);
            }
            word => words.push(word.to_string()),
        }
        index += 1;
    }

    let task = words.join(" ");
    if task.trim().is_empty() {
        return Err("hugr context requires text".to_string());
    }
    Ok(Command::Context {
        task,
        format,
        budget,
    })
}

fn parse_eval_command(args: &[String]) -> Result<Command, String> {
    let mut from_git = 30usize;
    let mut max_files = 8usize;
    let mut min_hit_rate = None;
    let mut format = OutputFormat::Markdown;
    let mut index = 2;

    while index < args.len() {
        match args[index].as_str() {
            "--json" => format = OutputFormat::Json,
            "--from-git" => {
                index += 1;
                from_git = parse_positive_usize(args.get(index), "--from-git")?;
            }
            "--max-files" => {
                index += 1;
                max_files = parse_positive_usize(args.get(index), "--max-files")?;
            }
            "--min-hit-rate" => {
                index += 1;
                min_hit_rate = Some(
                    args.get(index)
                        .ok_or("--min-hit-rate requires a value")?
                        .clone(),
                );
            }
            unknown => return Err(format!("unknown eval option '{unknown}'")),
        }
        index += 1;
    }

    Ok(Command::Eval {
        from_git,
        max_files,
        min_hit_rate,
        format,
    })
}

fn parse_positive_usize(value: Option<&String>, flag: &str) -> Result<usize, String> {
    let value = value.ok_or_else(|| format!("{flag} requires a value"))?;
    let parsed = value
        .parse::<usize>()
        .ok()
        .filter(|parsed| *parsed > 0)
        .ok_or_else(|| format!("{flag} requires a positive integer, got '{value}'"))?;
    Ok(parsed)
}

fn parse_install_command(args: &[String]) -> Result<Command, String> {
    let mut agent = None;
    let mut shared = false;

    for arg in args.iter().skip(2) {
        match arg.as_str() {
            "--shared" => shared = true,
            value if !value.starts_with("--") && agent.is_none() => {
                agent = Some(value.to_string());
            }
            unknown => return Err(format!("unknown install option '{unknown}'")),
        }
    }

    let agent =
        agent.ok_or("install requires an agent: hugr install <claude-code|cursor> [--shared]")?;
    Ok(Command::Install { agent, shared })
}

fn parse_hook_command(args: &[String]) -> Result<Command, String> {
    let agent = args
        .get(2)
        .cloned()
        .ok_or("hook requires an agent, e.g. hugr hook claude-code <event>")?;
    let event = args
        .get(3)
        .cloned()
        .ok_or("hook requires an event, e.g. hugr hook claude-code post-tool-use")?;
    Ok(Command::Hook { agent, event })
}

pub fn help_text() -> &'static str {
    "Hugr\n\nUsage:\n  hugr init\n  hugr status\n  hugr remember [--source <kind:locator>] [--confidence <0.0-1.0>] [--sensitivity <label>] [--valid-from <value>] [--valid-to <value>] <text>\n  hugr recall [--json] <query>\n  hugr context [--json] [--budget <tokens>] <task>\n  hugr index [--paths <p1,p2,...>]\n  hugr symbols [--json] <query>\n  hugr impact [--json] <file-or-symbol>\n  hugr replace-symbol [--json] [--kind <kind>] <path> <symbol> (--body <source> | --body-file <path>)\n  hugr rename-symbol [--json] [--kind <kind>] <path> <symbol> <new-symbol>\n  hugr move-symbol [--json] [--kind <kind>] [--rewrite-references] <source-path> <symbol> <destination-path>\n  hugr project status\n  hugr sync status [--json]\n  hugr sync push [--dry-run|--execute] [--json]\n  hugr sync pull [--dry-run|--execute] [--json]\n  hugr sync history [--json]\n  hugr session start <task>\n  hugr session event <kind> <detail>\n  hugr session end [summary]\n  hugr session promote [--llm] [--json]\n  hugr mcp\n  hugr daemon [--addr <host:port>]\n  hugr run [--] <command> [args...]\n  hugr observe command --status <code> -- <command> [args...]\n  hugr shell-hook <bash|zsh>\n  hugr improve [--execute] [--duplicates|--stale] [--json]\n  hugr forget [--json] <query>\n  hugr eval [--json] [--from-git <n>] [--max-files <n>] [--min-hit-rate <0.0-1.0>]\n  hugr install <claude-code|cursor> [--shared]\n  hugr doctor\n"
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
                format: OutputFormat::Markdown,
                budget: None
            })
        );
    }

    #[test]
    fn parses_context_budget_option() {
        let args = vec![
            "hugr".into(),
            "context".into(),
            "--budget".into(),
            "16000".into(),
            "add".into(),
            "hooks".into(),
        ];
        assert_eq!(
            Command::parse(&args),
            Ok(Command::Context {
                task: "add hooks".into(),
                format: OutputFormat::Markdown,
                budget: Some(16000)
            })
        );

        let invalid = vec![
            "hugr".into(),
            "context".into(),
            "--budget".into(),
            "zero".into(),
            "task".into(),
        ];
        assert!(Command::parse(&invalid).is_err());
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
        assert_eq!(
            Command::parse(&args),
            Ok(Command::Index { paths: Vec::new() })
        );
    }

    #[test]
    fn parses_index_command_with_paths() {
        let args = vec![
            "hugr".into(),
            "index".into(),
            "--paths".into(),
            "src/a.rs, src/b.rs".into(),
        ];
        assert_eq!(
            Command::parse(&args),
            Ok(Command::Index {
                paths: vec!["src/a.rs".to_string(), "src/b.rs".to_string()],
            })
        );
    }

    #[test]
    fn rejects_index_command_positional() {
        let args = vec!["hugr".into(), "index".into(), "src/a.rs".into()];
        assert!(Command::parse(&args).is_err());
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
    fn parses_replace_symbol_command() {
        assert_eq!(
            Command::parse(&[
                "hugr".into(),
                "replace-symbol".into(),
                "--json".into(),
                "--kind".into(),
                "function".into(),
                "src/lib.rs".into(),
                "greet".into(),
                "--body".into(),
                "pub fn greet() {}".into(),
            ]),
            Ok(Command::ReplaceSymbol {
                path: "src/lib.rs".into(),
                name: "greet".into(),
                kind: Some("function".into()),
                body: "pub fn greet() {}".into(),
                format: OutputFormat::Json,
            })
        );
        assert_eq!(
            Command::parse(&[
                "hugr".into(),
                "replace-symbol".into(),
                "src/lib.rs".into(),
                "greet".into(),
                "--body=pub fn greet() {}".into(),
            ]),
            Ok(Command::ReplaceSymbol {
                path: "src/lib.rs".into(),
                name: "greet".into(),
                kind: None,
                body: "pub fn greet() {}".into(),
                format: OutputFormat::Markdown,
            })
        );
    }

    #[test]
    fn parses_rename_symbol_command() {
        assert_eq!(
            Command::parse(&[
                "hugr".into(),
                "rename-symbol".into(),
                "--json".into(),
                "--kind".into(),
                "function".into(),
                "src/lib.rs".into(),
                "greet".into(),
                "welcome".into(),
            ]),
            Ok(Command::RenameSymbol {
                path: "src/lib.rs".into(),
                name: "greet".into(),
                new_name: "welcome".into(),
                kind: Some("function".into()),
                format: OutputFormat::Json,
            })
        );
    }

    #[test]
    fn rejects_rename_symbol_missing_positional() {
        assert_eq!(
            Command::parse(&[
                "hugr".into(),
                "rename-symbol".into(),
                "src/lib.rs".into(),
                "greet".into(),
            ]),
            Err("hugr rename-symbol requires <path> <symbol> <new-symbol>".into())
        );
    }

    #[test]
    fn parses_move_symbol_command() {
        assert_eq!(
            Command::parse(&[
                "hugr".into(),
                "move-symbol".into(),
                "--json".into(),
                "--kind=function".into(),
                "src/lib.rs".into(),
                "helper".into(),
                "src/helpers.rs".into(),
            ]),
            Ok(Command::MoveSymbol {
                source_path: "src/lib.rs".into(),
                name: "helper".into(),
                destination_path: "src/helpers.rs".into(),
                kind: Some("function".into()),
                rewrite_references: false,
                format: OutputFormat::Json,
            })
        );
    }

    #[test]
    fn parses_move_symbol_with_reference_rewrite() {
        assert_eq!(
            Command::parse(&[
                "hugr".into(),
                "move-symbol".into(),
                "--rewrite-references".into(),
                "src/lib.rs".into(),
                "helper".into(),
                "src/helpers.rs".into(),
            ]),
            Ok(Command::MoveSymbol {
                source_path: "src/lib.rs".into(),
                name: "helper".into(),
                destination_path: "src/helpers.rs".into(),
                kind: None,
                rewrite_references: true,
                format: OutputFormat::Markdown,
            })
        );
    }

    #[test]
    fn rejects_move_symbol_missing_positional() {
        assert_eq!(
            Command::parse(&[
                "hugr".into(),
                "move-symbol".into(),
                "src/lib.rs".into(),
                "helper".into(),
            ]),
            Err("hugr move-symbol requires <source-path> <symbol> <destination-path>".into())
        );
    }

    #[test]
    fn rejects_replace_symbol_without_body() {
        assert_eq!(
            Command::parse(&[
                "hugr".into(),
                "replace-symbol".into(),
                "src/lib.rs".into(),
                "greet".into(),
            ]),
            Err("hugr replace-symbol requires --body or --body-file".into())
        );
    }

    #[test]
    fn rejects_replace_symbol_with_both_body_sources() {
        assert_eq!(
            Command::parse(&[
                "hugr".into(),
                "replace-symbol".into(),
                "src/lib.rs".into(),
                "greet".into(),
                "--body".into(),
                "pub fn greet() {}".into(),
                "--body-file".into(),
                "body.txt".into(),
            ]),
            Err("hugr replace-symbol accepts --body or --body-file, not both".into())
        );
    }

    #[test]
    fn rejects_replace_symbol_missing_positional() {
        assert_eq!(
            Command::parse(&[
                "hugr".into(),
                "replace-symbol".into(),
                "src/lib.rs".into(),
                "--body".into(),
                "pub fn greet() {}".into(),
            ]),
            Err("hugr replace-symbol requires <path> <symbol>".into())
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
                format: OutputFormat::Json,
                llm: false
            })
        );
        assert_eq!(
            Command::parse(&[
                "hugr".into(),
                "session".into(),
                "promote".into(),
                "--llm".into(),
                "--json".into()
            ]),
            Ok(Command::SessionPromote {
                format: OutputFormat::Json,
                llm: true
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
