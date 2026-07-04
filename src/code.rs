use crate::discovery::FileCandidate;
use std::collections::HashSet;
use std::fs;
use std::path::Path;
use tree_sitter::{Node, Parser};

const MAX_INDEX_BYTES: u64 = 1_000_000;
const MAX_SIGNATURE_CHARS: usize = 180;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CodeSymbol {
    pub path: String,
    pub language: Option<String>,
    pub name: String,
    pub kind: String,
    pub line_start: i64,
    pub line_end: Option<i64>,
    pub signature: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CodeReference {
    pub path: String,
    pub language: Option<String>,
    pub target_path: String,
    pub target_name: String,
    pub target_kind: String,
    pub kind: String,
    pub line_start: i64,
    pub excerpt: String,
}

/// Extract symbols from in-memory source without touching disk.
///
/// Structural edit helpers use this both to locate an edit target and to validate
/// that replacement source still defines the expected symbol. It reuses the same
/// tree-sitter pipelines that populate the index, so located ranges match what
/// `hugr symbols` and the context compiler already trust.
pub(crate) fn symbols_in_source(
    path: &str,
    language: Option<&str>,
    contents: &str,
) -> Result<Vec<CodeSymbol>, String> {
    extract_symbols(path, language, contents)
}

/// Report whether the tree-sitter grammar for `language` accepts `contents` without
/// error. Languages without a wired grammar return `Ok(true)` so lenient line-scanner
/// languages are not blocked. Structural edit validation uses this so a syntactically
/// broken replacement body is refused rather than silently accepted by the fallback
/// symbol scanner.
pub(crate) fn parses_cleanly(
    path: &str,
    language: Option<&str>,
    contents: &str,
) -> Result<bool, String> {
    let Some(grammar) = grammar_for(path, language) else {
        return Ok(true);
    };
    let mut parser = Parser::new();
    parser
        .set_language(&grammar)
        .map_err(|error| error.to_string())?;
    let Some(tree) = parser.parse(contents, None) else {
        return Ok(false);
    };
    Ok(!tree.root_node().has_error())
}

fn grammar_for(path: &str, language: Option<&str>) -> Option<tree_sitter::Language> {
    let grammar = match language? {
        "rust" => tree_sitter_rust::LANGUAGE.into(),
        "python" => tree_sitter_python::LANGUAGE.into(),
        "typescript" if path.ends_with(".tsx") => tree_sitter_typescript::LANGUAGE_TSX.into(),
        "typescript" => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        "javascript" => tree_sitter_typescript::LANGUAGE_TSX.into(),
        "go" => tree_sitter_go::LANGUAGE.into(),
        "java" => tree_sitter_java::LANGUAGE.into(),
        "kotlin" => tree_sitter_kotlin_ng::LANGUAGE.into(),
        "swift" => tree_sitter_swift::LANGUAGE.into(),
        _ => return None,
    };
    Some(grammar)
}

pub(crate) fn index_files(root: &Path, files: &[FileCandidate]) -> Result<Vec<CodeSymbol>, String> {
    let mut symbols = Vec::new();
    let mut seen = HashSet::new();

    for file in files {
        if !is_symbol_language(file.language.as_deref()) {
            continue;
        }

        let path = root.join(&file.path);
        let Ok(metadata) = fs::metadata(&path) else {
            continue;
        };
        if !metadata.is_file() || metadata.len() > MAX_INDEX_BYTES {
            continue;
        }

        let Ok(contents) = fs::read_to_string(&path) else {
            continue;
        };

        for symbol in extract_symbols(&file.path, file.language.as_deref(), &contents)? {
            let key = (
                symbol.path.clone(),
                symbol.kind.clone(),
                symbol.name.clone(),
                symbol.line_start,
            );
            if seen.insert(key) {
                symbols.push(symbol);
            }
        }
    }

    Ok(symbols)
}

pub(crate) fn extract_references(
    root: &Path,
    files: &[FileCandidate],
    symbols: &[CodeSymbol],
) -> Result<Vec<CodeReference>, String> {
    let targets = reference_targets(symbols);
    if targets.is_empty() {
        return Ok(Vec::new());
    }

    let mut references = Vec::new();
    let mut seen = HashSet::new();

    for file in files {
        if !is_symbol_language(file.language.as_deref()) {
            continue;
        }

        let path = root.join(&file.path);
        let Ok(metadata) = fs::metadata(&path) else {
            continue;
        };
        if !metadata.is_file() || metadata.len() > MAX_INDEX_BYTES {
            continue;
        }

        let Ok(contents) = fs::read_to_string(&path) else {
            continue;
        };

        for (index, line) in contents.lines().enumerate() {
            let line_number = i64::try_from(index + 1).map_err(|error| error.to_string())?;
            let trimmed = line.trim();
            if trimmed.is_empty() || is_comment_line(trimmed) {
                continue;
            }

            for target in &targets {
                if is_declaration_line(&targets, &file.path, line_number, target) {
                    continue;
                }
                if line_declares_target(trimmed, target) {
                    continue;
                }
                if !contains_identifier(trimmed, &target.name) {
                    continue;
                }

                let kind = reference_kind(trimmed, target);
                let key = (
                    file.path.clone(),
                    target.path.clone(),
                    target.name.clone(),
                    line_number,
                    kind.clone(),
                );
                if seen.insert(key) {
                    references.push(CodeReference {
                        path: file.path.clone(),
                        language: file.language.clone(),
                        target_path: target.path.clone(),
                        target_name: target.name.clone(),
                        target_kind: target.kind.clone(),
                        kind,
                        line_start: line_number,
                        excerpt: clean_signature(trimmed),
                    });
                }
            }
        }
    }

    Ok(references)
}

fn extract_symbols(
    path: &str,
    language: Option<&str>,
    contents: &str,
) -> Result<Vec<CodeSymbol>, String> {
    if matches!(language, Some("rust")) {
        let symbols = extract_rust_symbols_with_tree_sitter(path, contents)?;
        if !symbols.is_empty() {
            return Ok(symbols);
        }
    }
    if matches!(language, Some("python")) {
        let symbols = extract_python_symbols_with_tree_sitter(path, contents)?;
        if !symbols.is_empty() {
            return Ok(symbols);
        }
    }
    if matches!(language, Some("typescript")) {
        let symbols = extract_typescript_symbols_with_tree_sitter(path, contents)?;
        if !symbols.is_empty() {
            return Ok(symbols);
        }
    }
    if matches!(language, Some("javascript")) {
        let symbols = extract_javascript_symbols_with_tree_sitter(path, contents)?;
        if !symbols.is_empty() {
            return Ok(symbols);
        }
    }
    if matches!(language, Some("go")) {
        let symbols = extract_go_symbols_with_tree_sitter(path, contents)?;
        if !symbols.is_empty() {
            return Ok(symbols);
        }
    }
    if matches!(language, Some("java")) {
        let symbols = extract_java_symbols_with_tree_sitter(path, contents)?;
        if !symbols.is_empty() {
            return Ok(symbols);
        }
    }
    if matches!(language, Some("kotlin")) {
        let symbols = extract_kotlin_symbols_with_tree_sitter(path, contents)?;
        if !symbols.is_empty() {
            return Ok(symbols);
        }
    }
    if matches!(language, Some("swift")) {
        let symbols = extract_swift_symbols_with_tree_sitter(path, contents)?;
        if !symbols.is_empty() {
            return Ok(symbols);
        }
    }

    extract_symbols_from_lines(path, language, contents)
}

fn extract_symbols_from_lines(
    path: &str,
    language: Option<&str>,
    contents: &str,
) -> Result<Vec<CodeSymbol>, String> {
    let mut symbols = Vec::new();
    let line_count = i64::try_from(contents.lines().count()).map_err(|error| error.to_string())?;

    for (index, line) in contents.lines().enumerate() {
        let line_number = i64::try_from(index + 1).map_err(|error| error.to_string())?;
        let trimmed = line.trim();
        if trimmed.is_empty() || is_comment_line(trimmed) {
            continue;
        }

        let extracted = match language {
            Some("rust") => extract_rust_symbol(trimmed),
            Some("python") => extract_python_symbol(trimmed),
            Some("javascript") | Some("typescript") => extract_javascript_symbol(trimmed),
            Some("go") => extract_go_symbol(trimmed),
            Some("swift") | Some("kotlin") | Some("java") | Some("c") | Some("cpp") => {
                extract_c_family_symbol(trimmed)
            }
            _ => None,
        };

        if let Some((kind, name)) = extracted {
            symbols.push(CodeSymbol {
                path: path.to_string(),
                language: language.map(str::to_string),
                name,
                kind,
                line_start: line_number,
                line_end: Some(line_number),
                signature: clean_signature(trimmed),
            });
        }
    }

    assign_symbol_ranges(&mut symbols, line_count);
    Ok(symbols)
}

fn extract_rust_symbols_with_tree_sitter(
    path: &str,
    contents: &str,
) -> Result<Vec<CodeSymbol>, String> {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_rust::LANGUAGE.into())
        .map_err(|error| error.to_string())?;
    let Some(tree) = parser.parse(contents, None) else {
        return Ok(Vec::new());
    };
    let root = tree.root_node();
    if root.has_error() {
        return Ok(Vec::new());
    }

    let mut symbols = Vec::new();
    collect_rust_symbols(path, contents, root, &mut symbols)?;
    symbols.sort_by(|left, right| {
        left.line_start
            .cmp(&right.line_start)
            .then_with(|| left.name.cmp(&right.name))
    });
    Ok(symbols)
}

