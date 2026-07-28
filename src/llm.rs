//! Optional LLM synthesis over an OpenAI-compatible chat completions
//! endpoint, mirroring the embedding provider's environment configuration.
//! Synthesis is strictly opt-in (`hugr session promote --llm`) and callers
//! must treat any error as a signal to fall back to the deterministic path:
//! the LLM never sits between an agent and its context. Facts reach this
//! module already secret-redacted by the storage layer.

use serde_json::{Value, json};
use std::env;
use std::io::Write as _;
use std::process::{Command as ProcessCommand, Stdio};

const DEFAULT_OPENAI_CHAT_MODEL: &str = "gpt-4o-mini";
const DEFAULT_OPENAI_CHAT_URL: &str = "https://api.openai.com/v1/chat/completions";
const DEFAULT_OLLAMA_CHAT_MODEL: &str = "llama3.2";
const DEFAULT_OLLAMA_CHAT_URL: &str = "http://localhost:11434/v1/chat/completions";
const MAX_PROMPT_FACT_CHARS: usize = 12_000;
const MAX_SYNTHESIS_CHARS: usize = 2_000;
const SYNTHESIS_INSTRUCTION: &str = "You distill agent work sessions into durable project \
memory. Reply with 2 to 4 plain sentences covering what was done, what failed, and which \
decisions still matter. No headings, no lists, no preamble.";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ChatSynthesizer {
    provider: String,
    api_key: String,
    model: String,
    url: String,
}

impl ChatSynthesizer {
    pub(crate) fn from_env() -> Result<Self, String> {
        Self::from_lookup(|key| env::var(key).ok())
    }

    fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Result<Self, String> {
        let provider = lookup("HUGR_LLM_PROVIDER")
            .unwrap_or_else(|| "ollama".to_string())
            .trim()
            .to_lowercase();

        match provider.as_str() {
            "openai" => {
                let api_key = lookup("HUGR_OPENAI_API_KEY")
                    .or_else(|| lookup("OPENAI_API_KEY"))
                    .map(|value| value.trim().to_string())
                    .filter(|value| !value.is_empty())
                    .ok_or_else(|| {
                        "HUGR_LLM_PROVIDER=openai requires HUGR_OPENAI_API_KEY or OPENAI_API_KEY"
                            .to_string()
                    })?;
                Ok(Self {
                    provider,
                    api_key,
                    model: trimmed_or(lookup("HUGR_OPENAI_CHAT_MODEL"), DEFAULT_OPENAI_CHAT_MODEL),
                    url: trimmed_or(lookup("HUGR_OPENAI_CHAT_URL"), DEFAULT_OPENAI_CHAT_URL),
                })
            }
            "ollama" => Ok(Self {
                provider,
                api_key: trimmed_or(lookup("HUGR_OLLAMA_API_KEY"), ""),
                model: trimmed_or(lookup("HUGR_OLLAMA_CHAT_MODEL"), DEFAULT_OLLAMA_CHAT_MODEL),
                url: trimmed_or(lookup("HUGR_OLLAMA_CHAT_URL"), DEFAULT_OLLAMA_CHAT_URL),
            }),
            unknown => Err(format!(
                "unknown llm provider '{unknown}'; expected openai or ollama"
            )),
        }
    }

    pub(crate) fn provider(&self) -> &str {
        &self.provider
    }

    pub(crate) fn model(&self) -> &str {
        &self.model
    }

    pub(crate) fn synthesize(&self, task: &str, facts: &[String]) -> Result<String, String> {
        let prompt = synthesis_prompt(task, facts);
        let body = chat_request(&self.model, &prompt);
        let response = post_json_with_curl(&self.url, &self.api_key, &body)?;
        parse_chat_response(&response)
    }
}

fn trimmed_or(value: Option<String>, default: &str) -> String {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| default.to_string())
}

fn synthesis_prompt(task: &str, facts: &[String]) -> String {
    let mut prompt = format!("Session task: {task}\nObserved session events:\n");
    let mut used = 0usize;
    for fact in facts {
        let line = format!("- {}\n", fact.trim());
        used += line.chars().count();
        if used > MAX_PROMPT_FACT_CHARS {
            prompt.push_str("- (remaining events truncated)\n");
            break;
        }
        prompt.push_str(&line);
    }
    prompt.push_str("\nDistill the durable learnings from this session.");
    prompt
}

fn chat_request(model: &str, prompt: &str) -> Value {
    json!({
        "model": model,
        "temperature": 0,
        "messages": [
            { "role": "system", "content": SYNTHESIS_INSTRUCTION },
            { "role": "user", "content": prompt }
        ]
    })
}

