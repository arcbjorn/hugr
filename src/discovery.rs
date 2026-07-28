use crate::error::{Error, Result};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;

/// A file the compiler is considering for the context pack, along with the
/// evidence that put it there.
///
/// The two signals are kept apart on purpose. They used to share one `score`
/// field, with embedding hits encoded as `10_000 + window - rank` and lexical
/// hits scored 0..~30. Merging took the larger of the two, so every embedding
/// hit outranked every filename match by three orders of magnitude, filled
/// the candidate limit, and left the filename signal discarded before ranking
/// ever ran — a task naming a file exactly would not rank that file first.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct FileCandidate {
    pub path: String,
    /// Term overlap between the task and this file's name and path.
    pub lexical_score: usize,
    /// Position in the source-embedding similarity results, best rank first.
    /// `None` when the embedding search did not return this file.
    pub embedding_rank: Option<usize>,
    pub language: Option<String>,
    pub size_bytes: Option<u64>,
}

/// Points per unit of lexical overlap. `candidate_for` awards 4 for a term
/// found in the file name and 2 for one found elsewhere in the path.
const LEXICAL_WEIGHT: usize = 20;
/// Embedding ranks beyond this contribute nothing; past a couple of dozen
/// results the ordering carries little signal.
const EMBEDDING_RANK_WINDOW: usize = 25;
/// Points per place gained within [`EMBEDDING_RANK_WINDOW`].
///
/// Deliberately scaled so the best possible embedding rank (24 * 3 = 72) sits
/// just below a single file-name match (4 * 20 = 80). A name match is precise
/// evidence — the task used a word that is in this file's name — whereas the
/// default embedding is a hashed bag of words with no inverse-document
/// weighting, so leading its similarity list says much less. The weight only
/// affects comparisons *between* the two signals: when nothing matches by
/// name every candidate scores zero lexically and the embedding still decides
/// the order on its own.
const EMBEDDING_WEIGHT: usize = 3;
/// Awarded to files small enough to read in full within a token budget.
const SMALL_FILE_BONUS: usize = 10;
const SMALL_FILE_BYTES: u64 = 128_000;

impl FileCandidate {
    /// How strongly this candidate matches the task.
    ///
    /// Both signals add, rather than one overriding the other: a file that
    /// matches by name *and* appears in the embedding results outranks one
    /// that only appears in the embedding results, and a strong name match
    /// can outrank a mediocre embedding rank.
    pub(crate) fn relevance(&self) -> usize {
        self.lexical_score * LEXICAL_WEIGHT + self.embedding_bonus() + self.size_bonus()
    }

    fn embedding_bonus(&self) -> usize {
        self.embedding_rank.map_or(0, |rank| {
            EMBEDDING_RANK_WINDOW.saturating_sub(rank.min(EMBEDDING_RANK_WINDOW)) * EMBEDDING_WEIGHT
        })
    }

    fn size_bonus(&self) -> usize {
        self.size_bytes.map_or(0, |bytes| {
            usize::from(bytes <= SMALL_FILE_BYTES) * SMALL_FILE_BONUS
        })
    }
}

pub(crate) trait FileFinder {
    fn find_files(&self, root: &Path) -> Result<Vec<PathBuf>>;
}

#[derive(Debug, Default)]
pub(crate) struct GitFileFinder;

impl FileFinder for GitFileFinder {
    fn find_files(&self, root: &Path) -> Result<Vec<PathBuf>> {
        let output = ProcessCommand::new("git")
            .arg("-C")
            .arg(root)
            .args(["ls-files", "--cached", "--others", "--exclude-standard"])
            .output()?;

        if !output.status.success() {
            return Err(Error::msg(
                String::from_utf8_lossy(&output.stderr).trim().to_string(),
            ));
        }

        Ok(String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(PathBuf::from)
            .filter(|path| !has_skipped_component(path))
            .collect())
    }
}

#[derive(Debug, Default)]
pub(crate) struct WalkingFileFinder;

impl FileFinder for WalkingFileFinder {
    fn find_files(&self, root: &Path) -> Result<Vec<PathBuf>> {
        let matcher = IgnoreMatcher::from_root(root);
        let mut files = Vec::new();
        visit(root, root, &matcher, &mut files)?;
        Ok(files)
    }
}