fn collect_rust_symbols(
    path: &str,
    contents: &str,
    node: Node<'_>,
    symbols: &mut Vec<CodeSymbol>,
) -> Result<(), String> {
    if let Some(symbol) = rust_symbol_from_node(path, contents, node)? {
        symbols.push(symbol);
    }

    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_rust_symbols(path, contents, child, symbols)?;
    }

    Ok(())
}

fn rust_symbol_from_node(
    path: &str,
    contents: &str,
    node: Node<'_>,
) -> Result<Option<CodeSymbol>, String> {
    let (kind, name_node) = match node.kind() {
        "function_item" => ("function", node.child_by_field_name("name")),
        "struct_item" => ("struct", node.child_by_field_name("name")),
        "enum_item" => ("enum", node.child_by_field_name("name")),
        "trait_item" => ("trait", node.child_by_field_name("name")),
        "mod_item" => ("module", node.child_by_field_name("name")),
        "type_item" => ("type", node.child_by_field_name("name")),
        "const_item" => ("constant", node.child_by_field_name("name")),
        "static_item" => ("static", node.child_by_field_name("name")),
        "impl_item" => ("impl", rust_impl_target_node(node)),
        _ => return Ok(None),
    };
    let Some(name_node) = name_node else {
        return Ok(None);
    };
    let name = node_text(name_node, contents)?;

    Ok(Some(CodeSymbol {
        path: path.to_string(),
        language: Some("rust".to_string()),
        name,
        kind: kind.to_string(),
        line_start: line_number(node.start_position().row),
        line_end: Some(line_number(node.end_position().row)),
        signature: rust_signature(node, contents),
    }))
}

fn rust_impl_target_node(node: Node<'_>) -> Option<Node<'_>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor).find(|child| {
        matches!(
            child.kind(),
            "type_identifier" | "scoped_type_identifier" | "generic_type" | "reference_type"
        )
    })
}

fn rust_signature(node: Node<'_>, contents: &str) -> String {
    let start = node.start_byte();
    let end = contents[node.start_byte()..node.end_byte()]
        .find('{')
        .map(|offset| start + offset)
        .unwrap_or_else(|| node.end_byte());
    clean_signature(&contents[start..end])
}

fn node_text(node: Node<'_>, contents: &str) -> Result<String, String> {
    node.utf8_text(contents.as_bytes())
        .map(str::trim)
        .map(str::to_string)
        .map_err(|error| error.to_string())
}

fn line_number(row: usize) -> i64 {
    i64::try_from(row + 1).unwrap_or(i64::MAX)
}

fn extract_python_symbols_with_tree_sitter(
    path: &str,
    contents: &str,
) -> Result<Vec<CodeSymbol>, String> {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_python::LANGUAGE.into())
        .map_err(|error| error.to_string())?;
    let Some(tree) = parser.parse(contents, None) else {
        return Ok(Vec::new());
    };
    let root = tree.root_node();
    if root.has_error() {
        return Ok(Vec::new());
    }

    let mut symbols = Vec::new();
    collect_python_symbols(path, contents, root, &mut symbols)?;
    symbols.sort_by(|left, right| {
        left.line_start
            .cmp(&right.line_start)
            .then_with(|| left.name.cmp(&right.name))
    });
    Ok(symbols)
}

fn collect_python_symbols(
    path: &str,
    contents: &str,
    node: Node<'_>,
    symbols: &mut Vec<CodeSymbol>,
) -> Result<(), String> {
    if let Some(symbol) = python_symbol_from_node(path, contents, node)? {
        symbols.push(symbol);
    }

    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_python_symbols(path, contents, child, symbols)?;
    }

    Ok(())
}

fn python_symbol_from_node(
    path: &str,
    contents: &str,
    node: Node<'_>,
) -> Result<Option<CodeSymbol>, String> {
    let (kind, name_node) = match node.kind() {
        "function_definition" => ("function", node.child_by_field_name("name")),
        "class_definition" => ("class", node.child_by_field_name("name")),
        _ => return Ok(None),
    };
    let Some(name_node) = name_node else {
        return Ok(None);
    };
    let name = node_text(name_node, contents)?;

    Ok(Some(CodeSymbol {
        path: path.to_string(),
        language: Some("python".to_string()),
        name,
        kind: kind.to_string(),
        line_start: line_number(node.start_position().row),
        line_end: Some(line_number(node.end_position().row)),
        signature: python_signature(node, contents),
    }))
}

fn python_signature(node: Node<'_>, contents: &str) -> String {
    contents
        .lines()
        .nth(node.start_position().row)
        .map(|line| clean_signature(line.trim_end_matches(':')))
        .unwrap_or_else(|| clean_signature(&contents[node.start_byte()..node.end_byte()]))
}

fn extract_typescript_symbols_with_tree_sitter(
    path: &str,
    contents: &str,
) -> Result<Vec<CodeSymbol>, String> {
    let mut parser = Parser::new();
    if path.ends_with(".tsx") {
        parser
            .set_language(&tree_sitter_typescript::LANGUAGE_TSX.into())
            .map_err(|error| error.to_string())?;
    } else {
        parser
            .set_language(&tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into())
            .map_err(|error| error.to_string())?;
    }
    let Some(tree) = parser.parse(contents, None) else {
        return Ok(Vec::new());
    };
    let root = tree.root_node();
    if root.has_error() {
        return Ok(Vec::new());
    }

    let mut symbols = Vec::new();
    collect_typescript_symbols(path, contents, root, &mut symbols)?;
    symbols.sort_by(|left, right| {
        left.line_start
            .cmp(&right.line_start)
            .then_with(|| left.name.cmp(&right.name))
    });
    Ok(symbols)
}

fn collect_typescript_symbols(
    path: &str,
    contents: &str,
    node: Node<'_>,
    symbols: &mut Vec<CodeSymbol>,
) -> Result<(), String> {
    if let Some(symbol) = typescript_symbol_from_node(path, contents, node)? {
        symbols.push(symbol);
    }

    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_typescript_symbols(path, contents, child, symbols)?;
    }

    Ok(())
}

fn typescript_symbol_from_node(
    path: &str,
    contents: &str,
    node: Node<'_>,
) -> Result<Option<CodeSymbol>, String> {
    let (kind, name_node) = match node.kind() {
        "abstract_class_declaration" | "class_declaration" => {
            ("class", node.child_by_field_name("name"))
        }
        "enum_declaration" => ("enum", node.child_by_field_name("name")),
        "function_declaration" | "generator_function_declaration" | "method_definition" => {
            ("function", node.child_by_field_name("name"))
        }
        "interface_declaration" => ("interface", node.child_by_field_name("name")),
        "type_alias_declaration" => ("type", node.child_by_field_name("name")),
        "variable_declarator" => typescript_function_variable(node),
        _ => return Ok(None),
    };
    let Some(name_node) = name_node else {
        return Ok(None);
    };
    let name = node_text(name_node, contents)?;

    Ok(Some(CodeSymbol {
        path: path.to_string(),
        language: Some("typescript".to_string()),
        name,
        kind: kind.to_string(),
        line_start: line_number(node.start_position().row),
        line_end: Some(line_number(node.end_position().row)),
        signature: line_signature(node, contents),
    }))
}

