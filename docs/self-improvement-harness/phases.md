# Phases: Research → Plan → Code → Test → Summarize

Design: The agent runs perpetually with agent‑controlled wall‑time per phase. Each phase has:
- Purpose: why it exists
- Entry: when and how it starts
- Exit: an explicit plain‑text tool call only available within the phase
- Artifacts: human‑readable outputs written under `orchestrator/episodes/<ts>/`
- Prompting: phase‑specific guidelines; context must be plain text

General rules
- Keep all agent context in natural language. Avoid JSON for inputs/outputs unless persisting machine‑readable results.
- Subagents (if any) inherit the same plain‑text constraint and return clean prose.
- The loop can run `/loop start --once --context <label>` to execute one full cycle tagged to a branch/track label.

## Research
- Purpose: discover relevant papers, posts, issues, and prior art for a chosen domain/problem.
- Entry: start of a cycle or after Summarize.
- Exit: `research.finish(<plain‑text summary>)` once:
  - A good set of relevant sources is collected
  - Detailed source notes & summaries are written
  - Search is thinning to less‑relevant results
- Artifacts:
  - `research.md`: overview, key findings, curated sources, inline summaries
  - Optional `sources.txt` (URLs only) to aid future offline runs
- Prompting: bias toward breadth first, then depth on top candidates; keep citations inline in prose.

## Plan
- Purpose: transform research into a concrete, testable branch plan.
- Entry: after `research.finish`.
- Exit: `plan.finish(<plain‑text plan summary>)` when:
  - A detailed plan exists targeting improvements to harness metrics or agent UX
  - Acceptance tests or harness deltas are specified
- Artifacts:
  - `plan.md`: target metrics, sub‑tasks, acceptance criteria, risks, rollbacks
- Prompting: require measurable outcomes; enumerate harness stages to be impacted.

## Code
- Purpose: implement the plan in a dedicated track.
- Entry: after `plan.finish`.
- Exit: `code.finish(<plain‑text status>)` once changes are ready for testing.
- Artifacts:
  - `changes.md`: rationale and overview of edits (optional)
  - Use `--context <label>` to tag harness runs and reports
- Prompting: keep edits minimal and focused; prefer diffs referencing files.

## Test
- Purpose: validate changes in a deterministic harness.
- Entry: after `code.finish`.
- Exit: `test.finish(<plain‑text summary>)` when harness runs complete.
- Artifacts:
  - `harness/results/YYYYMMDD/<ts>.json` (includes `context` if provided)
  - `_daily_summary.json` auto‑aggregates deltas and cached counts
- Prompting: if plan adds new coverage, wire new stage checks first.

## Summarize
- Purpose: generate a concise human report and optional deltas vs. previous runs.
- Entry: after `test.finish`.
- Exit: `summary.finish(<plain‑text executive summary>)`.
- Artifacts:
  - `summary.md`: top‑line outcome, meaningful conclusions, next targets
- Prompting: write approachable summaries; include short “what’s next”.