pub(crate) fn discover_candidate_files(
    root: &Path,
    task: &str,
    limit: usize,
) -> Result<Vec<FileCandidate>> {
    if limit == 0 {
        return Ok(Vec::new());
    }

    let files = GitFileFinder
        .find_files(root)
        .or_else(|_| WalkingFileFinder.find_files(root))?;

    Ok(rank_files(root, task, files, limit))
}

pub(crate) fn discover_project_files(root: &Path, limit: usize) -> Result<Vec<FileCandidate>> {
    if limit == 0 {
        return Ok(Vec::new());
    }

    let files = GitFileFinder
        .find_files(root)
        .or_else(|_| WalkingFileFinder.find_files(root))?;

    let mut candidates = files
        .into_iter()
        .filter_map(|path| file_candidate(root, &path, 0))
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| left.path.cmp(&right.path));
    candidates.truncate(limit);
    Ok(candidates)
}

pub(crate) fn merge_file_candidates(
    mut candidates: Vec<FileCandidate>,
    additional: Vec<FileCandidate>,
    limit: usize,
) -> Vec<FileCandidate> {
    if limit == 0 {
        return Vec::new();
    }

    candidates.extend(additional);
    let mut merged = Vec::<FileCandidate>::new();
    for candidate in candidates {
        match merged
            .iter_mut()
            .find(|existing| existing.path == candidate.path)
        {
            Some(existing) => {
                existing.lexical_score = existing.lexical_score.max(candidate.lexical_score);
                existing.embedding_rank = match (existing.embedding_rank, candidate.embedding_rank)
                {
                    (Some(existing_rank), Some(new_rank)) => Some(existing_rank.min(new_rank)),
                    (Some(rank), None) | (None, Some(rank)) => Some(rank),
                    (None, None) => None,
                };
                if existing.language.is_none() {
                    existing.language = candidate.language;
                }
                if existing.size_bytes.is_none() {
                    existing.size_bytes = candidate.size_bytes;
                }
            }
            None => merged.push(candidate),
        }
    }
    merged.sort_by(|left, right| {
        right
            .relevance()
            .cmp(&left.relevance())
            .then_with(|| left.path.cmp(&right.path))
    });
    merged.truncate(limit);
    merged
}

fn rank_files(root: &Path, task: &str, files: Vec<PathBuf>, limit: usize) -> Vec<FileCandidate> {
    let terms = query_terms(task);
    if terms.is_empty() {
        return Vec::new();
    }

    let mut scored = files
        .into_iter()
        .filter_map(|path| candidate_for(root, &path, &terms))
        .collect::<Vec<_>>();

    scored.sort_by(|left, right| {
        right
            .relevance()
            .cmp(&left.relevance())
            .then_with(|| left.path.cmp(&right.path))
    });
    scored.truncate(limit);
    scored
}

fn candidate_for(root: &Path, path: &Path, terms: &[String]) -> Option<FileCandidate> {
    let display = normalized_relative_path(path);
    let normalized = display.to_lowercase();
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("")
        .to_lowercase();
    let language = language_for(path).map(str::to_string);
    let language_score = language
        .as_deref()
        .map(str::to_lowercase)
        .unwrap_or_default();
    let mut score = 0;

    for term in terms {
        if file_name.contains(term) {
            score += 4;
        } else if normalized.contains(term) {
            score += 2;
        }

        if !language_score.is_empty() && language_score == *term {
            score += 3;
        }
    }

    if score == 0 {
        return None;
    }

    file_candidate(root, path, score)
}

fn file_candidate(root: &Path, path: &Path, lexical_score: usize) -> Option<FileCandidate> {
    let display = normalized_relative_path(path);
    let language = language_for(path).map(str::to_string);
    let size_bytes = fs::metadata(root.join(path))
        .ok()
        .map(|metadata| metadata.len());
    Some(FileCandidate {
        path: display,
        lexical_score,
        embedding_rank: None,
        language,
        size_bytes,
    })
}

fn visit(
    root: &Path,
    path: &Path,
    matcher: &IgnoreMatcher,
    files: &mut Vec<PathBuf>,
) -> Result<()> {
    let entries = fs::read_dir(path)?;

    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        let relative = path.strip_prefix(root).unwrap_or(&path);
        let is_dir = path.is_dir();

        if has_skipped_component(relative) || matcher.is_ignored(relative, is_dir) {
            continue;
        }

        if is_dir {
            visit(root, &path, matcher, files)?;
        } else if path.is_file() {
            files.push(relative.to_path_buf());
        }
    }

    Ok(())
}

