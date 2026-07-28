//! The crate's error type.
//!
//! Hugr threads failures up through long call chains — a context compile can
//! fail in the parser, the database, an embedding provider, or a subprocess —
//! and every layer wants to add a sentence of context without discarding what
//! actually went wrong. [`Error`] carries a human-readable message plus the
//! optional underlying cause, so `{error}` still prints the one-line summary
//! the CLI has always printed while the full chain stays reachable through
//! [`std::error::Error::source`].
//!
//! Foreign errors convert through [`From`], which is what lets `?` replace the
//! `map_err(|error| error.to_string())` that used to sit on every fallible
//! call. Conversions preserve the source error's own `Display` output as the
//! message, so error text is unchanged from the string-based implementation
//! this replaces.

use std::error::Error as StdError;
use std::fmt::{self, Display, Formatter};

/// The result type returned throughout the crate.
///
/// The error parameter defaults to [`Error`], so `Result<T>` is the common
/// case and `Result<T, OtherError>` still spells out anything unusual.
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// An error produced by any Hugr operation.
///
/// Deliberately opaque: callers get a message and a source chain rather than
/// a variant to match on. Nothing in the CLI or the MCP server branches on
/// error kind — they report and exit — so an enum would be surface area
/// without a consumer.
#[derive(Debug)]
pub struct Error {
    message: String,
    source: Option<Box<dyn StdError + Send + Sync + 'static>>,
}

impl Error {
    /// Builds an error from a message that has no underlying cause.
    pub(crate) fn msg(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            source: None,
        }
    }

    /// Builds an error that explains `message` and keeps `source` reachable.
    ///
    /// Use this instead of interpolating the cause into the message when the
    /// caller might want to inspect it.
    pub(crate) fn with_source(
        message: impl Into<String>,
        source: impl StdError + Send + Sync + 'static,
    ) -> Self {
        Self {
            message: message.into(),
            source: Some(Box::new(source)),
        }
    }
}

impl Display for Error {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl StdError for Error {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        self.source
            .as_ref()
            .map(|source| &**source as &(dyn StdError + 'static))
    }
}

impl From<String> for Error {
    fn from(message: String) -> Self {
        Self::msg(message)
    }
}

impl From<&str> for Error {
    fn from(message: &str) -> Self {
        Self::msg(message)
    }
}

/// Generates the `From` impls that let `?` accept a foreign error directly.
///
/// The message is the source's own `Display` output, matching what the
/// previous `map_err(|error| error.to_string())` produced at each call site.
macro_rules! from_source {
    ($($source:ty),+ $(,)?) => {
        $(
            impl From<$source> for Error {
                fn from(source: $source) -> Self {
                    Self {
                        message: source.to_string(),
                        source: Some(Box::new(source)),
                    }
                }
            }
        )+
    };
}

from_source!(
    libsql::Error,
    notify::Error,
    regex::Error,
    serde_json::Error,
    std::io::Error,
    std::num::ParseFloatError,
    std::num::ParseIntError,
    std::num::TryFromIntError,
    std::str::Utf8Error,
    std::string::FromUtf8Error,
    std::time::SystemTimeError,
    tree_sitter::LanguageError,
    tree_sitter::QueryError,
);

#[cfg(test)]
mod tests {
    use super::Error;
    use std::error::Error as StdError;

    #[test]
    fn displays_the_message_without_the_source() {
        let io = std::io::Error::other("disk on fire");
        let error = Error::with_source("cannot read index", io);

        assert_eq!(error.to_string(), "cannot read index");
        assert_eq!(error.source().unwrap().to_string(), "disk on fire");
    }

    #[test]
    fn converting_a_foreign_error_keeps_its_message_and_cause() {
        let error = Error::from(std::io::Error::other("permission denied"));

        assert_eq!(error.to_string(), "permission denied");
        assert!(error.source().is_some());
    }

    #[test]
    fn plain_messages_have_no_source() {
        let error = Error::msg("remote database URL is not configured");

        assert_eq!(error.to_string(), "remote database URL is not configured");
        assert!(error.source().is_none());
    }
}
