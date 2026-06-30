use crate::store::Memory;
use std::fmt::Write;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextPack {
    pub task: String,
    pub relevant_files: Vec<ContextFile>,
    pub relevant_memories: Vec<ContextMemory>,
    pub suggested_path: Vec<String>,
    pub citations: Vec<Citation>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextFile {
    pub path: String,
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
pub struct Citation {
    pub id: String,
    pub source_type: String,
    pub label: String,
}

impl ContextPack {
    pub fn new(task: &str, files: Vec<String>, memories: Vec<Memory>) -> Self {
        let relevant_files = files
            .into_iter()
            .map(|path| ContextFile {
                citation_id: format!("file:{path}"),
                path,
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
        let mut citations = relevant_files
            .iter()
            .map(|file| Citation {
                id: file.citation_id.clone(),
                source_type: "file".to_string(),
                label: file.path.clone(),
            })
            .collect::<Vec<_>>();
        citations.extend(relevant_memories.iter().map(|memory| Citation {
            id: memory.citation_id.clone(),
            source_type: "memory".to_string(),
            label: memory.text.clone(),
        }));

        Self {
            task: task.to_string(),
            relevant_files,
            relevant_memories,
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

#[cfg(test)]
mod tests {
    use super::{ContextPack, json_string};
    use crate::store::Memory;

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
    fn json_renderer_escapes_strings() {
        let pack = ContextPack::new("quote \"and\" newline\n", Vec::new(), Vec::new());
        let json = pack.render_json();

        assert!(json.contains("\"task\":\"quote \\\"and\\\" newline\\n\""));
        assert_eq!(json_string("tab\tbackslash\\"), "\"tab\\tbackslash\\\\\"");
    }
}
