# Graph Implementation Plan — With Existing Memory Crate

Current stack
- `codex-rs/memory`: embeddings (provider trait), KNN, JSONL vector store with fs2 locking.

Additions
- `memory::graph` module with `GraphStore` trait:
  - `put_node(Node)`, `put_edge(Edge)`, `get_node(id)`, `neighbors(id, rel, dir, limit)`
  - `iter_nodes(kind)`, `iter_edges(rel)` (bounded)
- `FileGraph` backend (default):
  - `graph/nodes.jsonl`, `graph/edges.jsonl`, compaction to rebuild canonical order
  - small in‑memory indices (LRU) for hot IDs
- `Neo4jGraph` (feature‑flagged): minimal adapter; fail gracefully if unreachable

Locking & safety
- Reuse fs2 locks (exclusive on writes, shared on reads) like the vector store.
- Shard by prefix if needed for performance (`nodes-00.jsonl`, etc.).

Ingestion adapters
- `episodes.rs`: parse `research.md`, `plan.md`, `summary.md` → nodes/edges
- `harness.rs`: parse run JSON → Run/Stage nodes and edges
- `git.rs` (later): map files/functions/tests (if symbol index present)

Testing
- Unit tests for FileGraph; property tests for compaction (lossless round‑trip).
- Ingestion golden tests with sample episodes and runs.

Integration
- Provide retrieval helpers returning plain‑text packs for prompts.
- TUI hooks later to peek basic graph stats for a topic/context.
