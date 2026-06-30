pub(crate) const DEFAULT_EMBEDDING_DIMENSIONS: usize = 1536;
pub(crate) const DETERMINISTIC_MODEL: &str = "hugr-deterministic-v1";

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Embedding {
    pub model: String,
    pub vector: Vec<f32>,
}

impl Embedding {
    pub fn dimensions(&self) -> usize {
        self.vector.len()
    }

    pub fn to_f32_blob(&self) -> Vec<u8> {
        let mut blob = Vec::with_capacity(self.vector.len() * 4);
        for value in &self.vector {
            blob.extend_from_slice(&value.to_le_bytes());
        }
        blob
    }
}

pub(crate) trait EmbeddingProvider {
    fn model(&self) -> &str;
    fn dimensions(&self) -> usize;
    fn embed(&self, text: &str) -> Result<Embedding, String>;
}

#[derive(Debug, Clone)]
pub(crate) struct DeterministicEmbeddingProvider {
    dimensions: usize,
}

impl DeterministicEmbeddingProvider {
    pub fn new(dimensions: usize) -> Result<Self, String> {
        if dimensions == 0 {
            return Err("embedding dimensions must be greater than zero".to_string());
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

    fn embed(&self, text: &str) -> Result<Embedding, String> {
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

fn embedding_terms(text: &str) -> Vec<String> {
    text.split(|char: char| !char.is_alphanumeric() && char != '_' && char != '-')
        .filter(|term| !term.is_empty())
        .map(|term| term.to_lowercase())
        .collect()
}

fn stable_hash(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf29ce484222325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
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
    use super::{DETERMINISTIC_MODEL, DeterministicEmbeddingProvider, EmbeddingProvider};

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
    fn embeddings_encode_as_f32_blob_bytes() {
        let provider = DeterministicEmbeddingProvider::new(8).unwrap();
        let embedding = provider.embed("plugin hooks").unwrap();

        assert_eq!(embedding.to_f32_blob().len(), 8 * 4);
    }
}
