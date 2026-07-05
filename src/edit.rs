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
            source_contents,
            destination_contents,
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
    source_contents: &str,
    destination_contents: &str,
    source_language: Option<&str>,
    destination_language: Option<&str>,
    inbound_references: &[CodeReference],
    reference_files: Vec<(String, String)>,
) -> Result<PlannedReferenceRewrite, String> {
    if inbound_references.is_empty() {
        return Ok(PlannedReferenceRewrite::default());
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

    let reference_contents = reference_files.into_iter().collect::<BTreeMap<_, _>>();
    let mut references_by_path = BTreeMap::<String, Vec<CodeReference>>::new();
    for reference in inbound_references {
        references_by_path
            .entry(reference.path.clone())
            .or_default()
            .push(reference.clone());
    }

    if source_language == Some("rust") && destination_language == Some("rust") {
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
        return rewrite_reference_files(
            target,
            references_by_path,
            reference_contents,
            &old_qualified,
            |path, contents, line_numbers| {
                rewrite_rust_references_on_lines(
                    path,
                    contents,
                    &old_module,
                    &new_module,
                    &target.name,
                    line_numbers,
                )
            },
        );
    }

    if source_language == Some("python") && destination_language == Some("python") {
        let old_module = python_module_path_for_source(&target.path).ok_or_else(|| {
            format!(
                "move-symbol --rewrite-references cannot derive a Python module path for {}",
                target.path
            )
        })?;
        let new_module = python_module_path_for_source(destination_path).ok_or_else(|| {
            format!(
                "move-symbol --rewrite-references cannot derive a Python module path for {destination_path}"
            )
        })?;
        let old_qualified = format!("{old_module}.{}", target.name);
        return rewrite_reference_files(
            target,
            references_by_path,
            reference_contents,
            &old_qualified,
            |path, contents, line_numbers| {
                rewrite_python_references_on_lines(
                    path,
                    contents,
                    &old_module,
                    &new_module,
                    &target.name,
                    line_numbers,
                )
            },
        );
    }

    if matches!(source_language, Some("typescript" | "javascript"))
        && source_language == destination_language
    {
        let old_reference = format!("{}#{}", target.path, target.name);
        return rewrite_reference_files(
            target,
            references_by_path,
            reference_contents,
            &old_reference,
            |path, contents, line_numbers| {
                rewrite_javascript_references_on_lines(
                    path,
                    contents,
                    &target.path,
                    destination_path,
                    &target.name,
                    line_numbers,
                )
            },
        );
    }

    if source_language == Some("go") && destination_language == Some("go") {
        return plan_go_same_package_reference_awareness(
            target,
            destination_path,
            source_contents,
            destination_contents,
            references_by_path,
            reference_contents,
        );
    }

    if source_language == Some("java") && destination_language == Some("java") {
        return plan_java_same_package_type_reference_awareness(
            target,
            destination_path,
            source_contents,
            destination_contents,
            references_by_path,
            reference_contents,
        );
    }

    if source_language == Some("kotlin") && destination_language == Some("kotlin") {
        return plan_kotlin_same_package_reference_awareness(
            target,
            source_contents,
            destination_contents,
            references_by_path,
            reference_contents,
        );
    }

    if source_language == Some("swift") && destination_language == Some("swift") {
        return plan_swift_same_module_reference_awareness(
            target,
            destination_path,
            references_by_path,
            reference_contents,
        );
    }

    Err(
        "move-symbol --rewrite-references currently supports Rust, Python, TypeScript, JavaScript, same-package Go, same-package Java type, same-package Kotlin, and same-module Swift source files only"
            .to_string(),
    )
}

fn plan_go_same_package_reference_awareness(
    target: &CodeSymbol,
    destination_path: &str,
    source_contents: &str,
    destination_contents: &str,
    references_by_path: BTreeMap<String, Vec<CodeReference>>,
    reference_contents: BTreeMap<String, String>,
) -> Result<PlannedReferenceRewrite, String> {
    let source_parent = path_parent_segments(&target.path);
    let destination_parent = path_parent_segments(destination_path);
    if source_parent != destination_parent {
        return Err(
            "move-symbol --rewrite-references currently supports Go moves only within the same package directory"
                .to_string(),
        );
    }
    let source_package = go_package_name(source_contents).ok_or_else(|| {
        format!(
            "move-symbol --rewrite-references cannot determine Go package for {}",
            target.path
        )
    })?;
    let destination_package = go_package_name(destination_contents).ok_or_else(|| {
        format!(
            "move-symbol --rewrite-references cannot determine Go package for {destination_path}"
        )
    })?;
    if source_package != destination_package {
        return Err(format!(
            "move-symbol --rewrite-references requires Go source and destination files to share package '{source_package}'"
        ));
    }

    for (path, references) in references_by_path {
        if path_parent_segments(&path) != source_parent {
            return Err(format!(
                "move-symbol --rewrite-references cannot keep Go reference {path} valid across package directories"
            ));
        }
        let Some(contents) = reference_contents.get(&path) else {
            return Err(format!(
                "move-symbol --rewrite-references missing source contents for referenced file {path}; rerun hugr index"
            ));
        };
        if !code::parses_cleanly(&path, language_for_path(&path), contents)? {
            return Err(format!(
                "referencing file {path} is not valid Go code; refusing to trust stale references"
            ));
        }
        let reference_package = go_package_name(contents).ok_or_else(|| {
            format!("move-symbol --rewrite-references cannot determine Go package for {path}")
        })?;
        if reference_package != source_package {
            return Err(format!(
                "move-symbol --rewrite-references requires Go reference file {path} to share package '{source_package}'"
            ));
        }
        for reference in references {
            let line = line_at(contents, reference.line_start).ok_or_else(|| {
                format!(
                    "move-symbol reference line {} is past end of {path}",
                    reference.line_start
                )
            })?;
            let (_line, matches) = replace_identifier_in_line(line, &target.name, &target.name);
            if matches == 0 {
                return Err(format!(
                    "move-symbol --rewrite-references could not verify Go reference '{}' on {path}:{}; rerun hugr index",
                    target.name, reference.line_start
                ));
            }
        }
    }

    Ok(PlannedReferenceRewrite::default())
}

fn go_package_name(contents: &str) -> Option<String> {
    for line in contents.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with("//") {
            continue;
        }
        let rest = trimmed.strip_prefix("package ")?;
        let name = rest.split_whitespace().next()?;
        return valid_identifier(name).then(|| name.to_string());
    }
    None
}

fn plan_java_same_package_type_reference_awareness(
    target: &CodeSymbol,
    destination_path: &str,
    source_contents: &str,
    destination_contents: &str,
    references_by_path: BTreeMap<String, Vec<CodeReference>>,
    reference_contents: BTreeMap<String, String>,
) -> Result<PlannedReferenceRewrite, String> {
    if !java_reference_safe_kind(&target.kind) {
        return Err(format!(
            "move-symbol --rewrite-references supports Java moves only for type declarations, not {} '{}'",
            target.kind, target.name
        ));
    }
    if java_signature_is_public(&target.signature)
        && path_file_stem(destination_path).as_deref() != Some(target.name.as_str())
    {
        return Err(format!(
            "move-symbol --rewrite-references requires public Java type '{}' to move into {}.java",
            target.name, target.name
        ));
    }

    let source_package = java_package_name(source_contents).ok_or_else(|| {
        format!(
            "move-symbol --rewrite-references cannot determine Java package for {}",
            target.path
        )
    })?;
    let destination_package = java_package_name(destination_contents).ok_or_else(|| {
        format!(
            "move-symbol --rewrite-references cannot determine Java package for {destination_path}"
        )
    })?;
    if source_package != destination_package {
        return Err(format!(
            "move-symbol --rewrite-references requires Java source and destination files to share package '{source_package}'"
        ));
    }

    for (path, references) in references_by_path {
        let Some(contents) = reference_contents.get(&path) else {
            return Err(format!(
                "move-symbol --rewrite-references missing source contents for referenced file {path}; rerun hugr index"
            ));
        };
        if !code::parses_cleanly(&path, language_for_path(&path), contents)? {
            return Err(format!(
                "referencing file {path} is not valid Java code; refusing to trust stale references"
            ));
        }
        let reference_package = java_package_name(contents).ok_or_else(|| {
            format!("move-symbol --rewrite-references cannot determine Java package for {path}")
        })?;
        if reference_package != source_package {
            return Err(format!(
                "move-symbol --rewrite-references requires Java reference file {path} to share package '{source_package}'"
            ));
        }
        for reference in references {
            let line = line_at(contents, reference.line_start).ok_or_else(|| {
                format!(
                    "move-symbol reference line {} is past end of {path}",
                    reference.line_start
                )
            })?;
            let (_line, matches) = replace_identifier_in_line(line, &target.name, &target.name);
            if matches == 0 {
                return Err(format!(
                    "move-symbol --rewrite-references could not verify Java reference '{}' on {path}:{}; rerun hugr index",
                    target.name, reference.line_start
                ));
            }
        }
    }

    Ok(PlannedReferenceRewrite::default())
}

fn java_reference_safe_kind(kind: &str) -> bool {
    matches!(
        kind,
        "annotation" | "class" | "enum" | "interface" | "record"
    )
}

fn java_signature_is_public(signature: &str) -> bool {
    signature.trim_start().starts_with("public ")
}

fn java_package_name(contents: &str) -> Option<String> {
    for line in contents.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with("//") {
            continue;
        }
        let rest = trimmed.strip_prefix("package ")?;
        let package = rest.trim_end_matches(';').trim();
        if package.is_empty()
            || !package
                .split('.')
                .all(|segment| valid_identifier(segment.trim()))
        {
            return None;
        }
        return Some(package.to_string());
    }
    None
}

fn plan_kotlin_same_package_reference_awareness(
    target: &CodeSymbol,
    source_contents: &str,
    destination_contents: &str,
    references_by_path: BTreeMap<String, Vec<CodeReference>>,
    reference_contents: BTreeMap<String, String>,
) -> Result<PlannedReferenceRewrite, String> {
    if !kotlin_reference_safe_kind(&target.kind) {
        return Err(format!(
            "move-symbol --rewrite-references supports Kotlin moves only for top-level type declarations, not {} '{}'",
            target.kind, target.name
        ));
    }

    let source_package = kotlin_package_name(source_contents);
    let destination_package = kotlin_package_name(destination_contents);
    if source_package != destination_package {
        return plan_kotlin_cross_package_reference_rewrites(
            target,
            source_package.as_deref(),
            destination_package.as_deref(),
            references_by_path,
            reference_contents,
        );
    }

    for (path, references) in references_by_path {
        let Some(contents) = reference_contents.get(&path) else {
            return Err(format!(
                "move-symbol --rewrite-references missing source contents for referenced file {path}; rerun hugr index"
            ));
        };
        if !code::parses_cleanly(&path, language_for_path(&path), contents)? {
            return Err(format!(
                "referencing file {path} is not valid Kotlin code; refusing to trust stale references"
            ));
        }
        let reference_package = kotlin_package_name(contents);
        if reference_package != source_package {
            return Err(format!(
                "move-symbol --rewrite-references requires Kotlin reference file {path} to share package '{}'",
                source_package.as_deref().unwrap_or("<root>")
            ));
        }
        for reference in references {
            let line = line_at(contents, reference.line_start).ok_or_else(|| {
                format!(
                    "move-symbol reference line {} is past end of {path}",
                    reference.line_start
                )
            })?;
            let (_line, matches) = replace_identifier_in_line(line, &target.name, &target.name);
            if matches == 0 {
                return Err(format!(
                    "move-symbol --rewrite-references could not verify Kotlin reference '{}' on {path}:{}; rerun hugr index",
                    target.name, reference.line_start
                ));
            }
        }
    }

    Ok(PlannedReferenceRewrite::default())
}

