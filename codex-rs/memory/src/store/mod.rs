pub mod jsonl;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbeddedRecord {
    pub repo_key: String,
    pub id: String,
    pub ts: u64,
    pub kind: String,
    pub title: String,
    pub text: String,
    pub dim: usize,
    pub vec: Vec<f32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SearchHit {
    pub id: String,
    pub score: f32,
    pub title: String,
    pub text: String,
    pub ts: u64,
}

