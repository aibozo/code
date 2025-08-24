# Graph Risk Matrix — Risks & Mitigations

Complexity
- Risk: Over‑engineering a graph system.
- Mitigation: start with FileGraph (JSONL); keep schema minimal; feature‑flag Neo4j.

Performance
- Risk: Large nodes/edges causing slow reads.
- Mitigation: shard files; LRU indices; cap retrieval neighborhoods; compaction tasks.

Correctness
- Risk: Duplicate IDs; inconsistent edges.
- Mitigation: stable ID strategy; ingestion validators; compaction dedupe.

Concurrency
- Risk: Race conditions on writes.
- Mitigation: fs2 locks + atomic renames (like vector store); test under contention.

UX
- Risk: Context bloat when injecting graph content.
- Mitigation: strict plain‑text packs with hard caps; hierarchical subagent compression.
