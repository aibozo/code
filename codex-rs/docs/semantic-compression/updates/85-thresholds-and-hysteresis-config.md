# 85 – Thresholds & Hysteresis Configuration

## Scope
- Introduce config for token thresholds and hysteresis targets used by preflight compaction.

## Keys (proposed)
- `memory.compact_threshold_pct = [75, 85, 95]`
- `memory.compact_target_pct = 70`
- `memory.summary_max_chars_per_volley = 400`
- `memory.max_summaries_per_request = 5`

## Behavior
- When estimated prompt usage ≥ highest threshold, compact until ≤ target.
- Lower thresholds trigger smaller compaction steps (e.g., compact one volley, then recheck).
- Thresholds apply to the final formatted input (instructions + deduped history + injection + status).

## Acceptance Criteria
- Defaults work well for typical repos; documented override path via `config.toml`.
- Safe bounds enforced (e.g., targets < lowest threshold; thresholds < 100).

