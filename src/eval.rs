//! Scores `hugr context` retrieval against the repository's own git history.
//!
//! Each recent commit is replayed as a ground-truth case: the commit subject
//! becomes the task and the source files it touched become the expected
//! evidence. The real context compiler runs for every case, so each scored
//! commit also persists a context-pack row and refreshes candidate indexes,
//! exactly like an ordinary `hugr context` invocation.
//!
//! The harness is deterministic and LLM-free. Absolute numbers are noisy by
//! construction (terse subjects, tree drift since old commits); the value is
//! the delta between two revisions of the ranking code, not the level.

use crate::cli::OutputFormat;
use crate::commands;
use crate::context;
use crate::store::Store;
use serde_json::json;
use std::collections::{BTreeMap, HashSet};
use std::path::Path;
use std::process::Command as ProcessCommand;

pub(crate) struct EvalOptions {
    pub from_git: usize,
    pub max_files: usize,
    pub min_hit_rate: Option<f64>,
    pub format: OutputFormat,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CommitCase {
    hash: String,
    subject: String,
    files: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PreparedCase {
    hash: String,
    task: String,
    expected: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
struct CommitScore {
    hash: String,
    task: String,
    expected: Vec<String>,
    recall: f64,
    first_hit_rank: Option<usize>,
    symbol_hit: bool,
    candidate_hit: bool,
}

impl CommitScore {
    fn hit(&self) -> bool {
        self.first_hit_rank.is_some()
    }

    fn mrr(&self) -> f64 {
        self.first_hit_rank
            .map(|rank| 1.0 / rank as f64)
            .unwrap_or(0.0)
    }
}

struct EvalReport {
    considered: usize,
    scores: Vec<CommitScore>,
    skips: BTreeMap<&'static str, usize>,
}

impl EvalReport {
    fn mean(&self, value: impl Fn(&CommitScore) -> f64) -> f64 {
        if self.scores.is_empty() {
            return 0.0;
        }
        self.scores.iter().map(value).sum::<f64>() / self.scores.len() as f64
    }

    fn file_recall(&self) -> f64 {
        self.mean(|score| score.recall)
    }

    fn hit_rate(&self) -> f64 {
        self.mean(|score| if score.hit() { 1.0 } else { 0.0 })
    }

    fn mrr(&self) -> f64 {
        self.mean(CommitScore::mrr)
    }

    fn symbol_file_hit_rate(&self) -> f64 {
        self.mean(|score| if score.symbol_hit { 1.0 } else { 0.0 })
    }

    fn candidate_hit_rate(&self) -> f64 {
        self.mean(|score| if score.candidate_hit { 1.0 } else { 0.0 })
    }
}

pub(crate) async fn run(options: EvalOptions) -> Result<(), String> {
    let store = Store::open_current();
    if store.is_remote_only()? {
        return Err("hugr eval requires local storage".to_string());
    }
    ensure_git_repository()?;

    let commits = collect_commit_cases(options.from_git.saturating_mul(4))?;
    let mut scores = Vec::new();
    let mut skips = BTreeMap::new();
    let mut considered = 0;

    for commit in &commits {
        if scores.len() >= options.from_git {
            break;
        }
        considered += 1;
        match prepare_case(commit, options.max_files, &|path| Path::new(path).is_file()) {
            Ok(case) => {
                let (pack, candidates) =
                    commands::compile_context_pack_with_file_candidates(&case.task).await?;
                let pack_files = pack
                    .relevant_files
                    .iter()
                    .map(|file| file.path.clone())
                    .collect::<Vec<_>>();
                let symbol_paths = pack
                    .important_symbols
                    .iter()
                    .map(|symbol| symbol.path.clone())
                    .collect::<Vec<_>>();
                scores.push(score_case(&case, &pack_files, &symbol_paths, &candidates));
            }
            Err(reason) => *skips.entry(reason).or_insert(0) += 1,
        }
    }

    let report = EvalReport {
        considered,
        scores,
        skips,
    };

    if options.format == OutputFormat::Json {
        println!("{}", render_json(&report));
    } else {
        print!("{}", render_text(&report));
    }

    if let Some(min_hit_rate) = options.min_hit_rate {
        if report.scores.is_empty() {
            return Err("no commits were evaluated; cannot enforce --min-hit-rate".to_string());
        }
        let hit_rate = report.hit_rate();
        if hit_rate < min_hit_rate {
            return Err(format!(
                "hit rate {hit_rate:.3} is below --min-hit-rate {min_hit_rate:.3}"
            ));
        }
    }

    Ok(())
}

fn ensure_git_repository() -> Result<(), String> {
    let output = ProcessCommand::new("git")
        .args(["rev-parse", "--is-inside-work-tree"])
        .output()
        .map_err(|error| error.to_string())?;
    if !output.status.success() {
        return Err("hugr eval requires a git repository".to_string());
    }
    Ok(())
}

fn collect_commit_cases(limit: usize) -> Result<Vec<CommitCase>, String> {
    let output = ProcessCommand::new("git")
        .args([
            "log",
            "--no-merges",
            "-n",
            &limit.to_string(),
            "--pretty=format:%H\u{1f}%s",
            "--name-only",
        ])
        .output()
        .map_err(|error| error.to_string())?;
    if !output.status.success() {
        return Err(format!(
            "git log failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(parse_git_log(&String::from_utf8_lossy(&output.stdout)))
}

fn parse_git_log(text: &str) -> Vec<CommitCase> {
    let mut cases: Vec<CommitCase> = Vec::new();

    for line in text.lines() {
        if let Some((hash, subject)) = line.split_once('\u{1f}') {
            cases.push(CommitCase {
                hash: hash.trim().to_string(),
                subject: subject.trim().to_string(),
                files: Vec::new(),
            });
        } else if !line.trim().is_empty() {
            if let Some(case) = cases.last_mut() {
                case.files.push(line.trim().to_string());
            }
        }
    }

    cases
}

/// Turns a commit subject into an agent-style task. A conventional-commit
/// prefix is folded into plain words so `feat(context): rank symbols` scores
/// as `context rank symbols` instead of matching the literal `feat`.
fn eval_task(subject: &str) -> String {
    let Some((prefix, rest)) = subject.split_once(':') else {
        return subject.trim().to_string();
    };
    let prefix = prefix.trim();
    let rest = rest.trim();
    if prefix.len() > 24 || prefix.contains(' ') || rest.is_empty() {
        return subject.trim().to_string();
    }

    match prefix.split_once('(') {
        Some((_, scope)) => {
            let scope = scope.trim_end_matches('!').trim_end_matches(')');
            if scope.is_empty() {
                rest.to_string()
            } else {
                format!("{scope} {rest}")
            }
        }
        None => rest.to_string(),
    }
}

fn prepare_case(
    commit: &CommitCase,
    max_files: usize,
    file_exists: &dyn Fn(&str) -> bool,
) -> Result<PreparedCase, &'static str> {
    let task = eval_task(&commit.subject);
    if context::context_query_terms(&task).len() < 2 {
        return Err("short_subject");
    }

    let expected = commit
        .files
        .iter()
        .filter(|path| context::context_file_likely_source(path))
        .filter(|path| file_exists(path))
        .cloned()
        .collect::<Vec<_>>();
    if expected.is_empty() {
        return Err("no_source_files");
    }
    if expected.len() > max_files {
        return Err("too_many_files");
    }

    Ok(PreparedCase {
        hash: commit.hash.clone(),
        task,
        expected,
    })
}

fn score_case(
    case: &PreparedCase,
    pack_files: &[String],
    symbol_paths: &[String],
    candidates: &[String],
) -> CommitScore {
    let expected = case
        .expected
        .iter()
        .map(String::as_str)
        .collect::<HashSet<_>>();
    let covered = case
        .expected
        .iter()
        .filter(|path| pack_files.iter().any(|found| &found == path))
        .count();

    CommitScore {
        hash: case.hash.clone(),
        task: case.task.clone(),
        expected: case.expected.clone(),
        recall: covered as f64 / case.expected.len() as f64,
        first_hit_rank: pack_files
            .iter()
            .position(|path| expected.contains(path.as_str()))
            .map(|index| index + 1),
        symbol_hit: symbol_paths
            .iter()
            .any(|path| expected.contains(path.as_str())),
        candidate_hit: candidates
            .iter()
            .any(|path| expected.contains(path.as_str())),
    }
}

fn render_text(report: &EvalReport) -> String {
    let mut rendered = String::new();
    rendered.push_str("Hugr context eval\n");
    rendered.push_str(&format!("  commits considered: {}\n", report.considered));
    rendered.push_str(&format!("  evaluated: {}\n", report.scores.len()));
    for (reason, count) in &report.skips {
        rendered.push_str(&format!("  skipped {reason}: {count}\n"));
    }
    if report.scores.is_empty() {
        rendered.push_str("  no commits evaluated\n");
        return rendered;
    }
    rendered.push_str(&format!("  file_recall: {:.3}\n", report.file_recall()));
    rendered.push_str(&format!("  hit_rate: {:.3}\n", report.hit_rate()));
    rendered.push_str(&format!("  mrr: {:.3}\n", report.mrr()));
    rendered.push_str(&format!(
        "  symbol_file_hit_rate: {:.3}\n",
        report.symbol_file_hit_rate()
    ));
    rendered.push_str(&format!(
        "  candidate_hit_rate: {:.3}\n",
        report.candidate_hit_rate()
    ));
    rendered.push_str("  commits:\n");
    for score in &report.scores {
        let hash = score.hash.get(..7).unwrap_or(&score.hash);
        let rank = score
            .first_hit_rank
            .map(|rank| rank.to_string())
            .unwrap_or_else(|| "-".to_string());
        rendered.push_str(&format!(
            "  - {hash} recall {:.2} rank {rank}: {}\n",
            score.recall, score.task
        ));
    }
    rendered
}

fn render_json(report: &EvalReport) -> String {
    let skips = report
        .skips
        .iter()
        .map(|(reason, count)| ((*reason).to_string(), json!(count)))
        .collect::<serde_json::Map<_, _>>();
    let per_commit = report
        .scores
        .iter()
        .map(|score| {
            json!({
                "hash": score.hash,
                "task": score.task,
                "expected": score.expected,
                "recall": score.recall,
                "hit": score.hit(),
                "first_hit_rank": score.first_hit_rank,
                "mrr": score.mrr(),
                "symbol_hit": score.symbol_hit,
                "candidate_hit": score.candidate_hit,
            })
        })
        .collect::<Vec<_>>();

    json!({
        "considered": report.considered,
        "evaluated": report.scores.len(),
        "skipped": skips,
        "file_recall": report.file_recall(),
        "hit_rate": report.hit_rate(),
        "mrr": report.mrr(),
        "symbol_file_hit_rate": report.symbol_file_hit_rate(),
        "candidate_hit_rate": report.candidate_hit_rate(),
        "per_commit": per_commit,
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::{CommitCase, eval_task, parse_git_log, prepare_case, score_case};

    #[test]
    fn parses_git_log_headers_and_files() {
        let text = "abc\u{1f}feat(context): rank symbols\nsrc/context.rs\nsrc/store.rs\n\ndef\u{1f}chore: empty commit\n\nghi\u{1f}fix(code): tokenize lines\nsrc/code.rs\n";

        let cases = parse_git_log(text);

        assert_eq!(cases.len(), 3);
        assert_eq!(cases[0].hash, "abc");
        assert_eq!(cases[0].files, vec!["src/context.rs", "src/store.rs"]);
        assert!(cases[1].files.is_empty());
        assert_eq!(cases[2].files, vec!["src/code.rs"]);
    }

    #[test]
    fn folds_conventional_prefixes_into_tasks() {
        assert_eq!(
            eval_task("feat(context): rank symbols by name"),
            "context rank symbols by name"
        );
        assert_eq!(eval_task("fix: tokenize lines"), "tokenize lines");
        assert_eq!(eval_task("update readme badges"), "update readme badges");
        assert_eq!(
            eval_task("feat(edit)!: breaking rename"),
            "edit breaking rename"
        );
    }

    fn commit(subject: &str, files: &[&str]) -> CommitCase {
        CommitCase {
            hash: "abc1234".to_string(),
            subject: subject.to_string(),
            files: files.iter().map(|file| file.to_string()).collect(),
        }
    }

    #[test]
    fn prepares_cases_with_existing_source_files() {
        let case = prepare_case(
            &commit(
                "feat(context): rank symbols",
                &["src/context.rs", "docs/DEV_PLAN.md", "src/missing.rs"],
            ),
            8,
            &|path| path != "src/missing.rs",
        )
        .expect("case should be prepared");

        assert_eq!(case.task, "context rank symbols");
        assert_eq!(case.expected, vec!["src/context.rs"]);
    }

    #[test]
    fn skips_unusable_commits_with_reasons() {
        let exists = |_: &str| true;

        assert_eq!(
            prepare_case(&commit("fix: typo", &["src/a.rs"]), 8, &exists),
            Err("short_subject")
        );
        assert_eq!(
            prepare_case(
                &commit("docs(plan): update the roadmap", &["docs/DEV_PLAN.md"]),
                8,
                &exists
            ),
            Err("no_source_files")
        );
        assert_eq!(
            prepare_case(
                &commit("feat(core): huge refactor", &["src/a.rs", "src/b.rs"]),
                1,
                &exists
            ),
            Err("too_many_files")
        );
    }

    #[test]
    fn scores_recall_rank_and_candidate_hits() {
        let case = prepare_case(
            &commit(
                "feat(context): rank symbols",
                &["src/context.rs", "src/store.rs"],
            ),
            8,
            &|_| true,
        )
        .unwrap();

        let score = score_case(
            &case,
            &["src/other.rs".to_string(), "src/context.rs".to_string()],
            &["src/store.rs".to_string()],
            &["src/store.rs".to_string()],
        );

        assert_eq!(score.recall, 0.5);
        assert_eq!(score.first_hit_rank, Some(2));
        assert_eq!(score.mrr(), 0.5);
        assert!(score.hit());
        assert!(score.symbol_hit);
        assert!(score.candidate_hit);

        let miss = score_case(&case, &[], &[], &[]);
        assert_eq!(miss.recall, 0.0);
        assert_eq!(miss.first_hit_rank, None);
        assert_eq!(miss.mrr(), 0.0);
    }
}
