# Graph Schema — Entities & Relations

Entities (minimal set first)
- Episode(ts, topic, summary): a 5‑phase loop instance.
- ResearchSource(url, title, year, notes): curated source.
- PlanItem(id, text, target_metric): unit of planned work.
- Run(ts, context, ok_count, error_count): harness run.
- Stage(name, status, cached): stage snapshot for a run.
- File(path): code file.
- Function(name, file, sig): code symbol.
- Test(name, file): test symbol.
- Issue(id, title): optional tracker artifact.

Relations
- Episode —(cites)→ ResearchSource
- Episode —(contains)→ PlanItem
- PlanItem —(targets)→ Stage or Metric
- Episode —(validated_by)→ Run
- Run —(includes)→ Stage
- File —(defines)→ Function
- Function —(calls)→ Function
- Test —(tests)→ Function/File
- Run —(improves|regresses)→ Metric (implicit via ok/error deltas)

Identity strategy
- Use stable keys:
  - Episode: timestamp key + topic
  - ResearchSource: URL hash
  - PlanItem: Episode ts + ordinal
  - Run: timestamp key + context
  - File/Function/Test: repo‑relative path + name

Storage format (FileGraph)
- `graph/nodes.jsonl` with `{kind, id, props}`
- `graph/edges.jsonl` with `{src, rel, dst, props}`
- Append‑only; compaction pass can rebuild canonical files.
