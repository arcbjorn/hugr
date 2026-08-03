use crate::code::CodeSymbol;
use serde::Serialize;
use std::collections::{HashMap, HashSet};

const INLINE_TEST_MODULE_SCORE: usize = 70;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct TestCandidate {
    pub path: String,
    pub reason: String,
    pub score: usize,
}

pub(crate) fn likely_tests_for_files(
    files: &[String],
    known_files: &[String],
    inline_test_paths: &HashSet<String>,
    limit: usize,
) -> Vec<TestCandidate> {
    if limit == 0 || files.is_empty() || (known_files.is_empty() && inline_test_paths.is_empty()) {
        return Vec::new();
    }

    let mut candidates = HashMap::<String, (usize, String)>::new();
    let test_files = known_files
        .iter()
        .filter(|path| is_test_path(path))
        .collect::<Vec<_>>();

    for file in files {
        if is_test_path(file) {
            upsert_candidate(&mut candidates, file, 100, "target is already a test file");
        } else if inline_test_paths.contains(file) {
            upsert_candidate(
                &mut candidates,
                file,
                INLINE_TEST_MODULE_SCORE,
                "file contains an inline test module",
            );
        }

        let source_stem = normalized_source_stem(file);
        if source_stem.len() < 3 {
            continue;
        }

        for test_file in &test_files {
            let Some(score) = test_score(file, &source_stem, test_file) else {
                continue;
            };
            let reason = if same_directory(file, test_file) {
                "same directory test file"
            } else if test_file.split('/').any(|component| component == "tests") {
                "repository tests directory match"
            } else {
                "matching test filename"
            };
            upsert_candidate(&mut candidates, test_file, score, reason);
        }
    }

    let mut ranked = candidates
        .into_iter()
        .map(|(path, (score, reason))| {
            (
                score,
                TestCandidate {
                    path,
                    reason,
                    score,
                },
            )
        })
        .collect::<Vec<_>>();
    ranked.sort_by(|left, right| {
        right
            .0
            .cmp(&left.0)
            .then_with(|| left.1.path.cmp(&right.1.path))
    });
    ranked.truncate(limit);
    ranked.into_iter().map(|(_, candidate)| candidate).collect()
}

fn test_score(source: &str, source_stem: &str, test_file: &str) -> Option<usize> {
    let test_stem = normalized_source_stem(test_file);
    let test_lower = test_file.to_lowercase();
    let mut score = 0;

    if test_stem == source_stem {
        score += 40;
    } else if test_stem.contains(source_stem) {
        score += 25;
    } else if !test_lower.contains(source_stem) {
        return None;
    } else {
        score += 15;
    }

    if same_directory(source, test_file) {
        score += 20;
    }
    if test_lower.contains("/tests/") || test_lower.starts_with("tests/") {
        score += 10;
    }

    Some(score)
}

fn upsert_candidate(
    candidates: &mut HashMap<String, (usize, String)>,
    path: &str,
    score: usize,
    reason: &str,
) {
    match candidates.get_mut(path) {
        Some((existing_score, existing_reason)) => {
            if score > *existing_score {
                *existing_score = score;
                *existing_reason = reason.to_string();
            }
        }
        None => {
            candidates.insert(path.to_string(), (score, reason.to_string()));
        }
    }
}

pub(crate) fn inline_test_paths_from_symbols(symbols: &[CodeSymbol]) -> HashSet<String> {
    symbols
        .iter()
        .filter(|symbol| is_inline_test_module_symbol(symbol))
        .map(|symbol| symbol.path.clone())
        .collect()
}

fn is_inline_test_module_symbol(symbol: &CodeSymbol) -> bool {
    symbol.kind == "module"
        && matches!(symbol.name.as_str(), "tests" | "test")
        && symbol.language.as_deref() == Some("rust")
}

/// Extensions whose ecosystems name tests `FooTest`/`FooTests` rather than
/// with a `_test` suffix or a `test_` prefix.
///
/// Java, Kotlin, and Swift are three of the eight indexed languages, and
/// `move-symbol` already understands their packages and modules — but their
/// test convention matched none of the patterns below, so `PaymentServiceTest.java`
/// beside `PaymentService.java` produced no mapping at all and `hugr impact`
/// reported no affected tests. The Maven/Gradle `src/test/java` layout was
/// caught by the directory check; anything outside it was not.
const CAMEL_CASE_TEST_EXTENSIONS: [&str; 4] = [".java", ".kt", ".kts", ".swift"];