fn typescript_function_variable(node: Node<'_>) -> (&'static str, Option<Node<'_>>) {
    let value = node.child_by_field_name("value");
    let is_function = value.is_some_and(|value| {
        matches!(
            value.kind(),
            "arrow_function" | "function_expression" | "generator_function"
        )
    });
    if !is_function {
        return ("variable", None);
    }

    let name = node
        .child_by_field_name("name")
        .filter(|name| name.kind() == "identifier");
    ("function", name)
}

fn line_signature(node: Node<'_>, contents: &str) -> String {
    contents
        .lines()
        .nth(node.start_position().row)
        .map(clean_signature)
        .unwrap_or_else(|| clean_signature(&contents[node.start_byte()..node.end_byte()]))
}

fn extract_javascript_symbols_with_tree_sitter(
    path: &str,
    contents: &str,
) -> Result<Vec<CodeSymbol>, String> {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_typescript::LANGUAGE_TSX.into())
        .map_err(|error| error.to_string())?;
    let Some(tree) = parser.parse(contents, None) else {
        return Ok(Vec::new());
    };
    let root = tree.root_node();
    if root.has_error() {
        return Ok(Vec::new());
    }

    let mut symbols = Vec::new();
    collect_typescript_symbols(path, contents, root, &mut symbols)?;
    for symbol in &mut symbols {
        symbol.language = Some("javascript".to_string());
    }
    symbols.sort_by(|left, right| {
        left.line_start
            .cmp(&right.line_start)
            .then_with(|| left.name.cmp(&right.name))
    });
    Ok(symbols)
}

fn extract_go_symbols_with_tree_sitter(
    path: &str,
    contents: &str,
) -> Result<Vec<CodeSymbol>, String> {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_go::LANGUAGE.into())
        .map_err(|error| error.to_string())?;
    let Some(tree) = parser.parse(contents, None) else {
        return Ok(Vec::new());
    };
    let root = tree.root_node();
    if root.has_error() {
        return Ok(Vec::new());
    }

    let mut symbols = Vec::new();
    collect_go_symbols(path, contents, root, &mut symbols)?;
    symbols.sort_by(|left, right| {
        left.line_start
            .cmp(&right.line_start)
            .then_with(|| left.name.cmp(&right.name))
    });
    Ok(symbols)
}

fn collect_go_symbols(
    path: &str,
    contents: &str,
    node: Node<'_>,
    symbols: &mut Vec<CodeSymbol>,
) -> Result<(), String> {
    if let Some(symbol) = go_symbol_from_node(path, contents, node)? {
        symbols.push(symbol);
    }

    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_go_symbols(path, contents, child, symbols)?;
    }

    Ok(())
}

fn go_symbol_from_node(
    path: &str,
    contents: &str,
    node: Node<'_>,
) -> Result<Option<CodeSymbol>, String> {
    let (kind, name_node) = match node.kind() {
        "function_declaration" | "method_declaration" => {
            ("function", node.child_by_field_name("name"))
        }
        "type_alias" => ("type", node.child_by_field_name("name")),
        "type_spec" => (go_type_spec_kind(node), node.child_by_field_name("name")),
        _ => return Ok(None),
    };
    let Some(name_node) = name_node else {
        return Ok(None);
    };
    let name = node_text(name_node, contents)?;

    Ok(Some(CodeSymbol {
        path: path.to_string(),
        language: Some("go".to_string()),
        name,
        kind: kind.to_string(),
        line_start: line_number(node.start_position().row),
        line_end: Some(line_number(node.end_position().row)),
        signature: line_signature(node, contents),
    }))
}

fn go_type_spec_kind(node: Node<'_>) -> &'static str {
    match node.child_by_field_name("type").map(|node| node.kind()) {
        Some("struct_type") => "struct",
        Some("interface_type") => "interface",
        _ => "type",
    }
}

fn extract_java_symbols_with_tree_sitter(
    path: &str,
    contents: &str,
) -> Result<Vec<CodeSymbol>, String> {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_java::LANGUAGE.into())
        .map_err(|error| error.to_string())?;
    let Some(tree) = parser.parse(contents, None) else {
        return Ok(Vec::new());
    };
    let root = tree.root_node();
    if root.has_error() {
        return Ok(Vec::new());
    }

    let mut symbols = Vec::new();
    collect_java_symbols(path, contents, root, &mut symbols)?;
    symbols.sort_by(|left, right| {
        left.line_start
            .cmp(&right.line_start)
            .then_with(|| left.name.cmp(&right.name))
    });
    Ok(symbols)
}

fn collect_java_symbols(
    path: &str,
    contents: &str,
    node: Node<'_>,
    symbols: &mut Vec<CodeSymbol>,
) -> Result<(), String> {
    if let Some(symbol) = java_symbol_from_node(path, contents, node)? {
        symbols.push(symbol);
    }

    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_java_symbols(path, contents, child, symbols)?;
    }

    Ok(())
}

fn java_symbol_from_node(
    path: &str,
    contents: &str,
    node: Node<'_>,
) -> Result<Option<CodeSymbol>, String> {
    let (kind, name_node) = match node.kind() {
        "annotation_type_declaration" => ("annotation", node.child_by_field_name("name")),
        "class_declaration" => ("class", node.child_by_field_name("name")),
        "constructor_declaration" => ("function", node.child_by_field_name("name")),
        "enum_declaration" => ("enum", node.child_by_field_name("name")),
        "interface_declaration" => ("interface", node.child_by_field_name("name")),
        "method_declaration" => ("function", node.child_by_field_name("name")),
        "record_declaration" => ("record", node.child_by_field_name("name")),
        _ => return Ok(None),
    };
    let Some(name_node) = name_node else {
        return Ok(None);
    };
    let name = node_text(name_node, contents)?;

    Ok(Some(CodeSymbol {
        path: path.to_string(),
        language: Some("java".to_string()),
        name,
        kind: kind.to_string(),
        line_start: line_number(node.start_position().row),
        line_end: Some(line_number(node.end_position().row)),
        signature: line_signature(node, contents),
    }))
}

fn extract_kotlin_symbols_with_tree_sitter(
    path: &str,
    contents: &str,
) -> Result<Vec<CodeSymbol>, String> {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_kotlin_ng::LANGUAGE.into())
        .map_err(|error| error.to_string())?;
    let Some(tree) = parser.parse(contents, None) else {
        return Ok(Vec::new());
    };
    let root = tree.root_node();
    if root.has_error() {
        return Ok(Vec::new());
    }

    let mut symbols = Vec::new();
    collect_kotlin_symbols(path, contents, root, &mut symbols)?;
    symbols.sort_by(|left, right| {
        left.line_start
            .cmp(&right.line_start)
            .then_with(|| left.name.cmp(&right.name))
    });
    Ok(symbols)
}

fn collect_kotlin_symbols(
    path: &str,
    contents: &str,
    node: Node<'_>,
    symbols: &mut Vec<CodeSymbol>,
) -> Result<(), String> {
    if let Some(symbol) = kotlin_symbol_from_node(path, contents, node)? {
        symbols.push(symbol);
    }

    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_kotlin_symbols(path, contents, child, symbols)?;
    }

    Ok(())
}

