use crate::discovery::FileCandidate;
use std::collections::HashSet;
use std::fs;
use std::path::Path;

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
                if is_declaration_line(&targets, &file.path, line_number, &target.name) {
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
    let mut symbols = Vec::new();

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
                line_end: None,
                signature: clean_signature(trimmed),
            });
        }
    }

    Ok(symbols)
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

fn reference_kind(line: &str, target: &CodeSymbol) -> String {
    if is_import_line(line) {
        "import".to_string()
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
    target_name: &str,
) -> bool {
    symbols.iter().any(|symbol| {
        symbol.path == path && symbol.name == target_name && symbol.line_start == line_number
    })
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
    use super::{CodeSymbol, contains_identifier, extract_references, extract_symbols};
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
