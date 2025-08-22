# Phase 1 — Summaries (MVP)

Purpose

- Ship an immediate win using per‑span summaries saved to a simple store and injected conservatively at prompt time.
- Zero new heavy dependencies and minimal code churn.

Feature Flags

- `memory.enabled = false` (default)
- `memory.inject.max_items = 2` (soft default when enabled)
- `memory.inject.max_chars = 500` (soft default when enabled)
- `memory.summarize_on_prune = true` (default when memory.enabled)

Data Model (Phase 1)

- File: `~/.codex/memory.jsonl`
- Record (JSON):
  - `repo_key` (string)
  - `session_id` (uuid string)
  - `ts` (unix ms)
  - `kind` ("summary")
  - `title` (string)
  - `text` (string; <= 400 chars ideal)
  - `msg_ids` (array of string ids where available; optional)

Exact Changes

1) Config Plumbing (PR-1)

- File: `codex-rs/core/src/config_types.rs`
  - Add
    - `pub struct MemoryConfig { pub enabled: bool, pub summarize_on_prune: bool, pub inject_max_items: usize, pub inject_max_chars: usize }`
  - Add default impl with disabled/off values.
- File: `codex-rs/core/src/config.rs`
  - Include `memory: MemoryConfig` in `Config` with load/override rules.
- File: `codex-rs/common/src/config_summary.rs`
  - Add a line summarizing memory status (enabled + budgets).
- Docs: update `codex-rs/config.md` and `codex-cli/README.md` config sections (see 20-config.md for exact text).

2) Summarizer Interface (PR-2)

- File: `codex-rs/core/src/memory/summarizer.rs` (new mod)
  - Trait
    - `pub trait Summarizer { fn summarize(&self, items: &[crate::models::ResponseItem]) -> Option<Summary>; }`
  - Types
    - `pub struct Summary { pub title: String, pub text: String }`
  - Impl A (default): `CompactSummarizer` — uses existing Compact flow under the hood
    - Implementation detail: build a mini prompt from `items` and call `run_compact_agent` with `prompt_for_compact_command.md`. Respect timeouts and swallow failures.
  - Impl B (mock): `NoopSummarizer` — returns None; used in tests and when memory disabled.

3) Memory Store (Phase 1 JSONL) (PR-3)

- File: `codex-rs/core/src/memory/store_jsonl.rs` (new)
  - `pub struct JsonlMemoryStore { path: PathBuf }`
  - `impl JsonlMemoryStore { pub fn new(home: &Path) -> Self; pub fn append(&self, repo_key: &str, session: &Uuid, summary: &Summary, msg_ids: &[String]) -> std::io::Result<()>; pub fn recent(&self, repo_key: &str, limit: usize) -> Vec<StoredSummary>; }`
  - Locking: replicate `fs2` advisory locking pattern used by `message_history.rs` (exclusive for append, shared for read).
  - `StoredSummary` contains fields in Data Model above.
- Repo key helper
  - File: `codex-rs/core/src/util.rs` (or `git_info.rs`)
  - `pub fn repo_key(cwd: &Path) -> String` — prefer Git root if available; fallback to canonicalized `cwd`.

4) Summarize-on-Prune Hook (PR-4)

- File: `codex-rs/core/src/conversation_history.rs`
  - Add submodule `prune.rs` with:
    - `pub struct ConversationHistoryPruner { pub keep_last: usize }`
    - `impl ConversationHistoryPruner { pub fn summarize_then_prune(&self, history: &mut ConversationHistory, summarizer: &dyn Summarizer, store: &JsonlMemoryStore, repo_key: &str, session_id: &Uuid) { /* see Rules below */ } }`
  - Rules:
    - If `history.items.len() > keep_last`, take the prefix slice being pruned; pass to `Summarizer::summarize`; if Some, append to store (with best-effort empty msg_ids).
    - Then apply the existing truncation logic to keep last N items (reuse `keep_last_messages(N)` to avoid duplication).

5) Prompt Injection Hook (PR-5)

- File: `codex-rs/core/src/codex.rs` in `run_turn()` before building `Prompt`
  - If `config.memory.enabled`, fetch up to `inject_max_items` of recent summaries from store for `repo_key`; trim to `inject_max_chars` total.
  - Form a single user message containing 1–3 bullet summaries or one message per summary. Suggested format:
    - Header: `[memory:summary v1 | repo=<repo_key> | ts=<iso>]`
    - Bullets: “- <title>: <text>” (wrapped to short lines).
  - Insert these messages into the local `turn_input` vector AFTER environment context additions but BEFORE existing conversation history (the simplest way is to add them to `Prompt.status_items` and rely on ordering in `Prompt::get_formatted_input()`; or extend `Prompt` with a `memory_items: Vec<ResponseItem>` field — choose one pattern and keep consistent).

6) Acceptance Criteria

- Disabled by default: no impact on prompts or behavior unless enabled.
- When enabled:
  - When a pruning event occurs, a new entry appears in `~/.codex/memory.jsonl`.
  - At prompt build, at most `inject_max_items` summaries are injected, capped by `inject_max_chars`.
  - Summaries do not include code blocks longer than a few lines (the summarizer must be instructed accordingly).
  - No changes to ephemeral screenshot filtering.
- Tests present (see 30-testing.md):
  - Unit tests for `repo_key`, jsonl append/lock, trimming to char budget, and injection order.
  - Integration test toggling `memory.enabled` to verify injection shows in `Prompt::get_formatted_input()`.

7) PR Notes

- Keep PRs small and scoped exactly as above to reduce conflicts with other feature work. Avoid renaming existing types.

