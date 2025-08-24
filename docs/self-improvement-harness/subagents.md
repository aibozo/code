# Subagents & Hierarchical Summarization

Goal: Use small, fast models (e.g., gpt‑5‑mini) as high‑effort summarizers and assistants for context management during heavy phases like Research and Summarize.

Principles
- Plain text all the way: prompts and outputs are natural language; avoid JSON unless persisting data.
- High reasoning effort: subagents run with elevated reasoning to improve reliability.
- Fail‑safes: wrap function/tool calls with retry and validation.

Auth & provisioning
- Preferred: OAuth‑backed subagent creation and invocation.
- Fallback: use the configured API key (also used for dynamic compression embeddings) when OAuth subagent creation is not available.

Patterns
- Hierarchical summarization: fan‑out across N sources → local summaries → 1‑2 aggregators → final synthesis.
- Chunked processing with overlap: keep chunks small enough for strong coherence in small models.
- Validator loop: after a tool call, validate required fields exist in plain text output; retry with short corrective hints.
- Budget control: cap concurrent subagent runs; prefer depth over breadth when nearing token/time limits.
 - Pacing: serialize heavy `gpt‑5` calls (≥2s between) and pace minis via a queue (≥0.5s between) to keep provider rates predictable and costs bounded.

Failure handling
- Detect malformed tool outputs (empty/irrelevant answers); retry up to K times with concise constraints.
- On repeated failure, escalate to parent agent with a short, plain‑text error capsule and the minimal context for recovery.

Planned tools (docs only; to be implemented)
- `subagents.spawn(role: "research-summarizer" | "report-synthesizer", input: <text>) -> <text>`
- `subagents.validate(spec: <text>, output: <text>) -> ok|needs_retry + hint`

Note schema (research mini → aggregator)
- abstract: trimmed core sentences (avoid paraphrase unless needed for clarity)
- contribution_one_liner: crisp claim/contribution, verbatim when informative
- methodology_bullets: 3–5 bullets, include datasets, techniques, key assumptions
- citation: URL/identifier and minimal metadata (title, year, authors)

Fan‑in selection
- Score sources by rank and novelty; cap per‑source tokens (B≈250–300) and total sources (M≈10).
- Preserve specificity: avoid over‑summarization; keep terms of art and method names intact.
- Emit an “omitted N sources” line to retain provenance signal without bloating the synthesizer input.