pub(crate) fn is_test_path(path: &str) -> bool {
    let lower = path.to_lowercase();
    let name = lower.rsplit('/').next().unwrap_or(&lower);
    let original_name = path.rsplit('/').next().unwrap_or(path);

    lower
        .split('/')
        .any(|component| matches!(component, "tests" | "test" | "__tests__" | "spec" | "specs"))
        || name.starts_with("test_")
        || name.ends_with("_test.go")
        || name.ends_with("_test.rs")
        || name.ends_with("_tests.rs")
        || name.contains(".test.")
        || name.contains(".spec.")
        || name.ends_with("_spec.rb")
        || name.ends_with("_test.py")
        || is_camel_case_test_name(name, original_name)
}

/// Matches `FooTest.java`, `FooTests.kt`, `FooSpec.kt`, and the
/// integration-test suffix `FooIT.java`.
///
/// The suffix has to begin a camel-case word in the *original* name, so
/// `PaymentTest.swift` matches while `Protest.swift` and `Manifest.kt` do not
/// — matching on the lowercased name alone turned any source ending in those
/// letters into a test file.
fn is_camel_case_test_name(name: &str, original: &str) -> bool {
    let Some(extension) = CAMEL_CASE_TEST_EXTENSIONS
        .iter()
        .find(|extension| name.ends_with(*extension))
    else {
        return false;
    };
    let stem = &original[..original.len() - extension.len()];
    ["Test", "Tests", "Spec", "IT"]
        .iter()
        .any(|suffix| starts_a_word(stem, suffix))
}

/// True when `stem` ends with `suffix` and something precedes it, so the
/// suffix reads as its own camel-case word rather than the tail of a longer
/// one.
fn starts_a_word(stem: &str, suffix: &str) -> bool {
    stem.strip_suffix(suffix)
        .is_some_and(|prefix| prefix.chars().next_back().is_some_and(char::is_lowercase))
}

/// Reduces a file name to the stem its test and source forms share, so
/// `PaymentServiceTest.java` and `PaymentService.java` compare equal.
fn normalized_source_stem(path: &str) -> String {
    let name = path.rsplit('/').next().unwrap_or(path);
    let stem = name.split('.').next().unwrap_or(name);
    // Strip a camel-case suffix from the original casing first, so
    // `PaymentServiceTest` reduces to `paymentservice` while `Protest` keeps
    // its stem intact.
    let stem = ["Tests", "Test", "Spec", "IT"]
        .iter()
        .find_map(|suffix| starts_a_word(stem, suffix).then(|| &stem[..stem.len() - suffix.len()]))
        .unwrap_or(stem);

    stem.trim_start_matches("test_")
        .trim_end_matches("_test")
        .trim_end_matches("_tests")
        .trim_end_matches("_spec")
        .to_lowercase()
}

fn same_directory(left: &str, right: &str) -> bool {
    directory(left) == directory(right)
}

fn directory(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(directory, _)| directory)
}

#[cfg(test)]
mod tests {
    use super::{inline_test_paths_from_symbols, is_test_path, likely_tests_for_files};
    use crate::code::CodeSymbol;
    use std::collections::HashSet;

    /// Java, Kotlin, and Swift are indexed languages whose tests are named
    /// `FooTest`/`FooTests`, which matched none of the `_test`/`test_`
    /// patterns. Outside the Maven `src/test/java` layout that left them
    /// undetected, so `hugr impact` reported no affected tests for a Java
    /// class sitting beside its own test.
    #[test]
    fn recognises_camel_case_test_files() {
        assert!(is_test_path("app/PaymentServiceTest.java"));
        assert!(is_test_path("app/PaymentServiceTests.kt"));
        assert!(is_test_path("app/PaymentServiceSpec.kt"));
        assert!(is_test_path("Sources/Pay/PaymentServiceTests.swift"));
        // The integration-test suffix.
        assert!(is_test_path("app/PaymentServiceIT.java"));
    }

