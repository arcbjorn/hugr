use crate::code::{self, CodeSymbol};
use crate::discovery::{self, FileCandidate};
use crate::store::Store;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct IndexSummary {
    pub file_count: usize,
    pub symbol_count: usize,
    pub sample_files: Vec<String>,
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