fn parse_chat_response(response: &str) -> Result<String, String> {
    let value = serde_json::from_str::<Value>(response).map_err(|error| error.to_string())?;
    if let Some(message) = value
        .get("error")
        .and_then(|error| error.get("message"))
        .and_then(Value::as_str)
    {
        return Err(format!("llm request failed: {message}"));
    }

    let content = value
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
        .and_then(|choice| choice.get("message"))
        .and_then(|message| message.get("content"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|content| !content.is_empty())
        .ok_or_else(|| "llm response did not include choices[0].message.content".to_string())?;

    if content.chars().count() > MAX_SYNTHESIS_CHARS {
        let mut truncated = content
            .chars()
            .take(MAX_SYNTHESIS_CHARS)
            .collect::<String>();
        truncated.push_str("...");
        return Ok(truncated);
    }
    Ok(content.to_string())
}

fn post_json_with_curl(url: &str, api_key: &str, body: &Value) -> Result<String, String> {
    let mut args = vec![
        "-fsS".to_string(),
        "-X".to_string(),
        "POST".to_string(),
        url.to_string(),
        "-H".to_string(),
        "Content-Type: application/json".to_string(),
    ];
    if !api_key.is_empty() {
        args.push("-H".to_string());
        args.push(format!("Authorization: Bearer {api_key}"));
    }
    args.push("--data-binary".to_string());
    args.push("@-".to_string());

    let mut child = ProcessCommand::new("curl")
        .args(&args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("failed to execute curl for llm request: {error}"))?;

    {
        let stdin = child
            .stdin
            .as_mut()
            .ok_or_else(|| "failed to open curl stdin".to_string())?;
        stdin
            .write_all(body.to_string().as_bytes())
            .map_err(|error| format!("failed to write llm request: {error}"))?;
    }

    let output = child
        .wait_with_output()
        .map_err(|error| format!("failed to read llm response: {error}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        if stderr.is_empty() {
            return Err(format!("llm request failed with status {}", output.status));
        }
        return Err(format!("llm request failed: {stderr}"));
    }

    String::from_utf8(output.stdout).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::{ChatSynthesizer, parse_chat_response, synthesis_prompt};

    fn env_lookup<'a>(values: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |name| {
            values
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| (*value).to_string())
        }
    }

    #[test]
    fn defaults_to_local_ollama_chat() {
        let synthesizer = ChatSynthesizer::from_lookup(|_| None).unwrap();

        assert_eq!(synthesizer.provider(), "ollama");
        assert_eq!(synthesizer.model(), "llama3.2");
        assert_eq!(
            synthesizer.url,
            "http://localhost:11434/v1/chat/completions"
        );
        assert!(synthesizer.api_key.is_empty());
    }

    #[test]
    fn openai_provider_requires_a_key_and_reads_overrides() {
        assert!(
            ChatSynthesizer::from_lookup(env_lookup(&[("HUGR_LLM_PROVIDER", "openai")])).is_err()
        );

        let synthesizer = ChatSynthesizer::from_lookup(env_lookup(&[
            ("HUGR_LLM_PROVIDER", "openai"),
            ("OPENAI_API_KEY", "secret"),
            ("HUGR_OPENAI_CHAT_MODEL", "gpt-4.1-mini"),
        ]))
        .unwrap();

        assert_eq!(synthesizer.provider(), "openai");
        assert_eq!(synthesizer.model(), "gpt-4.1-mini");
        assert_eq!(
            synthesizer.url,
            "https://api.openai.com/v1/chat/completions"
        );
    }

    #[test]
    fn rejects_unknown_llm_providers() {
        assert!(
            ChatSynthesizer::from_lookup(env_lookup(&[("HUGR_LLM_PROVIDER", "bedrock")])).is_err()
        );
    }

    #[test]
    fn prompts_include_task_and_cap_fact_volume() {
        let facts = (0..4000)
            .map(|index| format!("event {index} with some detail text"))
            .collect::<Vec<_>>();

        let prompt = synthesis_prompt("stabilize plugin hooks", &facts);

        assert!(prompt.starts_with("Session task: stabilize plugin hooks"));
        assert!(prompt.contains("- event 0 with some detail text"));
        assert!(prompt.contains("(remaining events truncated)"));
        assert!(prompt.chars().count() < 14_000);
    }

    #[test]
    fn parses_chat_responses_and_rejects_empty_content() {
        let ok = r#"{"choices":[{"message":{"role":"assistant","content":"  The session wired hooks. "}}]}"#;
        assert_eq!(parse_chat_response(ok).unwrap(), "The session wired hooks.");

        let error = r#"{"error":{"message":"model overloaded"}}"#;
        assert!(
            parse_chat_response(error)
                .unwrap_err()
                .contains("model overloaded")
        );

        let empty = r#"{"choices":[{"message":{"content":"   "}}]}"#;
        assert!(parse_chat_response(empty).is_err());

        assert!(parse_chat_response("not json").is_err());
    }
}
