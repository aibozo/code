# 81 – Volley Segmentation and Detection

## Scope
- Define volley boundaries in conversation history.
- Detect and skip summary items (`[memory:*]`) from compaction candidates.
- Provide helpers used by pruner and preflight compactor.

## Volley Definition
- Start: a `Message { role = "user" }`.
- Include: all subsequent assistant messages and tool calls/outputs until the next user message.
- Exclude: ephemeral status/screenshot items (`[EPHEMERAL:...]`).
- Exclude from candidates: any message starting with `[memory:` (treated as a summary or injected memory).

## API
- `segment_into_volleys(items: &[ResponseItem]) -> Vec<Range<usize>>`
- `is_summary_item(item: &ResponseItem) -> bool` (true when first text block starts with `[memory:`)
- `volley_estimated_tokens(items: &[ResponseItem], range) -> usize` (chars/4 heuristic)

## Acceptance Criteria
- Given mixed user/assistant/tool items, the helper returns contiguous user‑anchored ranges.
- Summary items and ephemeral blocks never appear in a volley’s candidate set.
- Unit tests cover edge cases (no user messages, leading assistant, interleaved tools).

## Notes
- Keep this module independent so both post‑turn pruner and preflight compactor can use it.

