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

/// Decodes the C-style quoting `git status` applies to unusual paths.
///
/// Git quotes a path whenever it contains a control character, a double quote,
/// a backslash, or a byte above 0x7f, escaping those bytes as `\"`, `\\`, the
/// usual `\n`/`\t`/… shorthands, or three-digit octal. Handling only `\"` left
/// the rest raw, so a UTF-8 filename came back as the literal
/// `caf\303\251.rs`. These paths reach agents inside context packs, and a path
/// that does not exist is one the agent cannot open.
///
/// Octal escapes are collected as bytes before decoding, since one character
/// spans several of them (`é` is `\303\251`). Anything that is not valid UTF-8
/// once decoded falls back to lossy conversion rather than failing the parse:
/// a slightly mangled path in a status listing beats dropping the entry.
fn unquote_path(path: &str) -> String {
    let Some(inner) = path
        .strip_prefix('"')
        .and_then(|path| path.strip_suffix('"'))
    else {
        // Unquoted paths are already literal.
        return path.to_string();
    };

    let mut decoded: Vec<u8> = Vec::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(character) = chars.next() {
        if character != '\\' {
            let mut buffer = [0_u8; 4];
            decoded.extend_from_slice(character.encode_utf8(&mut buffer).as_bytes());
            continue;
        }

        match chars.next() {
            Some('n') => decoded.push(b'\n'),
            Some('t') => decoded.push(b'\t'),
            Some('r') => decoded.push(b'\r'),
            Some('a') => decoded.push(0x07),
            Some('b') => decoded.push(0x08),
            Some('f') => decoded.push(0x0c),
            Some('v') => decoded.push(0x0b),
            Some('"') => decoded.push(b'"'),
            // `\\` is an escaped backslash; a trailing `\` with nothing after
            // it is malformed, and echoing the backslash is the closest
            // reading.
            Some('\\') | None => decoded.push(b'\\'),
            Some(first @ '0'..='7') => {
                // Octal is always exactly three digits from git.
                let mut value = first.to_digit(8).unwrap_or(0);
                for _ in 0..2 {
                    let Some(digit) = chars.clone().next().and_then(|next| next.to_digit(8)) else {
                        break;
                    };
                    chars.next();
                    value = value * 8 + digit;
                }
                decoded.push(u8::try_from(value).unwrap_or(b'?'));
            }
            // An escape git does not produce; keep it visible rather than
            // silently dropping the backslash.
            Some(other) => {
                decoded.push(b'\\');
                let mut buffer = [0_u8; 4];
                decoded.extend_from_slice(other.encode_utf8(&mut buffer).as_bytes());
            }
        }
    }

    String::from_utf8_lossy(&decoded).into_owned()
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
    use super::{parse_branch_header, parse_change_line, parse_status, unquote_path};

    /// `git status` quotes any path with a control character, a quote, a
    /// backslash, or a byte above 0x7f. Only `\"` used to be decoded, so a
    /// UTF-8 filename reached the context pack as the literal
    /// `caf\303\251.rs` — a path no agent can open.
    #[test]
    fn unquotes_the_escapes_git_actually_emits() {
        assert_eq!(unquote_path(r#""caf\303\251.rs""#), "café.rs");
        assert_eq!(unquote_path(r#""back\\slash.rs""#), r"back\slash.rs");
        assert_eq!(unquote_path(r#""say \"hi\".rs""#), r#"say "hi".rs"#);
        assert_eq!(unquote_path("\"tab\\there.rs\""), "tab\there.rs");
        assert_eq!(unquote_path("\"line\\nbreak.rs\""), "line\nbreak.rs");
    }

    /// Most paths need no quoting at all and must survive untouched — notably
    /// ones that merely contain a space, which git leaves unquoted.
    #[test]
    fn leaves_ordinary_paths_alone() {
        assert_eq!(unquote_path("src/lib.rs"), "src/lib.rs");
        assert_eq!(unquote_path("with space.rs"), "with space.rs");
        assert_eq!(unquote_path(r#""with space.rs""#), "with space.rs");
    }

    #[test]
    fn changed_paths_are_unquoted_including_renames() {
        let change = parse_change_line(r#"R  "old\303\251.rs" -> "new\303\251.rs""#).unwrap();

        assert_eq!(change.original_path.as_deref(), Some("oldé.rs"));
        assert_eq!(change.path, "newé.rs");
    }

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
