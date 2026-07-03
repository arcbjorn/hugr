use crate::code::{self, CodeSymbol};
use crate::discovery::{self, FileCandidate};
use crate::store::Store;
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct IndexSummary {
    pub file_count: usize,
    pub symbol_count: usize,
    pub sample_files: Vec<String>,
    pub file_roles: Vec<IndexClassification>,
    pub languages: Vec<IndexClassification>,
    pub symbol_kinds: Vec<IndexClassification>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct IndexClassification {
    pub name: String,
    pub count: usize,
}

pub(crate) async fn index_project(limit: usize) -> Result<IndexSummary, String> {
    let store = Store::open_current();
    let root = Path::new(".");
    let files = discovery::discover_project_files(root, limit)?;
    let symbols = index_candidates(&store, root, &files).await?;

    Ok(IndexSummary {
        file_count: files.len(),
        symbol_count: symbols.len(),
        sample_files: files
            .iter()
            .take(12)
            .map(|file| file.path.clone())
            .collect(),
        file_roles: classify_file_roles(&files),
        languages: classify_languages(&files),
        symbol_kinds: classify_symbol_kinds(&symbols),
    })
}

pub(crate) async fn index_candidates(
    store: &Store,
    root: &Path,
    files: &[FileCandidate],
) -> Result<Vec<CodeSymbol>, String> {
    store.record_discovered_files(files).await?;
    let symbols = code::index_files(root, files)?;
    let references = code::extract_references(root, files, &symbols)?;
    store
        .record_code_index(files, &symbols, &references)
        .await?;
    Ok(symbols)
}

pub(crate) fn format_classifications(classifications: &[IndexClassification]) -> String {
    if classifications.is_empty() {
        return "none".to_string();
    }

    classifications
        .iter()
        .map(|classification| format!("{}={}", classification.name, classification.count))
        .collect::<Vec<_>>()
        .join(", ")
}

fn classify_file_roles(files: &[FileCandidate]) -> Vec<IndexClassification> {
    count_by(files.iter().map(file_role))
}

fn classify_languages(files: &[FileCandidate]) -> Vec<IndexClassification> {
    count_by(
        files
            .iter()
            .map(|file| file.language.as_deref().unwrap_or("unknown").to_string()),
    )
}

fn classify_symbol_kinds(symbols: &[CodeSymbol]) -> Vec<IndexClassification> {
    count_by(symbols.iter().map(|symbol| symbol.kind.clone()))
}

fn file_role(file: &FileCandidate) -> String {
    let path = file.path.to_lowercase();
    let file_name = path.rsplit('/').next().unwrap_or(path.as_str());

    if path.starts_with("docs/")
        || path.starts_with("doc/")
        || matches!(
            file.language.as_deref(),
            Some("markdown" | "text" | "rst" | "asciidoc")
        )
    {
        return "documentation".to_string();
    }

    if path.starts_with("tests/")
        || path.starts_with("test/")
        || path.contains("/tests/")
        || path.contains("/test/")
        || path.contains("/__tests__/")
        || file_name.contains("_test.")
        || file_name.contains(".test.")
        || file_name.contains("_spec.")
        || file_name.contains(".spec.")
    {
        return "test".to_string();
    }

    if is_config_file(file_name) {
        return "configuration".to_string();
    }

    if file.language.is_some() {
        "source".to_string()
    } else {
        "other".to_string()
    }
}

fn is_config_file(file_name: &str) -> bool {
    matches!(
        file_name,
        "cargo.toml"
            | "cargo.lock"
            | "package.json"
            | "package-lock.json"
            | "pnpm-lock.yaml"
            | "yarn.lock"
            | "tsconfig.json"
            | "dockerfile"
            | "docker-compose.yml"
            | "makefile"
    ) || file_name.ends_with(".toml")
        || file_name.ends_with(".yaml")
        || file_name.ends_with(".yml")
        || file_name.ends_with(".lock")
}

fn count_by(values: impl Iterator<Item = String>) -> Vec<IndexClassification> {
    let mut counts = BTreeMap::<String, usize>::new();
    for value in values {
        *counts.entry(value).or_default() += 1;
    }

    let mut classifications = counts
        .into_iter()
        .map(|(name, count)| IndexClassification { name, count })
        .collect::<Vec<_>>();
    classifications.sort_by(|left, right| {
        right
            .count
            .cmp(&left.count)
            .then_with(|| left.name.cmp(&right.name))
    });
    classifications
}

#[cfg(test)]
mod tests {
    use super::{classify_file_roles, classify_languages, classify_symbol_kinds};
    use crate::code::CodeSymbol;
    use crate::discovery::FileCandidate;

    #[test]
    fn classifies_indexed_files_and_symbols() {
        let files = vec![
            file("src/lib.rs", Some("rust")),
            file("tests/plugin_hooks.rs", Some("rust")),
            file("docs/plugins.md", Some("markdown")),
            file("Cargo.toml", Some("toml")),
            file("assets/logo.png", None),
        ];
        let symbols = vec![symbol("function"), symbol("function"), symbol("struct")];

        assert_eq!(
            classify_file_roles(&files)
                .into_iter()
                .map(|classification| (classification.name, classification.count))
                .collect::<Vec<_>>(),
            vec![
                ("configuration".to_string(), 1),
                ("documentation".to_string(), 1),
                ("other".to_string(), 1),
                ("source".to_string(), 1),
                ("test".to_string(), 1),
            ]
        );
        assert_eq!(classify_languages(&files)[0].name, "rust");
        assert_eq!(classify_languages(&files)[0].count, 2);
        assert_eq!(classify_symbol_kinds(&symbols)[0].name, "function");
        assert_eq!(classify_symbol_kinds(&symbols)[0].count, 2);
    }

    fn file(path: &str, language: Option<&str>) -> FileCandidate {
        FileCandidate {
            path: path.to_string(),
            score: 0,
            language: language.map(str::to_string),
            size_bytes: None,
        }
    }

    fn symbol(kind: &str) -> CodeSymbol {
        CodeSymbol {
            path: "src/lib.rs".to_string(),
            language: Some("rust".to_string()),
            name: "plugin_hooks".to_string(),
            kind: kind.to_string(),
            line_start: 1,
            line_end: Some(1),
            signature: "fn plugin_hooks()".to_string(),
        }
    }
}
