# PR: M3 — Long-horizon memory (vector + graph)

Summary:
Design and stub the tiered memory system that supports retrieval and reflection across iterations. This PR provides concrete schemas, APIs, and ETL plans, with local adapters to be added in M3.1+.

Components:
1) Vector store (pgvector)
   - Collections: `files`, `diffs`, `plans`, `reflections`, `tests`
   - Schema (SQL):
     - `id UUID PK`, `kind TEXT`, `repo TEXT`, `path TEXT`, `content TEXT`, `embedding VECTOR(1536)`, `meta JSONB`, `ts TIMESTAMPTZ`
   - Index: HNSW on `embedding` with `M=16, ef_search=128`
   - Query: `search(text|code, k)` returns ranked items with cosine similarity

2) Graph store (Neo4j)
   - Node labels: `Function`, `File`, `Test`, `Issue`, `Module`
   - Relations: `CALLS`, `IMPORTS`, `CAUSES`, `VALIDATED_BY`
   - ETL: extract static call graph and issue→test→file links; store as relationships
   - Query API: `graph_query(cypher|template)` returns nodes/edges

3) ETL pipelines
   - `memory/etl/index_repo.py`: walks repo, extracts AST graphs, computes embeddings (OpenAI embeddings path), writes to pgvector + Neo4j
   - `memory/etl/ingest_episode.py`: ingests harness outputs (plans, diffs, results) into both stores
   - Batching: 500 items per txn; exponential backoff on failures

4) Service facade
   - File: `memory/service.ts|rs`
   - Methods:
     - `search(query, k, filters?) -> Vec<MemoryItem>`
     - `graph_query(cypher) -> GraphResult`
     - `append_episode(outcome, artifacts[]) -> EpisodeId`
   - Caches: LRU for hot embeddings; TTL 10m

5) Retention policy (ties to `configs/memory.yaml`)
   - Compaction triggers at 90% context utilization
   - Strategies: summarize, deduplicate, pin recent successes/failures

Acceptance criteria:
- SQL and Cypher schemas documented in this PR.
- ETL plans documented with exact CLI usage (no network calls required yet).
- Service facade API is specified with method signatures.

Follow-ups:
- M3.1: Implement local postgres + pgvector adapter (opt-in via env).
- M3.2: Implement local Neo4j adapter and GraphRAG pipeline.

