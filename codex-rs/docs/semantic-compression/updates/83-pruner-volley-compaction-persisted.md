# 83 – Post‑Turn Pruner: Volley‑Aware Persisted Compaction

## Scope
- Migrate pruner from fixed message count to volley‑aware pruning.
- Summarize pruned prefix into a single `[memory:context]` message and persist to the JSONL store.
- Keep K recent messages but select the tail on volley boundaries.

## Behavior
- After each completed turn (when memory is enabled):
  - Segment history into volleys.
  - Identify prunable prefix (older volleys excluding any `[memory:*]` items).
  - Summarize the pruned span (title + bullets: user intent, actions, outcomes, file anchors).
  - Persist the summary to the memory store (existing mechanism), then prune.

## Acceptance Criteria
- No summaries of summaries.
- Keeps tail on volley boundaries; no dangling assistant/tool without the user root.
- Persisted summaries retrievable by existing memory retrieval.

