# Phase 3 — Codebase Index (Optional)

Purpose

- Build a lightweight semantic index of the codebase to improve recall for repo‑wide questions.
- Keep it optional and incremental; reuse `file-search` and avoid blocking the UI.

Feature Flags

- `memory.code_index.enabled = false` (default)
- `memory.code_index.chunk_bytes = 1500` (default window size)
- `memory.code_index.top_k = 5`

Data Model (sqlite extension)

- Table `code_mem(id INTEGER PRIMARY KEY, ts INTEGER NOT NULL, repo_key TEXT NOT NULL, path TEXT NOT NULL, offset INTEGER NOT NULL, len INTEGER NOT NULL, text TEXT NOT NULL, vec BLOB NULL)`
- Indexes: `code_repo_path` (repo_key, path), `code_repo_ts` (repo_key, ts DESC)

Chunking Strategy

- Read files surfaced by `file-search` (or walk repo with ignore rules). Chunk into ~1.5KB windows with 200–300 byte overlaps for robustness.
- Skip binary/large files, `target/`, `.git/`, and existing ignored paths.

Indexer (background task)

- Component: `CodeIndexer`
  - Placement: `codex-memory` crate to keep DB/API in one place.
  - API:
    - `pub struct CodeIndexer { store: MemoryStore, provider: Box<dyn EmbeddingProvider> }`
    - `pub async fn index_repo(&self, repo_root: &Path, repo_key: &str, paths: impl Iterator<Item=PathBuf>) -> anyhow::Result<()>`
    - `pub async fn index_changed(&self, repo_root: &Path, repo_key: &str) -> anyhow::Result<()>` (later; read `git diff` to collect changed files)
- Schedule: only on demand (e.g., user toggles, or when large context queries start). Do not run unprompted by default.

Retrieval (hybrid)

- For a given turn, optionally run lexical search (via ripgrep) to seed candidate paths; then run semantic search over `code_mem` for the top paths.
- Rank by combined score: `score = 0.6 * semantic + 0.4 * lexical` (tunable; kept simple).
- Inject only short excerpts (first few lines around the best match) into the prompt as hints, not full files. Keep the same strict budget.

Prompt Format (code hints)

- A separate header for code hints so the model distinguishes them from dialogue summaries:
  - `[memory:code v1 | repo=<repo_key> | path=<path> | off=<offset>]`
  - Then 3–8 short lines of code/context, trimmed and annotated.

Acceptance Criteria

- Off by default.
- When on, index creation is incremental and cancellable.
- Retrieval is fast (<100ms for moderate repos) and degrades gracefully when embeddings are unavailable.
- Unit and integration tests cover chunking boundaries and retrieval sanity.

