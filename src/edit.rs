use crate::code::{self, CodeReference, CodeSymbol};
use crate::context::json_string;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;
use std::path::Path;

/// A structural symbol replacement that has been validated but not yet written.
///
/// `contents` is the full rewritten file text. `summary` describes the change for
/// rendering and provenance. The command layer writes `contents`, re-indexes, and
/// records a session event using `summary`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PlannedReplacement {
    pub contents: String,
    pub summary: SymbolReplacement,
}

/// A reference-aware symbol rename that has been validated but not yet written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PlannedRename {
    pub files: Vec<PlannedRenameFile>,
    pub summary: SymbolRename,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PlannedRenameFile {
    pub path: String,
    pub contents: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SymbolReplacement {
    pub path: String,
    pub language: Option<String>,
    pub name: String,
    pub kind: String,
    pub old_line_start: i64,
    pub old_line_end: i64,
    pub new_line_start: i64,
    pub new_line_end: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SymbolRename {
    pub target_path: String,
    pub language: Option<String>,
    pub old_name: String,
    pub new_name: String,
    pub kind: String,
    pub line_start: i64,
    pub line_end: i64,
    pub reference_count: usize,
    pub changed_files: Vec<SymbolRenameFile>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SymbolRenameFile {
    pub path: String,
    pub replacement_count: usize,
}

/// Plan a safe, symbol-aware replacement of one top-level symbol in a source file.
///
/// Safety contract:
/// - The target must resolve to exactly one indexed symbol. Zero matches or more than
///   one match are refused with an explanatory error listing candidates, so callers
///   disambiguate with `--kind` rather than editing the wrong code.
/// - The replacement body must parse for the file's language and define exactly one
///   symbol whose name matches the target (and whose kind matches when the target has
///   a determinate kind). This blocks accidental rename, deletion, multi-symbol paste,
///   and syntactically broken input.
///
/// On success the matched symbol's full line span is replaced by the body, re-indented
/// to the target's original leading whitespace so surrounding code stays well-formed.
pub(crate) fn plan_replacement(
    path: &str,
    contents: &str,
    name: &str,
    kind: Option<&str>,
    new_body: &str,
) -> Result<PlannedReplacement, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("replace-symbol requires a symbol name".to_string());
    }
    let new_body = new_body.trim_end_matches(['\n', '\r']);
    if new_body.trim().is_empty() {
        return Err("replace-symbol requires a non-empty replacement body".to_string());
    }

    let language = language_for_path(path);
    let language_label = language.unwrap_or("unknown");

    let symbols = code::symbols_in_source(path, language, contents)?;
    let target = resolve_target(&symbols, name, kind, "replace")?;

    let old_line_start = target.line_start;
    let old_line_end = target.line_end.unwrap_or(target.line_start);
    validate_span(old_line_start, old_line_end, path)?;

    validate_replacement_body(path, language, language_label, new_body, &target)?;

    let indent = leading_indent(contents, old_line_start);
    let reindented = reindent_body(new_body, &indent);
    let (contents, new_line_end) =
        splice_lines(contents, old_line_start, old_line_end, &reindented)?;

    Ok(PlannedReplacement {
        contents,
        summary: SymbolReplacement {
            path: path.to_string(),
            language: language.map(str::to_string),
            name: target.name.clone(),
            kind: target.kind.clone(),
            old_line_start,
            old_line_end,
            new_line_start: old_line_start,
            new_line_end,
        },
    })
}

pub(crate) fn resolve_symbol_in_source(
    path: &str,
    contents: &str,
    name: &str,
    kind: Option<&str>,
    action: &str,
) -> Result<CodeSymbol, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err(format!("{action}-symbol requires a symbol name"));
    }

    let language = language_for_path(path);
    let symbols = code::symbols_in_source(path, language, contents)?;
    resolve_target(&symbols, name, kind, action)
}

