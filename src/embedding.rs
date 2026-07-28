use crate::error::{Error, Result};
use serde_json::{Value, json};
use std::env;
use std::io::Write as _;
use std::process::{Command as ProcessCommand, Stdio};

pub(crate) const DEFAULT_EMBEDDING_DIMENSIONS: usize = 1536;
/// Width of the `F32_BLOB` vector columns in the schema. Every stored blob and
/// query literal is normalized to this width so providers with other native
/// dimensionalities still work against the fixed-width vector indexes.
pub(crate) const STORAGE_EMBEDDING_DIMENSIONS: usize = 1536;
pub(crate) const DETERMINISTIC_MODEL: &str = "hugr-deterministic-v1";
const DEFAULT_OPENAI_EMBEDDING_MODEL: &str = "text-embedding-3-small";
const DEFAULT_OPENAI_EMBEDDING_URL: &str = "https://api.openai.com/v1/embeddings";
const DEFAULT_OLLAMA_EMBEDDING_MODEL: &str = "nomic-embed-text";
const DEFAULT_OLLAMA_EMBEDDING_URL: &str = "http://localhost:11434/v1/embeddings";
const DEFAULT_OLLAMA_EMBEDDING_DIMENSIONS: usize = 768;
#[cfg(feature = "local-embeddings")]
const DEFAULT_LOCAL_EMBEDDING_MODEL: &str = "bge-small-en-v1.5";

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Embedding {
    pub model: String,
    pub vector: Vec<f32>,
}

impl Embedding {
    pub(crate) fn dimensions(&self) -> usize {
        self.vector.len()
    }

    /// Shorter vectors zero-pad up to the storage width, which leaves cosine
    /// and Euclidean ordering unchanged; longer vectors truncate, which is
    /// approximate but keeps oversized models usable against the schema.
    fn storage_vector(&self) -> Vec<f32> {
        let mut vector = self.vector.clone();
        vector.resize(STORAGE_EMBEDDING_DIMENSIONS, 0.0);
        vector
    }

    pub(crate) fn to_f32_blob(&self) -> Vec<u8> {
        let vector = self.storage_vector();
        let mut blob = Vec::with_capacity(vector.len() * 4);
        for value in &vector {
            blob.extend_from_slice(&value.to_le_bytes());
        }
        blob
    }

    pub(crate) fn to_vector_literal(&self) -> String {
        format!(
            "[{}]",
            self.storage_vector()
                .iter()
                .map(std::string::ToString::to_string)
                .collect::<Vec<_>>()
                .join(",")
        )
    }
}

pub(crate) trait EmbeddingProvider {
    fn model(&self) -> &str;
    fn dimensions(&self) -> usize;
    fn embed(&self, text: &str) -> Result<Embedding>;
}

#[derive(Debug, Clone)]
pub(crate) enum SelectedEmbeddingProvider {
    Deterministic(DeterministicEmbeddingProvider),
    OpenAi(OpenAiEmbeddingProvider),
    #[cfg(feature = "local-embeddings")]
    Local(LocalEmbeddingProvider),
}

impl SelectedEmbeddingProvider {
    pub(crate) fn from_env() -> Result<Self> {
        EmbeddingProviderConfig::from_env()?.provider()
    }
}

impl Default for SelectedEmbeddingProvider {
    fn default() -> Self {
        Self::Deterministic(DeterministicEmbeddingProvider::default())
    }
}

impl EmbeddingProvider for SelectedEmbeddingProvider {
    fn model(&self) -> &str {
        match self {
            Self::Deterministic(provider) => provider.model(),
            Self::OpenAi(provider) => provider.model(),
            #[cfg(feature = "local-embeddings")]
            Self::Local(provider) => provider.model(),
        }
    }

    fn dimensions(&self) -> usize {
        match self {
            Self::Deterministic(provider) => provider.dimensions(),
            Self::OpenAi(provider) => provider.dimensions(),
            #[cfg(feature = "local-embeddings")]
            Self::Local(provider) => provider.dimensions(),
        }
    }