    /// The suffix check must not swallow ordinary sources whose names merely
    /// end in those letters.
    #[test]
    fn ordinary_sources_are_not_test_files() {
        assert!(!is_test_path("app/PaymentService.java"));
        assert!(!is_test_path("app/Manifest.kt"));
        assert!(!is_test_path("src/latest.rs"));
        assert!(!is_test_path("Sources/Pay/Protest.swift"));
    }

    /// The mapping half of the same gap: detecting the test file is only
    /// useful if its stem still matches the source it covers.
    #[test]
    fn maps_camel_case_tests_back_to_their_source() {
        let known = vec![
            "app/PaymentService.java".to_string(),
            "app/PaymentServiceTest.java".to_string(),
        ];

        let tests = likely_tests_for_files(
            &["app/PaymentService.java".to_string()],
            &known,
            &HashSet::new(),
            5,
        );

        assert_eq!(
            tests.first().map(|test| test.path.as_str()),
            Some("app/PaymentServiceTest.java")
        );
    }

    #[test]
    fn maps_sources_to_likely_tests() {
        let known = vec![
            "src/plugin_hooks.rs".to_string(),
            "tests/plugin_hooks.rs".to_string(),
            "src/storage.rs".to_string(),
        ];

        let tests = likely_tests_for_files(
            &["src/plugin_hooks.rs".to_string()],
            &known,
            &HashSet::new(),
            5,
        );

        assert_eq!(tests.len(), 1);
        assert_eq!(tests[0].path, "tests/plugin_hooks.rs");
    }

    #[test]
    fn keeps_test_targets() {
        let known = vec!["src/plugin_hooks_test.rs".to_string()];

        let tests = likely_tests_for_files(&known, &known, &HashSet::new(), 5);

        assert_eq!(tests[0].path, "src/plugin_hooks_test.rs");
        assert_eq!(tests[0].reason, "target is already a test file");
        assert_eq!(tests[0].score, 100);
    }

    #[test]
    fn maps_inline_test_modules_to_their_own_file() {
        let files = vec!["src/context.rs".to_string()];
        let inline = HashSet::from(["src/context.rs".to_string()]);

        let tests = likely_tests_for_files(&files, &[], &inline, 5);

        assert_eq!(tests.len(), 1);
        assert_eq!(tests[0].path, "src/context.rs");
        assert_eq!(tests[0].reason, "file contains an inline test module");
        assert_eq!(tests[0].score, 70);
    }

    #[test]
    fn inline_candidates_rank_above_filename_matches() {
        let known = vec![
            "src/context.rs".to_string(),
            "tests/context_helpers.rs".to_string(),
        ];
        let inline = HashSet::from(["src/context.rs".to_string()]);

        let tests = likely_tests_for_files(&["src/context.rs".to_string()], &known, &inline, 5);

        assert_eq!(tests[0].path, "src/context.rs");
        assert!(
            tests
                .iter()
                .any(|test| test.path == "tests/context_helpers.rs")
        );
    }

    #[test]
    fn collects_inline_test_paths_from_rust_module_symbols() {
        let symbols = vec![
            CodeSymbol {
                path: "src/context.rs".to_string(),
                language: Some("rust".to_string()),
                name: "tests".to_string(),
                kind: "module".to_string(),
                line_start: 100,
                line_end: None,
                signature: "mod tests".to_string(),
            },
            CodeSymbol {
                path: "src/other.rs".to_string(),
                language: Some("rust".to_string()),
                name: "helpers".to_string(),
                kind: "module".to_string(),
                line_start: 5,
                line_end: None,
                signature: "mod helpers".to_string(),
            },
            CodeSymbol {
                path: "src/app.py".to_string(),
                language: Some("python".to_string()),
                name: "tests".to_string(),
                kind: "class".to_string(),
                line_start: 1,
                line_end: None,
                signature: "class tests".to_string(),
            },
        ];

        let inline = inline_test_paths_from_symbols(&symbols);

        assert_eq!(inline, HashSet::from(["src/context.rs".to_string()]));
    }
}