#[derive(Debug, Default)]
struct IgnoreMatcher {
    patterns: Vec<IgnorePattern>,
}

impl IgnoreMatcher {
    fn from_root(root: &Path) -> Self {
        let path = root.join(".gitignore");
        let Ok(contents) = fs::read_to_string(path) else {
            return Self::default();
        };

        let patterns = contents
            .lines()
            .filter_map(IgnorePattern::parse)
            .collect::<Vec<_>>();

        Self { patterns }
    }

    fn is_ignored(&self, path: &Path, is_dir: bool) -> bool {
        let path = normalized_relative_path(path);
        self.patterns
            .iter()
            .any(|pattern| pattern.matches(&path, is_dir))
    }
}

#[derive(Debug)]
struct IgnorePattern {
    value: String,
    directory_only: bool,
    anchored: bool,
}

impl IgnorePattern {
    fn parse(line: &str) -> Option<Self> {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with('!') {
            return None;
        }

        let anchored = line.starts_with('/');
        let value = line
            .trim_start_matches('/')
            .trim_end_matches('/')
            .to_string();
        if value.is_empty() {
            return None;
        }

        Some(Self {
            value,
            directory_only: line.ends_with('/'),
            anchored,
        })
    }

    fn matches(&self, path: &str, is_dir: bool) -> bool {
        if self.directory_only && !is_dir {
            let prefix = format!("{}/", self.value);
            if self.anchored {
                return path.starts_with(&prefix);
            }
            return path
                .split('/')
                .any(|component| wildcard_match(&self.value, component))
                || path.contains(&prefix);
        }

        if self.value.contains('/') || self.anchored {
            wildcard_match(&self.value, path)
        } else {
            path.split('/')
                .any(|component| wildcard_match(&self.value, component))
        }
    }
}

fn has_skipped_component(path: &Path) -> bool {
    path.components().any(|component| {
        let name = component.as_os_str().to_string_lossy();
        matches!(
            name.as_ref(),
            ".git"
                | ".hugr"
                | ".agent-out"
                | ".worktrees"
                | "target"
                | "node_modules"
                | "vendor"
                | "dist"
                | "build"
                | ".next"
                | "out"
                | "coverage"
        )
    })
}

fn query_terms(query: &str) -> Vec<String> {
    query
        .split(|char: char| !char.is_alphanumeric() && char != '_' && char != '-')
        .filter(|term| term.len() > 2)
        .map(str::to_lowercase)
        .collect()
}

fn normalized_relative_path(path: &Path) -> String {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

pub(crate) fn language_for(path: &Path) -> Option<&'static str> {
    match path.extension().and_then(|extension| extension.to_str())? {
        "rs" => Some("rust"),
        "toml" => Some("toml"),
        "md" => Some("markdown"),
        "json" => Some("json"),
        "js" | "jsx" => Some("javascript"),
        "ts" | "tsx" => Some("typescript"),
        "py" => Some("python"),
        "go" => Some("go"),
        "swift" => Some("swift"),
        "java" => Some("java"),
        "kt" => Some("kotlin"),
        "c" | "h" => Some("c"),
        "cpp" | "hpp" => Some("cpp"),
        "html" => Some("html"),
        "css" => Some("css"),
        "sql" => Some("sql"),
        _ => None,
    }
}

fn wildcard_match(pattern: &str, value: &str) -> bool {
    let pattern = pattern.as_bytes();
    let value = value.as_bytes();
    let mut table = vec![vec![false; value.len() + 1]; pattern.len() + 1];
    table[0][0] = true;

    for pattern_index in 1..=pattern.len() {
        if pattern[pattern_index - 1] == b'*' {
            table[pattern_index][0] = table[pattern_index - 1][0];
        }
    }

    for pattern_index in 1..=pattern.len() {
        for value_index in 1..=value.len() {
            table[pattern_index][value_index] = match pattern[pattern_index - 1] {
                b'*' => {
                    table[pattern_index - 1][value_index] || table[pattern_index][value_index - 1]
                }
                b'?' => table[pattern_index - 1][value_index - 1],
                char => char == value[value_index - 1] && table[pattern_index - 1][value_index - 1],
            };
        }
    }

    table[pattern.len()][value.len()]
}

