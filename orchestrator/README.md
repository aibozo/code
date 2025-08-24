# Orchestrator

Perpetual 5‑phase loop design (agent‑controlled cadence): Research → Plan → Code → Test → Summarize.

Goals:
- Continuous self‑improvement gated by the harness.
- Produce plain‑text artifacts per phase under `orchestrator/episodes/<ts>/`.
- Keep prompts plain‑text; avoid JSON except for stored results.
- Enforce policy profiles and approvals during actions.

Phases and artifacts:
- Research: `research.md` (topic overview, paper notes, source list)
- Plan: `plan.md` (target metric, branch plan, acceptance gates)
- Code: changes associated to `--context` label; optional `changes.md`
- Test: harness run JSON with `context` label
- Summarize: `summary.md` (concise, human‑readable)

Phase completion tools (design):
- `research.finish(text)`, `plan.finish(text)`, `code.finish(text)`, `test.finish(text)`, `summary.finish(text)`
  - Plain‑text inputs only; no JSON payloads.
  - Available only within their phase; transitions controlled by the agent.

Subagents (design):
- Research and summarization can spawn hierarchical high‑effort subagents (gpt‑5‑mini) via OAuth.
- Fallback to API‑key if OAuth subagents unavailable.
- Add function‑call fail‑safes (retry/validate) for small models.
