use crate::code::{CodeReference, CodeSymbol};
use crate::context::json_string;
use crate::store::Store;
use crate::testmap::TestCandidate;
use std::collections::BTreeSet;
use std::fmt::Write;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ImpactReport {
    pub target: String,
    pub matched_symbols: Vec<CodeSymbol>,
    pub references: Vec<CodeReference>,
    pub outbound_references: Vec<CodeReference>,
    pub affected_files: Vec<String>,
    pub likely_tests: Vec<TestCandidate>,
    pub notes: Vec<String>,
}

pub(crate) async fn analyze(
    store: &Store,
    target: &str,
    limit: usize,
) -> Result<ImpactReport, String> {
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

    pub(crate) fn render_json(&self) -> String {
        let mut rendered = String::new();

        rendered.push('{');
        let _ = write!(rendered, "\"target\":{},", json_string(&self.target));

        rendered.push_str("\"matched_symbols\":[");
        for (index, symbol) in self.matched_symbols.iter().enumerate() {
            if index > 0 {
                rendered.push(',');
            }
            let _ = write!(
                rendered,
                "{{\"path\":{},\"language\":{},\"name\":{},\"kind\":{},\"line_start\":{},\"line_end\":{},\"signature\":{}}}",
                json_string(&symbol.path),
                json_option_string(symbol.language.as_deref()),
                json_string(&symbol.name),
                json_string(&symbol.kind),
                symbol.line_start,
                json_optional_i64(symbol.line_end),
                json_string(&symbol.signature)
            );
        }
        rendered.push_str("],");

        rendered.push_str("\"references\":[");
        for (index, reference) in self.references.iter().enumerate() {
            if index > 0 {
                rendered.push(',');
            }
            let _ = write!(
                rendered,
                "{{\"path\":{},\"language\":{},\"target_path\":{},\"target_name\":{},\"target_kind\":{},\"kind\":{},\"line_start\":{},\"excerpt\":{}}}",
                json_string(&reference.path),
                json_option_string(reference.language.as_deref()),
                json_string(&reference.target_path),
                json_string(&reference.target_name),
                json_string(&reference.target_kind),
                json_string(&reference.kind),
                reference.line_start,
                json_string(&reference.excerpt)
            );
        }
        rendered.push_str("],");

        rendered.push_str("\"outbound_references\":[");
        for (index, reference) in self.outbound_references.iter().enumerate() {
            if index > 0 {
                rendered.push(',');
            }
            let _ = write!(
                rendered,
                "{{\"path\":{},\"language\":{},\"target_path\":{},\"target_name\":{},\"target_kind\":{},\"kind\":{},\"line_start\":{},\"excerpt\":{}}}",
                json_string(&reference.path),
                json_option_string(reference.language.as_deref()),
                json_string(&reference.target_path),
                json_string(&reference.target_name),
                json_string(&reference.target_kind),
                json_string(&reference.kind),
                reference.line_start,
                json_string(&reference.excerpt)
            );
        }
        rendered.push_str("],");

        rendered.push_str("\"affected_files\":[");
        for (index, file) in self.affected_files.iter().enumerate() {
            if index > 0 {
                rendered.push(',');
            }
            rendered.push_str(&json_string(file));
        }
        rendered.push_str("],");

        rendered.push_str("\"likely_tests\":[");
        for (index, test) in self.likely_tests.iter().enumerate() {
            if index > 0 {
                rendered.push(',');
            }
            let _ = write!(
                rendered,
                "{{\"path\":{},\"reason\":{},\"score\":{}}}",
                json_string(&test.path),
                json_string(&test.reason),
                test.score
            );
        }
        rendered.push_str("],");

        rendered.push_str("\"notes\":[");
        for (index, note) in self.notes.iter().enumerate() {
            if index > 0 {
                rendered.push(',');
            }
            rendered.push_str(&json_string(note));
        }
        rendered.push_str("]}");

        rendered
    }
}

fn json_option_string(value: Option<&str>) -> String {
    value.map(json_string).unwrap_or_else(|| "null".to_string())
}

fn json_optional_i64(value: Option<i64>) -> String {
    value
        .map(|value| value.to_string())
        .unwrap_or_else(|| "null".to_string())
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
}