#[cfg(test)]
mod tests {
    use super::{
        FileCandidate, WalkingFileFinder, discover_candidate_files, merge_file_candidates,
    };
    use crate::discovery::FileFinder;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    struct TempProject {
        root: PathBuf,
    }

    impl TempProject {
        fn new(name: &str) -> Self {
            let unique = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system time should be after unix epoch")
                .as_nanos();
            let root = std::env::temp_dir().join(format!("hugr_discovery_{name}_{unique}"));
            fs::create_dir_all(&root).unwrap();
            Self { root }
        }

        fn write(&self, path: &str, contents: &str) {
            let path = self.root.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, contents).unwrap();
        }

        fn root(&self) -> &Path {
            &self.root
        }
    }

    impl Drop for TempProject {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn walking_finder_respects_gitignore_and_skips_generated_dirs() {
        let project = TempProject::new("ignore");
        project.write(".gitignore", "ignored.rs\nlogs/\n");
        project.write("src/plugin_hooks.rs", "");
        project.write("ignored.rs", "");
        project.write("logs/output.txt", "");
        project.write("target/debug/build.rs", "");

        let mut files = WalkingFileFinder
            .find_files(project.root())
            .unwrap()
            .into_iter()
            .map(|path| path.display().to_string())
            .collect::<Vec<_>>();
        files.sort();

        assert_eq!(files, vec![".gitignore", "src/plugin_hooks.rs"]);
    }

    #[test]
    fn discovery_ranks_matching_paths() {
        let project = TempProject::new("rank");
        project.write("src/plugin_hooks.rs", "");
        project.write("src/storage.rs", "");

        let candidates = discover_candidate_files(project.root(), "add plugin hooks", 5).unwrap();

        assert_eq!(candidates.first().unwrap().path, "src/plugin_hooks.rs");
        assert_eq!(
            candidates.first().unwrap().language.as_deref(),
            Some("rust")
        );
    }

    fn candidate(path: &str, lexical_score: usize, embedding_rank: Option<usize>) -> FileCandidate {
        FileCandidate {
            path: path.to_string(),
            lexical_score,
            embedding_rank,
            language: Some("rust".to_string()),
            size_bytes: Some(1_000),
        }
    }

    /// The regression this split exists for: the embedding signal used to be
    /// encoded as `10_000 + window - rank` in the same field the lexical
    /// score used, and merging kept the larger number. Every embedding hit
    /// therefore outranked every filename match and filled the limit, so a
    /// task naming a file exactly did not rank that file first.
    #[test]
    fn a_named_file_outranks_an_embedding_only_hit() {
        let merged = merge_file_candidates(
            vec![candidate("src/redact.rs", 4, None)],
            vec![
                candidate("src/main.rs", 0, Some(1)),
                candidate("src/lib.rs", 0, Some(2)),
            ],
            3,
        );

        assert_eq!(merged[0].path, "src/redact.rs");
    }

    #[test]
    fn both_signals_add_rather_than_one_winning() {
        let named_and_embedded = candidate("src/redact.rs", 4, Some(4));
        let named_only = candidate("src/redact.rs", 4, None);
        let embedded_only = candidate("src/main.rs", 0, Some(4));

        assert!(named_and_embedded.relevance() > named_only.relevance());
        assert!(named_and_embedded.relevance() > embedded_only.relevance());
    }

    /// Merging the same path from both sources keeps the best of each signal.
    #[test]
    fn merging_a_path_from_both_sources_keeps_both_signals() {
        let merged = merge_file_candidates(
            vec![candidate("src/redact.rs", 6, None)],
            vec![candidate("src/redact.rs", 0, Some(3))],
            5,
        );

        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].lexical_score, 6);
        assert_eq!(merged[0].embedding_rank, Some(3));
    }

    /// Embedding hits must still fill the pack when nothing matches by name,
    /// which is the case the old ordering got right.
    #[test]
    fn embedding_hits_still_rank_when_no_name_matches() {
        let merged = merge_file_candidates(
            Vec::new(),
            vec![
                candidate("src/second.rs", 0, Some(2)),
                candidate("src/first.rs", 0, Some(1)),
            ],
            2,
        );

        assert_eq!(merged[0].path, "src/first.rs");
        assert_eq!(merged[1].path, "src/second.rs");
    }
}
