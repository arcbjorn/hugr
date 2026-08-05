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

/// Lexical score added to a file that contains a symbol the task matched.
///
/// Comfortably outweighs a single file-name term hit (4) and, combined with a
/// weak path hit, can reach an exact stem match (8). That overlap is
/// deliberate and measured: 3 was tried, so that an exact name always wins, and
/// it retrieved less on a large foreign repository (hit rate 0.333 against
/// 0.367, recall 0.169 against 0.186). A symbol match means the task named an
/// identifier *defined in this file*, which turns out to be about as strong as
/// the file's own name.
const SYMBOL_PATH_BONUS: usize = 6;

/// Re-ranks `candidates` by symbol evidence, adding files the ranking missed.
///
/// File ranking sees only names and paths, so it misses a file whose *contents*
/// match the task. The symbol index already resolves that, and measurably
/// better: `symbol_file_hit_rate` runs above `hit_rate` on every repository
/// measured so far. Feeding those paths back in turns a signal the pack already
/// computed into ranking evidence, rather than adding a new source.
///
/// A symbol file that never reached the candidate set is *inserted* rather than
/// only boosted. Ranking and the symbol index disagree about which files matter,
/// and on a large repository `symbol_file_hit_rate` (0.433) exceeds even
/// `candidate_hit_rate` (0.400) — so some files the symbol index identified
/// correctly were absent from the candidate set entirely and no amount of
/// re-ordering could recover them.
pub(crate) fn promote_symbol_paths(
    mut candidates: Vec<FileCandidate>,
    symbols: &[crate::code::CodeSymbol],
) -> Vec<FileCandidate> {
    if symbols.is_empty() {
        return candidates;
    }

    let symbol_paths = symbols
        .iter()
        .map(|symbol| symbol.path.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    for candidate in &mut candidates {
        if symbol_paths.contains(candidate.path.as_str()) {
            candidate.lexical_score += SYMBOL_PATH_BONUS;
        }
    }

    let known = candidates
        .iter()
        .map(|candidate| candidate.path.as_str().to_string())
        .collect::<std::collections::BTreeSet<_>>();
    for path in symbol_paths {
        if !known.contains(path) {
            candidates.push(FileCandidate {
                path: path.to_string(),
                lexical_score: SYMBOL_PATH_BONUS,
                embedding_rank: None,
                language: language_for(Path::new(path)).map(str::to_string),
                size_bytes: fs::metadata(path).ok().map(|metadata| metadata.len()),
            });
        }
    }

    candidates.sort_by(|left, right| {
        right
            .relevance()
            .cmp(&left.relevance())
            .then_with(|| left.path.cmp(&right.path))
    });
    candidates
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
    // The name without its extension, so `worker.go` can be recognised as an
    // exact match for the term `worker`.
    let stem = file_name
        .split_once('.')
        .map_or(file_name.as_str(), |(stem, _)| stem);
    let mut score = 0;

    for term in terms {
        if stem == term {
            // An exact stem match is the strongest filename evidence there is:
            // the task named this file. Substring matching alone scored
            // `worker.go` and `worker_retry_test.go` identically, so a task
            // about `worker` ranked several test files above the source they
            // test and the real target was evicted by the token budget.
            score += 8;
        } else if file_name.contains(term) {
            score += 4;
        } else if normalized.contains(term) {
            score += 2;
        }

        if !language_score.is_empty() && language_score == *term {
            score += 3;
        }
    }

    // A test file is rarely what a task naming its subject is about, and it
    // matches every term the subject does. Without this, `foo_test.go`,
    // `foo_swap_test.go`, and friends crowd out `foo.go` on sheer count.
    // Halving keeps them reachable — a task really about tests still surfaces
    // them, and `affected_tests` covers them separately.
    if crate::testmap::is_test_path(&normalized) {
        score /= 2;
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

/// Marks a file `hugr` is staging on its way to replacing a real source file.
/// Multi-file edits write here first so a failure cannot leave the tree half
/// rewritten; a crash in that window leaves the staged file behind, so both
/// the walker below and the daemon's watcher skip the suffix rather than
/// indexing a half-written duplicate of a source file.
pub(crate) const STAGING_SUFFIX: &str = ".hugr-tmp";

pub(crate) fn is_staging_path(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.ends_with(STAGING_SUFFIX))
}

fn has_skipped_component(path: &Path) -> bool {
    if is_staging_path(path) {
        return true;
    }
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
        promote_symbol_paths,
    };
    use crate::code::CodeSymbol;
    use crate::discovery::FileFinder;

    fn symbol_in(path: &str, name: &str) -> CodeSymbol {
        CodeSymbol {
            path: path.to_string(),
            language: Some("rust".to_string()),
            name: name.to_string(),
            kind: "function".to_string(),
            line_start: 1,
            line_end: Some(2),
            signature: format!("fn {name}()"),
        }
    }

    /// File ranking sees only names and paths, so it misses a file whose
    /// *contents* match the task. The symbol index already resolves that —
    /// `symbol_file_hit_rate` runs above `hit_rate` on every repository
    /// measured — but those paths were computed after ranking and never fed
    /// back into it.
    #[test]
    fn a_file_defining_a_matched_symbol_outranks_one_that_does_not() {
        let candidates = vec![
            // Sorts first and scores higher lexically, so only the symbol
            // bonus can reorder these.
            FileCandidate {
                path: "src/aaa_unrelated.rs".to_string(),
                lexical_score: 4,
                ..FileCandidate::default()
            },
            FileCandidate {
                path: "src/holds_the_symbol.rs".to_string(),
                lexical_score: 2,
                ..FileCandidate::default()
            },
        ];

        let promoted = promote_symbol_paths(
            candidates,
            &[symbol_in("src/holds_the_symbol.rs", "unique_suffix")],
        );

        assert_eq!(promoted.first().unwrap().path, "src/holds_the_symbol.rs");
    }

    /// Boosting alone cannot recover a file the ranking never produced, and on
    /// a large repository the symbol index identified files the candidate set
    /// did not contain at all (`symbol_file_hit_rate` above
    /// `candidate_hit_rate`). Those are inserted rather than dropped.
    #[test]
    fn a_symbol_file_missing_from_the_candidates_is_inserted() {
        let candidates = vec![FileCandidate {
            path: "src/unrelated.rs".to_string(),
            lexical_score: 2,
            ..FileCandidate::default()
        }];

        let promoted = promote_symbol_paths(
            candidates,
            &[symbol_in("src/never_ranked.rs", "unique_suffix")],
        );

        assert_eq!(promoted.first().unwrap().path, "src/never_ranked.rs");
        assert_eq!(promoted.len(), 2, "the original candidate is kept");
    }

    /// Inserting must not duplicate a path the candidate set already holds.
    #[test]
    fn an_existing_candidate_is_boosted_not_duplicated() {
        let candidates = vec![FileCandidate {
            path: "src/holds.rs".to_string(),
            lexical_score: 2,
            ..FileCandidate::default()
        }];

        let promoted =
            promote_symbol_paths(candidates, &[symbol_in("src/holds.rs", "unique_suffix")]);

        assert_eq!(promoted.len(), 1);
        assert_eq!(promoted[0].lexical_score, 2 + 6);
    }

    #[test]
    fn promotion_is_a_no_op_without_symbols() {
        let candidates = vec![FileCandidate {
            path: "src/only.rs".to_string(),
            lexical_score: 3,
            ..FileCandidate::default()
        }];

        let promoted = promote_symbol_paths(candidates.clone(), &[]);

        assert_eq!(promoted, candidates);
    }

    /// The bonus is large enough that a file defining the symbol can draw level
    /// with one whose *name* matches exactly. That is intentional — a smaller
    /// bonus that always loses to the filename retrieved measurably less — so
    /// this pins the boundary rather than a strict filename-wins rule: an exact
    /// name still beats a symbol file that has no other evidence.
    #[test]
    fn an_exact_name_match_beats_a_symbol_file_with_no_other_evidence() {
        let candidates = vec![
            FileCandidate {
                path: "src/worker.rs".to_string(),
                lexical_score: 8,
                ..FileCandidate::default()
            },
            FileCandidate {
                path: "src/other.rs".to_string(),
                lexical_score: 0,
                ..FileCandidate::default()
            },
        ];

        let promoted = promote_symbol_paths(candidates, &[symbol_in("src/other.rs", "worker")]);

        assert_eq!(promoted.first().unwrap().path, "src/worker.rs");
    }
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    struct TempProject {
        root: PathBuf,
    }

    /// A suffix no other temp directory in this process can repeat.
    ///
    /// `SystemTime::now().as_nanos()` names the unit, not the resolution: on
    /// macOS it advances in 1µs steps, and most back-to-back reads return the
    /// same value. The counter makes uniqueness unconditional.
    fn unique_suffix() -> String {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time should be after unix epoch")
            .as_nanos();
        let sequence = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        format!("{nanos}_{sequence}")
    }

    impl TempProject {
        fn new(name: &str) -> Self {
            let unique = unique_suffix();
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

    /// A crash between a multi-file edit's staging write and its rename leaves
    /// a `.hugr-tmp` sibling behind. Indexing it would add a near-duplicate of
    /// a real source file to the graph.
    #[test]
    fn walking_finder_skips_staged_edit_files() {
        let project = TempProject::new("staging");
        project.write("src/plugin_hooks.rs", "");
        project.write("src/.plugin_hooks.rs.hugr-tmp", "");

        let files = WalkingFileFinder
            .find_files(project.root())
            .unwrap()
            .into_iter()
            .map(|path| path.display().to_string())
            .collect::<Vec<_>>();

        assert_eq!(files, vec!["src/plugin_hooks.rs"]);
    }

    /// The regression found by running `hugr eval` against a foreign Go
    /// repository. Several files matched one term and every one scored the
    /// same 4, because the name check was a plain substring test. The source
    /// file the commit actually touched lost the tiebreak to its own test
    /// files and was then evicted by the token budget.
    #[test]
    fn an_exact_name_match_outranks_a_substring_match() {
        let project = TempProject::new("exact");
        // `alpha_worker.go` sorts first and matches the term as a substring,
        // so only the exact-stem bonus can put `worker.go` on top: the
        // alphabetical tiebreak actively works against the right answer here.
        project.write("internal/queue/alpha_worker.go", "");
        project.write("internal/queue/worker.go", "");
        project.write("internal/queue/worker_retry_test.go", "");

        let candidates =
            discover_candidate_files(project.root(), "worker rename enqueue dequeue", 5).unwrap();

        assert_eq!(candidates.first().unwrap().path, "internal/queue/worker.go");
    }

    /// Tests match every term their subject does, so without a penalty they
    /// crowd out the source on sheer count. They stay reachable, just below it.
    #[test]
    fn test_files_rank_below_the_source_they_cover() {
        let project = TempProject::new("tests_below");
        // The test sorts *before* the source alphabetically, so only the test
        // penalty can order these correctly.
        project.write("src/a_payments_test.rs", "");
        project.write("src/payments.rs", "");

        let candidates = discover_candidate_files(project.root(), "payments retry", 5).unwrap();
        let paths = candidates
            .iter()
            .map(|candidate| candidate.path.as_str())
            .collect::<Vec<_>>();

        assert_eq!(paths.first(), Some(&"src/payments.rs"));
        assert!(
            paths.contains(&"src/a_payments_test.rs"),
            "the test file must still be reachable: {paths:?}"
        );
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
