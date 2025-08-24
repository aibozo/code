# Graph Ingestion — Sources & Transforms

Phase artifacts → graph
- Research (research.md):
  - Episode node
  - ResearchSource nodes; edges Episode—(cites)→ResearchSource
- Plan (plan.md):
  - PlanItem nodes (ordered); Episode—(contains)→PlanItem
  - PlanItem—(targets)→Metric or Stage name(s)
- Code (changes.md optional):
  - File/Function/Test nodes inferred from diffs (best‑effort)
- Test (harness JSON):
  - Run node; edges Run—(includes)→Stage; Episode—(validated_by)→Run
  - Derive improves|regresses edges vs previous run/context
- Summarize (summary.md):
  - Update Episode.summary; optionally add derived tags

Signals → graph (non‑phase)
- Git: File nodes, Function definitions (if index available), Commit nodes later
- Issues/PRs (optional): Issue nodes + cross‑refs

Processing model
- Append‑only writes to `graph/*.jsonl`, with `fs2` locks (like embeddings store)
- Compaction task rebuilds canonical files periodically
