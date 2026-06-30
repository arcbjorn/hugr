use crate::code::{CodeReference, CodeSymbol};
use crate::context::json_string;
use crate::store::Store;
use std::collections::BTreeSet;
use std::fmt::Write;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ImpactReport {
    pub target: String,
    pub matched_symbols: Vec<CodeSymbol>,
    pub references: Vec<CodeReference>,
    pub affected_files: Vec<String>,
    pub notes: Vec<String>,
}

pub(crate) async fn analyze(
    store: &Store,
    target: &str,
    limit: usize,
) -> Result<ImpactReport, String> {
    let matched_symbols = store.symbols_for_target(target, limit).await?;
    let references = store.references_to_symbols(&matched_symbols, limit).await?;
    Ok(ImpactReport::new(target, matched_symbols, references))
}

impl ImpactReport {
    fn new(target: &str, matched_symbols: Vec<CodeSymbol>, references: Vec<CodeReference>) -> Self {
        let mut affected = BTreeSet::new();
        for symbol in &matched_symbols {
            affected.insert(symbol.path.clone());
        }
        for reference in &references {
            affected.insert(reference.path.clone());
        }

        let notes = if matched_symbols.is_empty() {
            vec!["No indexed symbols matched the target.".to_string()]
        } else if references.is_empty() {
            vec!["No direct indexed references found for matched symbols.".to_string()]
        } else {
            Vec::new()
        };

        Self {
            target: target.to_string(),
            matched_symbols,
            references,
            affected_files: affected.into_iter().collect(),
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
                    "- {} {} at {}:{}",
                    symbol.kind, symbol.name, symbol.path, symbol.line_start
                );
            }
        }
        rendered.push('\n');

        rendered.push_str("## Direct References\n");
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

        rendered.push_str("## Affected Files\n");
        if self.affected_files.is_empty() {
            rendered.push_str("No affected files found.\n");
        } else {
            for file in &self.affected_files {
                let _ = writeln!(rendered, "- {file}");
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
                "{{\"path\":{},\"language\":{},\"name\":{},\"kind\":{},\"line_start\":{},\"signature\":{}}}",
                json_string(&symbol.path),
                json_option_string(symbol.language.as_deref()),
                json_string(&symbol.name),
                json_string(&symbol.kind),
                symbol.line_start,
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

        rendered.push_str("\"affected_files\":[");
        for (index, file) in self.affected_files.iter().enumerate() {
            if index > 0 {
                rendered.push(',');
            }
            rendered.push_str(&json_string(file));
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

#[cfg(test)]
mod tests {
    use super::ImpactReport;
    use crate::code::{CodeReference, CodeSymbol};

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
                line_end: None,
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
        );

        let markdown = report.render_markdown();
        let json = report.render_json();

        assert!(markdown.contains("- struct PluginHooks at src/plugin_hooks.rs:1"));
        assert!(markdown.contains("- src/main.rs:4 [reference]"));
        assert!(json.contains("\"target\":\"PluginHooks\""));
        assert!(json.contains("\"affected_files\""));
    }
}