    fn embed(&self, text: &str) -> Result<Embedding> {
        match self {
            Self::Deterministic(provider) => provider.embed(text),
            Self::OpenAi(provider) => provider.embed(text),
            #[cfg(feature = "local-embeddings")]
            Self::Local(provider) => provider.embed(text),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum EmbeddingProviderConfig {
    Deterministic {
        dimensions: usize,
    },
    OpenAi {
        api_key: String,
        model: String,
        url: String,
        dimensions: usize,
    },
    #[cfg(feature = "local-embeddings")]
    Local {
        model: String,
    },
}

impl EmbeddingProviderConfig {
    pub(crate) fn from_env() -> Result<Self> {
        Self::from_lookup(|key| env::var(key).ok())
    }

    fn provider(self) -> Result<SelectedEmbeddingProvider> {
        match self {
            Self::Deterministic { dimensions } => Ok(SelectedEmbeddingProvider::Deterministic(
                DeterministicEmbeddingProvider::new(dimensions)?,
            )),
            Self::OpenAi {
                api_key,
                model,
                url,
                dimensions,
            } => Ok(SelectedEmbeddingProvider::OpenAi(OpenAiEmbeddingProvider {
                api_key,
                model,
                url,
                dimensions,
            })),
            #[cfg(feature = "local-embeddings")]
            Self::Local { model } => Ok(SelectedEmbeddingProvider::Local(
                LocalEmbeddingProvider::new(&model)?,
            )),
        }
    }

    fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Result<Self> {
        let provider = lookup("HUGR_EMBEDDING_PROVIDER")
            .unwrap_or_else(|| "deterministic".to_string())
            .trim()
            .to_lowercase();
        let dimensions_override = lookup("HUGR_EMBEDDING_DIMENSIONS")
            .as_deref()
            .map(parse_dimensions)
            .transpose()?;

        match provider.as_str() {
            "deterministic" | "offline" => Ok(Self::Deterministic {
                dimensions: dimensions_override.unwrap_or(DEFAULT_EMBEDDING_DIMENSIONS),
            }),
            "openai" => {
                let api_key = lookup("HUGR_OPENAI_API_KEY")
                    .or_else(|| lookup("OPENAI_API_KEY"))
                    .map(|value| value.trim().to_string())
                    .filter(|value| !value.is_empty())
                    .ok_or_else(|| {
                        "HUGR_EMBEDDING_PROVIDER=openai requires HUGR_OPENAI_API_KEY or OPENAI_API_KEY"
                            .to_string()
                    })?;
                let model = lookup("HUGR_OPENAI_EMBEDDING_MODEL")
                    .map(|value| value.trim().to_string())
                    .filter(|value| !value.is_empty())
                    .unwrap_or_else(|| DEFAULT_OPENAI_EMBEDDING_MODEL.to_string());
                let url = lookup("HUGR_OPENAI_EMBEDDING_URL")
                    .map(|value| value.trim().to_string())
                    .filter(|value| !value.is_empty())
                    .unwrap_or_else(|| DEFAULT_OPENAI_EMBEDDING_URL.to_string());

                Ok(Self::OpenAi {
                    api_key,
                    model,
                    url,
                    dimensions: dimensions_override.unwrap_or(DEFAULT_EMBEDDING_DIMENSIONS),
                })
            }
            // Ollama exposes an OpenAI-compatible embeddings endpoint, so the
            // alias reuses that transport with localhost defaults and no key:
            // real local semantic recall without a cloud account.
            "ollama" => {
                let api_key = lookup("HUGR_OLLAMA_API_KEY")
                    .map(|value| value.trim().to_string())
                    .filter(|value| !value.is_empty())
                    .unwrap_or_default();
                let model = lookup("HUGR_OLLAMA_EMBEDDING_MODEL")
                    .map(|value| value.trim().to_string())
                    .filter(|value| !value.is_empty())
                    .unwrap_or_else(|| DEFAULT_OLLAMA_EMBEDDING_MODEL.to_string());
                let url = lookup("HUGR_OLLAMA_EMBEDDING_URL")
                    .map(|value| value.trim().to_string())
                    .filter(|value| !value.is_empty())
                    .unwrap_or_else(|| DEFAULT_OLLAMA_EMBEDDING_URL.to_string());

                Ok(Self::OpenAi {
                    api_key,
                    model,
                    url,
                    dimensions: dimensions_override.unwrap_or(DEFAULT_OLLAMA_EMBEDDING_DIMENSIONS),
                })
            }
            // In-process ONNX embeddings: no service, no key, model fetched
            // once into the cache directory. Available unless the binary was
            // built without the local-embeddings feature.
            "local" => {
                #[cfg(feature = "local-embeddings")]
                {
                    let model = lookup("HUGR_LOCAL_EMBEDDING_MODEL")
                        .map(|value| value.trim().to_string())
                        .filter(|value| !value.is_empty())
                        .unwrap_or_else(|| DEFAULT_LOCAL_EMBEDDING_MODEL.to_string());
                    local_embedding_model(&model)?;
                    Ok(Self::Local { model })
                }
                #[cfg(not(feature = "local-embeddings"))]
                {
                    Err(Error::msg(
                        "this hugr build does not include local embeddings; rebuild with \
                         --features local-embeddings or use the ollama or openai provider"
                            .to_string(),
                    ))
                }
            }
            unknown => Err(Error::msg(format!(
                "unknown embedding provider '{unknown}'; expected deterministic, local, openai, or ollama"
            ))),
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct DeterministicEmbeddingProvider {
    dimensions: usize,
}

impl DeterministicEmbeddingProvider {
    pub(crate) fn new(dimensions: usize) -> Result<Self> {
        if dimensions == 0 {
            return Err(Error::msg(
                "embedding dimensions must be greater than zero".to_string(),
            ));
        }

        Ok(Self { dimensions })
    }
}

impl Default for DeterministicEmbeddingProvider {
    fn default() -> Self {
        Self::new(DEFAULT_EMBEDDING_DIMENSIONS)
            .expect("default embedding dimensions should be valid")
    }
}

impl EmbeddingProvider for DeterministicEmbeddingProvider {
    fn model(&self) -> &str {
        DETERMINISTIC_MODEL
    }

    fn dimensions(&self) -> usize {
        self.dimensions
    }

    fn embed(&self, text: &str) -> Result<Embedding> {
        let mut vector = vec![0.0; self.dimensions()];

        for term in embedding_terms(text) {
            let hash = stable_hash(term.as_bytes());
            let index = (hash as usize) % self.dimensions();
            let sign = if hash & (1 << 63) == 0 { 1.0 } else { -1.0 };
            let weight = 1.0 + (((hash >> 32) & 0xff) as f32 / 255.0);
            vector[index] += sign * weight;
        }

        normalize(&mut vector);

        Ok(Embedding {
            model: self.model().to_string(),
            vector,
        })
    }
}

/// In-process ONNX text embeddings via fastembed. The engine is loaded once
/// on first use (downloading the model into the cache directory when absent)
/// and shared behind a mutex because ONNX sessions want exclusive access
/// during inference.
#[cfg(feature = "local-embeddings")]
#[derive(Clone)]
pub(crate) struct LocalEmbeddingProvider {
    model_name: String,
    dimensions: usize,
    engine: std::sync::Arc<std::sync::OnceLock<Result<std::sync::Mutex<fastembed::TextEmbedding>>>>,
}

#[cfg(feature = "local-embeddings")]
impl std::fmt::Debug for LocalEmbeddingProvider {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LocalEmbeddingProvider")
            .field("model_name", &self.model_name)
            .field("dimensions", &self.dimensions)
            .finish_non_exhaustive()
    }
}

#[cfg(feature = "local-embeddings")]
impl LocalEmbeddingProvider {
    pub fn new(model_name: &str) -> Result<Self> {
        let (_, dimensions) = local_embedding_model(model_name)?;
        Ok(Self {
            model_name: model_name.to_string(),
            dimensions,
            engine: std::sync::Arc::new(std::sync::OnceLock::new()),
        })
    }
}

#[cfg(feature = "local-embeddings")]
impl EmbeddingProvider for LocalEmbeddingProvider {
    fn model(&self) -> &str {
        &self.model_name
    }

    fn dimensions(&self) -> usize {
        self.dimensions
    }

    fn embed(&self, text: &str) -> Result<Embedding> {
        let engine = self.engine.get_or_init(|| {
            build_local_embedding_engine(&self.model_name).map(std::sync::Mutex::new)
        });
        let engine = engine
            .as_ref()
            .map_err(|error| Error::msg(error.to_string()))?;
        let mut engine = engine
            .lock()
            .map_err(|_| Error::msg("local embedding engine mutex poisoned".to_string()))?;
        let mut vectors = engine
            .embed(vec![text.to_string()], None)
            .map_err(|error| {
                Error::with_source(format!("local embedding failed: {error}"), error)
            })?;
        let vector = vectors
            .pop()
            .ok_or_else(|| Error::msg("local embedding returned no vector".to_string()))?;

        Ok(Embedding {
            model: self.model_name.clone(),
            vector,
        })
    }
}

/// Maps a user-facing model name to fastembed's model id and its native
/// dimensionality. Kept to a small curated set so configuration errors
/// surface at startup instead of after a model download.
#[cfg(feature = "local-embeddings")]
fn local_embedding_model(name: &str) -> Result<(fastembed::EmbeddingModel, usize)> {
    match name {
        "bge-small-en-v1.5" => Ok((fastembed::EmbeddingModel::BGESmallENV15, 384)),
        "bge-base-en-v1.5" => Ok((fastembed::EmbeddingModel::BGEBaseENV15, 768)),
        "all-minilm-l6-v2" => Ok((fastembed::EmbeddingModel::AllMiniLML6V2, 384)),
        "nomic-embed-text-v1.5" => Ok((fastembed::EmbeddingModel::NomicEmbedTextV15, 768)),
        "multilingual-e5-small" => Ok((fastembed::EmbeddingModel::MultilingualE5Small, 384)),
        unknown => Err(Error::msg(format!(
            "unknown local embedding model '{unknown}'; expected one of bge-small-en-v1.5, \
             bge-base-en-v1.5, all-minilm-l6-v2, nomic-embed-text-v1.5, multilingual-e5-small"
        ))),
    }
}

#[cfg(feature = "local-embeddings")]
fn build_local_embedding_engine(model_name: &str) -> Result<fastembed::TextEmbedding> {
    let (model, _) = local_embedding_model(model_name)?;
    let mut options = fastembed::TextInitOptions::new(model).with_show_download_progress(false);
    if let Some(cache_dir) = local_embedding_cache_dir(|name| env::var(name).ok()) {
        options = options.with_cache_dir(cache_dir);
    }
    fastembed::TextEmbedding::try_new(options).map_err(|error| {
        Error::with_source(
            format!("failed to load local embedding model '{model_name}': {error}"),
            error,
        )
    })
}

/// Model cache resolution: explicit override, else ~/.hugr/models next to the
/// global memory store, else fastembed's own default.
#[cfg(feature = "local-embeddings")]
fn local_embedding_cache_dir(
    lookup: impl Fn(&str) -> Option<String>,
) -> Option<std::path::PathBuf> {
    if let Some(dir) = lookup("HUGR_LOCAL_EMBEDDING_CACHE").filter(|value| !value.trim().is_empty())
    {
        return Some(std::path::PathBuf::from(dir));
    }
    lookup("HOME")
        .filter(|value| !value.trim().is_empty())
        .map(|home| std::path::PathBuf::from(home).join(".hugr").join("models"))
}

#[derive(Debug, Clone)]
pub(crate) struct OpenAiEmbeddingProvider {
    api_key: String,
    model: String,
    url: String,
    dimensions: usize,
}

impl EmbeddingProvider for OpenAiEmbeddingProvider {
    fn model(&self) -> &str {
        &self.model
    }

    fn dimensions(&self) -> usize {
        self.dimensions
    }

    fn embed(&self, text: &str) -> Result<Embedding> {
        let body = openai_embedding_request(&self.model, text);
        let response = post_json_with_curl(&self.url, &self.api_key, &body)?;
        parse_openai_embedding_response(&response, &self.model, self.dimensions)
    }
}

fn openai_embedding_request(model: &str, text: &str) -> Value {
    json!({
        "model": model,
        "input": text
    })
}

fn post_json_with_curl(url: &str, api_key: &str, body: &Value) -> Result<String> {
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
        .map_err(|error| {
            Error::with_source(
                format!("failed to execute curl for embeddings: {error}"),
                error,
            )
        })?;

    {
        let stdin = child
            .stdin
            .as_mut()
            .ok_or_else(|| Error::msg("failed to open curl stdin".to_string()))?;
        stdin
            .write_all(body.to_string().as_bytes())
            .map_err(|error| {
                Error::with_source(format!("failed to write embedding request: {error}"), error)
            })?;
    }

    let output = child.wait_with_output().map_err(|error| {
        Error::with_source(format!("failed to read embedding response: {error}"), error)
    })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        if stderr.is_empty() {
            return Err(Error::msg(format!(
                "embedding request failed with status {}",
                output.status
            )));
        }
        return Err(Error::msg(format!("embedding request failed: {stderr}")));
    }

    String::from_utf8(output.stdout).map_err(Error::from)
}

fn parse_openai_embedding_response(
    response: &str,
    fallback_model: &str,
    expected_dimensions: usize,
) -> Result<Embedding> {
    let value = serde_json::from_str::<Value>(response)?;
    if let Some(message) = value
        .get("error")
        .and_then(|error| error.get("message"))
        .and_then(Value::as_str)
    {
        return Err(Error::msg(format!("embedding request failed: {message}")));
    }

    let embedding = value
        .get("data")
        .and_then(Value::as_array)
        .and_then(|data| data.first())
        .and_then(|item| item.get("embedding"))
        .and_then(Value::as_array)
        .ok_or_else(|| {
            Error::msg("embedding response did not include data[0].embedding".to_string())
        })?;
    let vector = embedding
        .iter()
        .map(|value| {
            value.as_f64().map(|value| value as f32).ok_or_else(|| {
                Error::msg("embedding response included a non-numeric value".to_string())
            })
        })
        .collect::<Result<Vec<_>, _>>()?;

    if vector.len() != expected_dimensions {
        return Err(Error::msg(format!(
            "embedding response dimensions {} did not match expected {}",
            vector.len(),
            expected_dimensions
        )));
    }

    let model = value
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or(fallback_model)
        .to_string();

    Ok(Embedding { model, vector })
}

fn parse_dimensions(value: &str) -> Result<usize> {
    let dimensions = value.trim().parse::<usize>().map_err(|error| {
        Error::with_source(format!("invalid HUGR_EMBEDDING_DIMENSIONS: {error}"), error)
    })?;
    if dimensions == 0 {
        Err(Error::msg(
            "embedding dimensions must be greater than zero".to_string(),
        ))
    } else {
        Ok(dimensions)
    }
}

fn embedding_terms(text: &str) -> Vec<String> {
    text.split(|char: char| !char.is_alphanumeric() && char != '_' && char != '-')
        .filter(|term| !term.is_empty())
        .map(str::to_lowercase)
        .collect()
}

fn stable_hash(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

fn normalize(vector: &mut [f32]) {
    let magnitude = vector.iter().map(|value| value * value).sum::<f32>().sqrt();

    if magnitude == 0.0 {
        return;
    }

    for value in vector {
        *value /= magnitude;
    }
}

#[cfg(test)]
mod tests {
    use super::{
        DEFAULT_EMBEDDING_DIMENSIONS, DETERMINISTIC_MODEL, DeterministicEmbeddingProvider,
        EmbeddingProvider, EmbeddingProviderConfig, STORAGE_EMBEDDING_DIMENSIONS,
        parse_openai_embedding_response,
    };
    use std::collections::HashMap;

    #[test]
    fn deterministic_provider_rejects_zero_dimensions() {
        assert!(DeterministicEmbeddingProvider::new(0).is_err());
    }

    #[test]
    fn deterministic_provider_returns_stable_normalized_embeddings() {
        let provider = DeterministicEmbeddingProvider::new(16).unwrap();
        let left = provider.embed("plugin hooks").unwrap();
        let right = provider.embed("plugin hooks").unwrap();
        let other = provider.embed("database migrations").unwrap();

        assert_eq!(left.model, DETERMINISTIC_MODEL);
        assert_eq!(left.dimensions(), 16);
        assert_eq!(left, right);
        assert_ne!(left, other);

        let magnitude = left
            .vector
            .iter()
            .map(|value| value * value)
            .sum::<f32>()
            .sqrt();
        assert!((magnitude - 1.0).abs() < 0.0001);
    }

    #[test]
    fn embeddings_encode_as_storage_width_f32_blobs() {
        let provider = DeterministicEmbeddingProvider::new(8).unwrap();
        let embedding = provider.embed("plugin hooks").unwrap();

        assert_eq!(embedding.dimensions(), 8, "native dimensions are reported");
        assert_eq!(
            embedding.to_f32_blob().len(),
            STORAGE_EMBEDDING_DIMENSIONS * 4,
            "stored blobs are padded to the schema vector width"
        );
    }

    #[test]
    fn embeddings_encode_as_storage_width_vector_literals() {
        let provider = DeterministicEmbeddingProvider::new(4).unwrap();
        let embedding = provider.embed("plugin hooks").unwrap();
        let literal = embedding.to_vector_literal();

        assert!(literal.starts_with('['));
        assert!(literal.ends_with(']'));
        assert_eq!(
            literal.matches(',').count(),
            STORAGE_EMBEDDING_DIMENSIONS - 1
        );
    }

    #[test]
    fn oversized_embeddings_truncate_to_storage_width() {
        let embedding = super::Embedding {
            model: "test".to_string(),
            vector: vec![1.0; STORAGE_EMBEDDING_DIMENSIONS + 100],
        };

        assert_eq!(
            embedding.to_f32_blob().len(),
            STORAGE_EMBEDDING_DIMENSIONS * 4
        );
    }

    #[test]
    fn zero_padding_preserves_cosine_ordering() {
        let provider = DeterministicEmbeddingProvider::new(16).unwrap();
        let query = provider.embed("plugin hooks").unwrap();
        let close = provider.embed("plugin hooks loaded").unwrap();
        let far = provider.embed("database migrations").unwrap();

        let native = |left: &super::Embedding, right: &super::Embedding| {
            left.vector
                .iter()
                .zip(&right.vector)
                .map(|(a, b)| a * b)
                .sum::<f32>()
        };
        let padded = |left: &super::Embedding, right: &super::Embedding| {
            left.to_f32_blob()
                .chunks_exact(4)
                .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
                .zip(
                    right
                        .to_f32_blob()
                        .chunks_exact(4)
                        .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]])),
                )
                .map(|(a, b)| a * b)
                .sum::<f32>()
        };

        let native_order = native(&query, &close) > native(&query, &far);
        let padded_order = padded(&query, &close) > padded(&query, &far);
        assert_eq!(native_order, padded_order);
    }

    #[cfg(feature = "local-embeddings")]
    #[test]
    fn config_reads_local_provider_and_validates_models() {
        let config = EmbeddingProviderConfig::from_lookup(env_lookup(&[(
            "HUGR_EMBEDDING_PROVIDER",
            "local",
        )]))
        .unwrap();
        assert_eq!(
            config,
            EmbeddingProviderConfig::Local {
                model: "bge-small-en-v1.5".to_string()
            }
        );

        let overridden = EmbeddingProviderConfig::from_lookup(env_lookup(&[
            ("HUGR_EMBEDDING_PROVIDER", "local"),
            ("HUGR_LOCAL_EMBEDDING_MODEL", "all-minilm-l6-v2"),
        ]))
        .unwrap();
        assert_eq!(
            overridden,
            EmbeddingProviderConfig::Local {
                model: "all-minilm-l6-v2".to_string()
            }
        );

        let unknown = EmbeddingProviderConfig::from_lookup(env_lookup(&[
            ("HUGR_EMBEDDING_PROVIDER", "local"),
            ("HUGR_LOCAL_EMBEDDING_MODEL", "made-up-model"),
        ]));
        assert!(unknown.is_err(), "unknown models must fail at config time");
    }

    #[cfg(feature = "local-embeddings")]
    #[test]
    fn local_provider_reports_dimensions_without_loading_the_model() {
        let provider = super::LocalEmbeddingProvider::new("bge-small-en-v1.5").unwrap();
        assert_eq!(provider.dimensions(), 384);
        assert_eq!(provider.model(), "bge-small-en-v1.5");
    }

    #[cfg(feature = "local-embeddings")]
    #[test]
    fn local_cache_dir_prefers_override_then_home() {
        let override_lookup =
            |name: &str| (name == "HUGR_LOCAL_EMBEDDING_CACHE").then(|| "/models".to_string());
        assert_eq!(
            super::local_embedding_cache_dir(override_lookup),
            Some(std::path::PathBuf::from("/models"))
        );

        let home_lookup = |name: &str| (name == "HOME").then(|| "/home/dev".to_string());
        assert_eq!(
            super::local_embedding_cache_dir(home_lookup),
            Some(std::path::PathBuf::from("/home/dev/.hugr/models"))
        );

        assert_eq!(super::local_embedding_cache_dir(|_| None), None);
    }

    #[cfg(feature = "local-embeddings")]
    #[test]
    #[ignore = "downloads the embedding model on first run"]
    fn local_provider_embeds_with_real_semantics() {
        let provider = super::LocalEmbeddingProvider::new("bge-small-en-v1.5").unwrap();

        let query = provider.embed("database connection pooling").unwrap();
        let close = provider
            .embed("reusing sql connections efficiently")
            .unwrap();
        let far = provider.embed("css button hover animation").unwrap();

        assert_eq!(query.dimensions(), 384);
        assert_eq!(
            query.to_f32_blob().len(),
            STORAGE_EMBEDDING_DIMENSIONS * 4,
            "local vectors must normalize to storage width"
        );

        let cosine = |left: &super::Embedding, right: &super::Embedding| {
            let dot = left
                .vector
                .iter()
                .zip(&right.vector)
                .map(|(a, b)| a * b)
                .sum::<f32>();
            let norm = |v: &[f32]| v.iter().map(|x| x * x).sum::<f32>().sqrt();
            dot / (norm(&left.vector) * norm(&right.vector))
        };
        assert!(
            cosine(&query, &close) > cosine(&query, &far),
            "semantically close text must score higher"
        );
    }

    #[test]
    fn config_reads_ollama_provider_with_local_defaults() {
        let config = EmbeddingProviderConfig::from_lookup(env_lookup(&[(
            "HUGR_EMBEDDING_PROVIDER",
            "ollama",
        )]))
        .unwrap();

        assert_eq!(
            config,
            EmbeddingProviderConfig::OpenAi {
                api_key: String::new(),
                model: "nomic-embed-text".to_string(),
                url: "http://localhost:11434/v1/embeddings".to_string(),
                dimensions: 768
            }
        );
    }

    #[test]
    fn config_reads_ollama_overrides() {
        let config = EmbeddingProviderConfig::from_lookup(env_lookup(&[
            ("HUGR_EMBEDDING_PROVIDER", "ollama"),
            ("HUGR_OLLAMA_EMBEDDING_MODEL", "mxbai-embed-large"),
            (
                "HUGR_OLLAMA_EMBEDDING_URL",
                "http://box:11434/v1/embeddings",
            ),
            ("HUGR_EMBEDDING_DIMENSIONS", "1024"),
        ]))
        .unwrap();

        assert_eq!(
            config,
            EmbeddingProviderConfig::OpenAi {
                api_key: String::new(),
                model: "mxbai-embed-large".to_string(),
                url: "http://box:11434/v1/embeddings".to_string(),
                dimensions: 1024
            }
        );
    }

    #[test]
    fn config_defaults_to_deterministic_provider() {
        let config = EmbeddingProviderConfig::from_lookup(|_| None).unwrap();

        assert_eq!(
            config,
            EmbeddingProviderConfig::Deterministic {
                dimensions: DEFAULT_EMBEDDING_DIMENSIONS
            }
        );
    }

    #[test]
    fn config_reads_openai_provider() {
        let config = EmbeddingProviderConfig::from_lookup(env_lookup(&[
            ("HUGR_EMBEDDING_PROVIDER", "openai"),
            ("OPENAI_API_KEY", "secret"),
            ("HUGR_OPENAI_EMBEDDING_MODEL", "text-embedding-3-small"),
            ("HUGR_EMBEDDING_DIMENSIONS", "1536"),
        ]))
        .unwrap();

        assert_eq!(
            config,
            EmbeddingProviderConfig::OpenAi {
                api_key: "secret".to_string(),
                model: "text-embedding-3-small".to_string(),
                url: "https://api.openai.com/v1/embeddings".to_string(),
                dimensions: 1536
            }
        );
    }

    #[test]
    fn config_requires_openai_key() {
        let error = EmbeddingProviderConfig::from_lookup(env_lookup(&[(
            "HUGR_EMBEDDING_PROVIDER",
            "openai",
        )]))
        .unwrap_err()
        .to_string();

        assert!(error.contains("requires HUGR_OPENAI_API_KEY"));
    }

    #[test]
    fn parses_openai_embedding_response() {
        let response = r#"
        {
            "object": "list",
            "model": "text-embedding-3-small",
            "data": [
                {
                    "object": "embedding",
                    "index": 0,
                    "embedding": [0.25, -0.5, 0.75]
                }
            ]
        }
        "#;

        let embedding = parse_openai_embedding_response(response, "fallback-model", 3).unwrap();

        assert_eq!(embedding.model, "text-embedding-3-small");
        assert_eq!(embedding.vector, vec![0.25, -0.5, 0.75]);
    }

    #[test]
    fn rejects_openai_dimension_mismatch() {
        let response = r#"{"data":[{"embedding":[1.0,2.0]}]}"#;

        let error = parse_openai_embedding_response(response, "model", 3)
            .unwrap_err()
            .to_string();

        assert!(error.contains("did not match expected"));
    }

    fn env_lookup(values: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let values = values
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect::<HashMap<_, _>>();

        move |key| values.get(key).cloned()
    }
}
