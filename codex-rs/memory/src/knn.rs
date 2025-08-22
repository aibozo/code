use crate::embedding::cosine_similarity;

#[derive(Debug, Clone)]
pub struct ScoredIdx {
    pub idx: usize,
    pub score: f32,
}

/// Return indices of the top_k most similar vectors (cosine similarity),
/// in descending score order.
pub fn top_k_cosine<'a>(haystack: &'a [Vec<f32>], query: &[f32], top_k: usize) -> Vec<ScoredIdx> {
    if top_k == 0 || haystack.is_empty() { return Vec::new(); }
    let mut scored: Vec<ScoredIdx> = haystack
        .iter()
        .enumerate()
        .map(|(i, v)| ScoredIdx { idx: i, score: cosine_similarity(v, query) })
        .collect();
    scored.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
    if scored.len() > top_k { scored.truncate(top_k); }
    scored
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn topk_basic() {
        let hay = vec![
            vec![1.0, 0.0],
            vec![0.7, 0.3],
            vec![0.0, 1.0],
        ];
        let q = vec![1.0, 0.0];
        let res = top_k_cosine(&hay, &q, 2);
        assert_eq!(res.len(), 2);
        assert_eq!(res[0].idx, 0);
    }
}

