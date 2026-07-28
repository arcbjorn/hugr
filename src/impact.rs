use crate::code::{CodeReference, CodeSymbol};
use crate::error::Result;
use crate::store::Store;
use crate::testmap::TestCandidate;
use serde::Serialize;
use std::collections::BTreeSet;
use std::fmt::Write;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct ImpactReport {
    pub target: String,
    pub matched_symbols: Vec<CodeSymbol>,
    pub references: Vec<CodeReference>,
    pub outbound_references: Vec<CodeReference>,
    pub affected_files: Vec<String>,
    pub likely_tests: Vec<TestCandidate>,
    pub notes: Vec<String>,
}

pub(crate) async fn analyze(store: &Store, target: &str, limit: usize) -> Result<ImpactReport> {
    let matched_symbols = store.symbols_for_target(target, limit).await?;
    let references = store.references_to_symbols(&matched_symbols, limit).await?;
    let outbound_references = if is_file_target(target, &matched_symbols) {
        store.references_from_path(target, limit).await?
    } else {
        store
            .references_from_symbols(&matched_symbols, limit)
            .await?
    };
    let mut report = ImpactReport::new(
        target,
        matched_symbols,
        references,
        outbound_references,
        Vec::new(),
    );
    report.likely_tests = store
        .likely_tests_for_files(&report.affected_files, limit)
        .await?;
    Ok(report)
}

impl ImpactReport {
    fn new(
        target: &str,
        matched_symbols: Vec<CodeSymbol>,
        references: Vec<CodeReference>,
        outbound_references: Vec<CodeReference>,
        likely_tests: Vec<TestCandidate>,
    ) -> Self {
        let mut affected = BTreeSet::new();
        for symbol in &matched_symbols {
            affected.insert(symbol.path.clone());
        }
        for reference in &references {
            affected.insert(reference.path.clone());
        }
        for reference in &outbound_references {
            affected.insert(reference.path.clone());
            affected.insert(reference.target_path.clone());
        }

        let notes = if matched_symbols.is_empty() {
            vec!["No indexed symbols matched the target.".to_string()]
        } else if references.is_empty() && outbound_references.is_empty() {
            vec!["No direct indexed relationships found for matched symbols.".to_string()]
        } else {
            Vec::new()
        };

        Self {
            target: target.to_string(),
            matched_symbols,
            references,
            outbound_references,
            affected_files: affected.into_iter().collect(),
            likely_tests,
            notes,
        }
    }

    pub(crate) fn render_markdown(&self) -> String {
        let mut rendered = String::new();

        rendered.push_str("# Hugr Impact Report\n\n");
        rendered.push_str("## Target\n");
        rendered.push_str(&self.target);
        rendered.push_str("\n\n");

        rendered.push_str("## Matched Symbols\n");
        if self.matched_symbols.is_empty() {
            rendered.push_str("No indexed symbols matched.\n");
        } else {
            for symbol in &self.matched_symbols {
                let _ = writeln!(
                    rendered,
                    "- {} {} at {}",
                    symbol.kind,
                    symbol.name,
                    symbol_location(symbol)
                );
            }
        }
        rendered.push('\n');

        rendered.push_str("## References To Target\n");
        if self.references.is_empty() {
            rendered.push_str("No direct indexed references found.\n");
        } else {
            for reference in &self.references {
                let _ = writeln!(
                    rendered,
                    "- {}:{} [{}] -> {} {}: {}",
                    reference.path,
                    reference.line_start,
                    reference.kind,
                    reference.target_kind,
                    reference.target_name,
                    reference.excerpt
                );
            }
        }
        rendered.push('\n');

        rendered.push_str("## References From Target Scope\n");
        if self.outbound_references.is_empty() {
            rendered.push_str("No outbound indexed references found.\n");
        } else {
            for reference in &self.outbound_references {
                let _ = writeln!(
                    rendered,
                    "- {}:{} [{}] -> {} {}: {}",
                    reference.path,
                    reference.line_start,
                    reference.kind,
                    reference.target_kind,
                    reference.target_name,
                    reference.excerpt
                );
            }
        }
        rendered.push('\n');

        rendered.push_str("## Affected Files\n");
        if self.affected_files.is_empty() {
            rendered.push_str("No affected files found.\n");
        } else {
            for file in &self.affected_files {
                let _ = writeln!(rendered, "- {file}");
            }
        }

        rendered.push_str("\n## Likely Tests\n");
        if self.likely_tests.is_empty() {
            rendered.push_str("No likely tests mapped yet.\n");
        } else {
            for test in &self.likely_tests {
                let _ = writeln!(
                    rendered,
                    "- {} ({}, score {})",
                    test.path, test.reason, test.score
                );
            }
        }

        if !self.notes.is_empty() {
            rendered.push_str("\n## Notes\n");
            for note in &self.notes {
                let _ = writeln!(rendered, "- {note}");
            }
        }

        rendered
    }

    /// Renders the report as compact JSON. Field order follows the struct
    /// declarations, which the snapshot test pins.
    pub(crate) fn render_json(&self) -> String {
        crate::json::render(self)
    }
}

fn symbol_location(symbol: &CodeSymbol) -> String {
    match symbol.line_end {
        Some(line_end) if line_end > symbol.line_start => {
            format!("{}:{}-{}", symbol.path, symbol.line_start, line_end)
        }
        _ => format!("{}:{}", symbol.path, symbol.line_start),
    }
}