fn kotlin_symbol_from_node(
    path: &str,
    contents: &str,
    node: Node<'_>,
) -> Result<Option<CodeSymbol>, String> {
    let (kind, name) = match node.kind() {
        "class_declaration" => {
            let Some(name_node) = node.child_by_field_name("name") else {
                return Ok(None);
            };
            (
                kotlin_class_declaration_kind(node, contents),
                node_text(name_node, contents)?,
            )
        }
        "companion_object" => {
            let name = node
                .child_by_field_name("name")
                .map(|name_node| node_text(name_node, contents))
                .transpose()?
                .unwrap_or_else(|| "Companion".to_string());
            ("object", name)
        }
        "function_declaration" => {
            let Some(name_node) = node.child_by_field_name("name") else {
                return Ok(None);
            };
            ("function", node_text(name_node, contents)?)
        }
        "object_declaration" => {
            let Some(name_node) = node.child_by_field_name("name") else {
                return Ok(None);
            };
            ("object", node_text(name_node, contents)?)
        }
        "secondary_constructor" => ("function", "constructor".to_string()),
        "type_alias" => {
            let Some(name_node) = node.child_by_field_name("type") else {
                return Ok(None);
            };
            ("type", node_text(name_node, contents)?)
        }
        _ => return Ok(None),
    };

    Ok(Some(CodeSymbol {
        path: path.to_string(),
        language: Some("kotlin".to_string()),
        name,
        kind: kind.to_string(),
        line_start: line_number(node.start_position().row),
        line_end: Some(line_number(node.end_position().row)),
        signature: line_signature(node, contents),
    }))
}

fn kotlin_class_declaration_kind(node: Node<'_>, contents: &str) -> &'static str {
    let signature = line_signature(node, contents);
    let declaration = strip_leading_modifiers(&signature);
    if keyword_remainder(declaration, "enum")
        .and_then(|rest| keyword_remainder(rest, "class"))
        .is_some()
    {
        "enum"
    } else if keyword_remainder(declaration, "annotation")
        .and_then(|rest| keyword_remainder(rest, "class"))
        .is_some()
    {
        "annotation"
    } else if keyword_remainder(declaration, "fun")
        .and_then(|rest| keyword_remainder(rest, "interface"))
        .is_some()
        || keyword_remainder(declaration, "interface").is_some()
    {
        "interface"
    } else {
        "class"
    }
}

fn extract_swift_symbols_with_tree_sitter(
    path: &str,
    contents: &str,
) -> Result<Vec<CodeSymbol>, String> {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_swift::LANGUAGE.into())
        .map_err(|error| error.to_string())?;
    let Some(tree) = parser.parse(contents, None) else {
        return Ok(Vec::new());
    };
    let root = tree.root_node();
    if root.has_error() {
        return Ok(Vec::new());
    }

    let mut symbols = Vec::new();
    collect_swift_symbols(path, contents, root, &mut symbols)?;
    symbols.sort_by(|left, right| {
        left.line_start
            .cmp(&right.line_start)
            .then_with(|| left.name.cmp(&right.name))
    });
    Ok(symbols)
}

fn collect_swift_symbols(
    path: &str,
    contents: &str,
    node: Node<'_>,
    symbols: &mut Vec<CodeSymbol>,
) -> Result<(), String> {
    if let Some(symbol) = swift_symbol_from_node(path, contents, node)? {
        symbols.push(symbol);
    }

    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_swift_symbols(path, contents, child, symbols)?;
    }

    Ok(())
}

fn swift_symbol_from_node(
    path: &str,
    contents: &str,
    node: Node<'_>,
) -> Result<Option<CodeSymbol>, String> {
    let (kind, name) = match node.kind() {
        "associatedtype_declaration" | "typealias_declaration" => {
            let Some(name_node) = node.child_by_field_name("name") else {
                return Ok(None);
            };
            ("type", node_text(name_node, contents)?)
        }
        "class_declaration" => {
            let Some(name_node) = node.child_by_field_name("name") else {
                return Ok(None);
            };
            (
                swift_type_declaration_kind(node, contents),
                node_text(name_node, contents)?,
            )
        }
        "deinit_declaration" => ("function", "deinit".to_string()),
        "function_declaration" | "protocol_function_declaration" => {
            let Some(name_node) = node.child_by_field_name("name") else {
                return Ok(None);
            };
            ("function", node_text(name_node, contents)?)
        }
        "init_declaration" => ("function", "init".to_string()),
        "protocol_declaration" => {
            let Some(name_node) = node.child_by_field_name("name") else {
                return Ok(None);
            };
            ("protocol", node_text(name_node, contents)?)
        }
        _ => return Ok(None),
    };

    Ok(Some(CodeSymbol {
        path: path.to_string(),
        language: Some("swift".to_string()),
        name,
        kind: kind.to_string(),
        line_start: line_number(node.start_position().row),
        line_end: Some(line_number(node.end_position().row)),
        signature: line_signature(node, contents),
    }))
}

fn swift_type_declaration_kind(node: Node<'_>, contents: &str) -> &'static str {
    if let Some(kind_node) = node.child_by_field_name("declaration_kind") {
        return match kind_node.kind() {
            "actor" => "actor",
            "enum" => "enum",
            "extension" => "extension",
            "struct" => "struct",
            _ => "class",
        };
    }

    let signature = line_signature(node, contents);
    let declaration = strip_leading_modifiers(&signature);
    if keyword_remainder(declaration, "actor").is_some() {
        "actor"
    } else if keyword_remainder(declaration, "enum").is_some() {
        "enum"
    } else if keyword_remainder(declaration, "extension").is_some() {
        "extension"
    } else if keyword_remainder(declaration, "struct").is_some() {
        "struct"
    } else {
        "class"
    }
}

fn extract_rust_symbol(line: &str) -> Option<(String, String)> {
    let line = strip_prefix_words(
        line,
        &[
            "pub", "async", "unsafe", "const", "extern", "default", "crate",
        ],
    );

    if let Some(name) = rust_impl_name(line) {
        return Some(("impl".to_string(), name));
    }

    for (keyword, kind) in [
        ("fn", "function"),
        ("struct", "struct"),
        ("enum", "enum"),
        ("trait", "trait"),
        ("mod", "module"),
        ("type", "type"),
        ("const", "constant"),
        ("static", "static"),
    ] {
        if let Some(name) = name_after_keyword(line, keyword) {
            return Some((kind.to_string(), name));
        }
    }

    None
}

fn extract_python_symbol(line: &str) -> Option<(String, String)> {
    let line = strip_prefix_words(line, &["async"]);
    name_after_keyword(line, "def")
        .map(|name| ("function".to_string(), name))
        .or_else(|| name_after_keyword(line, "class").map(|name| ("class".to_string(), name)))
}

fn extract_javascript_symbol(line: &str) -> Option<(String, String)> {
    let line = strip_prefix_words(line, &["export", "default", "async", "declare", "abstract"]);

    for (keyword, kind) in [
        ("function", "function"),
        ("class", "class"),
        ("interface", "interface"),
        ("type", "type"),
        ("enum", "enum"),
    ] {
        if let Some(name) = name_after_keyword(line, keyword) {
            return Some((kind.to_string(), name));
        }
    }

    for keyword in ["const", "let", "var"] {
        if let Some(name) = name_after_keyword(line, keyword) {
            let kind = if line.contains("=>") || line.contains("function") {
                "function"
            } else {
                "variable"
            };
            return Some((kind.to_string(), name));
        }
    }

    None
}

fn extract_go_symbol(line: &str) -> Option<(String, String)> {
    if let Some(rest) = keyword_remainder(line, "func") {
        let name = if rest.starts_with('(') {
            rest.find(')')
                .map(|index| rest[index + 1..].trim())
                .and_then(first_identifier)
        } else {
            first_identifier(rest)
        };
        if let Some(name) = name {
            return Some(("function".to_string(), name));
        }
    }

    if let Some(rest) = keyword_remainder(line, "type") {
        let name = first_identifier(rest)?;
        let kind = if rest.contains(" struct") {
            "struct"
        } else if rest.contains(" interface") {
            "interface"
        } else {
            "type"
        };
        return Some((kind.to_string(), name));
    }

    None
}

fn extract_c_family_symbol(line: &str) -> Option<(String, String)> {
    let line = strip_prefix_words(
        line,
        &[
            "public",
            "private",
            "protected",
            "internal",
            "open",
            "final",
            "static",
            "abstract",
            "export",
            "async",
            "inline",
            "data",
            "sealed",
            "inner",
            "value",
            "infix",
            "operator",
            "suspend",
            "tailrec",
            "external",
            "lateinit",
            "const",
            "mutating",
        ],
    );

    for (keyword, kind) in [
        ("func", "function"),
        ("fun", "function"),
        ("function", "function"),
        ("class", "class"),
        ("struct", "struct"),
        ("enum", "enum"),
        ("interface", "interface"),
        ("protocol", "protocol"),
        ("object", "object"),
        ("record", "record"),
    ] {
        if let Some(name) = name_after_keyword(line, keyword) {
            return Some((kind.to_string(), name));
        }
    }

    None
}

fn is_symbol_language(language: Option<&str>) -> bool {
    matches!(
        language,
        Some(
            "rust"
                | "python"
                | "javascript"
                | "typescript"
                | "go"
                | "swift"
                | "kotlin"
                | "java"
                | "c"
                | "cpp"
        )
    )
}

fn is_comment_line(line: &str) -> bool {
    line.starts_with("//")
        || line.starts_with('#')
        || line.starts_with("/*")
        || line.starts_with('*')
}

fn rust_impl_name(line: &str) -> Option<String> {
    let rest = keyword_remainder(line, "impl")?;
    let rest = rest
        .split('{')
        .next()
        .unwrap_or(rest)
        .split(" where ")
        .next()
        .unwrap_or(rest)
        .trim();
    if rest.is_empty() {
        None
    } else {
        Some(rest.to_string())
    }
}

fn strip_prefix_words<'a>(mut line: &'a str, prefixes: &[&str]) -> &'a str {
    loop {
        let trimmed = line.trim_start();
        let mut stripped = false;

        if let Some(rest) = trimmed.strip_prefix("pub(") {
            if let Some(end) = rest.find(')') {
                line = rest[end + 1..].trim_start();
                stripped = true;
            }
        }

        if !stripped {
            for prefix in prefixes {
                if let Some(rest) = keyword_remainder(trimmed, prefix) {
                    line = rest;
                    stripped = true;
                    break;
                }
            }
        }

        if !stripped {
            return trimmed;
        }
    }
}

