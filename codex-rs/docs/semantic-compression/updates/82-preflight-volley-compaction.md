# 82 – Preflight Volley‑Aware Compaction

## Scope
- Replace ad‑hoc older slice logic with volley‑aware compaction in preflight.
- Respect token thresholds with hysteresis to avoid thrash.
- Keep the latest N volleys intact; summarize older volleys into `[memory:context]` items.

## Behavior
- Estimate tokens for the fully formatted prompt.
- If above budget:
  - Replace oldest unsummarized volley(s) with compact summaries until ≤ target.
  - Reduce kept tail count (volleys) if needed.
  - Reduce per‑summary char budget if needed.
  - Last resort: drop non‑essential injection block.
- Never summarize any `[memory:*]` item; never summarize already summarized content.

## Config
- `memory.compact_threshold_pct` (default: [75, 85, 95])
- `memory.compact_target_pct` (default: 70)
- `memory.summary_max_chars_per_volley` (default: 400)
- `memory.max_summaries_per_request` (default: 5)

## Acceptance Criteria
- Requests remain under the model’s context window.
- Recent volleys remain intact unless budget forces otherwise.
- No duplicated summaries across retries.