fn is_file_target(target: &str, symbols: &[CodeSymbol]) -> bool {
    let target = target.trim().trim_start_matches("./").replace('\\', "/");
    symbols.iter().any(|symbol| symbol.path == target)
}

#[cfg(test)]
mod tests {
    use super::ImpactReport;
    use crate::code::{CodeReference, CodeSymbol};
    use crate::testmap::TestCandidate;

    #[test]
    fn renders_impact_report() {
        let report = ImpactReport::new(
            "PluginHooks",
            vec![CodeSymbol {
                path: "src/plugin_hooks.rs".to_string(),
                language: Some("rust".to_string()),
                name: "PluginHooks".to_string(),
                kind: "struct".to_string(),
                line_start: 1,
                line_end: Some(3),
                signature: "pub struct PluginHooks".to_string(),
            }],
            vec![CodeReference {
                path: "src/main.rs".to_string(),
                language: Some("rust".to_string()),
                target_path: "src/plugin_hooks.rs".to_string(),
                target_name: "PluginHooks".to_string(),
                target_kind: "struct".to_string(),
                kind: "reference".to_string(),
                line_start: 4,
                excerpt: "let _hooks = PluginHooks {};".to_string(),
            }],
            vec![CodeReference {
                path: "src/plugin_hooks.rs".to_string(),
                language: Some("rust".to_string()),
                target_path: "src/store.rs".to_string(),
                target_name: "Store".to_string(),
                target_kind: "struct".to_string(),
                kind: "reference".to_string(),
                line_start: 5,
                excerpt: "Store::open_current();".to_string(),
            }],
            vec![TestCandidate {
                path: "tests/plugin_hooks.rs".to_string(),
                reason: "repository tests directory match".to_string(),
                score: 50,
            }],
        );

        let markdown = report.render_markdown();
        let json = report.render_json();

        assert!(markdown.contains("- struct PluginHooks at src/plugin_hooks.rs:1-3"));
        assert!(markdown.contains("- src/main.rs:4 [reference]"));
        assert!(markdown.contains("## References From Target Scope"));
        assert!(json.contains("\"target\":\"PluginHooks\""));
        assert!(json.contains("\"line_end\":3"));
        assert!(json.contains("\"outbound_references\""));
        assert!(json.contains("\"likely_tests\""));
        assert!(json.contains("\"affected_files\""));
    }

    fn snapshot_report() -> ImpactReport {
        let reference = |language: Option<&str>| CodeReference {
            path: "src/main.rs".to_string(),
            language: language.map(str::to_string),
            target_path: "src/plugin_hooks.rs".to_string(),
            target_name: "run_after_config".to_string(),
            target_kind: "function".to_string(),
            kind: "call".to_string(),
            line_start: 8,
            excerpt: "run_after_config(); // \"quoted\"\ttab".to_string(),
        };

        ImpactReport {
            target: "run_after_config".to_string(),
            matched_symbols: vec![
                CodeSymbol {
                    path: "src/plugin_hooks.rs".to_string(),
                    language: Some("rust".to_string()),
                    name: "run_after_config".to_string(),
                    kind: "function".to_string(),
                    line_start: 12,
                    line_end: Some(40),
                    signature: "pub fn run_after_config()".to_string(),
                },
                CodeSymbol {
                    path: "src/other.rs".to_string(),
                    language: None,
                    name: "helper".to_string(),
                    kind: "function".to_string(),
                    line_start: 1,
                    line_end: None,
                    signature: String::new(),
                },
            ],
            references: vec![reference(Some("rust"))],
            outbound_references: vec![reference(None)],
            affected_files: vec!["src/main.rs".to_string()],
            likely_tests: vec![TestCandidate {
                path: "tests/plugin_hooks.rs".to_string(),
                reason: "matching test filename".to_string(),
                score: 80,
            }],
            notes: vec!["high fan-in".to_string()],
        }
    }

    /// Pins the `impact --json` bytes; agents parse this to decide blast
    /// radius before an edit.
    #[test]
    fn renders_a_stable_json_snapshot() {
        assert_eq!(snapshot_report().render_json(), SNAPSHOT);
    }

    const SNAPSHOT: &str = r#"{"target":"run_after_config","matched_symbols":[{"path":"src/plugin_hooks.rs","language":"rust","name":"run_after_config","kind":"function","line_start":12,"line_end":40,"signature":"pub fn run_after_config()"},{"path":"src/other.rs","language":null,"name":"helper","kind":"function","line_start":1,"line_end":null,"signature":""}],"references":[{"path":"src/main.rs","language":"rust","target_path":"src/plugin_hooks.rs","target_name":"run_after_config","target_kind":"function","kind":"call","line_start":8,"excerpt":"run_after_config(); // \"quoted\"\ttab"}],"outbound_references":[{"path":"src/main.rs","language":null,"target_path":"src/plugin_hooks.rs","target_name":"run_after_config","target_kind":"function","kind":"call","line_start":8,"excerpt":"run_after_config(); // \"quoted\"\ttab"}],"affected_files":["src/main.rs"],"likely_tests":[{"path":"tests/plugin_hooks.rs","reason":"matching test filename","score":80}],"notes":["high fan-in"]}"#;
}
