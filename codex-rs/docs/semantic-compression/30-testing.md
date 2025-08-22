# Testing Strategy

Principles

- Start with unit tests for pure logic (locking, budgeting, formatting).
- Add narrow integration tests at the crate seams (prompt assembly, config flags).
- Avoid network in CI unless explicitly enabled; mock embedding/summarizer providers.

Unit Tests

1) Jsonl store (`core/src/memory/store_jsonl.rs`)
   - Append with fs2 lock; read under shared lock; concurrent append simulation.
   - Truncation/rotation not required in Phase 1.

2) Prompt budgeting (`Prompt` injection helper)
   - Given N snippets and budgets, verify truncation preserves whole bullets and headers; total chars ≤ budget.

3) Repo key helper
   - Git root detection; fallback to canonicalized cwd.

4) Sqlite (Phase 2)
   - Vector (f32) pack/unpack round‑trip.
   - Cosine correctness for simple known inputs.
   - top_k retrieval ordering and dedupe.

Integration Tests

1) Phase 1 enabled path
   - Configure `memory.enabled=true` and small budgets; simulate a pruning event and assert a JSONL record is written; assert injection occurs in `Prompt::get_formatted_input()`.

2) Summarizer fallback
   - Use `NoopSummarizer` and confirm system still prunes; no injection when store empty.

3) Phase 2 retrieval
   - With `embedding.enabled=true`, use a mock `EmbeddingProvider` returning deterministic vectors; assert retrieval order and injection format.

4) Phase 3 code index
   - Chunk a small fixture file and ensure the indexer inserts rows; verify retrieval returns the right path/offset.

TUI/UX Verification (manual)

- Ensure injected memory messages render as normal user messages and are visually distinct via header line. No new widget types required for Phase 1.

Performance Budgets

- Injection adds ≤ 2ms overhead at prompt build for Phase 1; ≤ 10ms for Phase 2 retrieval on medium repos.