fn name_after_keyword(line: &str, keyword: &str) -> Option<String> {
    keyword_remainder(line, keyword).and_then(first_identifier)
}

fn keyword_remainder<'a>(line: &'a str, keyword: &str) -> Option<&'a str> {
    let line = line.trim_start();
    let rest = line.strip_prefix(keyword)?;
    if rest
        .chars()
        .next()
        .is_some_and(|char| char.is_alphanumeric() || char == '_')
    {
        return None;
    }
    Some(rest.trim_start())
}

fn first_identifier(value: &str) -> Option<String> {
    let value = value.trim_start();
    let mut identifier = String::new();

    for char in value.chars() {
        if identifier.is_empty() && !(char.is_alphabetic() || char == '_') {
            return None;
        }
        if char.is_alphanumeric() || char == '_' || char == '$' {
            identifier.push(char);
        } else {
            break;
        }
    }

    if identifier.is_empty() {
        None
    } else {
        Some(identifier)
    }
}

fn clean_signature(line: &str) -> String {
    let mut signature = line.trim().trim_end_matches('{').trim().to_string();
    if signature.chars().count() > MAX_SIGNATURE_CHARS {
        signature = signature
            .chars()
            .take(MAX_SIGNATURE_CHARS)
            .collect::<String>();
    }
    signature
}

fn reference_targets(symbols: &[CodeSymbol]) -> Vec<CodeSymbol> {
    let mut seen = HashSet::new();
    let mut targets = Vec::new();

    for symbol in symbols {
        if symbol.name.chars().count() < 3 {
            continue;
        }
        let key = (
            symbol.path.clone(),
            symbol.kind.clone(),
            symbol.name.clone(),
            symbol.line_start,
        );
        if seen.insert(key) {
            targets.push(symbol.clone());
        }
    }

    targets
}

fn assign_symbol_ranges(symbols: &mut [CodeSymbol], line_count: i64) {
    for index in 0..symbols.len() {
        let next_start = symbols.get(index + 1).map(|symbol| symbol.line_start);
        let line_end = next_start
            .map(|line| line.saturating_sub(1))
            .unwrap_or(line_count)
            .max(symbols[index].line_start);
        symbols[index].line_end = Some(line_end);
    }
}

fn reference_kind(line: &str, target: &CodeSymbol) -> String {
    if is_import_line(line) {
        "import".to_string()
    } else if is_implementation_line(line, target) {
        "implementation".to_string()
    } else if is_inheritance_line(line, target) {
        "inheritance".to_string()
    } else if is_type_reference_line(line, target) {
        "type_reference".to_string()
    } else if is_instantiation_line(line, target) {
        "instantiation".to_string()
    } else if target.kind == "function" && contains_member_call(line, &target.name) {
        "method_call".to_string()
    } else if target.kind == "function" && contains_call(line, &target.name) {
        "call".to_string()
    } else {
        "reference".to_string()
    }
}

fn is_declaration_line(
    symbols: &[CodeSymbol],
    path: &str,
    line_number: i64,
    target: &CodeSymbol,
) -> bool {
    symbols.iter().any(|symbol| {
        symbol.path == path
            && symbol.name == target.name
            && symbol.kind == target.kind
            && symbol.line_start == line_number
    })
}

fn line_declares_target(line: &str, target: &CodeSymbol) -> bool {
    let declaration = strip_leading_modifiers(line);
    match target.kind.as_str() {
        "function" => {
            starts_with_keyword_name(declaration, "fn", &target.name)
                || starts_with_keyword_name(declaration, "def", &target.name)
                || starts_with_keyword_name(declaration, "func", &target.name)
                || starts_with_keyword_name(declaration, "fun", &target.name)
                || starts_with_keyword_name(declaration, "function", &target.name)
        }
        "actor" => starts_with_keyword_name(declaration, "actor", &target.name),
        "annotation" => starts_with_keyword_name(declaration, "annotation", &target.name),
        "struct" => {
            starts_with_keyword_name(declaration, "struct", &target.name)
                || starts_with_keyword_name(declaration, "type", &target.name)
        }
        "class" => starts_with_keyword_name(declaration, "class", &target.name),
        "extension" => starts_with_keyword_name(declaration, "extension", &target.name),
        "interface" => starts_with_keyword_name(declaration, "interface", &target.name),
        "object" => starts_with_keyword_name(declaration, "object", &target.name),
        "protocol" => starts_with_keyword_name(declaration, "protocol", &target.name),
        "record" => starts_with_keyword_name(declaration, "record", &target.name),
        "trait" => starts_with_keyword_name(declaration, "trait", &target.name),
        "enum" => starts_with_keyword_name(declaration, "enum", &target.name),
        "type" => starts_with_keyword_name(declaration, "type", &target.name),
        _ => false,
    }
}

fn strip_leading_modifiers(line: &str) -> &str {
    let mut rest = line.trim_start();
    loop {
        let Some((token, after_token)) = split_first_token(rest) else {
            return rest;
        };
        if !is_declaration_modifier(token) {
            return rest;
        }
        rest = after_token.trim_start();
    }
}

fn split_first_token(value: &str) -> Option<(&str, &str)> {
    let split_index = value.find(char::is_whitespace)?;
    Some((&value[..split_index], &value[split_index..]))
}

fn is_declaration_modifier(token: &str) -> bool {
    token.starts_with("pub(")
        || matches!(
            token,
            "pub"
                | "async"
                | "unsafe"
                | "const"
                | "export"
                | "default"
                | "public"
                | "private"
                | "protected"
                | "static"
                | "final"
                | "abstract"
                | "open"
                | "override"
                | "data"
                | "sealed"
                | "inner"
                | "value"
                | "inline"
                | "infix"
                | "operator"
                | "suspend"
                | "tailrec"
                | "external"
                | "lateinit"
        )
}

fn starts_with_keyword_name(line: &str, keyword: &str, name: &str) -> bool {
    let Some(rest) = line.strip_prefix(keyword) else {
        return false;
    };
    if !rest.starts_with(char::is_whitespace) {
        return false;
    }

    let rest = rest.trim_start();
    rest.strip_prefix(name)
        .and_then(|after_name| after_name.chars().next())
        .is_some_and(|char| !is_identifier_char(char))
}

fn is_import_line(line: &str) -> bool {
    let line = line.trim_start();
    line.starts_with("use ")
        || line.starts_with("import ")
        || line.starts_with("from ")
        || line.starts_with("require(")
}

