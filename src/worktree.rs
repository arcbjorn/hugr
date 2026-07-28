use std::path::Path;
use std::process::Command as ProcessCommand;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WorktreeState {
    pub inside_worktree: bool,
    pub root_path: Option<String>,
    pub branch: Option<String>,
    pub upstream: Option<String>,
    pub ahead: i64,
    pub behind: i64,
    pub changed_files: Vec<ChangedFile>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ChangedFile {
    pub path: String,
    pub original_path: Option<String>,
    pub staged_status: Option<String>,
    pub unstaged_status: Option<String>,
}

pub(crate) fn inspect(root: &Path) -> WorktreeState {
    let status = git_output(root, &["status", "--porcelain=v1", "--branch"]);
    let root_path = git_output(root, &["rev-parse", "--show-toplevel"]);

    match status {
        Some(status) => parse_status(&status, root_path),
        None => WorktreeState {
            inside_worktree: false,
            root_path: None,
            branch: None,
            upstream: None,
            ahead: 0,
            behind: 0,
            changed_files: Vec::new(),
        },
    }
}

fn parse_status(status: &str, root_path: Option<String>) -> WorktreeState {
    let mut branch = None;
    let mut upstream = None;
    let mut ahead = 0;
    let mut behind = 0;
    let mut changed_files = Vec::new();

    for line in status.lines() {
        if let Some(header) = line.strip_prefix("## ") {
            let parsed = parse_branch_header(header);
            branch = parsed.branch;
            upstream = parsed.upstream;
            ahead = parsed.ahead;
            behind = parsed.behind;
        } else if let Some(change) = parse_change_line(line) {
            changed_files.push(change);
        }
    }

    WorktreeState {
        inside_worktree: true,
        root_path,
        branch,
        upstream,
        ahead,
        behind,
        changed_files,
    }
}

#[derive(Debug, PartialEq, Eq)]
struct BranchHeader {
    branch: Option<String>,
    upstream: Option<String>,
    ahead: i64,
    behind: i64,
}

fn parse_branch_header(header: &str) -> BranchHeader {
    let (names, counts) = header
        .split_once(" [")
        .map_or((header, ""), |(names, counts)| {
            (names, counts.trim_end_matches(']'))
        });
    let (branch, upstream) = names
        .split_once("...")
        .map(|(branch, upstream)| (branch, Some(upstream.to_string())))
        .unwrap_or((names, None));

    BranchHeader {
        branch: branch_name(branch),
        upstream,
        ahead: count_marker(counts, "ahead"),
        behind: count_marker(counts, "behind"),
    }
}

fn branch_name(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}

fn count_marker(counts: &str, marker: &str) -> i64 {
    counts
        .split(',')
        .filter_map(|part| {
            let part = part.trim();
            part.strip_prefix(marker)
                .and_then(|count| count.trim().parse::<i64>().ok())
        })
        .next()
        .unwrap_or(0)
}

fn parse_change_line(line: &str) -> Option<ChangedFile> {
    if line.len() < 4 {
        return None;
    }

    let mut chars = line.chars();
    let staged = chars.next()?;
    let unstaged = chars.next()?;
    let path = line.get(3..)?.trim();
    if path.is_empty() {
        return None;
    }

    let (original_path, path) = parse_changed_path(path);
    Some(ChangedFile {
        path,
        original_path,
        staged_status: status_code(staged),
        unstaged_status: status_code(unstaged),
    })
}

fn parse_changed_path(path: &str) -> (Option<String>, String) {
    path.split_once(" -> ")
        .map(|(original, current)| (Some(unquote_path(original)), unquote_path(current)))
        .unwrap_or((None, unquote_path(path)))
}

fn status_code(code: char) -> Option<String> {
    match code {
        ' ' => None,
        '?' => Some("untracked".to_string()),
        '!' => Some("ignored".to_string()),
        'A' => Some("added".to_string()),
        'M' => Some("modified".to_string()),
        'D' => Some("deleted".to_string()),
        'R' => Some("renamed".to_string()),
        'C' => Some("copied".to_string()),
        'U' => Some("unmerged".to_string()),
        other => Some(other.to_string()),
    }
}

fn unquote_path(path: &str) -> String {
    path.trim_matches('"').replace("\\\"", "\"")
}

fn git_output(root: &Path, args: &[&str]) -> Option<String> {
    let output = ProcessCommand::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if value.is_empty() { None } else { Some(value) }
}

#[cfg(test)]
mod tests {
    use super::{parse_branch_header, parse_change_line, parse_status};

    #[test]
    fn parses_branch_header_counts() {
        let header = parse_branch_header("main...origin/main [ahead 2, behind 1]");

        assert_eq!(header.branch.as_deref(), Some("main"));
        assert_eq!(header.upstream.as_deref(), Some("origin/main"));
        assert_eq!(header.ahead, 2);
        assert_eq!(header.behind, 1);
    }

    #[test]
    fn parses_status_changes() {
        let state = parse_status(
            "## feature...origin/feature [ahead 1]\n M src/lib.rs\nA  src/worktree.rs\n?? notes.md\nR  old.rs -> new.rs\n",
            Some("/repo".to_string()),
        );

        assert!(state.inside_worktree);
        assert_eq!(state.branch.as_deref(), Some("feature"));
        assert_eq!(state.ahead, 1);
        assert_eq!(state.changed_files.len(), 4);
        assert_eq!(state.changed_files[0].path, "src/lib.rs");
        assert_eq!(
            state.changed_files[0].unstaged_status.as_deref(),
            Some("modified")
        );
        assert_eq!(
            state.changed_files[3].original_path.as_deref(),
            Some("old.rs")
        );
        assert_eq!(state.changed_files[3].path, "new.rs");
    }

    #[test]
    fn parses_untracked_change() {
        let change = parse_change_line("?? src/testmap.rs").unwrap();

        assert_eq!(change.path, "src/testmap.rs");
        assert_eq!(change.staged_status.as_deref(), Some("untracked"));
        assert_eq!(change.unstaged_status.as_deref(), Some("untracked"));
    }
}
