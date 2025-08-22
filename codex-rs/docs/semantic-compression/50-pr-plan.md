# PR/Sprint Plan

Overview

- Keep PRs small, buildable, and testable on their own.
- Phase 1 spans 5 PRs; Phase 2 spans 2–3 PRs; Phase 3 spans 2–3 PRs.

Sprint 1 — Phase 1 Foundations

PR-1: Config plumbing
- Files: `core/src/config_types.rs`, `core/src/config.rs`, `common/src/config_summary.rs`, docs update (20-config.md)
- Adds `MemoryConfig` with defaults; no behavior changes when disabled.

PR-2: Summarizer interface
- Files: `core/src/memory/summarizer.rs` (new)
- Adds `Summarizer` trait, `Summary` type, and `NoopSummarizer`. Wires nothing yet.

PR-3: JSONL store
- Files: `core/src/memory/store_jsonl.rs` (new), unit tests
- Implements append/read with locking; repo key helper in `core/src/util.rs` or `git_info.rs`.

PR-4: Summarize-then-prune
- Files: `core/src/conversation_history.rs` (+ `prune.rs`), `core/src/codex.rs` (call site)
- Adds `ConversationHistoryPruner` and integrates summarize‑then‑prune when `memory.enabled`.

PR-5: Prompt injection
- Files: `core/src/codex.rs` (in `run_turn()` build path), `client_common.rs` (optional minor extension)
- Fetches recent summaries and injects them with strict budgets.

Sprint 2 — Phase 2 Embeddings

PR-6: codex-memory crate
- New crate `codex-memory`: EmbeddingProvider trait, OpenAiEmbeddingProvider (if allowed), MemoryStore (sqlite + migrations), vector helpers + tests.

PR-7: Integration
- Wire Phase 1 write path to also compute embeddings when enabled; add retrieval in prompt build path (top‑K + budget).

Sprint 3 — Phase 3 Code Index (Optional)

PR-8: CodeIndexer & chunking
- In `codex-memory`: add `CodeIndexer` and chunking helpers; basic indexer API and tests.

PR-9: Hybrid retrieval
- Combine lexical candidates (`file-search` or ripgrep) with semantic retrieval; add injection format for code hints.

PR-10: UX polish (optional)
- Add TUI notice cell when memory snippets/code hints are injected; toggle visibility.

Acceptance Gates per PR

- Each PR must compile via `./build-fast.sh` and include narrow tests where applicable (see 30-testing.md).
- Keep exported names exactly as documented to avoid integration mismatches.

