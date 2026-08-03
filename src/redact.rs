//! Deterministic secret redaction for observed text. Session events,
//! command-output tails, and diagnostics capture whatever a developer's
//! shell printed, which regularly includes credentials; those rows are also
//! candidates for sync and LLM synthesis, so secrets are scrubbed at the
//! storage boundary rather than at display time. Redaction is biased toward
//! safety: a false positive costs one obscured value, a false negative
//! stores a credential durably.

use regex::Regex;
use std::sync::LazyLock;

const REPLACEMENT: &str = "[REDACTED]";

static PEM_BLOCK: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"-----BEGIN [A-Z0-9 ]*PRIVATE KEY-----[\s\S]*?-----END [A-Z0-9 ]*PRIVATE KEY-----")
        .expect("pem pattern should compile")
});

/// Standalone token shapes that are unambiguous regardless of context.
static TOKEN_SHAPES: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [
        // OpenAI/Anthropic-style secret keys.
        r"\bsk-[A-Za-z0-9_-]{16,}\b",
        // GitHub personal/oauth/server tokens.
        r"\bgh[pousr]_[A-Za-z0-9]{20,}\b",
        // Slack tokens.
        r"\bxox[baprs]-[A-Za-z0-9-]{10,}\b",
        // AWS access key ids.
        r"\bAKIA[0-9A-Z]{16}\b",
        // JWTs (three base64url segments).
        r"\beyJ[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{5,}\b",
    ]
    .iter()
    .map(|pattern| Regex::new(pattern).expect("token pattern should compile"))
    .collect()
});

static AUTHORIZATION_HEADER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(authorization\s*:\s*(?:bearer|basic|token)\s+)\S+")
        .expect("authorization pattern should compile")
});

/// KEY=value / key: value assignments where the key names a credential.
static SECRET_ASSIGNMENT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?i)\b([A-Z0-9_.-]*(?:api[_.-]?key|secret|token|passwd|password|credential)[A-Z0-9_.-]*\s*[=:]\s*)("[^"]{4,}"|'[^']{4,}'|[^\s"']{4,})"#,
    )
    .expect("assignment pattern should compile")
});

/// --token/--api-key style flags followed by a space-separated value.
static SECRET_FLAG: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)(--?[A-Za-z0-9_-]*(?:api[_-]?key|secret|token|password)[A-Za-z0-9_-]*\s+)[^\s-][^\s]{3,}",
    )
    .expect("flag pattern should compile")
});

/// `-p` used as a password flag, which [`SECRET_FLAG`] cannot catch because
/// the flag name carries no hint of what it holds.
///
/// `-p` is badly overloaded — it is the port flag for `ssh` and the project
/// flag elsewhere — so only the two unambiguous spellings are redacted:
///
/// - the value attached to the flag (`mysql -pHunter2`), which no tool uses
///   for a port, and
/// - `-p <value>` immediately after a `login` subcommand (`docker login -p x`).
///
/// `ssh -p 2222 host` therefore keeps its port, which matters because these
/// command lines feed the diagnostics and session history an agent reads back.
static ATTACHED_PASSWORD_FLAG: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(\s-p)([^\s-][^\s]{2,})").expect("attached password pattern should compile")
});

static LOGIN_PASSWORD_FLAG: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(\blogin\b[^\n]*?\s-p\s+)([^\s][^\s]*)")
        .expect("login password pattern should compile")
});

/// user:password@ credentials embedded in URLs.
static URL_USERINFO: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(://[^/\s:@]+):([^@/\s]+)@").expect("url pattern should compile")
});

pub(crate) fn redact_secrets(text: &str) -> String {
    let mut redacted = PEM_BLOCK
        .replace_all(text, "[REDACTED PRIVATE KEY]")
        .into_owned();
    redacted = AUTHORIZATION_HEADER
        .replace_all(&redacted, format!("${{1}}{REPLACEMENT}"))
        .into_owned();
    redacted = SECRET_ASSIGNMENT
        .replace_all(&redacted, format!("${{1}}{REPLACEMENT}"))
        .into_owned();
    redacted = SECRET_FLAG
        .replace_all(&redacted, format!("${{1}}{REPLACEMENT}"))
        .into_owned();
    redacted = LOGIN_PASSWORD_FLAG
        .replace_all(&redacted, format!("${{1}}{REPLACEMENT}"))
        .into_owned();
    redacted = ATTACHED_PASSWORD_FLAG
        .replace_all(&redacted, format!("${{1}}{REPLACEMENT}"))
        .into_owned();
    redacted = URL_USERINFO
        .replace_all(&redacted, format!("${{1}}:{REPLACEMENT}@"))
        .into_owned();
    for shape in TOKEN_SHAPES.iter() {
        redacted = shape.replace_all(&redacted, REPLACEMENT).into_owned();
    }
    redacted
}