pub(crate) fn plan_rename(
    target: &CodeSymbol,
    references: &[CodeReference],
    files: Vec<(String, String)>,
    new_name: &str,
) -> Result<PlannedRename, String> {
    let new_name = new_name.trim();
    if !valid_identifier(new_name) {
        return Err(
            "rename-symbol requires a valid ASCII identifier for the new symbol name".to_string(),
        );
    }
    if new_name == target.name {
        return Err("rename-symbol new name must differ from the current name".to_string());
    }

    let mut contents_by_path = files.into_iter().collect::<BTreeMap<_, _>>();
    if !contents_by_path.contains_key(&target.path) {
        return Err(format!(
            "rename-symbol requires source contents for target file {}",
            target.path
        ));
    }

    reject_target_name_collision(target, contents_by_path.get(&target.path).unwrap(), new_name)?;

    let mut lines_by_path = BTreeMap::<String, BTreeSet<i64>>::new();
    lines_by_path
        .entry(target.path.clone())
        .or_default()
        .insert(target.line_start);
    for reference in references
        .iter()
        .filter(|reference| reference.target_path == target.path)
        .filter(|reference| reference.target_name == target.name)
    {
        lines_by_path
            .entry(reference.path.clone())
            .or_default()
            .insert(reference.line_start);
    }

    let mut planned_files = Vec::new();
    let mut changed_files = Vec::new();
    for (path, line_numbers) in lines_by_path {
        let Some(contents) = contents_by_path.remove(&path) else {
            return Err(format!(
                "rename-symbol missing source contents for referenced file {path}; rerun hugr index"
            ));
        };
        let (renamed, replacement_count) =
            replace_identifier_on_lines(&path, &contents, &target.name, new_name, &line_numbers)?;
        if replacement_count == 0 {
            return Err(format!(
                "rename-symbol found no occurrences of '{}' in selected lines for {path}",
                target.name
            ));
        }
        let language = language_for_path(&path);
        if !code::parses_cleanly(&path, language, &renamed)? {
            return Err(format!(
                "renamed source in {path} is not valid {} code; refusing to write partial refactor",
                language.unwrap_or("unknown")
            ));
        }
        planned_files.push(PlannedRenameFile {
            path: path.clone(),
            contents: renamed,
        });
        changed_files.push(SymbolRenameFile {
            path,
            replacement_count,
        });
    }

    validate_renamed_target(target, new_name, &planned_files)?;
    changed_files.sort_by(|left, right| left.path.cmp(&right.path));
    planned_files.sort_by(|left, right| left.path.cmp(&right.path));

    Ok(PlannedRename {
        files: planned_files,
        summary: SymbolRename {
            target_path: target.path.clone(),
            language: target.language.clone(),
            old_name: target.name.clone(),
            new_name: new_name.to_string(),
            kind: target.kind.clone(),
            line_start: target.line_start,
            line_end: target.line_end.unwrap_or(target.line_start),
            reference_count: references
                .iter()
                .filter(|reference| reference.target_path == target.path)
                .filter(|reference| reference.target_name == target.name)
                .count(),
            changed_files,
        },
    })
}

fn language_for_path(path: &str) -> Option<&'static str> {
    crate::discovery::language_for(Path::new(path))
}

fn resolve_target(
    symbols: &[CodeSymbol],
    name: &str,
    kind: Option<&str>,
    action: &str,
) -> Result<CodeSymbol, String> {
    let kind = kind.map(str::trim).filter(|kind| !kind.is_empty());
    let matches = symbols
        .iter()
        .filter(|symbol| symbol.name == name)
        .filter(|symbol| kind.is_none_or(|kind| symbol.kind == kind))
        .cloned()
        .collect::<Vec<_>>();

    match matches.as_slice() {
        [] => Err(no_match_error(symbols, name, kind, action)),
        [single] => Ok(single.clone()),
        many => Err(ambiguous_error(many, name)),
    }
}

fn no_match_error(
    symbols: &[CodeSymbol],
    name: &str,
    kind: Option<&str>,
    action: &str,
) -> String {
    let known = symbols
        .iter()
        .filter(|symbol| symbol.name == name)
        .map(|symbol| format!("{} at line {}", symbol.kind, symbol.line_start))
        .collect::<Vec<_>>();

    if let Some(kind) = kind {
        if known.is_empty() {
            format!("no symbol named '{name}' found to {action}")
        } else {
            format!(
                "no {kind} named '{name}' found to {action}; found {}",
                known.join(", ")
            )
        }
    } else {
        format!("no symbol named '{name}' found to {action}")
    }
}