/// Rewrites Kotlin `import` statements when a top-level declaration moves to a
/// different package. Each referencing file needs its qualified import updated:
/// a file in the source package gains a fresh `import <new_pkg>.<Symbol>`, a
/// file in another package that imported `<old_pkg>.<Symbol>` has that line
/// rewritten, and a file already in the destination package drops the now-
/// redundant import. Conservatively refuses cases it cannot rewrite safely
/// (wildcard imports, aliased imports, or a symbol referenced without a
/// resolvable import in a foreign package).
fn plan_kotlin_cross_package_reference_rewrites(
    target: &CodeSymbol,
    source_package: Option<&str>,
    destination_package: Option<&str>,
    references_by_path: BTreeMap<String, Vec<CodeReference>>,
    reference_contents: BTreeMap<String, String>,
) -> Result<PlannedReferenceRewrite, String> {
    let Some(destination_package) = destination_package else {
        return Err(
            "move-symbol --rewrite-references cannot move a Kotlin type into the root package with inbound references"
                .to_string(),
        );
    };
    let old_import = source_package.map(|pkg| format!("{pkg}.{}", target.name));
    let new_import = format!("{destination_package}.{}", target.name);

    let mut files = Vec::new();
    let mut changed_files = Vec::new();
    let mut rewritten_reference_count = 0;

    for (path, references) in references_by_path {
        let Some(contents) = reference_contents.get(&path) else {
            return Err(format!(
                "move-symbol --rewrite-references missing source contents for referenced file {path}; rerun hugr index"
            ));
        };
        if !code::parses_cleanly(&path, language_for_path(&path), contents)? {
            return Err(format!(
                "referencing file {path} is not valid Kotlin code; refusing to trust stale references"
            ));
        }
        if kotlin_has_wildcard_or_aliased_import(contents, &target.name) {
            return Err(format!(
                "move-symbol --rewrite-references cannot safely rewrite wildcard or aliased Kotlin import for '{}' in {path}",
                target.name
            ));
        }

        // Verify each indexed reference line still names the symbol.
        for reference in &references {
            let line = line_at(contents, reference.line_start).ok_or_else(|| {
                format!(
                    "move-symbol reference line {} is past end of {path}",
                    reference.line_start
                )
            })?;
            let (_line, matches) = replace_identifier_in_line(line, &target.name, &target.name);
            if matches == 0 {
                return Err(format!(
                    "move-symbol --rewrite-references could not verify Kotlin reference '{}' on {path}:{}; rerun hugr index",
                    target.name, reference.line_start
                ));
            }
        }

        let reference_package = kotlin_package_name(contents);
        let (rewritten, changes) = kotlin_rewrite_import_for_move(
            contents,
            old_import.as_deref(),
            &new_import,
            reference_package.as_deref() == Some(destination_package),
        )
        .ok_or_else(|| {
            format!(
                "move-symbol --rewrite-references could not update the Kotlin import for '{}' in {path}",
                target.name
            )
        })?;
        if changes == 0 {
            return Err(format!(
                "move-symbol --rewrite-references made no Kotlin import change in {path}"
            ));
        }
        if !code::parses_cleanly(&path, language_for_path(&path), &rewritten)? {
            return Err(format!(
                "referencing file {path} would not parse after moving '{}'",
                target.name
            ));
        }
        rewritten_reference_count += references.len();
        files.push(PlannedMoveFile {
            path: path.clone(),
            contents: rewritten,
        });
        changed_files.push(SymbolMoveFile {
            path,
            action: "rewrote imports".to_string(),
        });
    }

    Ok(PlannedReferenceRewrite {
        files,
        changed_files,
        rewritten_reference_count,
    })
}

/// Returns true if the file imports `symbol` via a wildcard (`import pkg.*`) or
/// an alias (`import pkg.Symbol as Other`), which the conservative rewriter
/// declines to touch.
fn kotlin_has_wildcard_or_aliased_import(contents: &str, symbol: &str) -> bool {
    for line in contents.lines() {
        let trimmed = line.trim();
        let Some(rest) = trimmed.strip_prefix("import ") else {
            continue;
        };
        let rest = rest.trim_end_matches(';').trim();
        if rest.ends_with(".*") {
            return true;
        }
        if let Some((path, _alias)) = rest.split_once(" as ") {
            if path.trim().rsplit('.').next() == Some(symbol) {
                return true;
            }
        }
    }
    false
}

/// Produces the rewritten file contents and the number of import changes for a
/// cross-package Kotlin move. When the file previously imported the old path it
/// is replaced; when the file now shares the destination package the redundant
/// import is dropped; otherwise a fresh import is inserted after the package
/// declaration (or existing imports). Returns None if it cannot place the import.
fn kotlin_rewrite_import_for_move(
    contents: &str,
    old_import: Option<&str>,
    new_import: &str,
    reference_in_destination_package: bool,
) -> Option<(String, usize)> {
    let mut lines = contents.lines().map(str::to_string).collect::<Vec<_>>();

    // Locate an existing `import <old_import>` line, if any.
    let existing = old_import.and_then(|old| {
        lines.iter().position(|line| {
            let trimmed = line.trim();
            trimmed
                .strip_prefix("import ")
                .map(|rest| rest.trim_end_matches(';').trim() == old)
                .unwrap_or(false)
        })
    });

    if reference_in_destination_package {
        // Same package as the destination now: drop the old import if present.
        if let Some(index) = existing {
            lines.remove(index);
            return Some((join_lines(&lines, contents), 1));
        }
        // No import to remove and none needed; nothing to change here means the
        // reference already resolves, so report zero (caller treats as error).
        return Some((contents.to_string(), 0));
    }

    if let Some(index) = existing {
        // Rewrite the existing import path to the new package.
        lines[index] = format!("import {new_import}");
        return Some((join_lines(&lines, contents), 1));
    }

    // Insert a fresh import after the last existing import, else after the
    // package declaration, else at the top.
    let insert_at = kotlin_import_insertion_index(&lines);
    lines.insert(insert_at, format!("import {new_import}"));
    Some((join_lines(&lines, contents), 1))
}

fn kotlin_import_insertion_index(lines: &[String]) -> usize {
    let mut last_import = None;
    let mut package_line = None;
    for (index, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if trimmed.starts_with("import ") {
            last_import = Some(index);
        } else if trimmed.starts_with("package ") {
            package_line = Some(index);
        }
    }
    if let Some(index) = last_import {
        index + 1
    } else if let Some(index) = package_line {
        index + 1
    } else {
        0
    }
}

fn join_lines(lines: &[String], original: &str) -> String {
    let joined = lines.join("\n");
    if original.ends_with('\n') {
        format!("{joined}\n")
    } else {
        joined
    }
}

fn kotlin_reference_safe_kind(kind: &str) -> bool {
    matches!(
        kind,
        "annotation" | "class" | "enum" | "interface" | "object" | "type"
    )
}

fn kotlin_package_name(contents: &str) -> Option<String> {
    for line in contents.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with("//") {
            continue;
        }
        let Some(rest) = trimmed.strip_prefix("package ") else {
            // The first meaningful line is not a package declaration, so this
            // file lives in the Kotlin root package.
            return None;
        };
        let package = rest.trim_end_matches(';').trim();
        if package.is_empty()
            || !package
                .split('.')
                .all(|segment| valid_identifier(segment.trim()))
        {
            return None;
        }
        return Some(package.to_string());
    }
    None
}

fn plan_swift_same_module_reference_awareness(
    target: &CodeSymbol,
    destination_path: &str,
    references_by_path: BTreeMap<String, Vec<CodeReference>>,
    reference_contents: BTreeMap<String, String>,
) -> Result<PlannedReferenceRewrite, String> {
    if !swift_reference_safe_kind(&target.kind) {
        return Err(format!(
            "move-symbol --rewrite-references supports Swift moves only for type declarations, not {} '{}'",
            target.kind, target.name
        ));
    }

    // Swift has no per-file import for same-module symbols, so a move inside one
    // module needs no textual rewrite. Swift also has no in-file module marker,
    // so the enclosing directory stands in for the module boundary.
    let source_parent = path_parent_segments(&target.path);
    let destination_parent = path_parent_segments(destination_path);
    if source_parent != destination_parent {
        return Err(
            "move-symbol --rewrite-references currently supports Swift moves only within the same module directory"
                .to_string(),
        );
    }

    for (path, references) in references_by_path {
        if path_parent_segments(&path) != source_parent {
            return Err(format!(
                "move-symbol --rewrite-references cannot keep Swift reference {path} valid across module directories"
            ));
        }
        let Some(contents) = reference_contents.get(&path) else {
            return Err(format!(
                "move-symbol --rewrite-references missing source contents for referenced file {path}; rerun hugr index"
            ));
        };
        if !code::parses_cleanly(&path, language_for_path(&path), contents)? {
            return Err(format!(
                "referencing file {path} is not valid Swift code; refusing to trust stale references"
            ));
        }
        for reference in references {
            let line = line_at(contents, reference.line_start).ok_or_else(|| {
                format!(
                    "move-symbol reference line {} is past end of {path}",
                    reference.line_start
                )
            })?;
            let (_line, matches) = replace_identifier_in_line(line, &target.name, &target.name);
            if matches == 0 {
                return Err(format!(
                    "move-symbol --rewrite-references could not verify Swift reference '{}' on {path}:{}; rerun hugr index",
                    target.name, reference.line_start
                ));
            }
        }
    }

    Ok(PlannedReferenceRewrite::default())
}

fn swift_reference_safe_kind(kind: &str) -> bool {
    matches!(
        kind,
        "actor" | "class" | "enum" | "extension" | "protocol" | "struct" | "type"
    )
}

fn path_file_stem(path: &str) -> Option<String> {
    let filename = normalize_path_string(path).rsplit('/').next()?.to_string();
    let stem = filename.rsplit_once('.').map(|(stem, _extension)| stem)?;
    if stem.is_empty() {
        None
    } else {
        Some(stem.to_string())
    }
}