fn contains_call(line: &str, name: &str) -> bool {
    line.match_indices(name).any(|(index, _)| {
        has_identifier_boundaries(line, index, name)
            && line[index + name.len()..]
                .trim_start_matches(char::is_whitespace)
                .starts_with('(')
    })
}

fn contains_member_call(line: &str, name: &str) -> bool {
    line.match_indices(name).any(|(index, _)| {
        if !has_identifier_boundaries(line, index, name) {
            return false;
        }
        if !line[index + name.len()..]
            .trim_start_matches(char::is_whitespace)
            .starts_with('(')
        {
            return false;
        }

        let prefix = line[..index].trim_end();
        prefix.ends_with('.') || prefix.ends_with("::") || prefix.ends_with("->")
    })
}

fn is_implementation_line(line: &str, target: &CodeSymbol) -> bool {
    if !is_type_like_symbol(target) || !contains_identifier(line, &target.name) {
        return false;
    }

    let trimmed = line.trim_start();
    let lower = trimmed.to_lowercase();
    lower.starts_with("impl ")
        || lower.starts_with("impl<")
        || lower.contains(" implements ")
        || lower.contains(": implements ")
}

fn is_inheritance_line(line: &str, target: &CodeSymbol) -> bool {
    if !is_type_like_symbol(target) || !contains_identifier(line, &target.name) {
        return false;
    }

    let lower = line.to_lowercase();
    lower.contains(" extends ") || is_python_base_class_line(line, &target.name)
}

fn is_python_base_class_line(line: &str, name: &str) -> bool {
    let trimmed = line.trim_start();
    if !trimmed.starts_with("class ") || !trimmed.ends_with(':') {
        return false;
    }

    let Some((_, bases)) = trimmed.split_once('(') else {
        return false;
    };
    let Some((bases, _)) = bases.rsplit_once(')') else {
        return false;
    };
    contains_identifier(bases, name)
}

fn is_instantiation_line(line: &str, target: &CodeSymbol) -> bool {
    if !is_type_like_symbol(target) {
        return false;
    }

    line.match_indices(&target.name).any(|(index, _)| {
        if !has_identifier_boundaries(line, index, &target.name) {
            return false;
        }

        let prefix = line[..index].trim_end();
        let suffix = line[index + target.name.len()..].trim_start_matches(char::is_whitespace);
        prefix.ends_with("new")
            || prefix.ends_with("new ")
            || suffix.starts_with('{')
            || suffix.starts_with('(')
    })
}

fn is_type_reference_line(line: &str, target: &CodeSymbol) -> bool {
    if !is_type_like_symbol(target) || !contains_identifier(line, &target.name) {
        return false;
    }

    line.contains(':')
        || line.contains("->")
        || line.contains("=>")
        || line.contains('<')
        || line.contains('>')
        || line.contains('&')
        || line.contains('*')
        || line.contains(" as ")
}

fn is_type_like_symbol(symbol: &CodeSymbol) -> bool {
    matches!(
        symbol.kind.as_str(),
        "actor"
            | "annotation"
            | "class"
            | "enum"
            | "extension"
            | "impl"
            | "interface"
            | "object"
            | "protocol"
            | "record"
            | "struct"
            | "trait"
            | "type"
    )
}

fn contains_identifier(line: &str, name: &str) -> bool {
    line.match_indices(name)
        .any(|(index, _)| has_identifier_boundaries(line, index, name))
}

fn has_identifier_boundaries(line: &str, index: usize, name: &str) -> bool {
    let before = line[..index].chars().next_back();
    let after = line[index + name.len()..].chars().next();
    !before.is_some_and(is_identifier_char) && !after.is_some_and(is_identifier_char)
}

fn is_identifier_char(char: char) -> bool {
    char.is_alphanumeric() || char == '_' || char == '$'
}

#[cfg(test)]
mod tests {
    use super::{
        CodeReference, CodeSymbol, contains_identifier, extract_references, extract_symbols,
        index_files,
    };
    use crate::discovery::FileCandidate;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn extracts_rust_symbols() {
        let symbols = extract_symbols(
            "src/plugin_hooks.rs",
            Some("rust"),
            r#"
pub struct PluginHooks {
}

impl PluginHooks {
    pub async fn run_after_config(&self) {}
}
"#,
        )
        .unwrap();

        assert!(has_symbol(&symbols, "struct", "PluginHooks", 2));
        assert!(has_symbol(&symbols, "impl", "PluginHooks", 5));
        assert!(has_symbol(&symbols, "function", "run_after_config", 6));
        assert_eq!(
            symbols
                .iter()
                .find(|symbol| symbol.name == "run_after_config")
                .and_then(|symbol| symbol.line_end),
            Some(6)
        );
    }

    #[test]
    fn tree_sitter_rust_extracts_multiline_ranges() {
        let symbols = extract_symbols(
            "src/plugin_hooks.rs",
            Some("rust"),
            r#"
pub fn run_after_config() -> bool {
    let loaded = true;
    loaded
}
"#,
        )
        .unwrap();

        let function = symbols
            .iter()
            .find(|symbol| symbol.name == "run_after_config")
            .unwrap();

        assert_eq!(function.kind, "function");
        assert_eq!(function.line_start, 2);
        assert_eq!(function.line_end, Some(5));
        assert!(function.signature.contains("pub fn run_after_config"));
    }

    #[test]
    fn tree_sitter_python_extracts_classes_and_functions() {
        let symbols = extract_symbols(
            "app/plugin_hooks.py",
            Some("python"),
            r#"
class PluginHooks:
    def run_after_config(self):
        loaded = True
        return loaded
"#,
        )
        .unwrap();

        let class = symbols
            .iter()
            .find(|symbol| symbol.name == "PluginHooks")
            .unwrap();
        let method = symbols
            .iter()
            .find(|symbol| symbol.name == "run_after_config")
            .unwrap();

        assert_eq!(class.kind, "class");
        assert_eq!(class.line_start, 2);
        assert_eq!(class.line_end, Some(5));
        assert_eq!(method.kind, "function");
        assert_eq!(method.line_start, 3);
        assert_eq!(method.line_end, Some(5));
        assert!(method.signature.contains("def run_after_config"));
    }

    #[test]
    fn extracts_common_script_symbols() {
        let symbols = extract_symbols(
            "src/pluginHooks.ts",
            Some("typescript"),
            r#"
export interface PluginHook {}
export const runPluginHooks = () => true;
export class PluginRegistry {}
"#,
        )
        .unwrap();

        assert!(has_symbol(&symbols, "interface", "PluginHook", 2));
        assert!(has_symbol(&symbols, "function", "runPluginHooks", 3));
        assert!(has_symbol(&symbols, "class", "PluginRegistry", 4));
    }

    #[test]
    fn tree_sitter_typescript_extracts_multiline_ranges() {
        let symbols = extract_symbols(
            "src/pluginHooks.ts",
            Some("typescript"),
            r#"
export interface PluginHook {
    enabled: boolean;
}

export type HookResult = {
    loaded: boolean;
};

export class PluginRegistry {
    runPluginHooks() {
        return true;
    }
}

export const createRegistry = () => {
    return new PluginRegistry();
};
"#,
        )
        .unwrap();

        let interface = symbols
            .iter()
            .find(|symbol| symbol.name == "PluginHook")
            .unwrap();
        let type_alias = symbols
            .iter()
            .find(|symbol| symbol.name == "HookResult")
            .unwrap();
        let class = symbols
            .iter()
            .find(|symbol| symbol.name == "PluginRegistry")
            .unwrap();
        let method = symbols
            .iter()
            .find(|symbol| symbol.name == "runPluginHooks")
            .unwrap();
        let arrow_function = symbols
            .iter()
            .find(|symbol| symbol.name == "createRegistry")
            .unwrap();

        assert_eq!(interface.kind, "interface");
        assert_eq!(interface.line_start, 2);
        assert_eq!(interface.line_end, Some(4));
        assert_eq!(type_alias.kind, "type");
        assert_eq!(type_alias.line_start, 6);
        assert_eq!(type_alias.line_end, Some(8));
        assert_eq!(class.kind, "class");
        assert_eq!(class.line_start, 10);
        assert_eq!(class.line_end, Some(14));
        assert_eq!(method.kind, "function");
        assert_eq!(method.line_start, 11);
        assert_eq!(method.line_end, Some(13));
        assert_eq!(arrow_function.kind, "function");
        assert_eq!(arrow_function.line_start, 16);
        assert_eq!(arrow_function.line_end, Some(18));
        assert!(
            arrow_function
                .signature
                .contains("export const createRegistry")
        );
    }

