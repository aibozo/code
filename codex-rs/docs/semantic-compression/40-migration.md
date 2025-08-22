# Migration & Rollout

Defaults

- All memory features disabled by default. Enabling any feature must be explicit.

Compatibility

- Phase 1 JSONL can coexist with Phase 2 sqlite. When sqlite is enabled, writes should go to sqlite; reads may fall back to JSONL once for bootstrap if desired (optional).
- No changes to protocol types or TUI event shapes required.

Rollout Steps

1) Land Phase 1 (disabled by default). Validate no prompt regressions.
2) Enable Phase 1 in a sample config to validate JSONL writes and injections in dogfood environments.
3) Land Phase 2 behind config, off by default; validate sqlite migrations are robust (idempotent) and roll forward/backward cleanly.
4) Land Phase 3 with feature flags; do not index by default. Provide a command or flag to trigger indexing on demand.

Backout Plan

- Disable via config. Delete `~/.codex/memory.jsonl` and/or `~/.codex/memory.sqlite` to wipe stored data.

Failure Modes & Handling

- Summarizer failure: swallow; proceed to prune.
- JSONL lock contention: retry briefly (same pattern as `message_history.rs`).
- Sqlite unavailable: log + skip embeddings.
- Embedding provider failure: soft fail; skip semantic search for that turn.