#[cfg(test)]
mod tests {
    use super::redact_secrets;

    #[test]
    fn redacts_standalone_token_shapes() {
        let text = "openai sk-abcdefghijklmnopqrstuv github ghp_ABCDEFGHIJKLMNOPQRSTuvwx aws AKIAIOSFODNN7EXAMPLE";

        let redacted = redact_secrets(text);

        assert!(!redacted.contains("sk-abcdefghijklmnopqrstuv"));
        assert!(!redacted.contains("ghp_ABCDEFGHIJKLMNOPQRST"));
        assert!(!redacted.contains("AKIAIOSFODNN7EXAMPLE"));
        assert_eq!(redacted.matches("[REDACTED]").count(), 3);
    }

    #[test]
    fn redacts_jwts() {
        let text =
            "jwt eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w5N_XgL0n3I9P";

        assert_eq!(redact_secrets(text), "jwt [REDACTED]");
    }

    #[test]
    fn redacts_secret_assignments_and_keeps_keys() {
        let text =
            "export OPENAI_API_KEY=abc123secret and DB_PASSWORD: 'hunter22' plus token=xyz9876";

        let redacted = redact_secrets(text);

        assert!(redacted.contains("OPENAI_API_KEY=[REDACTED]"));
        assert!(redacted.contains("DB_PASSWORD: [REDACTED]"));
        assert!(redacted.contains("token=[REDACTED]"));
        assert!(!redacted.contains("abc123secret"));
        assert!(!redacted.contains("hunter22"));
    }

    #[test]
    fn redacts_authorization_headers_and_cli_flags() {
        let text =
            "curl -H 'Authorization: Bearer abc.def.ghi' --api-key 12345678 --token abcdefgh";

        let redacted = redact_secrets(text);

        assert!(redacted.contains("Authorization: Bearer [REDACTED]"));
        assert!(redacted.contains("--api-key [REDACTED]"));
        assert!(redacted.contains("--token [REDACTED]"));
        assert!(!redacted.contains("12345678"));
    }

    #[test]
    fn redacts_every_api_key_spelling() {
        let text = "X-Api-Key: hunter2222 api_key=hunter3333 apikey:hunter4444 \
                    --api-key hunter5555 --api_key hunter6666";

        let redacted = redact_secrets(text);

        assert!(redacted.contains("X-Api-Key: [REDACTED]"));
        assert!(redacted.contains("api_key=[REDACTED]"));
        assert!(redacted.contains("apikey:[REDACTED]"));
        assert!(redacted.contains("--api-key [REDACTED]"));
        assert!(redacted.contains("--api_key [REDACTED]"));
        assert!(!redacted.contains("hunter"));
    }

    #[test]
    fn redacts_pem_blocks_and_url_userinfo() {
        let text = "-----BEGIN RSA PRIVATE KEY-----\nMIIEow\nlines\n-----END RSA PRIVATE KEY-----\npostgres://hugr:supersecret@db.host/prod";

        let redacted = redact_secrets(text);

        assert!(redacted.contains("[REDACTED PRIVATE KEY]"));
        assert!(!redacted.contains("MIIEow"));
        assert!(redacted.contains("postgres://hugr:[REDACTED]@db.host/prod"));
        assert!(!redacted.contains("supersecret"));
    }

    /// `-p` carries no hint of its contents, so the named-flag pattern misses
    /// it and `docker login -p hunter2` was stored in the clear.
    #[test]
    fn redacts_the_password_spellings_of_the_p_flag() {
        let redacted = redact_secrets("mysql -u root -pP@ssw0rd123 mydb");
        assert!(redacted.contains("-p[REDACTED]"), "{redacted}");
        assert!(!redacted.contains("P@ssw0rd123"));
        // The unrelated `-u root` is untouched.
        assert!(redacted.contains("-u root"));

        let redacted = redact_secrets("docker login -u me -p SuperSecret123");
        assert!(!redacted.contains("SuperSecret123"), "{redacted}");
    }

    /// `-p` is the port flag for ssh and the project flag elsewhere. These
    /// command lines feed diagnostics and session history an agent reads back,
    /// so over-redacting costs real context.
    #[test]
    fn leaves_non_password_p_flags_alone() {
        assert_eq!(
            redact_secrets("ssh -p 2222 deploy@host"),
            "ssh -p 2222 deploy@host"
        );
        assert_eq!(
            redact_secrets("docker compose -p myproject up"),
            "docker compose -p myproject up"
        );
    }

    #[test]
    fn leaves_ordinary_output_untouched() {
        let text = "test result: ok. 259 passed; cargo build --release finished in 3.2s; \
                    fn parse_token(input: &str) -> Token { tokenize(input) }";

        assert_eq!(redact_secrets(text), text);
    }
}