    #[test]
    fn tree_sitter_javascript_extracts_multiline_ranges() {
        let symbols = extract_symbols(
            "src/pluginHooks.jsx",
            Some("javascript"),
            r#"
export class PluginRegistry {
    register(hook) {
        return hook;
    }
}

export function createRegistry() {
    return new PluginRegistry();
}

const renderRegistry = () => (
    <PluginRegistry />
);
"#,
        )
        .unwrap();

        let class = symbols
            .iter()
            .find(|symbol| symbol.name == "PluginRegistry")
            .unwrap();
        let method = symbols
            .iter()
            .find(|symbol| symbol.name == "register")
            .unwrap();
        let function = symbols
            .iter()
            .find(|symbol| symbol.name == "createRegistry")
            .unwrap();
        let arrow_function = symbols
            .iter()
            .find(|symbol| symbol.name == "renderRegistry")
            .unwrap();

        assert_eq!(class.language.as_deref(), Some("javascript"));
        assert_eq!(class.kind, "class");
        assert_eq!(class.line_start, 2);
        assert_eq!(class.line_end, Some(6));
        assert_eq!(method.kind, "function");
        assert_eq!(method.line_start, 3);
        assert_eq!(method.line_end, Some(5));
        assert_eq!(function.kind, "function");
        assert_eq!(function.line_start, 8);
        assert_eq!(function.line_end, Some(10));
        assert_eq!(arrow_function.kind, "function");
        assert_eq!(arrow_function.line_start, 12);
        assert_eq!(arrow_function.line_end, Some(14));
    }

    #[test]
    fn tree_sitter_go_extracts_multiline_ranges() {
        let symbols = extract_symbols(
            "plugin/hooks.go",
            Some("go"),
            r#"
package plugin

type PluginRegistry struct {
    enabled bool
}

type PluginHook interface {
    RunPluginHooks() bool
}

func NewPluginRegistry() *PluginRegistry {
    return &PluginRegistry{}
}

func (r *PluginRegistry) RunPluginHooks() bool {
    return r.enabled
}
"#,
        )
        .unwrap();

        let registry = symbols
            .iter()
            .find(|symbol| symbol.name == "PluginRegistry")
            .unwrap();
        let hook = symbols
            .iter()
            .find(|symbol| symbol.name == "PluginHook")
            .unwrap();
        let constructor = symbols
            .iter()
            .find(|symbol| symbol.name == "NewPluginRegistry")
            .unwrap();
        let method = symbols
            .iter()
            .find(|symbol| symbol.name == "RunPluginHooks")
            .unwrap();

        assert_eq!(registry.kind, "struct");
        assert_eq!(registry.line_start, 4);
        assert_eq!(registry.line_end, Some(6));
        assert_eq!(hook.kind, "interface");
        assert_eq!(hook.line_start, 8);
        assert_eq!(hook.line_end, Some(10));
        assert_eq!(constructor.kind, "function");
        assert_eq!(constructor.line_start, 12);
        assert_eq!(constructor.line_end, Some(14));
        assert_eq!(method.kind, "function");
        assert_eq!(method.line_start, 16);
        assert_eq!(method.line_end, Some(18));
        assert!(method.signature.contains("RunPluginHooks"));
    }

    #[test]
    fn tree_sitter_java_extracts_multiline_ranges() {
        let symbols = extract_symbols(
            "src/main/java/plugin/PluginRegistry.java",
            Some("java"),
            r#"
package plugin;

public interface PluginHook {
    boolean runPluginHooks();
}

public record HookResult(boolean loaded) {}

public enum HookState {
    ENABLED,
    DISABLED
}

public class PluginRegistry implements PluginHook {
    public PluginRegistry() {
    }

    @Override
    public boolean runPluginHooks() {
        return true;
    }
}
"#,
        )
        .unwrap();

        let interface = symbols
            .iter()
            .find(|symbol| symbol.name == "PluginHook")
            .unwrap();
        let record = symbols
            .iter()
            .find(|symbol| symbol.name == "HookResult")
            .unwrap();
        let state = symbols
            .iter()
            .find(|symbol| symbol.name == "HookState")
            .unwrap();
        let class = symbols
            .iter()
            .find(|symbol| symbol.name == "PluginRegistry" && symbol.kind == "class")
            .unwrap();
        let constructor = symbols
            .iter()
            .find(|symbol| symbol.name == "PluginRegistry" && symbol.kind == "function")
            .unwrap();
        let method = symbols
            .iter()
            .filter(|symbol| symbol.name == "runPluginHooks")
            .max_by_key(|symbol| symbol.line_start)
            .unwrap();

        assert_eq!(interface.kind, "interface");
        assert_eq!(interface.line_start, 4);
        assert_eq!(interface.line_end, Some(6));
        assert_eq!(record.kind, "record");
        assert_eq!(record.line_start, 8);
        assert_eq!(record.line_end, Some(8));
        assert_eq!(state.kind, "enum");
        assert_eq!(state.line_start, 10);
        assert_eq!(state.line_end, Some(13));
        assert_eq!(class.language.as_deref(), Some("java"));
        assert_eq!(class.line_start, 15);
        assert_eq!(class.line_end, Some(23));
        assert_eq!(constructor.line_start, 16);
        assert_eq!(constructor.line_end, Some(17));
        assert_eq!(method.kind, "function");
        assert_eq!(method.line_start, 19);
        assert_eq!(method.line_end, Some(22));
    }

    #[test]
    fn tree_sitter_kotlin_extracts_multiline_ranges() {
        let symbols = extract_symbols(
            "src/main/kotlin/plugin/PluginRegistry.kt",
            Some("kotlin"),
            r#"
package plugin

interface PluginHook {
    fun runPluginHooks(): Boolean
}

typealias HookCallback = () -> Boolean

enum class HookState {
    ENABLED,
    DISABLED
}

object HookDefaults {
    fun createDefault(): PluginHook? = null
}

data class PluginRegistry(private val hooks: List<PluginHook>) {
    constructor(): this(emptyList())

    fun runPluginHooks(): Boolean {
        return hooks.all { it.runPluginHooks() }
    }

    companion object {
        fun empty(): PluginRegistry = PluginRegistry()
    }
}
"#,
        )
        .unwrap();

        let interface = symbols
            .iter()
            .find(|symbol| symbol.name == "PluginHook")
            .unwrap();
        let interface_method = symbols
            .iter()
            .find(|symbol| symbol.name == "runPluginHooks" && symbol.line_start == 5)
            .unwrap();
        let callback = symbols
            .iter()
            .find(|symbol| symbol.name == "HookCallback")
            .unwrap();
        let state = symbols
            .iter()
            .find(|symbol| symbol.name == "HookState")
            .unwrap();
        let defaults = symbols
            .iter()
            .find(|symbol| symbol.name == "HookDefaults")
            .unwrap();
        let default_function = symbols
            .iter()
            .find(|symbol| symbol.name == "createDefault")
            .unwrap();
        let registry = symbols
            .iter()
            .find(|symbol| symbol.name == "PluginRegistry" && symbol.kind == "class")
            .unwrap();
        let constructor = symbols
            .iter()
            .find(|symbol| symbol.name == "constructor")
            .unwrap();
        let method = symbols
            .iter()
            .filter(|symbol| symbol.name == "runPluginHooks")
            .max_by_key(|symbol| symbol.line_start)
            .unwrap();
        let companion = symbols
            .iter()
            .find(|symbol| symbol.name == "Companion")
            .unwrap();
        let empty = symbols
            .iter()
            .find(|symbol| symbol.name == "empty")
            .unwrap();

        assert_eq!(interface.kind, "interface");
        assert_eq!(interface.line_start, 4);
        assert_eq!(interface.line_end, Some(6));
        assert_eq!(interface_method.kind, "function");
        assert_eq!(interface_method.line_end, Some(5));
        assert_eq!(callback.kind, "type");
        assert_eq!(callback.line_start, 8);
        assert_eq!(callback.line_end, Some(8));
        assert_eq!(state.kind, "enum");
        assert_eq!(state.line_start, 10);
        assert_eq!(state.line_end, Some(13));
        assert_eq!(defaults.kind, "object");
        assert_eq!(defaults.line_start, 15);
        assert_eq!(defaults.line_end, Some(17));
        assert_eq!(default_function.line_start, 16);
        assert_eq!(registry.language.as_deref(), Some("kotlin"));
        assert_eq!(registry.line_start, 19);
        assert_eq!(registry.line_end, Some(29));
        assert_eq!(constructor.kind, "function");
        assert_eq!(constructor.line_start, 20);
        assert_eq!(method.kind, "function");
        assert_eq!(method.line_start, 22);
        assert_eq!(method.line_end, Some(24));
        assert_eq!(companion.kind, "object");
        assert_eq!(companion.line_start, 26);
        assert_eq!(companion.line_end, Some(28));
        assert_eq!(empty.kind, "function");
        assert_eq!(empty.line_start, 27);
    }

