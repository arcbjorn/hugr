use crate::code::{self, CodeSymbol};
use crate::discovery::{self, FileCandidate};
use crate::error::Result;
use crate::store::{PruneSummary, Store};
use std::collections::{BTreeMap, HashSet};
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct IndexSummary {
    pub file_count: usize,
    pub symbol_count: usize,
    pub sample_files: Vec<String>,
    pub file_roles: Vec<IndexClassification>,
    pub languages: Vec<IndexClassification>,
    pub symbol_kinds: Vec<IndexClassification>,
    pub pruned: PruneSummary,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct IndexClassification {
    pub name: String,
    pub count: usize,
}

pub(crate) async fn index_project(limit: usize) -> Result<IndexSummary> {
    let store = Store::open_current();
    let root = Path::new(".");
    let files = discovery::discover_project_files(root, limit)?;
    let symbols = index_candidates(&store, root, &files).await?;
    let pruned = store.prune_missing_index_rows(root).await?;

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
        pruned,
    })
}

pub(crate) async fn index_candidates(
    store: &Store,
    root: &Path,
    files: &[FileCandidate],
) -> Result<Vec<CodeSymbol>> {
    store.record_discovered_files(files).await?;
    let symbols = code::index_files(root, files)?;

    // References must resolve against the freshly parsed symbols for the
    // candidate files plus the stored symbols for every other file. Scoping
    // targets to the candidates alone would drop cross-file edges from these
    // files each time a partial index runs (context compilation indexes only
    // its 12 file candidates).
    let candidate_paths = files
        .iter()
        .map(|file| file.path.as_str())
        .collect::<HashSet<_>>();
    let mut reference_symbols = symbols.clone();
    reference_symbols.extend(
        store
            .stored_code_symbols()
            .await?
            .into_iter()
            .filter(|symbol| !candidate_paths.contains(symbol.path.as_str())),
    );
    let references = code::extract_references(root, files, &reference_symbols)?;
    store
        .record_code_index(files, &symbols, &references)
        .await?;
    Ok(symbols)
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct RefreshSummary {
    pub reparsed_files: usize,
    pub reference_files: usize,
    pub symbol_count: usize,
    pub pruned: PruneSummary,
}

/// Incrementally re-indexes only `changed_paths` plus the files whose stored
/// references point at them, instead of re-parsing the whole project. Falls back
/// to a full [`index_project`] when the store is cold (no symbols yet) or no
/// changed path maps to a discovered source file, so first-run and broad-change
/// cases stay correct.
///
/// Correctness: symbols are re-parsed only for changed files, but references are
/// re-extracted against the full stored+refreshed symbol set for the union of
/// changed files (outbound edges) and files that previously referenced a changed
/// file (inbound edges). Deleted files are pruned as in a full index.
pub(crate) async fn refresh_paths(
    limit: usize,
    changed_paths: &[String],
) -> Result<RefreshSummary> {
    let store = Store::open_current();
    let root = Path::new(".");

    let stored_symbols = store.stored_code_symbols().await?;
    if stored_symbols.is_empty() {
        // Cold store: nothing to be incremental about, do a normal full index.
        let summary = index_project(limit).await?;
        return Ok(RefreshSummary {
            reparsed_files: summary.file_count,
            reference_files: summary.file_count,
            symbol_count: summary.symbol_count,
            pruned: summary.pruned,
        });
    }

    let all_files = discovery::discover_project_files(root, limit)?;
    let changed_set = changed_paths.iter().cloned().collect::<HashSet<_>>();
    let changed_files = all_files
        .iter()
        .filter(|file| changed_set.contains(&file.path))
        .cloned()
        .collect::<Vec<_>>();
    if changed_files.is_empty() {
        // No changed path resolves to a tracked source file (e.g. only deletes or
        // ignored paths). Prune any now-missing rows and stop.
        let pruned = store.prune_missing_index_rows(root).await?;
        return Ok(RefreshSummary {
            pruned,
            ..RefreshSummary::default()
        });
    }

    store.record_discovered_files(&changed_files).await?;
    let refreshed_symbols = code::index_files(root, &changed_files)?;

    // Full symbol set = refreshed symbols for changed files + stored symbols for
    // every other file, so cross-file references still resolve to real targets.
    let changed_paths_only = changed_files
        .iter()
        .map(|file| file.path.clone())
        .collect::<HashSet<_>>();
    let mut full_symbols = stored_symbols
        .into_iter()
        .filter(|symbol| !changed_paths_only.contains(&symbol.path))
        .collect::<Vec<_>>();
    full_symbols.extend(refreshed_symbols.iter().cloned());

    // Reference files = changed files (outbound) + files that previously pointed
    // at a changed file (inbound). Re-scan just those against the full symbol set.
    let inbound_sources = store
        .reference_sources_targeting(&changed_paths_only.iter().cloned().collect::<Vec<_>>())
        .await?;
    let mut reference_paths = changed_paths_only.clone();
    reference_paths.extend(inbound_sources);
    let reference_files = all_files
        .iter()
        .filter(|file| reference_paths.contains(&file.path))
        .cloned()
        .collect::<Vec<_>>();
    let references = code::extract_references(root, &reference_files, &full_symbols)?;

    let reference_scope = reference_files
        .iter()
        .map(|file| file.path.clone())
        .collect::<HashSet<_>>();
    store
        .record_code_index_scoped(
            &changed_paths_only,
            &reference_scope,
            &refreshed_symbols,
            &references,
        )
        .await?;
    let pruned = store.prune_missing_index_rows(root).await?;

    Ok(RefreshSummary {
        reparsed_files: changed_files.len(),
        reference_files: reference_files.len(),
        symbol_count: refreshed_symbols.len(),
        pruned,
    })
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
            lexical_score: 0,
            embedding_rank: None,
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