fn ambiguous_error(matches: &[CodeSymbol], name: &str) -> String {
    let candidates = matches
        .iter()
        .map(|symbol| format!("{} at line {}", symbol.kind, symbol.line_start))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "symbol '{name}' is ambiguous ({}); pass --kind to select one: {candidates}",
        matches.len()
    )
}

fn validate_span(line_start: i64, line_end: i64, path: &str) -> Result<(), String> {
    if line_start < 1 || line_end < line_start {
        return Err(format!(
            "symbol span {line_start}-{line_end} in {path} is invalid"
        ));
    }
    Ok(())
}

fn validate_replacement_body(
    path: &str,
    language: Option<&str>,
    language_label: &str,
    new_body: &str,
    target: &CodeSymbol,
) -> Result<(), String> {
    if !code::parses_cleanly(path, language, new_body)? {
        return Err(format!(
            "replacement body is not valid {language_label} source; \
             replace-symbol will not write code that fails to parse"
        ));
    }

    let produced = code::symbols_in_source(path, language, new_body)?;
    let top_level = top_level_symbols(&produced);

    let named = top_level
        .iter()
        .filter(|symbol| symbol.name == target.name)
        .collect::<Vec<_>>();

    match named.as_slice() {
        [] => Err(format!(
            "replacement body does not define {language_label} symbol '{}'; \
             refusing to rename or remove it via replace-symbol",
            target.name
        )),
        [single] => {
            if single.kind == target.kind {
                Ok(())
            } else {
                Err(format!(
                    "replacement body defines '{}' as {} but the target is {}; \
                     replace-symbol will not change a symbol's kind",
                    target.name, single.kind, target.kind
                ))
            }
        }
        _ => Err(format!(
            "replacement body defines '{}' more than once",
            target.name
        )),
    }
}

/// Keep only outermost symbols so a replaced function containing nested items (or an
/// impl block with methods) validates against the declaration the caller targeted,
/// not its interior members.
fn top_level_symbols(symbols: &[CodeSymbol]) -> Vec<CodeSymbol> {
    let mut sorted = symbols.to_vec();
    sorted.sort_by(|left, right| {
        let left_end = left.line_end.unwrap_or(left.line_start);
        let right_end = right.line_end.unwrap_or(right.line_start);
        left.line_start
            .cmp(&right.line_start)
            .then_with(|| right_end.cmp(&left_end))
    });

    let mut top_level: Vec<CodeSymbol> = Vec::new();
    for symbol in sorted {
        let symbol_end = symbol.line_end.unwrap_or(symbol.line_start);
        let enclosed = top_level.iter().any(|outer| {
            let outer_end = outer.line_end.unwrap_or(outer.line_start);
            symbol.line_start >= outer.line_start && symbol_end <= outer_end
        });
        if !enclosed {
            top_level.push(symbol);
        }
    }
    top_level
}

fn leading_indent(contents: &str, line_start: i64) -> String {
    let index = usize::try_from(line_start - 1).unwrap_or(0);
    contents
        .lines()
        .nth(index)
        .map(|line| {
            line.chars()
                .take_while(|char| *char == ' ' || *char == '\t')
                .collect::<String>()
        })
        .unwrap_or_default()
}

