# Phase 2 — Embedding Memory (Optional)

Purpose

- Improve recall with semantic search over summaries and user-authored notes.
- Keep dependencies modest; no external vector DB. Use sqlite + cosine sim.

Feature Flags

- `memory.embedding.enabled = false` (default)
- `memory.embedding.provider = "openai" | "local"` (default: `openai` when enabled)
- `memory.embedding.top_k = 5` (default)
- `memory.embedding.dim = 3072` (example; depends on chosen model)

Crate: `codex-memory` (new)

- Location: `codex-rs/memory` (crate name `codex-memory`)
- Purpose: embeddings + store (sqlite) + retrieval API.
- Public surface area (keep small, stable):
  - Traits
    - `pub trait EmbeddingProvider { fn embed(&self, text: &str) -> anyhow::Result<Vec<f32>>; }`
  - Providers
    - `OpenAiEmbeddingProvider` (uses existing Codex auth/provider; calls embeddings endpoint). Keep this optional behind a feature flag if needed, but prefer default on.
    - `LocalEmbeddingProvider` behind feature `local-embeddings` (e.g., CodeBERT); not required to ship Phase 2.
  - Store
    - `pub struct MemoryStore { /* sqlite conn pool */ }`
    - `impl MemoryStore { pub fn open(path: &Path) -> anyhow::Result<Self>; pub fn migrate(&self) -> anyhow::Result<()>; }`
    - `pub fn insert_summary(&self, repo_key: &str, session_id: &Uuid, title: &str, text: &str, vector: Option<&[f32]>, ts_ms: i64) -> anyhow::Result<i64>`
    - `pub fn recent_summaries(&self, repo_key: &str, limit: usize) -> anyhow::Result<Vec<Row>>`
    - `pub fn similar_text(&self, repo_key: &str, query_vec: &[f32], top_k: usize) -> anyhow::Result<Vec<Row>>`
  - Types
    - `pub struct Row { pub id: i64, pub ts: i64, pub title: String, pub text: String, pub repo_key: String }`

Sqlite Schema

- Single DB at `~/.codex/memory.sqlite`.
- Tables:
  - `mem(id INTEGER PRIMARY KEY, ts INTEGER NOT NULL, repo_key TEXT NOT NULL, session_id TEXT, kind TEXT NOT NULL, title TEXT NOT NULL, text TEXT NOT NULL, vec BLOB NULL)`
  - Indexes: `CREATE INDEX mem_repo_ts ON mem(repo_key, ts DESC)`; `CREATE INDEX mem_repo_kind ON mem(repo_key, kind)`
- Vectors stored as raw little‑endian `f32` slices in BLOB.

Cosine Similarity

- Compute cosine in process: `cos(q, d) = dot(q, d) / (||q|| * ||d||)`.
- Sqlite query returns candidate rows by repo_key (optionally most recent N); compute cosine in Rust and pick top_k.
- Optionally pre‑normalize and store L2‑norm to speed up scoring.

Integration Steps

1) Write Path (PR-6)

- When Phase 1 writes a summary, if `memory.embedding.enabled`, embed `summary.text` (or `title + text`) and store vector.
- Provider selection: if `provider=openai`, use `OpenAiEmbeddingProvider` configured from the same `Config`/auth used for model calls.

2) Read Path (PR-7)

- At `run_turn()` prompt build time, if enabled:
  - Build a query string from current user input (+ last assistant reply if present).
  - Embed the query via provider.
  - Call `MemoryStore::similar_text(repo_key, query_vec, top_k)`.
  - Merge with `recent_summaries()` (recency bias) and dedupe by id, then trim to prompt budget.

3) Prompt Injection Format

- Keep the same `[memory:summary v1 ...]` header and bullet format to maintain continuity with Phase 1 and simplify the TUI/UX.

4) Acceptance Criteria

- Off by default.
- When on, retrieving top‑K does not exceed `memory.inject.max_chars` and `memory.inject.max_items`.
- Works with OpenAI embeddings when the user is logged in via Codex; degrades gracefully if the provider is unavailable (skip embeddings, fallback to recency only).
- Unit tests for vector packing/unpacking, cosine correctness, and retrieval ordering.

5) Error Handling & Privacy

- Soft‑fail embeddings (log + continue) rather than aborting a turn.
- Reuse sensitive‑pattern filtering approach from `message_history` (see 70-security-privacy.md) before persisting.

