# Graph Memory — Goals & Approach

Objectives
- Ground the perpetual loop in durable, queriable knowledge.
- Keep agent context plain‑text; the graph is an internal indexing layer.
- Remain offline‑friendly and deterministic; external backends are optional.

Constraints
- Plain‑text prompts/outputs; JSON only for stored nodes/edges and results.
- Small, incremental steps; do not fork the codebase structure prematurely.
- Reuse existing locking, file layout patterns from the `memory` crate.

Phased adoption
- M3a: FileGraph (local JSONL nodes/edges) + GraphStore trait.
- M3b: Ingest episodes (research, plan, summary) and harness runs.
- M3c: Retrieval API (topic/context) with summarization pipeline.
- M3d: Optional external backend (Neo4j) behind a feature flag.

Layers
- Storage: GraphStore trait; FileGraph and optional Neo4jGraph.
- Ingestion: transforms phase artifacts and signals into nodes/edges.
- Retrieval: query patterns (by context, neighborhood, time) -> plain‑text packs.
- Summarization: hierarchical, high‑effort subagents when needed.
- Observability: small counters, edge/node counts, and ingest timings.

Outputs to the agent
- Short, readable inserts (e.g., “Prior episodes on <topic>: …”) instead of large dumps.
- Always include provenance (Episode ts, Run ts) in prose, not JSON blocks.
