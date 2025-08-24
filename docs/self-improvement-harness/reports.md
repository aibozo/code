# Reports & Results

Directory layout
- `harness/results/YYYYMMDD/` — one directory per day
  - `<timestamp>.json` — run result (schema v1)
  - `_index.json` — index of runs for the day
  - `_daily_summary.json` — aggregated ok/error/cached across the day
- `harness/artifacts/<timestamp>.log` — harness log
- `harness/cache/<stage>.json` — per‑stage cache entries

Schema v1 (run file)
- `version`: `"v1"`
- `timestamp`: `YYYYMMDD-HHMMSS`
- `seed`: number
- `context`: optional string label (e.g., branch/track)
- `stages`: array of `{ name, status, metrics, cached }`

TUI integration
- `/reports` — date‐picker → run picker → detailed run
- `/reports <date>` — run picker for the given day
- `/reports <date> summary` — daily summary view
- `/reports <date> <timestamp>` — open a specific run
- `/reports trends [7|30]` — daily ok/error bars for recent days
- Footer hints: `↑/↓/PgUp/PgDn` scroll • `←/→` switch run • `Esc` close

Caching
- Stages declare `cached: true|false` in run JSON.
- Daily summary includes `total_cached` and per‑run `cached` counts.

Context labels
- `--context NAME` adds a `context` field in the run JSON and TUI headers for disambiguation.
- Planned: filter runs by `context` and show side‑by‑side deltas.

Planned enhancements
- By‑context reports: filter and compare across contexts/branches.
- Trend deltas vs moving average.
- Per‑stage sparkline of recent statuses.

Acceptance notes
- Reports remain human‑readable; keep summary text concise but clear.
- JSON is only for storage; prompts remain plain text.
