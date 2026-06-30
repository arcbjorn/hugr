use crate::code::CodeSymbol;
use crate::store::{Memory, SessionFact};
use crate::testmap::TestCandidate;
use crate::worktree::WorktreeState;
use std::fmt::Write;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextPack {
    pub task: String,
    pub relevant_files: Vec<ContextFile>,
    pub important_symbols: Vec<ContextSymbol>,
    pub affected_tests: Vec<ContextTest>,
    pub relevant_memories: Vec<ContextMemory>,
    pub recent_sessions: Vec<ContextSessionFact>,
    pub branch_state: Option<ContextBranchState>,
    pub suggested_path: Vec<String>,
    pub citations: Vec<Citation>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextFile {
    pub path: String,
    pub citation_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextSymbol {
    pub path: String,
    pub language: Option<String>,
    pub name: String,
    pub kind: String,
    pub line_start: i64,
    pub line_end: Option<i64>,
    pub signature: String,
    pub citation_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextTest {
    pub path: String,
    pub reason: String,
    pub citation_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextMemory {
    pub id: String,
    pub created_at_ms: i64,
    pub kind: String,
    pub text: String,
    pub citation_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextSessionFact {
    pub session_id: String,
    pub kind: String,
    pub detail: String,
    pub created_at_ms: i64,
    pub citation_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextBranchState {
    pub root_path: Option<String>,
    pub branch: Option<String>,
    pub upstream: Option<String>,
    pub ahead: i64,
    pub behind: i64,
    pub changed_files: Vec<ContextChangedFile>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextChangedFile {
    pub path: String,
    pub original_path: Option<String>,
    pub staged_status: Option<String>,
    pub unstaged_status: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Citation {
    pub id: String,
    pub source_type: String,
    pub label: String,
}

impl ContextPack {
    #[cfg(test)]
    pub fn new(task: &str, files: Vec<String>, memories: Vec<Memory>) -> Self {
        Self::with_sessions(task, files, memories, Vec::new())
    }

    #[cfg(test)]
    pub fn with_sessions(
        task: &str,
        files: Vec<String>,
        memories: Vec<Memory>,
        sessions: Vec<SessionFact>,
    ) -> Self {
        Self::with_sessions_symbols_tests_and_branch(
            task,
            files,
            memories,
            sessions,
            Vec::new(),
            Vec::new(),
            None,
        )
    }

    pub(crate) fn with_sessions_symbols_tests_and_branch(
        task: &str,
        files: Vec<String>,
        memories: Vec<Memory>,
        sessions: Vec<SessionFact>,
        symbols: Vec<CodeSymbol>,
        tests: Vec<TestCandidate>,
        branch_state: Option<WorktreeState>,
    ) -> Self {
        let relevant_files = files
            .into_iter()
            .map(|path| ContextFile {
                citation_id: format!("file:{path}"),
                path,
            })
            .collect::<Vec<_>>();
        let important_symbols = symbols
            .into_iter()
            .map(|symbol| ContextSymbol {
                citation_id: format!(
                    "symbol:{}:{}:{}",
                    symbol.path, symbol.line_start, symbol.name
                ),
                path: symbol.path,
                language: symbol.language,
                name: symbol.name,
                kind: symbol.kind,
                line_start: symbol.line_start,
                line_end: symbol.line_end,
                signature: symbol.signature,
            })
            .collect::<Vec<_>>();
        let affected_tests = tests
            .into_iter()
            .map(|test| ContextTest {
                citation_id: format!("test:{}", test.path),
                path: test.path,
                reason: test.reason,
            })
            .collect::<Vec<_>>();
        let relevant_memories = memories
            .into_iter()
            .map(|memory| ContextMemory {
                citation_id: memory.id.clone(),
                id: memory.id,
                created_at_ms: memory.created_at_ms,
                kind: memory.kind,
                text: memory.text,
            })
            .collect::<Vec<_>>();
        let recent_sessions = sessions
            .into_iter()
            .map(|fact| ContextSessionFact {
                citation_id: format!("session:{}", fact.session_id),
                session_id: fact.session_id,
                kind: fact.kind,
                detail: fact.detail,
                created_at_ms: fact.created_at_ms,
            })
            .collect::<Vec<_>>();
        let branch_state = branch_state
            .filter(|state| state.inside_worktree)
            .map(|state| ContextBranchState {
                root_path: state.root_path,
                branch: state.branch,
                upstream: state.upstream,
                ahead: state.ahead,
                behind: state.behind,
                changed_files: state
                    .changed_files
                    .into_iter()
                    .map(|file| ContextChangedFile {
                        path: file.path,
                        original_path: file.original_path,
                        staged_status: file.staged_status,
                        unstaged_status: file.unstaged_status,
                    })
                    .collect(),
            });
        let mut citations = relevant_files
            .iter()
            .map(|file| Citation {
                id: file.citation_id.clone(),
                source_type: "file".to_string(),
                label: file.path.clone(),
            })
            .collect::<Vec<_>>();
        citations.extend(important_symbols.iter().map(|symbol| Citation {
            id: symbol.citation_id.clone(),
            source_type: "symbol".to_string(),
            label: format!(
                "{} {} at {}:{}",
                symbol.kind, symbol.name, symbol.path, symbol.line_start
            ),
        }));
        citations.extend(affected_tests.iter().map(|test| Citation {
            id: test.citation_id.clone(),
            source_type: "test".to_string(),
            label: test.path.clone(),
        }));
        citations.extend(relevant_memories.iter().map(|memory| Citation {
            id: memory.citation_id.clone(),
            source_type: "memory".to_string(),
            label: memory.text.clone(),
        }));
        citations.extend(recent_sessions.iter().map(|fact| Citation {
            id: fact.citation_id.clone(),
            source_type: "session".to_string(),
            label: fact.detail.clone(),
        }));

        Self {
            task: task.to_string(),
            relevant_files,
            important_symbols,
            affected_tests,
            relevant_memories,
            recent_sessions,
            branch_state,
            suggested_path: vec![
                "Inspect the relevant files and symbols.".to_string(),
                "Check whether any memories are stale before relying on them.".to_string(),
                "Make the smallest change that satisfies the task.".to_string(),
                "Run the narrowest useful tests, then broaden if risk is unclear.".to_string(),
            ],
            citations,
        }
    }

    pub fn render_markdown(&self) -> String {
        let mut rendered = String::new();

        rendered.push_str("# Hugr Context Pack\n\n");
        rendered.push_str("## Task\n");
        rendered.push_str(&self.task);
        rendered.push_str("\n\n");

        rendered.push_str("## Relevant Files\n");
        if self.relevant_files.is_empty() {
            rendered.push_str("No file candidates found yet.\n");
        } else {
            for file in &self.relevant_files {
                let _ = writeln!(rendered, "- {} [{}]", file.path, file.citation_id);
            }
        }
        rendered.push('\n');

        rendered.push_str("## Important Symbols\n");
        if self.important_symbols.is_empty() {
            rendered.push_str("No matching symbols indexed yet.\n");
        } else {
            for symbol in &self.important_symbols {
                let _ = writeln!(
                    rendered,
                    "- {} {} at {} [{}]",
                    symbol.kind,
                    symbol.name,
                    symbol_location(symbol),
                    symbol.citation_id
                );
            }
        }
        rendered.push('\n');

        rendered.push_str("## Affected Tests\n");
        if self.affected_tests.is_empty() {
            rendered.push_str("No likely tests mapped yet.\n");
        } else {
            for test in &self.affected_tests {
                let _ = writeln!(
                    rendered,
                    "- {} ({}) [{}]",
                    test.path, test.reason, test.citation_id
                );
            }
        }
        rendered.push('\n');

        rendered.push_str("## Relevant Memories\n");
        if self.relevant_memories.is_empty() {
            rendered.push_str("No matching memories yet.\n");
        } else {
            for memory in &self.relevant_memories {
                let _ = writeln!(
                    rendered,
                    "- {} [{}]: {}",
                    memory.id, memory.kind, memory.text
                );
            }
        }
        rendered.push('\n');

        rendered.push_str("## Recent Sessions\n");
        if self.recent_sessions.is_empty() {
            rendered.push_str("No matching session facts yet.\n");
        } else {
            for fact in &self.recent_sessions {
                let _ = writeln!(
                    rendered,
                    "- {} [{}]: {}",
                    fact.session_id, fact.kind, fact.detail
                );
            }
        }
        rendered.push('\n');

        rendered.push_str("## Branch State\n");
        if let Some(branch) = &self.branch_state {
            let branch_name = branch.branch.as_deref().unwrap_or("unknown");
            let upstream = branch.upstream.as_deref().unwrap_or("none");
            let _ = writeln!(
                rendered,
                "- branch: {branch_name}\n- upstream: {upstream}\n- ahead: {}\n- behind: {}",
                branch.ahead, branch.behind
            );
            if branch.changed_files.is_empty() {
                rendered.push_str("- changes: clean\n");
            } else {
                rendered.push_str("- changes:\n");
                for file in &branch.changed_files {
                    let _ = writeln!(rendered, "  - {} [{}]", file.path, change_label(file));
                }
            }
        } else {
            rendered.push_str("No git worktree detected.\n");
        }
        rendered.push('\n');

        rendered.push_str("## Suggested Path\n");
        for (index, step) in self.suggested_path.iter().enumerate() {
            let _ = writeln!(rendered, "{}. {}", index + 1, step);
        }
        rendered.push('\n');

        rendered.push_str("## Citations\n");
        if self.citations.is_empty() {
            rendered.push_str("- No citations yet.\n");
        } else {
            for citation in &self.citations {
                let _ = writeln!(
                    rendered,
                    "- {} [{}]: {}",
                    citation.id, citation.source_type, citation.label
                );
            }
        }

        rendered
    }

    pub fn render_json(&self) -> String {
        let mut rendered = String::new();

        rendered.push('{');
        let _ = write!(rendered, "\"task\":{},", json_string(&self.task));

        rendered.push_str("\"relevant_files\":[");
        for (index, file) in self.relevant_files.iter().enumerate() {
            if index > 0 {
                rendered.push(',');
            }
            let _ = write!(
                rendered,
                "{{\"path\":{},\"citation_id\":{}}}",
                json_string(&file.path),
                json_string(&file.citation_id)
            );
        }
        rendered.push_str("],");

        rendered.push_str("\"important_symbols\":[");
        for (index, symbol) in self.important_symbols.iter().enumerate() {
            if index > 0 {
                rendered.push(',');
            }
            let _ = write!(
                rendered,
                "{{\"path\":{},\"language\":{},\"name\":{},\"kind\":{},\"line_start\":{},\"line_end\":{},\"signature\":{},\"citation_id\":{}}}",
                json_string(&symbol.path),
                json_option_string(symbol.language.as_deref()),
                json_string(&symbol.name),
                json_string(&symbol.kind),
                symbol.line_start,
                json_optional_i64(symbol.line_end),
                json_string(&symbol.signature),
                json_string(&symbol.citation_id)
            );
        }
        rendered.push_str("],");

        rendered.push_str("\"affected_tests\":[");
        for (index, test) in self.affected_tests.iter().enumerate() {
            if index > 0 {
                rendered.push(',');
            }
            let _ = write!(
                rendered,
                "{{\"path\":{},\"reason\":{},\"citation_id\":{}}}",
                json_string(&test.path),
                json_string(&test.reason),
                json_string(&test.citation_id)
            );
        }
        rendered.push_str("],");

        rendered.push_str("\"relevant_memories\":[");
        for (index, memory) in self.relevant_memories.iter().enumerate() {
            if index > 0 {
                rendered.push(',');
            }
            let _ = write!(
                rendered,
                "{{\"id\":{},\"created_at_ms\":{},\"kind\":{},\"text\":{},\"citation_id\":{}}}",
                json_string(&memory.id),
                memory.created_at_ms,
                json_string(&memory.kind),
                json_string(&memory.text),
                json_string(&memory.citation_id)
            );
        }
        rendered.push_str("],");

        rendered.push_str("\"recent_sessions\":[");
        for (index, fact) in self.recent_sessions.iter().enumerate() {
            if index > 0 {
                rendered.push(',');
            }
            let _ = write!(
                rendered,
                "{{\"session_id\":{},\"kind\":{},\"detail\":{},\"created_at_ms\":{},\"citation_id\":{}}}",
                json_string(&fact.session_id),
                json_string(&fact.kind),
                json_string(&fact.detail),
                fact.created_at_ms,
                json_string(&fact.citation_id)
            );
        }
        rendered.push_str("],");

        rendered.push_str("\"branch_state\":");
        if let Some(branch) = &self.branch_state {
            rendered.push('{');
            let _ = write!(
                rendered,
                "\"root_path\":{},\"branch\":{},\"upstream\":{},\"ahead\":{},\"behind\":{},\"changed_files\":[",
                json_option_string(branch.root_path.as_deref()),
                json_option_string(branch.branch.as_deref()),
                json_option_string(branch.upstream.as_deref()),
                branch.ahead,
                branch.behind
            );
            for (index, file) in branch.changed_files.iter().enumerate() {
                if index > 0 {
                    rendered.push(',');
                }
                let _ = write!(
                    rendered,
                    "{{\"path\":{},\"original_path\":{},\"staged_status\":{},\"unstaged_status\":{}}}",
                    json_string(&file.path),
                    json_option_string(file.original_path.as_deref()),
                    json_option_string(file.staged_status.as_deref()),
                    json_option_string(file.unstaged_status.as_deref())
                );
            }
            rendered.push_str("]},");
        } else {
            rendered.push_str("null,");
        }

        rendered.push_str("\"suggested_path\":[");
        for (index, step) in self.suggested_path.iter().enumerate() {
            if index > 0 {
                rendered.push(',');
            }
            rendered.push_str(&json_string(step));
        }
        rendered.push_str("],");

        rendered.push_str("\"citations\":[");
        for (index, citation) in self.citations.iter().enumerate() {
            if index > 0 {
                rendered.push(',');
            }
            let _ = write!(
                rendered,
                "{{\"id\":{},\"source_type\":{},\"label\":{}}}",
                json_string(&citation.id),
                json_string(&citation.source_type),
                json_string(&citation.label)
            );
        }
        rendered.push_str("]}");

        rendered
    }
}

pub(crate) fn json_string(value: &str) -> String {
    let mut escaped = String::from("\"");
    for char in value.chars() {
        match char {
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            char if char.is_control() => {
                let _ = write!(escaped, "\\u{:04x}", char as u32);
            }
            char => escaped.push(char),
        }
    }
    escaped.push('"');
    escaped
}

fn json_option_string(value: Option<&str>) -> String {
    value.map(json_string).unwrap_or_else(|| "null".to_string())
}

fn json_optional_i64(value: Option<i64>) -> String {
    value
        .map(|value| value.to_string())
        .unwrap_or_else(|| "null".to_string())
}

fn symbol_location(symbol: &ContextSymbol) -> String {
    match symbol.line_end {
        Some(line_end) if line_end > symbol.line_start => {
            format!("{}:{}-{}", symbol.path, symbol.line_start, line_end)
        }
        _ => format!("{}:{}", symbol.path, symbol.line_start),
    }
}

fn change_label(file: &ContextChangedFile) -> String {
    match (&file.staged_status, &file.unstaged_status) {
        (Some(staged), Some(unstaged)) if staged == unstaged => staged.clone(),
        (Some(staged), Some(unstaged)) => format!("staged {staged}, unstaged {unstaged}"),
        (Some(staged), None) => format!("staged {staged}"),
        (None, Some(unstaged)) => format!("unstaged {unstaged}"),
        (None, None) => "changed".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::{ContextPack, json_string};
    use crate::code::CodeSymbol;
    use crate::store::Memory;
    use crate::testmap::TestCandidate;
    use crate::worktree::{ChangedFile, WorktreeState};

    #[test]
    fn markdown_includes_citations_for_files_and_memories() {
        let pack = ContextPack::new(
            "add plugin hooks",
            vec!["src/plugin.rs".to_string()],
            vec![Memory {
                id: "mem_1".to_string(),
                created_at_ms: 7,
                kind: "fact".to_string(),
                text: "plugin hooks run after configuration is loaded".to_string(),
            }],
        );

        let markdown = pack.render_markdown();

        assert!(markdown.contains("- src/plugin.rs [file:src/plugin.rs]"));
        assert!(
            markdown.contains("- mem_1 [memory]: plugin hooks run after configuration is loaded")
        );
    }

    #[test]
    fn markdown_and_json_include_symbols() {
        let pack = ContextPack::with_sessions_symbols_tests_and_branch(
            "add plugin hooks",
            vec!["src/plugin_hooks.rs".to_string()],
            Vec::new(),
            Vec::new(),
            vec![CodeSymbol {
                path: "src/plugin_hooks.rs".to_string(),
                language: Some("rust".to_string()),
                name: "PluginHooks".to_string(),
                kind: "struct".to_string(),
                line_start: 3,
                line_end: Some(8),
                signature: "pub struct PluginHooks".to_string(),
            }],
            vec![TestCandidate {
                path: "tests/plugin_hooks.rs".to_string(),
                reason: "repository tests directory match".to_string(),
            }],
            None,
        );

        let markdown = pack.render_markdown();
        let json = pack.render_json();

        assert!(markdown.contains("- struct PluginHooks at src/plugin_hooks.rs:3-8"));
        assert!(json.contains("\"important_symbols\""));
        assert!(json.contains("\"name\":\"PluginHooks\""));
        assert!(json.contains("\"line_end\":8"));
        assert!(markdown.contains("- tests/plugin_hooks.rs"));
        assert!(json.contains("\"affected_tests\""));
    }

    #[test]
    fn json_renderer_escapes_strings() {
        let pack = ContextPack::new("quote \"and\" newline\n", Vec::new(), Vec::new());
        let json = pack.render_json();

        assert!(json.contains("\"task\":\"quote \\\"and\\\" newline\\n\""));
        assert_eq!(json_string("tab\tbackslash\\"), "\"tab\\tbackslash\\\\\"");
    }

    #[test]
    fn markdown_and_json_include_branch_state() {
        let pack = ContextPack::with_sessions_symbols_tests_and_branch(
            "add plugin hooks",
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Some(WorktreeState {
                inside_worktree: true,
                root_path: Some("/repo".to_string()),
                branch: Some("feature".to_string()),
                upstream: Some("origin/feature".to_string()),
                ahead: 2,
                behind: 1,
                changed_files: vec![ChangedFile {
                    path: "src/lib.rs".to_string(),
                    original_path: None,
                    staged_status: None,
                    unstaged_status: Some("modified".to_string()),
                }],
            }),
        );

        let markdown = pack.render_markdown();
        let json = pack.render_json();

        assert!(markdown.contains("## Branch State"));
        assert!(markdown.contains("- branch: feature"));
        assert!(markdown.contains("src/lib.rs [unstaged modified]"));
        assert!(json.contains("\"branch_state\""));
        assert!(json.contains("\"ahead\":2"));
        assert!(json.contains("\"unstaged_status\":\"modified\""));
    }
}
