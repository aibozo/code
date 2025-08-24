# Phase Prompt Templates (Plain‑Text)

Principles
- Prompts are plain text; no JSON structures.
- Keep the agent’s freedom to generate rich prose; specify constraints inline.
- Phase headers make it explicit which finish tool is available.

Template: Research
```
You are in Research phase.
Goal: discover relevant, high‑quality sources (papers, posts, code) for <topic>.
Constraints:
- Collect breadth first, then deepen top sources.
- Write findings as clear prose; include short inline citations (title • year).
- Stop when additional results are marginally relevant.
Finish tool: research.finish(text).
Deliverables:
- research.md with overview, key findings, curated sources, and summaries.
```

Template: Plan
```
You are in Plan phase.
Goal: transform research into a testable branch plan targeting harness improvements.
Constraints:
- Define measurable acceptance criteria and affected harness stages.
- Include risks and rollbacks.
Finish tool: plan.finish(text).
Deliverables: plan.md.
```

Template: Code
```
You are in Code phase.
Goal: implement the plan minimally and cleanly.
Constraints:
- Keep changes focused; respect existing style.
- Reference files and diffs directly.
Finish tool: code.finish(text).
Deliverables: changes.md (optional), context label for runs.
```

Template: Test
```
You are in Test phase.
Goal: validate via harness (deterministic, cached stages where applicable).
Constraints:
- If new coverage is needed, wire a stage first.
Finish tool: test.finish(text).
Deliverables: harness run JSON.
```

Template: Summarize
```
You are in Summarize phase.
Goal: condense results and decide what’s next.
Constraints:
- Write executive summary in plain English.
- Include short deltas vs prior runs if relevant.
Finish tool: summary.finish(text).
Deliverables: summary.md with conclusions and next targets.
```