fn rewrite_reference_files<F>(
    target: &CodeSymbol,
    references_by_path: BTreeMap<String, Vec<CodeReference>>,
    reference_contents: BTreeMap<String, String>,
    old_reference_description: &str,
    mut rewrite_file: F,
) -> Result<PlannedReferenceRewrite, String>
where
    F: FnMut(&str, &str, &BTreeSet<i64>) -> Result<(String, usize), String>,
{
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
        let (rewritten, replacement_count) = rewrite_file(&path, contents, &line_numbers)?;
        if replacement_count == 0 {
            return Err(format!(
                "move-symbol --rewrite-references could not rewrite {old_reference_description} in indexed references for {path}"
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

fn rewrite_rust_references_on_lines(
    path: &str,
    contents: &str,
    old_module: &str,
    new_module: &str,
    symbol_name: &str,
    line_numbers: &BTreeSet<i64>,
) -> Result<(String, usize), String> {
    let trailing_newline = contents.ends_with('\n');
    let mut lines = contents.lines().map(str::to_string).collect::<Vec<_>>();
    let mut replacement_count = 0;
    let old_qualified = format!("{old_module}::{symbol_name}");
    let new_qualified = format!("{new_module}::{symbol_name}");
    let old_module_leaf = rust_module_leaf(old_module);
    let old_module_aliases = rust_module_aliases(contents, old_module);

    for line_number in line_numbers.iter().rev() {
        if *line_number < 1 {
            return Err(format!(
                "move-symbol reference line {line_number} in {path} is invalid"
            ));
        }
        let index = usize::try_from(line_number - 1).map_err(|error| error.to_string())?;
        let Some(line) = lines.get(index) else {
            return Err(format!(
                "move-symbol reference line {line_number} is past end of {path}"
            ));
        };
        let (rewritten, line_replacements) = rewrite_rust_reference_line(
            line,
            old_module,
            new_module,
            &old_module_leaf,
            &old_module_aliases,
            symbol_name,
            &old_qualified,
            &new_qualified,
        );
        if line_replacements > 0 {
            lines.splice(index..=index, rewritten);
            replacement_count += line_replacements;
        }
    }

    let mut rendered = lines.join("\n");
    if trailing_newline {
        rendered.push('\n');
    }
    Ok((rendered, replacement_count))
}

fn rust_module_leaf(module_path: &str) -> String {
    module_path
        .rsplit("::")
        .next()
        .unwrap_or(module_path)
        .to_string()
}

fn rewrite_rust_reference_line(
    line: &str,
    old_module: &str,
    new_module: &str,
    old_module_leaf: &str,
    old_module_aliases: &BTreeSet<String>,
    symbol_name: &str,
    old_qualified: &str,
    new_qualified: &str,
) -> (Vec<String>, usize) {
    let (rewritten, qualified_replacements) =
        replace_rust_path_in_line(line, old_qualified, new_qualified);
    if qualified_replacements > 0 {
        return (vec![rewritten], qualified_replacements);
    }

    if let Some((rewritten, import_replacements)) =
        rewrite_rust_braced_use_line(line, old_module, new_module, symbol_name)
    {
        return (rewritten, import_replacements);
    }

    let old_leaf_qualified = format!("{old_module_leaf}::{symbol_name}");
    let (rewritten, leaf_replacements) =
        replace_rust_path_in_line(line, &old_leaf_qualified, new_qualified);
    if leaf_replacements > 0 {
        return (vec![rewritten], leaf_replacements);
    }

    for alias in old_module_aliases {
        let old_alias_qualified = format!("{alias}::{symbol_name}");
        let (rewritten, alias_replacements) =
            replace_rust_path_in_line(line, &old_alias_qualified, new_qualified);
        if alias_replacements > 0 {
            return (vec![rewritten], alias_replacements);
        }
    }

    (vec![line.to_string()], 0)
}

fn replace_rust_path_in_line(line: &str, old_path: &str, new_path: &str) -> (String, usize) {
    let mut rendered = String::new();
    let mut cursor = 0;
    let mut replacements = 0;

    for (start, matched) in line.match_indices(old_path) {
        let end = start + matched.len();
        if !rust_path_boundary_before(line[..start].chars().next_back())
            || !rust_path_boundary_after(line[end..].chars().next())
        {
            continue;
        }
        rendered.push_str(&line[cursor..start]);
        rendered.push_str(new_path);
        cursor = end;
        replacements += 1;
    }

    if replacements == 0 {
        return (line.to_string(), 0);
    }
    rendered.push_str(&line[cursor..]);
    (rendered, replacements)
}

fn rust_path_boundary_before(char: Option<char>) -> bool {
    char.is_none_or(|char| !(char.is_alphanumeric() || char == '_' || char == ':'))
}

fn rust_path_boundary_after(char: Option<char>) -> bool {
    char.is_none_or(|char| !(char.is_alphanumeric() || char == '_'))
}

fn rewrite_rust_braced_use_line(
    line: &str,
    old_module: &str,
    new_module: &str,
    symbol_name: &str,
) -> Option<(Vec<String>, usize)> {
    let trimmed = line.trim();
    if trimmed.contains("//") || !trimmed.ends_with(';') {
        return None;
    }

    let (prefix, tree_text) = rust_use_tree_parts(line)?;
    let mut tree = parse_rust_use_tree(tree_text)?;
    let module_segments = rust_module_segments(old_module)?;
    let mut moved_items = Vec::new();
    let old_tree_empty =
        remove_symbol_from_use_tree(&mut tree, &module_segments, symbol_name, &mut moved_items);
    if moved_items.is_empty() {
        return None;
    }

    let mut rewritten = Vec::new();
    if !old_tree_empty {
        rewritten.push(format!("{prefix}{};", render_rust_use_tree(&tree)));
    }
    rewritten.push(format!(
        "{prefix}{new_module}::{{{}}};",
        moved_items.join(", ")
    ));
    Some((rewritten, moved_items.len()))
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum RustUseTree {
    Name(String),
    Path {
        segment: String,
        child: Box<RustUseTree>,
    },
    Group(Vec<RustUseTree>),
}

fn rust_module_segments(module_path: &str) -> Option<Vec<String>> {
    let segments = module_path
        .split("::")
        .map(str::trim)
        .filter(|segment| !segment.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();
    if segments.is_empty() {
        None
    } else {
        Some(segments)
    }
}

fn rust_use_tree_parts(line: &str) -> Option<(&str, &str)> {
    let use_start = find_rust_use_keyword(line)?;
    let after_use = use_start + "use".len();
    let whitespace_len = line[after_use..]
        .chars()
        .take_while(|char| char.is_whitespace())
        .map(char::len_utf8)
        .sum::<usize>();
    if whitespace_len == 0 {
        return None;
    }

    let tree_start = after_use + whitespace_len;
    let semicolon = line.rfind(';')?;
    if semicolon <= tree_start || !line[semicolon + 1..].trim().is_empty() {
        return None;
    }

    let tree_text = line[tree_start..semicolon].trim();
    if tree_text.is_empty() {
        None
    } else {
        Some((&line[..tree_start], tree_text))
    }
}

fn find_rust_use_keyword(line: &str) -> Option<usize> {
    line.match_indices("use").find_map(|(index, _)| {
        let before = line[..index].chars().next_back();
        let after = line[index + "use".len()..].chars().next();
        if before.is_none_or(|char| char.is_whitespace())
            && after.is_some_and(|char| char.is_whitespace())
        {
            Some(index)
        } else {
            None
        }
    })
}

fn parse_rust_use_tree(value: &str) -> Option<RustUseTree> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }

    if let Some(inner) = strip_wrapping_braces(value) {
        let items = split_top_level_commas(inner)?
            .into_iter()
            .map(parse_rust_use_tree)
            .collect::<Option<Vec<_>>>()?;
        if items.is_empty() {
            return None;
        }
        return Some(RustUseTree::Group(items));
    }

    if let Some(index) = find_top_level_double_colon(value)? {
        let segment = value[..index].trim();
        let rest = value[index + "::".len()..].trim();
        if segment.is_empty() || rest.is_empty() {
            return None;
        }
        return Some(RustUseTree::Path {
            segment: segment.to_string(),
            child: Box::new(parse_rust_use_tree(rest)?),
        });
    }

    Some(RustUseTree::Name(value.to_string()))
}

fn strip_wrapping_braces(value: &str) -> Option<&str> {
    let value = value.trim();
    if !value.starts_with('{') {
        return None;
    }

    let mut depth = 0usize;
    let mut closing_index = None;
    for (index, char) in value.char_indices() {
        match char {
            '{' => depth += 1,
            '}' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    closing_index = Some(index);
                    break;
                }
            }
            _ => {}
        }
    }

    let closing_index = closing_index?;
    if !value[closing_index + 1..].trim().is_empty() {
        return None;
    }
    Some(&value[1..closing_index])
}

fn split_top_level_commas(value: &str) -> Option<Vec<&str>> {
    let mut items = Vec::new();
    let mut depth = 0usize;
    let mut start = 0usize;

    for (index, char) in value.char_indices() {
        match char {
            '{' => depth += 1,
            '}' => depth = depth.checked_sub(1)?,
            ',' if depth == 0 => {
                let item = value[start..index].trim();
                if !item.is_empty() {
                    items.push(item);
                }
                start = index + char.len_utf8();
            }
            _ => {}
        }
    }

    if depth != 0 {
        return None;
    }
    let item = value[start..].trim();
    if !item.is_empty() {
        items.push(item);
    }
    Some(items)
}

fn find_top_level_double_colon(value: &str) -> Option<Option<usize>> {
    let mut depth = 0usize;
    let mut chars = value.char_indices().peekable();
    while let Some((index, char)) = chars.next() {
        match char {
            '{' => depth += 1,
            '}' => depth = depth.checked_sub(1)?,
            ':' if depth == 0 => {
                if chars.peek().is_some_and(|(_, next_char)| *next_char == ':') {
                    return Some(Some(index));
                }
            }
            _ => {}
        }
    }
    if depth == 0 { Some(None) } else { None }
}

fn render_rust_use_tree(tree: &RustUseTree) -> String {
    match tree {
        RustUseTree::Name(name) => name.clone(),
        RustUseTree::Path { segment, child } => {
            format!("{segment}::{}", render_rust_use_tree(child))
        }
        RustUseTree::Group(items) => {
            let rendered_items = items
                .iter()
                .map(render_rust_use_tree)
                .collect::<Vec<_>>()
                .join(", ");
            format!("{{{rendered_items}}}")
        }
    }
}

fn remove_symbol_from_use_tree(
    tree: &mut RustUseTree,
    module_segments: &[String],
    symbol_name: &str,
    moved_items: &mut Vec<String>,
) -> bool {
    if module_segments.is_empty() {
        return false;
    }

    match tree {
        RustUseTree::Name(_) => false,
        RustUseTree::Path { segment, child } => {
            if segment != &module_segments[0] {
                return false;
            }
            if module_segments.len() == 1 {
                return remove_symbol_from_module_child(child, symbol_name, moved_items);
            }
            remove_symbol_from_use_tree(child, &module_segments[1..], symbol_name, moved_items)
                && rust_use_tree_is_empty(child)
        }
        RustUseTree::Group(items) => {
            let mut retained = Vec::with_capacity(items.len());
            for mut item in std::mem::take(items) {
                if !remove_symbol_from_use_tree(
                    &mut item,
                    module_segments,
                    symbol_name,
                    moved_items,
                ) {
                    retained.push(item);
                }
            }
            *items = retained;
            items.is_empty()
        }
    }
}

fn remove_symbol_from_module_child(
    child: &mut RustUseTree,
    symbol_name: &str,
    moved_items: &mut Vec<String>,
) -> bool {
    match child {
        RustUseTree::Name(item) => {
            if rust_use_item_imports_symbol(item, symbol_name) {
                moved_items.push(item.clone());
                true
            } else {
                false
            }
        }
        RustUseTree::Path { .. } => false,
        RustUseTree::Group(items) => {
            let mut retained = Vec::with_capacity(items.len());
            for item in std::mem::take(items) {
                if matches_direct_rust_use_item(&item, symbol_name) {
                    moved_items.push(render_rust_use_tree(&item));
                } else {
                    retained.push(item);
                }
            }
            *items = retained;
            items.is_empty()
        }
    }
}

fn matches_direct_rust_use_item(item: &RustUseTree, symbol_name: &str) -> bool {
    match item {
        RustUseTree::Name(item) => rust_use_item_imports_symbol(item, symbol_name),
        RustUseTree::Path { .. } | RustUseTree::Group(_) => false,
    }
}

fn rust_use_tree_is_empty(tree: &RustUseTree) -> bool {
    match tree {
        RustUseTree::Group(items) => items.is_empty(),
        RustUseTree::Path { child, .. } => rust_use_tree_is_empty(child),
        RustUseTree::Name(_) => false,
    }
}

fn rust_use_item_imports_symbol(item: &str, symbol_name: &str) -> bool {
    rust_use_item_name(item) == symbol_name
}

fn rust_use_item_name(item: &str) -> String {
    item.split_once(" as ")
        .map(|(name, _alias)| name.trim())
        .unwrap_or_else(|| item.trim())
        .to_string()
}

fn rust_use_item_alias(item: &str) -> Option<String> {
    item.split_once(" as ")
        .map(|(_name, alias)| alias.trim())
        .filter(|alias| valid_identifier(alias))
        .map(str::to_string)
}

fn rust_module_aliases(contents: &str, old_module: &str) -> BTreeSet<String> {
    let Some(module_segments) = rust_module_segments(old_module) else {
        return BTreeSet::new();
    };
    let mut aliases = BTreeSet::new();
    for line in contents.lines() {
        let trimmed = line.trim();
        if trimmed.contains("//") || !trimmed.ends_with(';') {
            continue;
        }
        let Some((_prefix, tree_text)) = rust_use_tree_parts(line) else {
            continue;
        };
        let Some(tree) = parse_rust_use_tree(tree_text) else {
            continue;
        };
        collect_rust_module_aliases(&tree, &module_segments, &mut aliases);
    }
    aliases
}

fn collect_rust_module_aliases(
    tree: &RustUseTree,
    module_segments: &[String],
    aliases: &mut BTreeSet<String>,
) {
    if module_segments.is_empty() {
        return;
    }

    match tree {
        RustUseTree::Name(item) => {
            if module_segments.len() == 1
                && rust_use_item_name(item) == module_segments[0]
                && let Some(alias) = rust_use_item_alias(item)
            {
                aliases.insert(alias);
            }
        }
        RustUseTree::Path { segment, child } => {
            if segment != &module_segments[0] {
                return;
            }
            if module_segments.len() == 1 {
                collect_rust_module_aliases_from_child(child, aliases);
            } else {
                collect_rust_module_aliases(child, &module_segments[1..], aliases);
            }
        }
        RustUseTree::Group(items) => {
            for item in items {
                collect_rust_module_aliases(item, module_segments, aliases);
            }
        }
    }
}

fn collect_rust_module_aliases_from_child(tree: &RustUseTree, aliases: &mut BTreeSet<String>) {
    match tree {
        RustUseTree::Name(item) => {
            if rust_use_item_name(item) == "self"
                && let Some(alias) = rust_use_item_alias(item)
            {
                aliases.insert(alias);
            }
        }
        RustUseTree::Path { .. } => {}
        RustUseTree::Group(items) => {
            for item in items {
                if let RustUseTree::Name(item) = item
                    && rust_use_item_name(item) == "self"
                    && let Some(alias) = rust_use_item_alias(item)
                {
                    aliases.insert(alias);
                }
            }
        }
    }
}

fn python_module_path_for_source(path: &str) -> Option<String> {
    let without_extension = path.strip_suffix(".py")?;
    let module = if without_extension == "__init__" {
        String::new()
    } else if let Some(package) = without_extension.strip_suffix("/__init__") {
        package.replace('/', ".")
    } else {
        without_extension.replace('/', ".")
    };
    if module.is_empty() {
        None
    } else {
        Some(module)
    }
}

fn rewrite_python_references_on_lines(
    path: &str,
    contents: &str,
    old_module: &str,
    new_module: &str,
    symbol_name: &str,
    line_numbers: &BTreeSet<i64>,
) -> Result<(String, usize), String> {
    let trailing_newline = contents.ends_with('\n');
    let mut lines = contents.lines().map(str::to_string).collect::<Vec<_>>();
    let old_module_leaf = python_module_leaf(old_module);
    let new_module_leaf = python_module_leaf(new_module);
    let mut replacement_count = rewrite_python_module_import_lines(
        &mut lines,
        old_module,
        new_module,
        &old_module_leaf,
        &new_module_leaf,
    );
    let allow_module_qualified_rewrites = replacement_count > 0;

    for line_number in line_numbers.iter().rev() {
        if *line_number < 1 {
            return Err(format!(
                "move-symbol reference line {line_number} in {path} is invalid"
            ));
        }
        let index = usize::try_from(line_number - 1).map_err(|error| error.to_string())?;
        let Some(line) = lines.get(index) else {
            return Err(format!(
                "move-symbol reference line {line_number} is past end of {path}"
            ));
        };
        let (rewritten, line_replacements) = rewrite_python_reference_line(
            line,
            old_module,
            new_module,
            &old_module_leaf,
            &new_module_leaf,
            symbol_name,
            allow_module_qualified_rewrites,
        );
        if line_replacements > 0 {
            lines.splice(index..=index, rewritten);
            replacement_count += line_replacements;
        }
    }

    let mut rendered = lines.join("\n");
    if trailing_newline {
        rendered.push('\n');
    }
    Ok((rendered, replacement_count))
}

fn python_module_leaf(module_path: &str) -> String {
    module_path
        .rsplit('.')
        .next()
        .unwrap_or(module_path)
        .to_string()
}

fn rewrite_python_module_import_lines(
    lines: &mut [String],
    old_module: &str,
    new_module: &str,
    old_module_leaf: &str,
    new_module_leaf: &str,
) -> usize {
    let mut replacement_count = 0;
    for line in lines {
        if python_import_line_is_unsupported(line) {
            continue;
        }
        if let Some((rewritten, replacements)) = rewrite_python_import_line(
            line,
            old_module,
            new_module,
            old_module_leaf,
            new_module_leaf,
        ) {
            *line = rewritten;
            replacement_count += replacements;
        } else if let Some((rewritten, replacements)) = rewrite_python_from_module_import_line(
            line,
            old_module,
            new_module,
            old_module_leaf,
            new_module_leaf,
        ) {
            *line = rewritten;
            replacement_count += replacements;
        }
    }
    replacement_count
}

fn rewrite_python_reference_line(
    line: &str,
    old_module: &str,
    new_module: &str,
    old_module_leaf: &str,
    new_module_leaf: &str,
    symbol_name: &str,
    allow_module_qualified_rewrites: bool,
) -> (Vec<String>, usize) {
    if let Some((rewritten, replacements)) = rewrite_python_from_symbol_import_line(
        line,
        old_module,
        new_module,
        old_module_leaf,
        new_module_leaf,
        symbol_name,
    ) {
        return (rewritten, replacements);
    }

    if !allow_module_qualified_rewrites {
        return (vec![line.to_string()], 0);
    }

    let old_qualified = format!("{old_module}.{symbol_name}");
    let new_qualified = format!("{new_module}.{symbol_name}");
    let (rewritten, qualified_replacements) =
        replace_python_attr_in_line(line, &old_qualified, &new_qualified);
    if qualified_replacements > 0 {
        return (vec![rewritten], qualified_replacements);
    }

    let old_leaf_qualified = format!("{old_module_leaf}.{symbol_name}");
    let new_leaf_qualified = format!("{new_module_leaf}.{symbol_name}");
    let (rewritten, leaf_replacements) =
        replace_python_attr_in_line(line, &old_leaf_qualified, &new_leaf_qualified);
    if leaf_replacements > 0 {
        return (vec![rewritten], leaf_replacements);
    }

    (vec![line.to_string()], 0)
}

fn rewrite_python_from_symbol_import_line(
    line: &str,
    old_module: &str,
    new_module: &str,
    old_module_leaf: &str,
    new_module_leaf: &str,
    symbol_name: &str,
) -> Option<(Vec<String>, usize)> {
    if python_import_line_is_unsupported(line) {
        return None;
    }
    let (prefix, module, items) = python_from_import_parts(line)?;
    let rewritten_module = python_new_module_for_import(
        module,
        old_module,
        new_module,
        old_module_leaf,
        new_module_leaf,
    )?;
    let mut moved_items = Vec::new();
    let mut kept_items = Vec::new();
    for item in items {
        if python_import_item_name(&item) == symbol_name {
            moved_items.push(item);
        } else {
            kept_items.push(item);
        }
    }
    if moved_items.is_empty() {
        return None;
    }

    let mut rewritten = Vec::new();
    if !kept_items.is_empty() {
        rewritten.push(format!(
            "{prefix}from {module} import {}",
            kept_items.join(", ")
        ));
    }
    rewritten.push(format!(
        "{prefix}from {rewritten_module} import {}",
        moved_items.join(", ")
    ));
    Some((rewritten, moved_items.len()))
}

fn rewrite_python_import_line(
    line: &str,
    old_module: &str,
    new_module: &str,
    old_module_leaf: &str,
    new_module_leaf: &str,
) -> Option<(String, usize)> {
    let (prefix, items) = python_import_parts(line)?;
    let mut replacements = 0;
    let rewritten_items = items
        .into_iter()
        .map(|item| {
            let name = python_import_item_name(&item);
            if let Some(rewritten_name) = python_new_module_for_import(
                &name,
                old_module,
                new_module,
                old_module_leaf,
                new_module_leaf,
            ) {
                replacements += 1;
                python_replace_import_item_name(&item, &rewritten_name)
            } else {
                item
            }
        })
        .collect::<Vec<_>>();
    if replacements == 0 {
        None
    } else {
        Some((
            format!("{prefix}import {}", rewritten_items.join(", ")),
            replacements,
        ))
    }
}

fn rewrite_python_from_module_import_line(
    line: &str,
    old_module: &str,
    new_module: &str,
    old_module_leaf: &str,
    new_module_leaf: &str,
) -> Option<(String, usize)> {
    let (prefix, module, items) = python_from_import_parts(line)?;
    let mut replacements = 0;
    let rewritten_items = items
        .into_iter()
        .map(|item| {
            let item_name = python_import_item_name(&item);
            if python_from_item_resolves_to_old_module(
                module,
                &item_name,
                old_module,
                old_module_leaf,
            ) {
                replacements += 1;
                python_replace_import_item_name(
                    &item,
                    &python_from_item_new_name(module, new_module, new_module_leaf),
                )
            } else {
                item
            }
        })
        .collect::<Vec<_>>();
    if replacements == 0 {
        None
    } else {
        Some((
            format!(
                "{prefix}from {module} import {}",
                rewritten_items.join(", ")
            ),
            replacements,
        ))
    }
}

fn python_import_line_is_unsupported(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed.contains('#')
        || trimmed.contains('\\')
        || trimmed.contains('(')
        || trimmed.contains(')')
        || trimmed.ends_with(',')
}

fn python_import_parts(line: &str) -> Option<(&str, Vec<String>)> {
    let trimmed = line.trim_start();
    let prefix_len = line.len().saturating_sub(trimmed.len());
    let rest = trimmed.strip_prefix("import ")?;
    if rest.trim().is_empty() || rest.contains(" import ") {
        return None;
    }
    Some((&line[..prefix_len], split_python_import_items(rest)?))
}

fn python_from_import_parts(line: &str) -> Option<(&str, &str, Vec<String>)> {
    let trimmed = line.trim_start();
    let prefix_len = line.len().saturating_sub(trimmed.len());
    let rest = trimmed.strip_prefix("from ")?;
    let (module, items) = rest.split_once(" import ")?;
    let module = module.trim();
    if module.is_empty() || items.trim().is_empty() {
        return None;
    }
    Some((
        &line[..prefix_len],
        module,
        split_python_import_items(items)?,
    ))
}

fn split_python_import_items(items: &str) -> Option<Vec<String>> {
    let parsed = items
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();
    if parsed.is_empty() {
        None
    } else {
        Some(parsed)
    }
}

fn python_new_module_for_import(
    module: &str,
    old_module: &str,
    new_module: &str,
    old_module_leaf: &str,
    new_module_leaf: &str,
) -> Option<String> {
    if module == old_module {
        Some(new_module.to_string())
    } else if module == old_module_leaf {
        Some(new_module_leaf.to_string())
    } else if module == format!(".{old_module_leaf}") {
        Some(format!(".{new_module_leaf}"))
    } else {
        None
    }
}

fn python_from_item_resolves_to_old_module(
    from_module: &str,
    item_name: &str,
    old_module: &str,
    old_module_leaf: &str,
) -> bool {
    if item_name != old_module_leaf {
        return false;
    }
    if from_module.starts_with('.') {
        return true;
    }
    let joined = format!("{from_module}.{item_name}");
    joined == old_module
}

fn python_from_item_new_name(from_module: &str, new_module: &str, new_module_leaf: &str) -> String {
    if from_module.starts_with('.') {
        new_module_leaf.to_string()
    } else {
        new_module
            .rsplit_once('.')
            .map(|(_parent, leaf)| leaf.to_string())
            .unwrap_or_else(|| new_module_leaf.to_string())
    }
}

fn python_import_item_name(item: &str) -> String {
    item.split_once(" as ")
        .map(|(name, _alias)| name.trim())
        .unwrap_or_else(|| item.trim())
        .to_string()
}

fn python_replace_import_item_name(item: &str, new_name: &str) -> String {
    if let Some((_name, alias)) = item.split_once(" as ") {
        format!("{new_name} as {}", alias.trim())
    } else {
        new_name.to_string()
    }
}

fn replace_python_attr_in_line(line: &str, old_attr: &str, new_attr: &str) -> (String, usize) {
    let mut rendered = String::new();
    let mut cursor = 0;
    let mut replacements = 0;

    for (start, matched) in line.match_indices(old_attr) {
        let end = start + matched.len();
        if !python_attr_boundary_before(line[..start].chars().next_back())
            || !python_attr_boundary_after(line[end..].chars().next())
        {
            continue;
        }
        rendered.push_str(&line[cursor..start]);
        rendered.push_str(new_attr);
        cursor = end;
        replacements += 1;
    }

    if replacements == 0 {
        return (line.to_string(), 0);
    }
    rendered.push_str(&line[cursor..]);
    (rendered, replacements)
}

fn python_attr_boundary_before(char: Option<char>) -> bool {
    char.is_none_or(|char| !(char.is_alphanumeric() || char == '_' || char == '.'))
}

fn python_attr_boundary_after(char: Option<char>) -> bool {
    char.is_none_or(|char| !(char.is_alphanumeric() || char == '_'))
}

fn rewrite_javascript_references_on_lines(
    path: &str,
    contents: &str,
    source_path: &str,
    destination_path: &str,
    symbol_name: &str,
    line_numbers: &BTreeSet<i64>,
) -> Result<(String, usize), String> {
    let trailing_newline = contents.ends_with('\n');
    let mut lines = contents.lines().map(str::to_string).collect::<Vec<_>>();
    let namespace_aliases = javascript_namespace_aliases_for_source(&lines, path, source_path);
    let used_namespace_aliases =
        javascript_used_namespace_aliases(&lines, line_numbers, &namespace_aliases, symbol_name)?;
    let mut replacement_count = rewrite_javascript_namespace_import_lines(
        &mut lines,
        path,
        source_path,
        destination_path,
        &used_namespace_aliases,
    );

    for line_number in line_numbers.iter().rev() {
        if *line_number < 1 {
            return Err(format!(
                "move-symbol reference line {line_number} in {path} is invalid"
            ));
        }
        let index = usize::try_from(line_number - 1).map_err(|error| error.to_string())?;
        let Some(line) = lines.get(index) else {
            return Err(format!(
                "move-symbol reference line {line_number} is past end of {path}"
            ));
        };
        let (rewritten, line_replacements) = rewrite_javascript_reference_line(
            line,
            path,
            source_path,
            destination_path,
            symbol_name,
        );
        if line_replacements > 0 {
            lines.splice(index..=index, rewritten);
            replacement_count += line_replacements;
        }
    }

    let mut rendered = lines.join("\n");
    if trailing_newline {
        rendered.push('\n');
    }
    Ok((rendered, replacement_count))
}

fn javascript_used_namespace_aliases(
    lines: &[String],
    line_numbers: &BTreeSet<i64>,
    namespace_aliases: &BTreeSet<String>,
    symbol_name: &str,
) -> Result<BTreeSet<String>, String> {
    let mut used_aliases = BTreeSet::new();
    for line_number in line_numbers {
        if *line_number < 1 {
            return Err(format!(
                "move-symbol reference line {line_number} is invalid"
            ));
        }
        let index = usize::try_from(line_number - 1).map_err(|error| error.to_string())?;
        let Some(line) = lines.get(index) else {
            return Err(format!(
                "move-symbol reference line {line_number} is past end"
            ));
        };
        for alias in namespace_aliases {
            if javascript_line_has_member_reference(line, alias, symbol_name) {
                used_aliases.insert(alias.clone());
            }
        }
    }
    Ok(used_aliases)
}

fn rewrite_javascript_reference_line(
    line: &str,
    reference_path: &str,
    source_path: &str,
    destination_path: &str,
    symbol_name: &str,
) -> (Vec<String>, usize) {
    if let Some((rewritten, replacements)) = rewrite_javascript_named_import_line(
        line,
        reference_path,
        source_path,
        destination_path,
        symbol_name,
    ) {
        return (rewritten, replacements);
    }

    (vec![line.to_string()], 0)
}

fn rewrite_javascript_named_import_line(
    line: &str,
    reference_path: &str,
    source_path: &str,
    destination_path: &str,
    symbol_name: &str,
) -> Option<(Vec<String>, usize)> {
    if javascript_import_line_is_unsupported(line) {
        return None;
    }
    let parts = javascript_named_import_parts(line)?;
    if !javascript_module_spec_targets_path(&parts.module_spec, reference_path, source_path) {
        return None;
    }

    let new_module_spec = javascript_module_spec_for_destination(
        reference_path,
        destination_path,
        &parts.module_spec,
    )?;
    let mut moved_items = Vec::new();
    let mut kept_items = Vec::new();
    for item in parts.items {
        if javascript_import_item_name(&item) == symbol_name {
            moved_items.push(item);
        } else {
            kept_items.push(item);
        }
    }
    if moved_items.is_empty() {
        return None;
    }

    let mut rewritten = Vec::new();
    if !kept_items.is_empty() {
        rewritten.push(render_javascript_named_import_line(
            &parts.leading,
            &parts.import_keyword,
            &kept_items,
            &parts.quote,
            &parts.module_spec,
            &parts.suffix,
        ));
    }
    rewritten.push(render_javascript_named_import_line(
        &parts.leading,
        &parts.import_keyword,
        &moved_items,
        &parts.quote,
        &new_module_spec,
        &parts.suffix,
    ));
    Some((rewritten, moved_items.len()))
}

fn rewrite_javascript_namespace_import_lines(
    lines: &mut [String],
    reference_path: &str,
    source_path: &str,
    destination_path: &str,
    used_aliases: &BTreeSet<String>,
) -> usize {
    if used_aliases.is_empty() {
        return 0;
    }

    let mut replacement_count = 0;
    for line in lines {
        if javascript_import_line_is_unsupported(line) {
            continue;
        }
        let Some(parts) = javascript_namespace_import_parts(line) else {
            continue;
        };
        if !used_aliases.contains(&parts.alias)
            || !javascript_module_spec_targets_path(&parts.module_spec, reference_path, source_path)
        {
            continue;
        }
        let Some(new_module_spec) = javascript_module_spec_for_destination(
            reference_path,
            destination_path,
            &parts.module_spec,
        ) else {
            continue;
        };
        *line = render_javascript_namespace_import_line(
            &parts.leading,
            &parts.alias,
            &parts.quote,
            &new_module_spec,
            &parts.suffix,
        );
        replacement_count += 1;
    }
    replacement_count
}

fn javascript_namespace_aliases_for_source(
    lines: &[String],
    reference_path: &str,
    source_path: &str,
) -> BTreeSet<String> {
    let mut aliases = BTreeSet::new();
    for line in lines {
        if javascript_import_line_is_unsupported(line) {
            continue;
        }
        let Some(parts) = javascript_namespace_import_parts(line) else {
            continue;
        };
        if javascript_module_spec_targets_path(&parts.module_spec, reference_path, source_path) {
            aliases.insert(parts.alias);
        }
    }
    aliases
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct JavascriptNamedImportParts {
    leading: String,
    import_keyword: String,
    items: Vec<String>,
    quote: String,
    module_spec: String,
    suffix: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct JavascriptNamespaceImportParts {
    leading: String,
    alias: String,
    quote: String,
    module_spec: String,
    suffix: String,
}

fn javascript_named_import_parts(line: &str) -> Option<JavascriptNamedImportParts> {
    let trimmed = line.trim_start();
    let leading = line[..line.len().saturating_sub(trimmed.len())].to_string();
    let (import_keyword, rest) = javascript_import_keyword_and_rest(trimmed)?;
    let rest = rest.trim_start();
    let after_open = rest.strip_prefix('{')?;
    let close_index = after_open.find('}')?;
    let item_text = &after_open[..close_index];
    let after_brace = after_open[close_index + 1..].trim_start();
    let after_from = after_brace.strip_prefix("from")?.trim_start();
    let (quote, module_spec, suffix) = javascript_module_spec_parts(after_from)?;
    Some(JavascriptNamedImportParts {
        leading,
        import_keyword,
        items: split_javascript_import_items(item_text)?,
        quote,
        module_spec,
        suffix,
    })
}

fn javascript_namespace_import_parts(line: &str) -> Option<JavascriptNamespaceImportParts> {
    let trimmed = line.trim_start();
    let leading = line[..line.len().saturating_sub(trimmed.len())].to_string();
    let rest = trimmed.strip_prefix("import ")?.trim_start();
    let rest = rest.strip_prefix("*")?.trim_start();
    let rest = rest.strip_prefix("as")?.trim_start();
    let (alias, after_alias) = split_javascript_identifier(rest)?;
    let after_from = after_alias.trim_start().strip_prefix("from")?.trim_start();
    let (quote, module_spec, suffix) = javascript_module_spec_parts(after_from)?;
    Some(JavascriptNamespaceImportParts {
        leading,
        alias: alias.to_string(),
        quote,
        module_spec,
        suffix,
    })
}

fn javascript_import_keyword_and_rest(trimmed: &str) -> Option<(String, &str)> {
    for keyword in ["import type", "import", "export type", "export"] {
        if let Some(rest) = trimmed.strip_prefix(keyword)
            && rest.starts_with(char::is_whitespace)
        {
            return Some((keyword.to_string(), rest));
        }
    }
    None
}

fn javascript_module_spec_parts(value: &str) -> Option<(String, String, String)> {
    let value = value.trim_start();
    let quote = value.chars().next()?;
    if quote != '"' && quote != '\'' {
        return None;
    }
    let after_quote = &value[quote.len_utf8()..];
    let end_quote = after_quote.find(quote)?;
    let module_spec = after_quote[..end_quote].to_string();
    let suffix = after_quote[end_quote + quote.len_utf8()..].trim();
    if suffix != ";" && !suffix.is_empty() {
        return None;
    }
    Some((quote.to_string(), module_spec, suffix.to_string()))
}

fn split_javascript_import_items(items: &str) -> Option<Vec<String>> {
    let parsed = items
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();
    if parsed.is_empty()
        || parsed
            .iter()
            .any(|item| item.contains('{') || item.contains('}'))
    {
        None
    } else {
        Some(parsed)
    }
}

fn split_javascript_identifier(value: &str) -> Option<(&str, &str)> {
    let trimmed = value.trim_start();
    let mut end = 0usize;
    for (index, char) in trimmed.char_indices() {
        if index == 0 {
            if !(char.is_ascii_alphabetic() || char == '_' || char == '$') {
                return None;
            }
            end = char.len_utf8();
            continue;
        }
        if char.is_ascii_alphanumeric() || char == '_' || char == '$' {
            end = index + char.len_utf8();
        } else {
            break;
        }
    }
    if end == 0 {
        None
    } else {
        Some((&trimmed[..end], &trimmed[end..]))
    }
}

fn render_javascript_named_import_line(
    leading: &str,
    import_keyword: &str,
    items: &[String],
    quote: &str,
    module_spec: &str,
    suffix: &str,
) -> String {
    format!(
        "{leading}{import_keyword} {{ {} }} from {quote}{module_spec}{quote}{suffix}",
        items.join(", ")
    )
}

fn render_javascript_namespace_import_line(
    leading: &str,
    alias: &str,
    quote: &str,
    module_spec: &str,
    suffix: &str,
) -> String {
    format!("{leading}import * as {alias} from {quote}{module_spec}{quote}{suffix}")
}

fn javascript_import_line_is_unsupported(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed.contains("//")
        || trimmed.contains("/*")
        || trimmed.contains('\\')
        || trimmed.ends_with(',')
}

fn javascript_import_item_name(item: &str) -> String {
    let item = item
        .strip_prefix("type ")
        .map(str::trim_start)
        .unwrap_or(item.trim());
    item.split_once(" as ")
        .map(|(name, _alias)| name.trim())
        .unwrap_or(item)
        .to_string()
}

fn javascript_line_has_member_reference(
    line: &str,
    namespace_alias: &str,
    symbol_name: &str,
) -> bool {
    let member = format!("{namespace_alias}.{symbol_name}");
    line.match_indices(&member).any(|(index, _)| {
        let end = index + member.len();
        javascript_member_boundary_before(line[..index].chars().next_back())
            && javascript_member_boundary_after(line[end..].chars().next())
    })
}

fn javascript_member_boundary_before(char: Option<char>) -> bool {
    char.is_none_or(|char| !(char.is_alphanumeric() || char == '_' || char == '$' || char == '.'))
}

fn javascript_member_boundary_after(char: Option<char>) -> bool {
    char.is_none_or(|char| !(char.is_alphanumeric() || char == '_' || char == '$'))
}

fn javascript_module_spec_targets_path(
    spec: &str,
    reference_path: &str,
    target_path: &str,
) -> bool {
    if !spec.starts_with('.') {
        return false;
    }
    let Some(resolved) = resolve_relative_module_spec(reference_path, spec) else {
        return false;
    };
    let target = normalize_path_string(target_path);
    let target_without_extension = strip_javascript_extension(&target);
    resolved == target || resolved == target_without_extension
}

fn javascript_module_spec_for_destination(
    reference_path: &str,
    destination_path: &str,
    old_spec: &str,
) -> Option<String> {
    let include_extension = javascript_spec_includes_extension(old_spec);
    let mut target = normalize_path_string(destination_path);
    if !include_extension {
        target = strip_javascript_extension(&target);
    }
    relative_module_spec(reference_path, &target)
}

fn resolve_relative_module_spec(reference_path: &str, spec: &str) -> Option<String> {
    let mut segments = path_parent_segments(reference_path);
    for segment in spec.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                segments.pop()?;
            }
            value => segments.push(value.to_string()),
        }
    }
    if segments.is_empty() {
        None
    } else {
        Some(segments.join("/"))
    }
}

fn relative_module_spec(reference_path: &str, target_path: &str) -> Option<String> {
    let from = path_parent_segments(reference_path);
    let to = path_segments(target_path);
    if to.is_empty() {
        return None;
    }

    let mut common = 0usize;
    while common < from.len() && common < to.len() && from[common] == to[common] {
        common += 1;
    }

    let mut relative = Vec::new();
    for _ in common..from.len() {
        relative.push("..".to_string());
    }
    relative.extend(to.iter().skip(common).cloned());
    if relative.is_empty() {
        return None;
    }
    let rendered = relative.join("/");
    if rendered.starts_with("..") {
        Some(rendered)
    } else {
        Some(format!("./{rendered}"))
    }
}

fn path_parent_segments(path: &str) -> Vec<String> {
    let mut segments = path_segments(path);
    segments.pop();
    segments
}

fn path_segments(path: &str) -> Vec<String> {
    normalize_path_string(path)
        .split('/')
        .filter(|segment| !segment.is_empty() && *segment != ".")
        .map(str::to_string)
        .collect()
}

fn normalize_path_string(path: &str) -> String {
    path.replace('\\', "/")
}

fn strip_javascript_extension(path: &str) -> String {
    for extension in [
        ".d.ts", ".tsx", ".jsx", ".mts", ".cts", ".mjs", ".cjs", ".ts", ".js",
    ] {
        if let Some(stripped) = path.strip_suffix(extension) {
            return stripped.to_string();
        }
    }
    path.to_string()
}

fn javascript_spec_includes_extension(spec: &str) -> bool {
    let last_segment = spec.rsplit('/').next().unwrap_or(spec);
    [
        ".d.ts", ".tsx", ".jsx", ".mts", ".cts", ".mjs", ".cjs", ".ts", ".js",
    ]
    .iter()
    .any(|extension| last_segment.ends_with(extension))
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

fn line_at(contents: &str, line_number: i64) -> Option<&str> {
    if line_number < 1 {
        return None;
    }
    let index = usize::try_from(line_number - 1).ok()?;
    contents.lines().nth(index)
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
    fn plans_move_with_python_from_import_rewrite() {
        let source = "def helper():\n    return 1\n\n\ndef other():\n    return 2\n";
        let destination = "def existing():\n    return 0\n";
        let caller =
            "from plugin_hooks import helper as run_helper, other\n\nvalue = run_helper()\n";
        let target =
            resolve_symbol_in_source("plugin_hooks.py", source, "helper", None, "move").unwrap();
        let references = vec![CodeReference {
            path: "main.py".to_string(),
            language: Some("python".to_string()),
            target_path: "plugin_hooks.py".to_string(),
            target_name: "helper".to_string(),
            target_kind: "function".to_string(),
            kind: "import".to_string(),
            line_start: 1,
            excerpt: "from plugin_hooks import helper as run_helper, other".to_string(),
        }];

        let planned = plan_move(
            &target,
            &references,
            source,
            "helpers.py",
            destination,
            vec![("main.py".to_string(), caller.to_string())],
            true,
        )
        .unwrap();
        let caller_file = planned
            .files
            .iter()
            .find(|file| file.path == "main.py")
            .unwrap();

        assert!(
            caller_file
                .contents
                .contains("from plugin_hooks import other")
        );
        assert!(
            caller_file
                .contents
                .contains("from helpers import helper as run_helper")
        );
        assert!(caller_file.contents.contains("run_helper()"));
        assert_eq!(planned.summary.rewritten_reference_count, 1);
    }

    #[test]
    fn plans_move_with_python_module_import_and_qualified_call_rewrite() {
        let source = "def helper():\n    return 1\n\n\ndef other():\n    return 2\n";
        let destination = "def existing():\n    return 0\n";
        let caller = "import plugin_hooks\n\nvalue = plugin_hooks.helper()\n";
        let target =
            resolve_symbol_in_source("plugin_hooks.py", source, "helper", None, "move").unwrap();
        let references = vec![CodeReference {
            path: "main.py".to_string(),
            language: Some("python".to_string()),
            target_path: "plugin_hooks.py".to_string(),
            target_name: "helper".to_string(),
            target_kind: "function".to_string(),
            kind: "call".to_string(),
            line_start: 3,
            excerpt: "value = plugin_hooks.helper()".to_string(),
        }];

        let planned = plan_move(
            &target,
            &references,
            source,
            "helpers.py",
            destination,
            vec![("main.py".to_string(), caller.to_string())],
            true,
        )
        .unwrap();
        let caller_file = planned
            .files
            .iter()
            .find(|file| file.path == "main.py")
            .unwrap();

        assert!(caller_file.contents.contains("import helpers"));
        assert!(caller_file.contents.contains("value = helpers.helper()"));
        assert!(!caller_file.contents.contains("plugin_hooks"));
        assert_eq!(planned.summary.rewritten_reference_count, 2);
    }

    #[test]
    fn plans_move_with_typescript_named_import_rewrite() {
        let source = "export function helper() {\n    return 1;\n}\n\nexport function other() {\n    return 2;\n}\n";
        let destination = "export function existing() {\n    return 0;\n}\n";
        let caller = "import { helper as runHelper, other } from \"./pluginHooks\";\n\nconst value = runHelper();\n";
        let target =
            resolve_symbol_in_source("src/pluginHooks.ts", source, "helper", None, "move").unwrap();
        let references = vec![CodeReference {
            path: "src/main.ts".to_string(),
            language: Some("typescript".to_string()),
            target_path: "src/pluginHooks.ts".to_string(),
            target_name: "helper".to_string(),
            target_kind: "function".to_string(),
            kind: "import".to_string(),
            line_start: 1,
            excerpt: "import { helper as runHelper, other } from \"./pluginHooks\";".to_string(),
        }];

        let planned = plan_move(
            &target,
            &references,
            source,
            "src/helpers.ts",
            destination,
            vec![("src/main.ts".to_string(), caller.to_string())],
            true,
        )
        .unwrap();
        let caller_file = planned
            .files
            .iter()
            .find(|file| file.path == "src/main.ts")
            .unwrap();

        assert!(
            caller_file
                .contents
                .contains("import { other } from \"./pluginHooks\";")
        );
        assert!(
            caller_file
                .contents
                .contains("import { helper as runHelper } from \"./helpers\";")
        );
        assert!(caller_file.contents.contains("runHelper();"));
        assert_eq!(planned.summary.rewritten_reference_count, 1);
    }

    #[test]
    fn plans_move_with_typescript_namespace_import_rewrite() {
        let source = "export function helper() {\n    return 1;\n}\n";
        let destination = "export function existing() {\n    return 0;\n}\n";
        let caller = "import * as hooks from \"./pluginHooks\";\n\nconst value = hooks.helper();\n";
        let target =
            resolve_symbol_in_source("src/pluginHooks.ts", source, "helper", None, "move").unwrap();
        let references = vec![CodeReference {
            path: "src/main.ts".to_string(),
            language: Some("typescript".to_string()),
            target_path: "src/pluginHooks.ts".to_string(),
            target_name: "helper".to_string(),
            target_kind: "function".to_string(),
            kind: "call".to_string(),
            line_start: 3,
            excerpt: "const value = hooks.helper();".to_string(),
        }];

        let planned = plan_move(
            &target,
            &references,
            source,
            "src/helpers.ts",
            destination,
            vec![("src/main.ts".to_string(), caller.to_string())],
            true,
        )
        .unwrap();
        let caller_file = planned
            .files
            .iter()
            .find(|file| file.path == "src/main.ts")
            .unwrap();

        assert!(
            caller_file
                .contents
                .contains("import * as hooks from \"./helpers\";")
        );
        assert!(caller_file.contents.contains("hooks.helper();"));
        assert!(!caller_file.contents.contains("./pluginHooks"));
        assert_eq!(planned.summary.rewritten_reference_count, 1);
    }

    #[test]
    fn plans_go_same_package_move_with_references_without_text_rewrites() {
        let source = "package plugin\n\nfunc helper() int {\n    return 1\n}\n\nfunc other() int {\n    return 2\n}\n";
        let destination = "package plugin\n\nfunc existing() int {\n    return 0\n}\n";
        let caller = "package plugin\n\nfunc useHelper() int {\n    return helper()\n}\n";
        let target =
            resolve_symbol_in_source("plugin/hooks.go", source, "helper", None, "move").unwrap();
        let references = vec![CodeReference {
            path: "plugin/caller.go".to_string(),
            language: Some("go".to_string()),
            target_path: "plugin/hooks.go".to_string(),
            target_name: "helper".to_string(),
            target_kind: "function".to_string(),
            kind: "call".to_string(),
            line_start: 4,
            excerpt: "return helper()".to_string(),
        }];

        let planned = plan_move(
            &target,
            &references,
            source,
            "plugin/helpers.go",
            destination,
            vec![("plugin/caller.go".to_string(), caller.to_string())],
            true,
        )
        .unwrap();

        assert_eq!(planned.summary.rewritten_reference_count, 0);
        assert!(
            planned
                .files
                .iter()
                .all(|file| file.path != "plugin/caller.go")
        );
        assert!(
            planned
                .files
                .iter()
                .find(|file| file.path == "plugin/helpers.go")
                .unwrap()
                .contents
                .contains("func helper() int")
        );
    }

    #[test]
    fn rejects_go_reference_aware_move_across_package_directories() {
        let source = "package plugin\n\nfunc helper() int {\n    return 1\n}\n";
        let destination = "package other\n\nfunc existing() int {\n    return 0\n}\n";
        let caller = "package plugin\n\nfunc useHelper() int {\n    return helper()\n}\n";
        let target =
            resolve_symbol_in_source("plugin/hooks.go", source, "helper", None, "move").unwrap();
        let references = vec![CodeReference {
            path: "plugin/caller.go".to_string(),
            language: Some("go".to_string()),
            target_path: "plugin/hooks.go".to_string(),
            target_name: "helper".to_string(),
            target_kind: "function".to_string(),
            kind: "call".to_string(),
            line_start: 4,
            excerpt: "return helper()".to_string(),
        }];

        let error = plan_move(
            &target,
            &references,
            source,
            "other/helpers.go",
            destination,
            vec![("plugin/caller.go".to_string(), caller.to_string())],
            true,
        )
        .unwrap_err();

        assert!(error.contains("same package directory"), "{error}");
    }

    #[test]
    fn plans_java_same_package_type_move_with_references_without_text_rewrites() {
        let source = "package plugin;\n\nclass Helper {\n    int value() { return 1; }\n}\n\nclass Other {}\n";
        let destination = "package plugin;\n\nclass Existing {}\n";
        let caller = "package plugin;\n\nclass Caller {\n    Helper helper = new Helper();\n}\n";
        let target = resolve_symbol_in_source(
            "src/plugin/PluginHooks.java",
            source,
            "Helper",
            None,
            "move",
        )
        .unwrap();
        let references = vec![CodeReference {
            path: "src/plugin/Caller.java".to_string(),
            language: Some("java".to_string()),
            target_path: "src/plugin/PluginHooks.java".to_string(),
            target_name: "Helper".to_string(),
            target_kind: "class".to_string(),
            kind: "instantiation".to_string(),
            line_start: 4,
            excerpt: "Helper helper = new Helper();".to_string(),
        }];

        let planned = plan_move(
            &target,
            &references,
            source,
            "src/plugin/Helper.java",
            destination,
            vec![("src/plugin/Caller.java".to_string(), caller.to_string())],
            true,
        )
        .unwrap();

        assert_eq!(planned.summary.rewritten_reference_count, 0);
        assert!(
            planned
                .files
                .iter()
                .all(|file| file.path != "src/plugin/Caller.java")
        );
        assert!(
            planned
                .files
                .iter()
                .find(|file| file.path == "src/plugin/Helper.java")
                .unwrap()
                .contents
                .contains("class Helper")
        );
    }

    #[test]
    fn rejects_java_reference_aware_method_move() {
        let source = "package plugin;\n\nclass Helper {\n    int value() { return 1; }\n}\n";
        let destination = "package plugin;\n\nclass Existing {}\n";
        let caller =
            "package plugin;\n\nclass Caller {\n    int value = new Helper().value();\n}\n";
        let target =
            resolve_symbol_in_source("src/plugin/Helper.java", source, "value", None, "move")
                .unwrap();
        let references = vec![CodeReference {
            path: "src/plugin/Caller.java".to_string(),
            language: Some("java".to_string()),
            target_path: "src/plugin/Helper.java".to_string(),
            target_name: "value".to_string(),
            target_kind: "function".to_string(),
            kind: "call".to_string(),
            line_start: 4,
            excerpt: "int value = new Helper().value();".to_string(),
        }];

        let error = plan_move(
            &target,
            &references,
            source,
            "src/plugin/Existing.java",
            destination,
            vec![("src/plugin/Caller.java".to_string(), caller.to_string())],
            true,
        )
        .unwrap_err();

        assert!(error.contains("only for type declarations"), "{error}");
    }

    #[test]
    fn plans_kotlin_same_package_type_move_with_references_without_text_rewrites() {
        let source =
            "package plugin\n\nclass Helper {\n    fun value(): Int = 1\n}\n\nclass Other\n";
        let destination = "package plugin\n\nclass Existing\n";
        let caller = "package plugin\n\nclass Caller {\n    val helper = Helper()\n}\n";
        let target =
            resolve_symbol_in_source("src/plugin/Hooks.kt", source, "Helper", None, "move")
                .unwrap();
        let references = vec![CodeReference {
            path: "src/plugin/Caller.kt".to_string(),
            language: Some("kotlin".to_string()),
            target_path: "src/plugin/Hooks.kt".to_string(),
            target_name: "Helper".to_string(),
            target_kind: "class".to_string(),
            kind: "instantiation".to_string(),
            line_start: 4,
            excerpt: "val helper = Helper()".to_string(),
        }];

        let planned = plan_move(
            &target,
            &references,
            source,
            "src/plugin/Helper.kt",
            destination,
            vec![("src/plugin/Caller.kt".to_string(), caller.to_string())],
            true,
        )
        .unwrap();

        assert_eq!(planned.summary.rewritten_reference_count, 0);
        assert!(
            planned
                .files
                .iter()
                .all(|file| file.path != "src/plugin/Caller.kt")
        );
        assert!(
            planned
                .files
                .iter()
                .find(|file| file.path == "src/plugin/Helper.kt")
                .unwrap()
                .contents
                .contains("class Helper")
        );
    }

    #[test]
    fn rewrites_foreign_package_import_on_kotlin_cross_package_move() {
        // Caller lives in a third package and imports plugin.Helper; the move to
        // package `other` should rewrite that import.
        let source = "package plugin\n\nclass Helper\n";
        let destination = "package other\n\nclass Existing\n";
        let caller =
            "package app\n\nimport plugin.Helper\n\nclass Caller {\n    val helper = Helper()\n}\n";
        let target =
            resolve_symbol_in_source("src/plugin/Hooks.kt", source, "Helper", None, "move")
                .unwrap();
        let references = vec![CodeReference {
            path: "src/app/Caller.kt".to_string(),
            language: Some("kotlin".to_string()),
            target_path: "src/plugin/Hooks.kt".to_string(),
            target_name: "Helper".to_string(),
            target_kind: "class".to_string(),
            kind: "instantiation".to_string(),
            line_start: 6,
            excerpt: "val helper = Helper()".to_string(),
        }];

        let planned = plan_move(
            &target,
            &references,
            source,
            "src/other/Helper.kt",
            destination,
            vec![("src/app/Caller.kt".to_string(), caller.to_string())],
            true,
        )
        .unwrap();

        assert_eq!(planned.summary.rewritten_reference_count, 1);
        let rewritten = planned
            .files
            .iter()
            .find(|file| file.path == "src/app/Caller.kt")
            .unwrap();
        assert!(rewritten.contents.contains("import other.Helper"));
        assert!(!rewritten.contents.contains("import plugin.Helper"));
    }

    #[test]
    fn inserts_import_for_source_package_referencer_on_kotlin_cross_package_move() {
        // Caller is in the source package (no import today); after moving Helper
        // to package `other`, it must gain `import other.Helper`.
        let source = "package plugin\n\nclass Helper\n";
        let destination = "package other\n\nclass Existing\n";
        let caller = "package plugin\n\nclass Caller {\n    val helper = Helper()\n}\n";
        let target =
            resolve_symbol_in_source("src/plugin/Hooks.kt", source, "Helper", None, "move")
                .unwrap();
        let references = vec![CodeReference {
            path: "src/plugin/Caller.kt".to_string(),
            language: Some("kotlin".to_string()),
            target_path: "src/plugin/Hooks.kt".to_string(),
            target_name: "Helper".to_string(),
            target_kind: "class".to_string(),
            kind: "instantiation".to_string(),
            line_start: 4,
            excerpt: "val helper = Helper()".to_string(),
        }];

        let planned = plan_move(
            &target,
            &references,
            source,
            "src/other/Helper.kt",
            destination,
            vec![("src/plugin/Caller.kt".to_string(), caller.to_string())],
            true,
        )
        .unwrap();

        let rewritten = planned
            .files
            .iter()
            .find(|file| file.path == "src/plugin/Caller.kt")
            .unwrap();
        assert!(rewritten.contents.contains("import other.Helper"));
        // Import goes after the package line.
        let package_index = rewritten
            .contents
            .lines()
            .position(|line| line.trim().starts_with("package "))
            .unwrap();
        let import_index = rewritten
            .contents
            .lines()
            .position(|line| line.trim() == "import other.Helper")
            .unwrap();
        assert!(import_index > package_index);
    }

    #[test]
    fn drops_import_for_destination_package_referencer_on_kotlin_cross_package_move() {
        // Caller is already in the destination package but imports plugin.Helper;
        // after the move that import is redundant and should be removed.
        let source = "package plugin\n\nclass Helper\n";
        let destination = "package other\n\nclass Existing\n";
        let caller = "package other\n\nimport plugin.Helper\n\nclass Caller {\n    val helper = Helper()\n}\n";
        let target =
            resolve_symbol_in_source("src/plugin/Hooks.kt", source, "Helper", None, "move")
                .unwrap();
        let references = vec![CodeReference {
            path: "src/other/Caller.kt".to_string(),
            language: Some("kotlin".to_string()),
            target_path: "src/plugin/Hooks.kt".to_string(),
            target_name: "Helper".to_string(),
            target_kind: "class".to_string(),
            kind: "instantiation".to_string(),
            line_start: 6,
            excerpt: "val helper = Helper()".to_string(),
        }];

        let planned = plan_move(
            &target,
            &references,
            source,
            "src/other/Helper.kt",
            destination,
            vec![("src/other/Caller.kt".to_string(), caller.to_string())],
            true,
        )
        .unwrap();

        let rewritten = planned
            .files
            .iter()
            .find(|file| file.path == "src/other/Caller.kt")
            .unwrap();
        assert!(!rewritten.contents.contains("import plugin.Helper"));
    }

    #[test]
    fn rejects_wildcard_import_on_kotlin_cross_package_move() {
        let source = "package plugin\n\nclass Helper\n";
        let destination = "package other\n\nclass Existing\n";
        let caller =
            "package app\n\nimport plugin.*\n\nclass Caller {\n    val helper = Helper()\n}\n";
        let target =
            resolve_symbol_in_source("src/plugin/Hooks.kt", source, "Helper", None, "move")
                .unwrap();
        let references = vec![CodeReference {
            path: "src/app/Caller.kt".to_string(),
            language: Some("kotlin".to_string()),
            target_path: "src/plugin/Hooks.kt".to_string(),
            target_name: "Helper".to_string(),
            target_kind: "class".to_string(),
            kind: "instantiation".to_string(),
            line_start: 6,
            excerpt: "val helper = Helper()".to_string(),
        }];

        let error = plan_move(
            &target,
            &references,
            source,
            "src/other/Helper.kt",
            destination,
            vec![("src/app/Caller.kt".to_string(), caller.to_string())],
            true,
        )
        .unwrap_err();

        assert!(error.contains("wildcard or aliased"), "{error}");
    }

    #[test]
    fn rejects_kotlin_reference_aware_function_move() {
        let source = "package plugin\n\nfun helper(): Int = 1\n";
        let destination = "package plugin\n\nclass Existing\n";
        let caller = "package plugin\n\nfun useHelper(): Int = helper()\n";
        let target =
            resolve_symbol_in_source("src/plugin/Hooks.kt", source, "helper", None, "move")
                .unwrap();
        let references = vec![CodeReference {
            path: "src/plugin/Caller.kt".to_string(),
            language: Some("kotlin".to_string()),
            target_path: "src/plugin/Hooks.kt".to_string(),
            target_name: "helper".to_string(),
            target_kind: "function".to_string(),
            kind: "call".to_string(),
            line_start: 3,
            excerpt: "fun useHelper(): Int = helper()".to_string(),
        }];

        let error = plan_move(
            &target,
            &references,
            source,
            "src/plugin/Helpers.kt",
            destination,
            vec![("src/plugin/Caller.kt".to_string(), caller.to_string())],
            true,
        )
        .unwrap_err();

        assert!(error.contains("top-level type declarations"), "{error}");
    }

    #[test]
    fn plans_swift_same_module_type_move_with_references_without_text_rewrites() {
        let source = "struct Helper {\n    let value = 1\n}\n\nstruct Other {}\n";
        let destination = "struct Existing {}\n";
        let caller = "struct Caller {\n    let helper = Helper()\n}\n";
        let target =
            resolve_symbol_in_source("Sources/App/Hooks.swift", source, "Helper", None, "move")
                .unwrap();
        let references = vec![CodeReference {
            path: "Sources/App/Caller.swift".to_string(),
            language: Some("swift".to_string()),
            target_path: "Sources/App/Hooks.swift".to_string(),
            target_name: "Helper".to_string(),
            target_kind: "struct".to_string(),
            kind: "instantiation".to_string(),
            line_start: 2,
            excerpt: "let helper = Helper()".to_string(),
        }];

        let planned = plan_move(
            &target,
            &references,
            source,
            "Sources/App/Helper.swift",
            destination,
            vec![("Sources/App/Caller.swift".to_string(), caller.to_string())],
            true,
        )
        .unwrap();

        assert_eq!(planned.summary.rewritten_reference_count, 0);
        assert!(
            planned
                .files
                .iter()
                .all(|file| file.path != "Sources/App/Caller.swift")
        );
        assert!(
            planned
                .files
                .iter()
                .find(|file| file.path == "Sources/App/Helper.swift")
                .unwrap()
                .contents
                .contains("struct Helper")
        );
    }

    #[test]
    fn rejects_swift_reference_aware_move_across_module_directories() {
        let source = "struct Helper {}\n";
        let destination = "struct Existing {}\n";
        let caller = "struct Caller {\n    let helper = Helper()\n}\n";
        let target =
            resolve_symbol_in_source("Sources/App/Hooks.swift", source, "Helper", None, "move")
                .unwrap();
        let references = vec![CodeReference {
            path: "Sources/App/Caller.swift".to_string(),
            language: Some("swift".to_string()),
            target_path: "Sources/App/Hooks.swift".to_string(),
            target_name: "Helper".to_string(),
            target_kind: "struct".to_string(),
            kind: "instantiation".to_string(),
            line_start: 2,
            excerpt: "let helper = Helper()".to_string(),
        }];

        let error = plan_move(
            &target,
            &references,
            source,
            "Sources/Other/Helper.swift",
            destination,
            vec![("Sources/App/Caller.swift".to_string(), caller.to_string())],
            true,
        )
        .unwrap_err();

        assert!(error.contains("same module directory"), "{error}");
    }

    #[test]
    fn rejects_swift_reference_aware_function_move() {
        let source = "func helper() -> Int {\n    return 1\n}\n";
        let destination = "struct Existing {}\n";
        let caller = "func useHelper() -> Int {\n    return helper()\n}\n";
        let target =
            resolve_symbol_in_source("Sources/App/Hooks.swift", source, "helper", None, "move")
                .unwrap();
        let references = vec![CodeReference {
            path: "Sources/App/Caller.swift".to_string(),
            language: Some("swift".to_string()),
            target_path: "Sources/App/Hooks.swift".to_string(),
            target_name: "helper".to_string(),
            target_kind: "function".to_string(),
            kind: "call".to_string(),
            line_start: 2,
            excerpt: "return helper()".to_string(),
        }];

        let error = plan_move(
            &target,
            &references,
            source,
            "Sources/App/Helpers.swift",
            destination,
            vec![("Sources/App/Caller.swift".to_string(), caller.to_string())],
            true,
        )
        .unwrap_err();

        assert!(error.contains("type declarations"), "{error}");
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
            resolve_symbol_in_source("src/plugin_hooks.rs", source, "helper", None, "move")
                .unwrap();
        let references = vec![
            CodeReference {
                path: "src/main.rs".to_string(),
                language: Some("rust".to_string()),
                target_path: "src/plugin_hooks.rs".to_string(),
                target_name: "helper".to_string(),
                target_kind: "function".to_string(),
                kind: "import".to_string(),
                line_start: 1,
                excerpt: "use crate::plugin_hooks::helper;".to_string(),
            },
            CodeReference {
                path: "src/main.rs".to_string(),
                language: Some("rust".to_string()),
                target_path: "src/plugin_hooks.rs".to_string(),
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
            vec![(
                "src/main.rs".to_string(),
                caller.replace("crate::lib::helper", "crate::plugin_hooks::helper"),
            )],
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

    #[test]
    fn plans_move_with_braced_rust_import_rewrite() {
        let source = "pub fn helper() -> u8 {\n    1\n}\n\npub fn other() {}\n";
        let destination = "pub fn existing() {}\n";
        let caller =
            "use crate::plugin_hooks::{helper, other};\n\nfn main() {\n    let _ = helper();\n}\n";
        let target =
            resolve_symbol_in_source("src/plugin_hooks.rs", source, "helper", None, "move")
                .unwrap();
        let references = vec![
            CodeReference {
                path: "src/main.rs".to_string(),
                language: Some("rust".to_string()),
                target_path: "src/plugin_hooks.rs".to_string(),
                target_name: "helper".to_string(),
                target_kind: "function".to_string(),
                kind: "import".to_string(),
                line_start: 1,
                excerpt: "use crate::plugin_hooks::{helper, other};".to_string(),
            },
            CodeReference {
                path: "src/main.rs".to_string(),
                language: Some("rust".to_string()),
                target_path: "src/plugin_hooks.rs".to_string(),
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

        assert!(
            caller_file
                .contents
                .contains("use crate::plugin_hooks::{other};")
        );
        assert!(
            caller_file
                .contents
                .contains("use crate::helpers::{helper};")
        );
        assert!(caller_file.contents.contains("helper();"));
        assert_eq!(planned.summary.rewritten_reference_count, 1);
    }

    #[test]
    fn plans_move_with_braced_rust_import_rewrite_without_empty_old_import() {
        let source = "pub fn helper() -> u8 {\n    1\n}\n";
        let destination = "pub fn existing() {}\n";
        let caller =
            "use crate::plugin_hooks::{helper};\n\nfn main() {\n    let _ = helper();\n}\n";
        let target =
            resolve_symbol_in_source("src/plugin_hooks.rs", source, "helper", None, "move")
                .unwrap();
        let references = vec![CodeReference {
            path: "src/main.rs".to_string(),
            language: Some("rust".to_string()),
            target_path: "src/plugin_hooks.rs".to_string(),
            target_name: "helper".to_string(),
            target_kind: "function".to_string(),
            kind: "import".to_string(),
            line_start: 1,
            excerpt: "use crate::plugin_hooks::{helper};".to_string(),
        }];

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

        assert!(!caller_file.contents.contains("plugin_hooks::{}"));
        assert!(!caller_file.contents.contains("plugin_hooks::{"));
        assert!(
            caller_file
                .contents
                .contains("use crate::helpers::{helper};")
        );
        assert_eq!(planned.summary.rewritten_reference_count, 1);
    }

    #[test]
    fn plans_move_with_module_qualified_rust_call_rewrite() {
        let source = "pub fn helper() -> u8 {\n    1\n}\n";
        let destination = "pub fn existing() {}\n";
        let caller = "fn main() {\n    let _ = plugin_hooks::helper();\n}\n";
        let target =
            resolve_symbol_in_source("src/plugin_hooks.rs", source, "helper", None, "move")
                .unwrap();
        let references = vec![CodeReference {
            path: "src/main.rs".to_string(),
            language: Some("rust".to_string()),
            target_path: "src/plugin_hooks.rs".to_string(),
            target_name: "helper".to_string(),
            target_kind: "function".to_string(),
            kind: "call".to_string(),
            line_start: 2,
            excerpt: "let _ = plugin_hooks::helper();".to_string(),
        }];

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

        assert!(caller_file.contents.contains("crate::helpers::helper();"));
        assert!(!caller_file.contents.contains("plugin_hooks::helper"));
        assert_eq!(planned.summary.rewritten_reference_count, 1);
    }

    #[test]
    fn plans_move_with_nested_and_aliased_rust_import_rewrites() {
        let source = "pub fn helper() -> u8 {\n    1\n}\n\npub fn other() {}\n";
        let destination = "pub fn existing() {}\n";
        let caller = "use crate::{config::Settings, plugin_hooks::{helper as run_helper, other}, plugin_hooks as hooks};\n\nfn main() {\n    let _ = run_helper();\n    let _ = hooks::helper();\n}\n";
        let target =
            resolve_symbol_in_source("src/plugin_hooks.rs", source, "helper", None, "move")
                .unwrap();
        let references = vec![
            CodeReference {
                path: "src/main.rs".to_string(),
                language: Some("rust".to_string()),
                target_path: "src/plugin_hooks.rs".to_string(),
                target_name: "helper".to_string(),
                target_kind: "function".to_string(),
                kind: "import".to_string(),
                line_start: 1,
                excerpt: "use crate::{config::Settings, plugin_hooks::{helper as run_helper, other}, plugin_hooks as hooks};".to_string(),
            },
            CodeReference {
                path: "src/main.rs".to_string(),
                language: Some("rust".to_string()),
                target_path: "src/plugin_hooks.rs".to_string(),
                target_name: "helper".to_string(),
                target_kind: "function".to_string(),
                kind: "call".to_string(),
                line_start: 5,
                excerpt: "let _ = hooks::helper();".to_string(),
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

        assert!(caller_file.contents.contains(
            "use crate::{config::Settings, plugin_hooks::{other}, plugin_hooks as hooks};"
        ));
        assert!(
            caller_file
                .contents
                .contains("use crate::helpers::{helper as run_helper};")
        );
        assert!(caller_file.contents.contains("run_helper();"));
        assert!(caller_file.contents.contains("crate::helpers::helper();"));
        assert!(!caller_file.contents.contains("hooks::helper"));
        assert_eq!(planned.summary.rewritten_reference_count, 2);
    }
}
