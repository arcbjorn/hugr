//! Serialization helpers shared by the crate's JSON renderers.
//!
//! Output shapes are pinned by snapshot tests because the CLI's `--json`
//! mode and the MCP server are a contract that agents parse. Anything here
//! that changes key order, key names, or escaping is a wire change.

use serde::{Serialize, Serializer};

/// Serializes a field that already holds serialized JSON.
///
/// Memories carry a `structured_payload` string produced elsewhere. Splicing
/// the parsed value in keeps consumers able to walk into it
/// (`structured_payload.source.type`) instead of handing them a quoted blob.
/// A payload that does not parse falls back to a plain string, so a malformed
/// row can never produce malformed output.
pub(crate) fn serialize_embedded_json<S>(
    payload: &Option<String>,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    match payload {
        Some(payload) => match serde_json::from_str::<serde_json::Value>(payload) {
            Ok(value) => value.serialize(serializer),
            Err(_) => serializer.serialize_str(payload),
        },
        None => serializer.serialize_none(),
    }
}

/// Renders a value that cannot fail to serialize.
///
/// Every type this is called with is built from strings, integers, options,
/// and vectors of the same, and the one custom serializer above falls back to
/// a string rather than erroring — so a failure here is a bug in this crate,
/// not a runtime condition a caller could handle.
pub(crate) fn render<T: Serialize>(value: &T) -> String {
    serde_json::to_string(value)
        .unwrap_or_else(|error| unreachable!("json rendering cannot fail: {error}"))
}

#[cfg(test)]
mod tests {
    use super::{render, serialize_embedded_json};
    use serde::Serialize;

    #[derive(Serialize)]
    struct Row {
        id: &'static str,
        #[serde(serialize_with = "serialize_embedded_json")]
        payload: Option<String>,
    }

    #[test]
    fn splices_parsable_payloads_and_keeps_field_order() {
        let row = Row {
            id: "mem_1",
            payload: Some(r#"{"source":"session"}"#.to_string()),
        };

        assert_eq!(
            render(&row),
            r#"{"id":"mem_1","payload":{"source":"session"}}"#
        );
    }

    #[test]
    fn falls_back_to_a_string_when_the_payload_is_not_json() {
        let row = Row {
            id: "mem_2",
            payload: Some("not json".to_string()),
        };

        assert_eq!(render(&row), r#"{"id":"mem_2","payload":"not json"}"#);
    }

    #[test]
    fn renders_a_missing_payload_as_null() {
        let row = Row {
            id: "mem_3",
            payload: None,
        };

        assert_eq!(render(&row), r#"{"id":"mem_3","payload":null}"#);
    }
}
