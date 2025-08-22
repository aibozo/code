use std::io;

/// Errors that can occur when producing embeddings.
#[derive(Debug)]
pub enum EmbeddingError {
    Io(io::Error),
    InvalidDimension { expected: usize, got: usize },
}

impl From<io::Error> for EmbeddingError {
    fn from(e: io::Error) -> Self { EmbeddingError::Io(e) }
}

/// Provider interface for generating fixed-dimension embeddings.
pub trait EmbeddingProvider: Send + Sync {
    /// Generate an embedding vector for each input text.
    /// Implementations must return vectors with the requested `dim`.
    fn embed(&self, texts: &[String], dim: usize) -> Result<Vec<Vec<f32>>, EmbeddingError>;
}

/// A deterministic, no-op embedding provider that returns all-zero vectors.
/// Useful for tests and when embeddings are disabled.
pub struct NoopEmbeddingProvider;

impl EmbeddingProvider for NoopEmbeddingProvider {
    fn embed(&self, texts: &[String], dim: usize) -> Result<Vec<Vec<f32>>, EmbeddingError> {
        let mut out = Vec::with_capacity(texts.len());
        for _ in texts {
            out.push(vec![0.0; dim]);
        }
        Ok(out)
    }
}

/// Compute the cosine similarity between two equal-length vectors.
/// Returns 0.0 when either vector has zero norm or the dimensions differ.
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() { return 0.0; }
    let mut dot = 0.0f64;
    let mut na = 0.0f64;
    let mut nb = 0.0f64;
    for i in 0..a.len() {
        let x = a[i] as f64;
        let y = b[i] as f64;
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    if na == 0.0 || nb == 0.0 { return 0.0; }
    (dot / (na.sqrt() * nb.sqrt())) as f32
}

#[cfg(test)]
mod tests {
    use super::cosine_similarity;

    #[test]
    fn cosine_orders_similarities() {
        let q = vec![1.0, 0.0, 0.0];
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![0.5, 0.5, 0.0];
        let c = vec![0.0, 1.0, 0.0];
        let s_a = cosine_similarity(&q, &a);
        let s_b = cosine_similarity(&q, &b);
        let s_c = cosine_similarity(&q, &c);
        assert!(s_a > s_b && s_b > s_c);
    }
}

