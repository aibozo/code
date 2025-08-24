# Phase Tools (Design)

Constraint: inputs are plain text; tools are only available in their respective phase. The TUI and CLI do not force phase transitions; the agent decides when to call a finish tool.

Proposed tools (names are conceptual; actual wiring lives in core):
- `research.finish(text)` — exit Research phase with a concise plain‑text summary; must confirm sufficient sources and thinning search relevance.
- `plan.finish(text)` — exit Plan phase with a testable plan summary; include acceptance gates.
- `code.finish(text)` — exit Code phase; describe what changed and what to test.
- `test.finish(text)` — exit Test phase; summarize harness outcomes and any new coverage added.
- `summary.finish(text)` — exit Summarize phase; produce an executive summary and “what’s next”.

Availability
- Only one finish tool is included in the tool list at a time, based on the current phase.
- Tools must be idempotent; repeated calls overwrite the current phase artifact.

Artifacts
- Each finish tool writes to `orchestrator/episodes/<ts>/<phase>.md`.
- Tools can add light tags (e.g., `Context: <label>`) but content must remain human‑readable prose.

Validation & retries
- Minimal schema validation: non‑empty text, 8+ words, no JSON blocks.
- On failure: return a short corrective hint; agent retries up to K times.

Transition policy
- After a successful finish call, the next phase is unlocked and its tool becomes available.
- The loop can be paused/resumed; current phase persists.