    #[test]
    fn tree_sitter_swift_extracts_multiline_ranges() {
        let symbols = extract_symbols(
            "Sources/Plugin/PluginRegistry.swift",
            Some("swift"),
            r#"
public protocol PluginHook {
    func runPluginHooks() -> Bool
}

public struct HookResult {
    let loaded: Bool
}

public enum HookState {
    case enabled
    case disabled
}

public actor PluginRegistry: PluginHook {
    public init() {
    }

    public func runPluginHooks() -> Bool {
        return true
    }
}

extension PluginRegistry {
    public func resetHooks() {
    }
}

public typealias HookCallback = () -> Bool
"#,
        )
        .unwrap();

        let protocol = symbols
            .iter()
            .find(|symbol| symbol.name == "PluginHook")
            .unwrap();
        let protocol_method = symbols
            .iter()
            .find(|symbol| symbol.name == "runPluginHooks" && symbol.line_start == 3)
            .unwrap();
        let result = symbols
            .iter()
            .find(|symbol| symbol.name == "HookResult")
            .unwrap();
        let state = symbols
            .iter()
            .find(|symbol| symbol.name == "HookState")
            .unwrap();
        let registry = symbols
            .iter()
            .find(|symbol| symbol.name == "PluginRegistry" && symbol.kind == "actor")
            .unwrap();
        let initializer = symbols.iter().find(|symbol| symbol.name == "init").unwrap();
        let method = symbols
            .iter()
            .filter(|symbol| symbol.name == "runPluginHooks")
            .max_by_key(|symbol| symbol.line_start)
            .unwrap();
        let extension = symbols
            .iter()
            .find(|symbol| symbol.name == "PluginRegistry" && symbol.kind == "extension")
            .unwrap();
        let reset = symbols
            .iter()
            .find(|symbol| symbol.name == "resetHooks")
            .unwrap();
        let callback = symbols
            .iter()
            .find(|symbol| symbol.name == "HookCallback")
            .unwrap();

        assert_eq!(protocol.kind, "protocol");
        assert_eq!(protocol.line_start, 2);
        assert_eq!(protocol.line_end, Some(4));
        assert_eq!(protocol_method.kind, "function");
        assert_eq!(protocol_method.line_end, Some(3));
        assert_eq!(result.kind, "struct");
        assert_eq!(result.line_start, 6);
        assert_eq!(result.line_end, Some(8));
        assert_eq!(state.kind, "enum");
        assert_eq!(state.line_start, 10);
        assert_eq!(state.line_end, Some(13));
        assert_eq!(registry.language.as_deref(), Some("swift"));
        assert_eq!(registry.line_start, 15);
        assert_eq!(registry.line_end, Some(22));
        assert_eq!(initializer.kind, "function");
        assert_eq!(initializer.line_start, 16);
        assert_eq!(initializer.line_end, Some(17));
        assert_eq!(method.kind, "function");
        assert_eq!(method.line_start, 19);
        assert_eq!(method.line_end, Some(21));
        assert_eq!(extension.kind, "extension");
        assert_eq!(extension.line_start, 24);
        assert_eq!(extension.line_end, Some(27));
        assert_eq!(reset.line_start, 25);
        assert_eq!(reset.line_end, Some(26));
        assert_eq!(callback.kind, "type");
        assert_eq!(callback.line_start, 29);
        assert_eq!(callback.line_end, Some(29));
    }

    fn has_symbol(symbols: &[CodeSymbol], kind: &str, name: &str, line_start: i64) -> bool {
        symbols.iter().any(|symbol| {
            symbol.kind == kind && symbol.name == name && symbol.line_start == line_start
        })
    }

    #[test]
    fn identifier_matching_respects_boundaries() {
        assert!(contains_identifier(
            "run_after_config();",
            "run_after_config"
        ));
        assert!(!contains_identifier(
            "run_after_configuration();",
            "run_after_config"
        ));
    }

    #[test]
    fn extracts_references_to_indexed_symbols() {
        let project = TempProject::new("references");
        project.write(
            "src/plugin_hooks.rs",
            r#"
pub struct PluginHooks {}

pub fn run_after_config() {}
"#,
        );
        project.write(
            "src/main.rs",
            r#"
use crate::plugin_hooks::PluginHooks;

fn main() {
    let _hooks = PluginHooks {};
    run_after_config();
}
"#,
        );
        let files = vec![candidate("src/main.rs"), candidate("src/plugin_hooks.rs")];
        let symbols = extract_symbols(
            "src/plugin_hooks.rs",
            Some("rust"),
            &fs::read_to_string(project.root().join("src/plugin_hooks.rs")).unwrap(),
        )
        .unwrap();

        let references = extract_references(project.root(), &files, &symbols).unwrap();

        assert!(references.iter().any(|reference| {
            reference.path == "src/main.rs"
                && reference.target_name == "PluginHooks"
                && reference.kind == "import"
        }));
        assert!(references.iter().any(|reference| {
            reference.path == "src/main.rs"
                && reference.target_name == "run_after_config"
                && reference.kind == "call"
        }));
    }

    #[test]
    fn extracts_richer_symbol_graph_edge_kinds() {
        let project = TempProject::new("richer_edges");
        project.write(
            "src/plugin.rs",
            r#"
pub trait PluginHook {
    fn run_plugin_hooks(&self);
}

pub struct PluginRegistry {}

impl PluginHook for PluginRegistry {
    fn run_plugin_hooks(&self) {}
}

pub fn build_registry() -> PluginRegistry {
    PluginRegistry {}
}

pub fn execute(registry: &PluginRegistry) {
    registry.run_plugin_hooks();
}
"#,
        );

        let files = vec![candidate("src/plugin.rs")];
        let symbols = index_files(project.root(), &files).unwrap();
        let references = extract_references(project.root(), &files, &symbols).unwrap();

        assert!(has_reference_kind(
            &references,
            "PluginHook",
            "implementation"
        ));
        assert!(has_reference_kind(
            &references,
            "PluginRegistry",
            "implementation"
        ));
        assert!(has_reference_kind(
            &references,
            "PluginRegistry",
            "type_reference"
        ));
        assert!(has_reference_kind(
            &references,
            "PluginRegistry",
            "instantiation"
        ));
        assert!(has_reference_kind(
            &references,
            "run_plugin_hooks",
            "method_call"
        ));
    }

    fn has_reference_kind(references: &[CodeReference], target_name: &str, kind: &str) -> bool {
        references
            .iter()
            .any(|reference| reference.target_name == target_name && reference.kind == kind)
    }

    fn candidate(path: &str) -> FileCandidate {
        FileCandidate {
            path: path.to_string(),
            score: 0,
            language: Some("rust".to_string()),
            size_bytes: None,
        }
    }

    struct TempProject {
        root: PathBuf,
    }

    impl TempProject {
        fn new(name: &str) -> Self {
            let unique = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system time should be after unix epoch")
                .as_nanos();
            let root = std::env::temp_dir().join(format!("hugr_code_{name}_{unique}"));
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
}