/// Re-indent the body so its first line adopts the target's original indentation while
/// interior lines keep their relative indentation. Blank lines stay blank.
fn reindent_body(new_body: &str, indent: &str) -> String {
    if indent.is_empty() {
        return new_body.to_string();
    }

    let base = body_base_indent(new_body);
    new_body
        .lines()
        .map(|line| {
            if line.trim().is_empty() {
                String::new()
            } else {
                let stripped = line.strip_prefix(&base).unwrap_or(line);
                format!("{indent}{stripped}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The shared leading whitespace of the body's first non-blank line, used as the anchor
/// that re-indentation strips before applying the target indent.
fn body_base_indent(new_body: &str) -> String {
    new_body
        .lines()
        .find(|line| !line.trim().is_empty())
        .map(|line| {
            line.chars()
                .take_while(|char| *char == ' ' || *char == '\t')
                .collect::<String>()
        })
        .unwrap_or_default()
}

/// Replace the 1-based inclusive line range with `replacement`, returning the new file
/// text and the 1-based line where the replacement ends.
fn splice_lines(
    contents: &str,
    line_start: i64,
    line_end: i64,
    replacement: &str,
) -> Result<(String, i64), String> {
    let start = usize::try_from(line_start - 1).map_err(|error| error.to_string())?;
    let end = usize::try_from(line_end - 1).map_err(|error| error.to_string())?;

    let trailing_newline = contents.ends_with('\n');
    let lines = contents.lines().collect::<Vec<_>>();
    if end >= lines.len() {
        return Err(format!(
            "symbol span ends at line {line_end} but file has {} lines",
            lines.len()
        ));
    }

    let replacement_lines = replacement.split('\n').collect::<Vec<_>>();
    let mut result = Vec::with_capacity(lines.len() - (end - start + 1) + replacement_lines.len());
    result.extend_from_slice(&lines[..start]);
    result.extend_from_slice(&replacement_lines);
    result.extend_from_slice(&lines[end + 1..]);

    let new_line_end = i64::try_from(start + replacement_lines.len())
        .map_err(|error| error.to_string())?
        .max(line_start);

    let mut rendered = result.join("\n");
    if trailing_newline {
        rendered.push('\n');
    }
    Ok((rendered, new_line_end))
}

impl SymbolReplacement {
    pub(crate) fn render_markdown(&self) -> String {
        let mut rendered = String::new();
        rendered.push_str("# Hugr Symbol Replacement\n\n");
        let _ = writeln!(rendered, "## Symbol\n{} {}", self.kind, self.name);
        let _ = writeln!(
            rendered,
            "\n## Location\n{}:{}-{} -> {}:{}-{}",
            self.path,
            self.old_line_start,
            self.old_line_end,
            self.path,
            self.new_line_start,
            self.new_line_end
        );
        let _ = writeln!(
            rendered,
            "\n## Language\n{}",
            self.language.as_deref().unwrap_or("unknown")
        );
        rendered
    }

    pub(crate) fn render_json(&self) -> String {
        format!(
            "{{\"path\":{},\"language\":{},\"name\":{},\"kind\":{},\
             \"old_line_start\":{},\"old_line_end\":{},\
             \"new_line_start\":{},\"new_line_end\":{}}}",
            json_string(&self.path),
            self.language
                .as_deref()
                .map(json_string)
                .unwrap_or_else(|| "null".to_string()),
            json_string(&self.name),
            json_string(&self.kind),
            self.old_line_start,
            self.old_line_end,
            self.new_line_start,
            self.new_line_end
        )
    }
}

#[cfg(test)]
mod tests {
    use super::plan_replacement;

    const RUST_SOURCE: &str =
        "pub struct Registry;\n\npub fn greet() -> u8 {\n    1\n}\n\npub fn other() {}\n";

    #[test]
    fn replaces_a_unique_function_body() {
        let planned = plan_replacement(
            "src/lib.rs",
            RUST_SOURCE,
            "greet",
            None,
            "pub fn greet() -> u8 {\n    42\n}",
        )
        .unwrap();

        assert!(planned.contents.contains("42"));
        assert!(!planned.contents.contains("    1\n"));
        assert!(planned.contents.contains("pub struct Registry;"));
        assert!(planned.contents.contains("pub fn other() {}"));
        assert!(planned.contents.ends_with('\n'));
        assert_eq!(planned.summary.name, "greet");
        assert_eq!(planned.summary.kind, "function");
        assert_eq!(planned.summary.old_line_start, 3);
        assert_eq!(planned.summary.old_line_end, 5);
        assert_eq!(planned.summary.new_line_start, 3);
        assert_eq!(planned.summary.new_line_end, 5);
    }

    #[test]
    fn expands_line_span_when_body_grows() {
        let planned = plan_replacement(
            "src/lib.rs",
            RUST_SOURCE,
            "greet",
            None,
            "pub fn greet() -> u8 {\n    let value = 42;\n    value\n}",
        )
        .unwrap();

        assert_eq!(planned.summary.old_line_end, 5);
        assert_eq!(planned.summary.new_line_end, 6);
        assert!(planned.contents.contains("let value = 42;"));
        assert!(planned.contents.contains("pub fn other() {}"));
    }

    #[test]
    fn rejects_missing_symbol() {
        let error = plan_replacement(
            "src/lib.rs",
            RUST_SOURCE,
            "absent",
            None,
            "pub fn absent() {}",
        )
        .unwrap_err();
        assert!(error.contains("no symbol named 'absent'"), "{error}");
    }

    #[test]
    fn rejects_rename_in_body() {
        let error = plan_replacement(
            "src/lib.rs",
            RUST_SOURCE,
            "greet",
            None,
            "pub fn renamed() -> u8 {\n    42\n}",
        )
        .unwrap_err();
        assert!(error.contains("does not define"), "{error}");
        assert!(error.contains("greet"), "{error}");
    }

    #[test]
    fn rejects_kind_change_in_body() {
        let error = plan_replacement(
            "src/lib.rs",
            "pub fn thing() {}\n",
            "thing",
            None,
            "pub struct thing;",
        )
        .unwrap_err();
        assert!(error.contains("kind"), "{error}");
    }

    #[test]
    fn rejects_syntactically_broken_body() {
        let error = plan_replacement(
            "src/lib.rs",
            RUST_SOURCE,
            "greet",
            None,
            "pub fn greet() -> u8 { 42",
        )
        .unwrap_err();
        assert!(error.contains("not valid rust source"), "{error}");
    }

    #[test]
    fn disambiguates_with_kind() {
        let source = "pub struct Thing;\n\npub fn Thing() {}\n";
        let planned = plan_replacement(
            "src/lib.rs",
            source,
            "Thing",
            Some("function"),
            "pub fn Thing() {\n    let _ = 1;\n}",
        )
        .unwrap();
        assert_eq!(planned.summary.kind, "function");
        assert!(planned.contents.contains("pub struct Thing;"));
        assert!(planned.contents.contains("let _ = 1;"));
    }

    #[test]
    fn ambiguous_without_kind_is_refused() {
        let source = "pub struct Thing;\n\npub fn Thing() {}\n";
        let error =
            plan_replacement("src/lib.rs", source, "Thing", None, "pub fn Thing() {}").unwrap_err();
        assert!(error.contains("ambiguous"), "{error}");
        assert!(error.contains("--kind"), "{error}");
    }

    #[test]
    fn preserves_indentation_of_nested_symbol() {
        let source = "impl Registry {\n    pub fn value(&self) -> u8 {\n        1\n    }\n}\n";
        let planned = plan_replacement(
            "src/lib.rs",
            source,
            "value",
            None,
            "pub fn value(&self) -> u8 {\n    2\n}",
        )
        .unwrap();

        assert!(
            planned.contents.contains("    pub fn value(&self) -> u8 {"),
            "first line should be indented: {}",
            planned.contents
        );
        assert!(
            planned.contents.contains("        2"),
            "interior line should keep relative indent: {}",
            planned.contents
        );
        assert!(planned.contents.contains("impl Registry {"));
    }

    #[test]
    fn replaces_python_function() {
        let source = "class Registry:\n    pass\n\n\ndef greet():\n    return 1\n";
        let planned = plan_replacement(
            "app/main.py",
            source,
            "greet",
            None,
            "def greet():\n    return 42",
        )
        .unwrap();
        assert!(planned.contents.contains("return 42"));
        assert!(planned.contents.contains("class Registry:"));
        assert_eq!(planned.summary.language.as_deref(), Some("python"));
    }

    #[test]
    fn renders_markdown_and_json() {
        let planned = plan_replacement(
            "src/lib.rs",
            RUST_SOURCE,
            "greet",
            None,
            "pub fn greet() -> u8 {\n    42\n}",
        )
        .unwrap();
        let markdown = planned.summary.render_markdown();
        let json = planned.summary.render_json();

        assert!(markdown.contains("function greet"));
        assert!(markdown.contains("src/lib.rs:3-5 -> src/lib.rs:3-5"));
        assert!(json.contains("\"name\":\"greet\""));
        assert!(json.contains("\"kind\":\"function\""));
        assert!(json.contains("\"old_line_start\":3"));
        assert!(json.contains("\"new_line_end\":5"));
    }
}
