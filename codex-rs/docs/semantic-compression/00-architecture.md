# Architecture & Insertion Points

This document identifies exactly where semantic compression integrates into Codex, with file paths and function boundaries to avoid ambiguity.

Key Components (existing)

- In‑session transcript: `codex-rs/core/src/conversation_history.rs` (struct `ConversationHistory`).
- Cross‑session history: `codex-rs/core/src/message_history.rs` (`append_entry`, `history_metadata`, `lookup`).
- Turn driver: `codex-rs/core/src/codex.rs`
  - `run_agent()` and `run_turn()` build prompts and stream results.
  - `Prompt` assembly happens in `run_turn()` via `Prompt { ... status_items }`.
  - Ephemeral context added per turn via `build_turn_status_items()`.
- Prompt formatter: `codex-rs/core/src/client_common.rs` (struct `Prompt`)
  - `get_formatted_input()` prepends developer instructions, environment context, and user instructions; then appends history and per‑turn status.

New Concepts (to add)

- Summarizer (Phase 1): a small interface and implementation that produces short summaries from a span of `ResponseItem`s, reusing the existing “Compact” prompt path when possible.
- Memory Store (Phase 1→2): a persistence layer for short summaries (and later vectors). JSONL is sufficient in Phase 1; upgrade to sqlite in Phase 2.
- Embedding Provider (Phase 2): trait that encapsulates embedding calls; supports OpenAI embeddings and (optionally) local embeddings via a feature flag.
- Codebase Index (Phase 3): background job that produces chunked file‑level embeddings; stored alongside memory in sqlite.

Exact Hook Points

1) Before pruning conversation history
   - Add a summarization+persist step immediately before any pruning truncates older messages.
   - If the codebase currently does not enforce a size limit via `ConversationHistory::keep_last_messages()` calls, create a new pruning function that callers can opt into. Suggested surface:
     - `ConversationHistoryPruner::summarize_then_prune(history: &mut ConversationHistory, summarizer: &dyn Summarizer, store: &dyn MemoryWriter, keep_last: usize)`
     - Placement: `codex-rs/core/src/conversation_history.rs` (new submodule `prune.rs`).

2) At prompt construction (per turn)
   - Extend `run_turn()` (in `codex-rs/core/src/codex.rs`) to fetch a tiny set of relevant memory snippets and inject them into the `Prompt` as an additional slice, placed after environment context and before the in‑session history.
   - Keep a very strict budget: max 3 snippets, max ~500 chars total by default (configurable). This minimizes interference with the model’s short‑term reasoning.

3) Storage
   - Phase 1: JSONL file under `~/.codex/memory.jsonl` (simple append‑only; reuse `fs2` locking approach from `message_history.rs`).
   - Phase 2+: sqlite DB under `~/.codex/memory.sqlite` with one file for all projects, namespaced by repo root.
   - Repo namespace key: absolute Git root path (resolved once per session). If not a Git repo, use canonicalized `cwd`.

Message Placement in Prompt

- Inject as user messages, using a distinct tag header to make content easy to spot and audit:
  - Example single message content:
    - Line 1: `[memory:summary v1 | repo=<repo_key> | ts=<iso>]`
    - Lines 2+: concise bullet points.
- Rationale: fits with current `Prompt::get_formatted_input()` structure that already accepts user messages for environment context and instructions.

Compatibility Guardrails

- No changes to ephemeral screenshot rules.
- No changes to approval/sandbox policy logic.
- Summarization calls must be off by default; memory injection must be off or strictly limited by default (see 20-config.md).

