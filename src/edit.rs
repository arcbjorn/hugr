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

/// A multi-file symbol move that has been validated but not yet written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PlannedMove {
    pub files: Vec<PlannedMoveFile>,
    pub summary: SymbolMove,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PlannedMoveFile {
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SymbolMove {
    pub source_path: String,
    pub destination_path: String,
    pub language: Option<String>,
    pub name: String,
    pub kind: String,
    pub old_line_start: i64,
    pub old_line_end: i64,
    pub moved_line_count: usize,
    pub rewritten_reference_count: usize,
    pub changed_files: Vec<SymbolMoveFile>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SymbolMoveFile {
    pub path: String,
    pub action: String,
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

    reject_target_name_collision(
        target,
        contents_by_path.get(&target.path).unwrap(),
        new_name,
    )?;

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

pub(crate) fn plan_move(
    target: &CodeSymbol,
    references: &[CodeReference],
    source_contents: &str,
    destination_path: &str,
    destination_contents: &str,
    reference_files: Vec<(String, String)>,
    rewrite_references: bool,
) -> Result<PlannedMove, String> {
    let destination_path = destination_path.trim();
    if destination_path.is_empty() {
        return Err("move-symbol requires a destination path".to_string());
    }
    if destination_path == target.path {
        return Err("move-symbol destination must differ from the source path".to_string());
    }

    let source_language = language_for_path(&target.path);
    let destination_language = language_for_path(destination_path);
    if source_language != destination_language {
        return Err(format!(
            "move-symbol requires source and destination languages to match (source: {}, destination: {})",
            source_language.unwrap_or("unknown"),
            destination_language.unwrap_or("unknown")
        ));
    }

    let inbound_references = references
        .iter()
        .filter(|reference| reference.target_path == target.path)
        .filter(|reference| reference.target_name == target.name)
        .cloned()
        .collect::<Vec<_>>();
    let inbound_reference_count = inbound_references.len();
    if inbound_reference_count > 0 && !rewrite_references {
        return Err(format!(
            "move-symbol refuses to move '{}' because it has {inbound_reference_count} indexed inbound reference(s); pass --rewrite-references to rewrite supported references",
            target.name
        ));
    }

    let old_line_start = target.line_start;
    let old_line_end = target.line_end.unwrap_or(target.line_start);
    validate_span(old_line_start, old_line_end, &target.path)?;

    reject_destination_symbol_collision(destination_path, destination_contents, target)?;

    let reference_rewrite = if rewrite_references {
        plan_reference_rewrites(
            target,
            destination_path,
            source_language,
            destination_language,
            &inbound_references,
            reference_files,
        )?
    } else {
        PlannedReferenceRewrite::default()
    };

    let moved_body =
        extract_line_range(source_contents, old_line_start, old_line_end, &target.path)?;
    let source_after =
        remove_line_range(source_contents, old_line_start, old_line_end, &target.path)?;
    let destination_after = append_symbol_body(destination_contents, &moved_body);

    if !code::parses_cleanly(&target.path, source_language, &source_after)? {
        return Err(format!(
            "source file {} would not parse after moving '{}'",
            target.path, target.name
        ));
    }
    if !code::parses_cleanly(destination_path, destination_language, &destination_after)? {
        return Err(format!(
            "destination file {destination_path} would not parse after moving '{}'",
            target.name
        ));
    }

    validate_moved_symbol_in_destination(destination_path, &destination_after, target)?;

    let mut files = vec![
        PlannedMoveFile {
            path: target.path.clone(),
            contents: source_after,
        },
        PlannedMoveFile {
            path: destination_path.to_string(),
            contents: destination_after,
        },
    ];
    files.extend(reference_rewrite.files);
    files.sort_by(|left, right| left.path.cmp(&right.path));

    let mut changed_files = vec![
        SymbolMoveFile {
            path: target.path.clone(),
            action: "removed".to_string(),
        },
        SymbolMoveFile {
            path: destination_path.to_string(),
            action: "appended".to_string(),
        },
    ];
    changed_files.extend(reference_rewrite.changed_files);
    changed_files.sort_by(|left, right| left.path.cmp(&right.path));

    Ok(PlannedMove {
        files,
        summary: SymbolMove {
            source_path: target.path.clone(),
            destination_path: destination_path.to_string(),
            language: target.language.clone(),
            name: target.name.clone(),
            kind: target.kind.clone(),
            old_line_start,
            old_line_end,
            moved_line_count: moved_body.lines().count(),
            rewritten_reference_count: reference_rewrite.rewritten_reference_count,
            changed_files,
        },
    })
}

#[derive(Debug, Default)]
struct PlannedReferenceRewrite {
    files: Vec<PlannedMoveFile>,
    changed_files: Vec<SymbolMoveFile>,
    rewritten_reference_count: usize,
}

fn plan_reference_rewrites(
    target: &CodeSymbol,
    destination_path: &str,
    source_language: Option<&str>,
    destination_language: Option<&str>,
    inbound_references: &[CodeReference],
    reference_files: Vec<(String, String)>,
) -> Result<PlannedReferenceRewrite, String> {
    if inbound_references.is_empty() {
        return Ok(PlannedReferenceRewrite::default());
    }
    if source_language != Some("rust") || destination_language != Some("rust") {
        return Err(
            "move-symbol --rewrite-references currently supports Rust source files only"
                .to_string(),
        );
    }
    if inbound_references
        .iter()
        .any(|reference| reference.path == target.path || reference.path == destination_path)
    {
        return Err(
            "move-symbol --rewrite-references does not yet support references from the source or destination file"
                .to_string(),
        );
    }

    let old_module = rust_module_path_for_source(&target.path).ok_or_else(|| {
        format!(
            "move-symbol --rewrite-references cannot derive a Rust module path for {}",
            target.path
        )
    })?;
    let new_module = rust_module_path_for_source(destination_path).ok_or_else(|| {
        format!(
            "move-symbol --rewrite-references cannot derive a Rust module path for {destination_path}"
        )
    })?;
    let old_qualified = format!("{old_module}::{}", target.name);
    let new_qualified = format!("{new_module}::{}", target.name);
    let reference_contents = reference_files.into_iter().collect::<BTreeMap<_, _>>();
    let mut references_by_path = BTreeMap::<String, Vec<CodeReference>>::new();
    for reference in inbound_references {
        references_by_path
            .entry(reference.path.clone())
            .or_default()
            .push(reference.clone());
    }

    let mut files = Vec::new();
    let mut changed_files = Vec::new();
    let mut rewritten_reference_count = 0;
    for (path, references) in references_by_path {
        let Some(contents) = reference_contents.get(&path) else {
            return Err(format!(
                "move-symbol --rewrite-references missing source contents for referenced file {path}; rerun hugr index"
            ));
        };
        let mut line_numbers = BTreeSet::new();
        for reference in &references {
            line_numbers.insert(reference.line_start);
        }
        let (rewritten, replacement_count) = replace_qualified_path_on_lines(
            &path,
            contents,
            &old_qualified,
            &new_qualified,
            &line_numbers,
        )?;
        if replacement_count == 0 {
            return Err(format!(
                "move-symbol --rewrite-references could not rewrite {old_qualified} in indexed references for {path}"
            ));
        }
        if !code::parses_cleanly(&path, language_for_path(&path), &rewritten)? {
            return Err(format!(
                "referencing file {path} would not parse after moving '{}'",
                target.name
            ));
        }
        rewritten_reference_count += replacement_count;
        files.push(PlannedMoveFile {
            path: path.clone(),
            contents: rewritten,
        });
        changed_files.push(SymbolMoveFile {
            path,
            action: "rewrote references".to_string(),
        });
    }

    Ok(PlannedReferenceRewrite {
        files,
        changed_files,
        rewritten_reference_count,
    })
}

fn rust_module_path_for_source(path: &str) -> Option<String> {
    let without_prefix = path.strip_prefix("src/")?;
    let without_extension = without_prefix.strip_suffix(".rs")?;
    let module = match without_extension {
        "lib" | "main" => "crate".to_string(),
        value if value.ends_with("/mod") => {
            let value = value.trim_end_matches("/mod");
            format!("crate::{}", value.replace('/', "::"))
        }
        value => format!("crate::{}", value.replace('/', "::")),
    };
    Some(module)
}

fn replace_qualified_path_on_lines(
    path: &str,
    contents: &str,
    old_qualified: &str,
    new_qualified: &str,
    line_numbers: &BTreeSet<i64>,
) -> Result<(String, usize), String> {
    let trailing_newline = contents.ends_with('\n');
    let mut lines = contents.lines().map(str::to_string).collect::<Vec<_>>();
    let mut replacement_count = 0;

    for line_number in line_numbers {
        if *line_number < 1 {
            return Err(format!(
                "move-symbol reference line {line_number} in {path} is invalid"
            ));
        }
        let index = usize::try_from(line_number - 1).map_err(|error| error.to_string())?;
        let Some(line) = lines.get_mut(index) else {
            return Err(format!(
                "move-symbol reference line {line_number} is past end of {path}"
            ));
        };
        let count = line.matches(old_qualified).count();
        if count > 0 {
            *line = line.replace(old_qualified, new_qualified);
            replacement_count += count;
        }
    }

    let mut rendered = lines.join("\n");
    if trailing_newline {
        rendered.push('\n');
    }
    Ok((rendered, replacement_count))
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

fn no_match_error(symbols: &[CodeSymbol], name: &str, kind: Option<&str>, action: &str) -> String {
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

fn extract_line_range(
    contents: &str,
    line_start: i64,
    line_end: i64,
    path: &str,
) -> Result<String, String> {
    let start = usize::try_from(line_start - 1).map_err(|error| error.to_string())?;
    let end = usize::try_from(line_end - 1).map_err(|error| error.to_string())?;
    let lines = contents.lines().collect::<Vec<_>>();
    if end >= lines.len() {
        return Err(format!(
            "symbol span ends at line {line_end} but {path} has {} lines",
            lines.len()
        ));
    }
    Ok(lines[start..=end].join("\n"))
}

fn remove_line_range(
    contents: &str,
    line_start: i64,
    line_end: i64,
    path: &str,
) -> Result<String, String> {
    let start = usize::try_from(line_start - 1).map_err(|error| error.to_string())?;
    let end = usize::try_from(line_end - 1).map_err(|error| error.to_string())?;
    let trailing_newline = contents.ends_with('\n');
    let lines = contents.lines().collect::<Vec<_>>();
    if end >= lines.len() {
        return Err(format!(
            "symbol span ends at line {line_end} but {path} has {} lines",
            lines.len()
        ));
    }

    let mut result = Vec::with_capacity(lines.len().saturating_sub(end - start + 1));
    result.extend_from_slice(&lines[..start]);
    result.extend_from_slice(&lines[end + 1..]);

    let mut rendered = result.join("\n");
    if trailing_newline && !rendered.is_empty() {
        rendered.push('\n');
    }
    Ok(rendered)
}

fn append_symbol_body(destination_contents: &str, moved_body: &str) -> String {
    let moved_body = moved_body.trim_end_matches(['\n', '\r']);
    if destination_contents.trim().is_empty() {
        return format!("{moved_body}\n");
    }

    let mut rendered = destination_contents.trim_end_matches('\n').to_string();
    rendered.push_str("\n\n");
    rendered.push_str(moved_body);
    rendered.push('\n');
    rendered
}

fn valid_identifier(value: &str) -> bool {
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first.is_ascii_alphabetic() || first == '_')
        && chars.all(|char| char.is_ascii_alphanumeric() || char == '_')
}

fn reject_destination_symbol_collision(
    destination_path: &str,
    destination_contents: &str,
    target: &CodeSymbol,
) -> Result<(), String> {
    let language = language_for_path(destination_path);
    let symbols = code::symbols_in_source(destination_path, language, destination_contents)?;
    if symbols
        .iter()
        .any(|symbol| symbol.name == target.name && symbol.kind == target.kind)
    {
        return Err(format!(
            "move-symbol would collide with existing {} '{}' in {}",
            target.kind, target.name, destination_path
        ));
    }
    Ok(())
}

fn reject_target_name_collision(
    target: &CodeSymbol,
    contents: &str,
    new_name: &str,
) -> Result<(), String> {
    let language = language_for_path(&target.path);
    let symbols = code::symbols_in_source(&target.path, language, contents)?;
    if symbols.iter().any(|symbol| {
        symbol.name == new_name
            && symbol.kind == target.kind
            && !(symbol.path == target.path
                && symbol.line_start == target.line_start
                && symbol.kind == target.kind)
    }) {
        return Err(format!(
            "rename-symbol would collide with existing {} '{}' in {}",
            target.kind, new_name, target.path
        ));
    }
    Ok(())
}

fn validate_moved_symbol_in_destination(
    destination_path: &str,
    destination_contents: &str,
    target: &CodeSymbol,
) -> Result<(), String> {
    let language = language_for_path(destination_path);
    let symbols = code::symbols_in_source(destination_path, language, destination_contents)?;
    let matches = symbols
        .iter()
        .filter(|symbol| symbol.name == target.name && symbol.kind == target.kind)
        .count();
    if matches != 1 {
        return Err(format!(
            "move-symbol could not verify moved {} '{}' in {}",
            target.kind, target.name, destination_path
        ));
    }
    Ok(())
}

fn replace_identifier_on_lines(
    path: &str,
    contents: &str,
    old_name: &str,
    new_name: &str,
    line_numbers: &BTreeSet<i64>,
) -> Result<(String, usize), String> {
    let trailing_newline = contents.ends_with('\n');
    let mut lines = contents.lines().map(str::to_string).collect::<Vec<_>>();
    let mut replacement_count = 0;

    for line_number in line_numbers {
        if *line_number < 1 {
            return Err(format!(
                "rename-symbol reference line {line_number} in {path} is invalid"
            ));
        }
        let index = usize::try_from(line_number - 1).map_err(|error| error.to_string())?;
        let Some(line) = lines.get_mut(index) else {
            return Err(format!(
                "rename-symbol reference line {line_number} is past end of {path}"
            ));
        };
        let (renamed, line_replacements) = replace_identifier_in_line(line, old_name, new_name);
        if line_replacements == 0 {
            return Err(format!(
                "rename-symbol found no '{}' identifier on {path}:{line_number}; rerun hugr index",
                old_name
            ));
        }
        *line = renamed;
        replacement_count += line_replacements;
    }

    let mut rendered = lines.join("\n");
    if trailing_newline {
        rendered.push('\n');
    }
    Ok((rendered, replacement_count))
}

fn replace_identifier_in_line(line: &str, old_name: &str, new_name: &str) -> (String, usize) {
    let mut rendered = String::new();
    let mut cursor = 0;
    let mut replacements = 0;

    for (start, matched) in line.match_indices(old_name) {
        let end = start + matched.len();
        if !identifier_boundary(line[..start].chars().next_back())
            || !identifier_boundary(line[end..].chars().next())
        {
            continue;
        }
        rendered.push_str(&line[cursor..start]);
        rendered.push_str(new_name);
        cursor = end;
        replacements += 1;
    }

    if replacements == 0 {
        return (line.to_string(), 0);
    }
    rendered.push_str(&line[cursor..]);
    (rendered, replacements)
}

fn identifier_boundary(char: Option<char>) -> bool {
    char.is_none_or(|char| !(char.is_alphanumeric() || char == '_'))
}

fn validate_renamed_target(
    target: &CodeSymbol,
    new_name: &str,
    planned_files: &[PlannedRenameFile],
) -> Result<(), String> {
    let target_file = planned_files
        .iter()
        .find(|file| file.path == target.path)
        .ok_or_else(|| {
            format!(
                "rename-symbol did not produce rewritten target file {}",
                target.path
            )
        })?;
    let language = language_for_path(&target.path);
    let symbols = code::symbols_in_source(&target.path, language, &target_file.contents)?;
    let renamed = symbols.iter().filter(|symbol| {
        symbol.name == new_name
            && symbol.kind == target.kind
            && symbol.line_start == target.line_start
    });
    if renamed.count() != 1 {
        return Err(format!(
            "rename-symbol could not verify renamed {} '{}' at {}:{}",
            target.kind, new_name, target.path, target.line_start
        ));
    }
    if symbols.iter().any(|symbol| {
        symbol.name == target.name
            && symbol.kind == target.kind
            && symbol.line_start == target.line_start
    }) {
        return Err(format!(
            "rename-symbol left old {} '{}' at {}:{}",
            target.kind, target.name, target.path, target.line_start
        ));
    }
    Ok(())
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

impl SymbolRename {
    pub(crate) fn render_markdown(&self) -> String {
        let mut rendered = String::new();
        rendered.push_str("# Hugr Symbol Rename\n\n");
        let _ = writeln!(
            rendered,
            "## Symbol\n{} {} -> {}",
            self.kind, self.old_name, self.new_name
        );
        let _ = writeln!(
            rendered,
            "\n## Location\n{}:{}-{}",
            self.target_path, self.line_start, self.line_end
        );
        let _ = writeln!(
            rendered,
            "\n## Language\n{}",
            self.language.as_deref().unwrap_or("unknown")
        );
        let _ = writeln!(
            rendered,
            "\n## References\n{} indexed reference(s)",
            self.reference_count
        );
        rendered.push_str("\n## Changed Files\n");
        for file in &self.changed_files {
            let _ = writeln!(
                rendered,
                "- {}: {} replacement(s)",
                file.path, file.replacement_count
            );
        }
        rendered
    }

    pub(crate) fn render_json(&self) -> String {
        let changed_files = self
            .changed_files
            .iter()
            .map(|file| {
                format!(
                    "{{\"path\":{},\"replacement_count\":{}}}",
                    json_string(&file.path),
                    file.replacement_count
                )
            })
            .collect::<Vec<_>>()
            .join(",");
        format!(
            "{{\"target_path\":{},\"language\":{},\"old_name\":{},\"new_name\":{},\
             \"kind\":{},\"line_start\":{},\"line_end\":{},\"reference_count\":{},\
             \"changed_files\":[{}]}}",
            json_string(&self.target_path),
            self.language
                .as_deref()
                .map(json_string)
                .unwrap_or_else(|| "null".to_string()),
            json_string(&self.old_name),
            json_string(&self.new_name),
            json_string(&self.kind),
            self.line_start,
            self.line_end,
            self.reference_count,
            changed_files
        )
    }
}

impl SymbolMove {
    pub(crate) fn render_markdown(&self) -> String {
        let mut rendered = String::new();
        rendered.push_str("# Hugr Symbol Move\n\n");
        let _ = writeln!(rendered, "## Symbol\n{} {}", self.kind, self.name);
        let _ = writeln!(
            rendered,
            "\n## Move\n{}:{}-{} -> {}",
            self.source_path, self.old_line_start, self.old_line_end, self.destination_path
        );
        let _ = writeln!(
            rendered,
            "\n## Language\n{}",
            self.language.as_deref().unwrap_or("unknown")
        );
        let _ = writeln!(rendered, "\n## Lines\n{}", self.moved_line_count);
        let _ = writeln!(
            rendered,
            "\n## Rewritten References\n{}",
            self.rewritten_reference_count
        );
        rendered.push_str("\n## Changed Files\n");
        for file in &self.changed_files {
            let _ = writeln!(rendered, "- {}: {}", file.path, file.action);
        }
        rendered
    }

    pub(crate) fn render_json(&self) -> String {
        let changed_files = self
            .changed_files
            .iter()
            .map(|file| {
                format!(
                    "{{\"path\":{},\"action\":{}}}",
                    json_string(&file.path),
                    json_string(&file.action)
                )
            })
            .collect::<Vec<_>>()
            .join(",");
        format!(
            "{{\"source_path\":{},\"destination_path\":{},\"language\":{},\"name\":{},\
             \"kind\":{},\"old_line_start\":{},\"old_line_end\":{},\
             \"moved_line_count\":{},\"rewritten_reference_count\":{},\
             \"changed_files\":[{}]}}",
            json_string(&self.source_path),
            json_string(&self.destination_path),
            self.language
                .as_deref()
                .map(json_string)
                .unwrap_or_else(|| "null".to_string()),
            json_string(&self.name),
            json_string(&self.kind),
            self.old_line_start,
            self.old_line_end,
            self.moved_line_count,
            self.rewritten_reference_count,
            changed_files
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{plan_move, plan_rename, plan_replacement, resolve_symbol_in_source};
    use crate::code::CodeReference;

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

    #[test]
    fn plans_reference_aware_symbol_rename() {
        let target_source = "pub fn run_after_config() -> u8 {\n    1\n}\n";
        let caller_source = "use crate::plugin_hooks::run_after_config;\n\nfn main() {\n    run_after_config();\n}\n";
        let target = resolve_symbol_in_source(
            "src/plugin_hooks.rs",
            target_source,
            "run_after_config",
            None,
            "rename",
        )
        .unwrap();
        let references = vec![
            CodeReference {
                path: "src/main.rs".to_string(),
                language: Some("rust".to_string()),
                target_path: "src/plugin_hooks.rs".to_string(),
                target_name: "run_after_config".to_string(),
                target_kind: "function".to_string(),
                kind: "import".to_string(),
                line_start: 1,
                excerpt: "use crate::plugin_hooks::run_after_config;".to_string(),
            },
            CodeReference {
                path: "src/main.rs".to_string(),
                language: Some("rust".to_string()),
                target_path: "src/plugin_hooks.rs".to_string(),
                target_name: "run_after_config".to_string(),
                target_kind: "function".to_string(),
                kind: "call".to_string(),
                line_start: 4,
                excerpt: "run_after_config();".to_string(),
            },
        ];

        let planned = plan_rename(
            &target,
            &references,
            vec![
                ("src/plugin_hooks.rs".to_string(), target_source.to_string()),
                ("src/main.rs".to_string(), caller_source.to_string()),
            ],
            "run_before_config",
        )
        .unwrap();

        let target_file = planned
            .files
            .iter()
            .find(|file| file.path == "src/plugin_hooks.rs")
            .unwrap();
        let caller_file = planned
            .files
            .iter()
            .find(|file| file.path == "src/main.rs")
            .unwrap();

        assert!(target_file.contents.contains("run_before_config"));
        assert!(!target_file.contents.contains("run_after_config"));
        assert!(caller_file.contents.contains("run_before_config();"));
        assert!(!caller_file.contents.contains("run_after_config"));
        assert_eq!(planned.summary.old_name, "run_after_config");
        assert_eq!(planned.summary.new_name, "run_before_config");
        assert_eq!(planned.summary.reference_count, 2);
        assert_eq!(planned.summary.changed_files.len(), 2);
        assert!(
            planned
                .summary
                .render_markdown()
                .contains("run_after_config -> run_before_config")
        );
        assert!(
            planned
                .summary
                .render_json()
                .contains("\"new_name\":\"run_before_config\"")
        );
    }

    #[test]
    fn rejects_rename_with_invalid_identifier() {
        let target =
            resolve_symbol_in_source("src/lib.rs", RUST_SOURCE, "greet", None, "rename").unwrap();
        let error = plan_rename(
            &target,
            &[],
            vec![("src/lib.rs".to_string(), RUST_SOURCE.to_string())],
            "not-valid",
        )
        .unwrap_err();

        assert!(error.contains("valid ASCII identifier"), "{error}");
    }

    #[test]
    fn rejects_rename_when_reference_line_is_stale() {
        let target =
            resolve_symbol_in_source("src/lib.rs", RUST_SOURCE, "greet", None, "rename").unwrap();
        let references = vec![CodeReference {
            path: "src/main.rs".to_string(),
            language: Some("rust".to_string()),
            target_path: "src/lib.rs".to_string(),
            target_name: "greet".to_string(),
            target_kind: "function".to_string(),
            kind: "call".to_string(),
            line_start: 1,
            excerpt: "greet();".to_string(),
        }];
        let error = plan_rename(
            &target,
            &references,
            vec![
                ("src/lib.rs".to_string(), RUST_SOURCE.to_string()),
                (
                    "src/main.rs".to_string(),
                    "fn main() { other(); }\n".to_string(),
                ),
            ],
            "welcome",
        )
        .unwrap_err();

        assert!(error.contains("rerun hugr index"), "{error}");
    }

    #[test]
    fn plans_unreferenced_symbol_move_between_files() {
        let source = "pub fn helper() -> u8 {\n    1\n}\n\npub fn other() {}\n";
        let destination = "pub fn existing() {}\n";
        let target =
            resolve_symbol_in_source("src/lib.rs", source, "helper", None, "move").unwrap();

        let planned = plan_move(
            &target,
            &[],
            source,
            "src/helpers.rs",
            destination,
            Vec::new(),
            false,
        )
        .unwrap();
        let source_file = planned
            .files
            .iter()
            .find(|file| file.path == "src/lib.rs")
            .unwrap();
        let destination_file = planned
            .files
            .iter()
            .find(|file| file.path == "src/helpers.rs")
            .unwrap();

        assert!(!source_file.contents.contains("helper"));
        assert!(source_file.contents.contains("pub fn other() {}"));
        assert!(destination_file.contents.contains("pub fn existing() {}"));
        assert!(destination_file.contents.contains("pub fn helper() -> u8"));
        assert_eq!(planned.summary.source_path, "src/lib.rs");
        assert_eq!(planned.summary.destination_path, "src/helpers.rs");
        assert_eq!(planned.summary.moved_line_count, 3);
        assert!(
            planned
                .summary
                .render_markdown()
                .contains("src/lib.rs:1-3 -> src/helpers.rs")
        );
        assert!(
            planned
                .summary
                .render_json()
                .contains("\"destination_path\":\"src/helpers.rs\"")
        );
    }

    #[test]
    fn rejects_move_when_inbound_references_exist() {
        let target =
            resolve_symbol_in_source("src/lib.rs", RUST_SOURCE, "greet", None, "move").unwrap();
        let references = vec![CodeReference {
            path: "src/main.rs".to_string(),
            language: Some("rust".to_string()),
            target_path: "src/lib.rs".to_string(),
            target_name: "greet".to_string(),
            target_kind: "function".to_string(),
            kind: "call".to_string(),
            line_start: 1,
            excerpt: "greet();".to_string(),
        }];

        let error = plan_move(
            &target,
            &references,
            RUST_SOURCE,
            "src/helpers.rs",
            "pub fn existing() {}\n",
            Vec::new(),
            false,
        )
        .unwrap_err();

        assert!(error.contains("indexed inbound reference"), "{error}");
    }

    #[test]
    fn rejects_move_when_destination_has_same_symbol() {
        let target =
            resolve_symbol_in_source("src/lib.rs", RUST_SOURCE, "greet", None, "move").unwrap();
        let error = plan_move(
            &target,
            &[],
            RUST_SOURCE,
            "src/helpers.rs",
            "pub fn greet() {}\n",
            Vec::new(),
            false,
        )
        .unwrap_err();

        assert!(error.contains("would collide"), "{error}");
    }

    #[test]
    fn plans_move_with_rust_reference_rewrites() {
        let source = "pub fn helper() -> u8 {\n    1\n}\n\npub fn other() {}\n";
        let destination = "pub fn existing() {}\n";
        let caller = "use crate::lib::helper;\n\nfn main() {\n    let _ = helper();\n}\n";
        let target =
            resolve_symbol_in_source("src/lib.rs", source, "helper", None, "move").unwrap();
        let references = vec![
            CodeReference {
                path: "src/main.rs".to_string(),
                language: Some("rust".to_string()),
                target_path: "src/lib.rs".to_string(),
                target_name: "helper".to_string(),
                target_kind: "function".to_string(),
                kind: "import".to_string(),
                line_start: 1,
                excerpt: "use crate::lib::helper;".to_string(),
            },
            CodeReference {
                path: "src/main.rs".to_string(),
                language: Some("rust".to_string()),
                target_path: "src/lib.rs".to_string(),
                target_name: "helper".to_string(),
                target_kind: "function".to_string(),
                kind: "call".to_string(),
                line_start: 4,
                excerpt: "helper();".to_string(),
            },
        ];

        let planned = plan_move(
            &target,
            &references,
            source,
            "src/helpers.rs",
            destination,
            vec![("src/main.rs".to_string(), caller.to_string())],
            true,
        )
        .unwrap();
        let caller_file = planned
            .files
            .iter()
            .find(|file| file.path == "src/main.rs")
            .unwrap();

        assert!(caller_file.contents.contains("use crate::helpers::helper;"));
        assert!(caller_file.contents.contains("helper();"));
        assert_eq!(planned.summary.rewritten_reference_count, 1);
        assert!(
            planned
                .summary
                .render_json()
                .contains("\"rewritten_reference_count\":1")
        );
    }
}
