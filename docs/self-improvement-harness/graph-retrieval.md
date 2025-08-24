# Graph Retrieval — Queries & Summarization

Query patterns
- By context: filter Episodes and Runs with matching `context`; gather neighbors.
- By topic: nearest Episodes by embedding of topic + shared citations.
- Neighborhood: k‑hop from a node (Episode/Run/PlanItem) constrained by recency.
- Regression hunt: last N Runs where error increased; backtrack to PlanItem/changes.

Summarization pipeline
- Gather small neighborhoods (≤ 20 nodes, 40 edges).
- Convert to plain‑text bullets with provenance.
- Optional hierarchical subagents to compress larger neighborhoods.

Output shape (plain text)
- “Prior Episodes: <ts> — <summary> (sources: ...)”
- “Recent Runs (context: <name>): <ts> ok X err Y; Δ ok +a err -b”
- “Plan candidates targeting <stage>: …”

Performance & limits
- Cap hops and nodes; use recency decay.
- Cache frequent query results for this session in memory.
