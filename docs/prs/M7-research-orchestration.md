# PR: M7 — Research Orchestration & Hierarchical Agents

Summary
This PR proposes a robust research pipeline built on our existing subagent infrastructure. Lightweight `gpt‑5‑mini` agents fan out to gather evidence in parallel, and a single `gpt‑5` agent synthesizes the results with high reasoning effort. We enforce hard concurrency caps (1× gpt‑5, 5× minis), schedule queued agents automatically, and surface live status in the main TUI without blocking user workflows. Outputs become durable episode artifacts and citations in the memory graph.

Motivation
- Keep the main agent responsive while background research proceeds.
- Bound spend via hard caps and budgets; leverage minis for breadth and a single heavy model for depth.
- Produce consistent, auditable artifacts and citations, with stable reruns (caching and deterministic sharding).

Architecture
1) Subagents & caps
   - Extend `AgentManager` with per‑model caps and a simple queue scheduler.
   - Models: minis = `gpt‑5‑mini` (read‑only), synthesizer = `gpt‑5` (high reasoning).
   - Caps (hard‑coded): `gpt‑5` ≤ 1, `gpt‑5‑mini` ≤ 5. Extra requests queue pending.
   - Provider pacing: OAuth `gpt‑5` calls are linearized (single in‑flight) with a minimum 2s delay between successive requests; mini agents are paced by the queue with a minimum 0.5s delay between requests.

2) Orchestration flow
   - Plan: derive research targets (queries or subtopics) from the main goal.
   - Retrieve: ArXiv MVP connector fetches normalized sources; cache by query+filters.
   - Shard: divide top‑N sources/clusters across ≤ K minis; each mini produces `notes-mini-*.md` with citations.
   - Synthesize: once minis complete (or timeout), pass curated notes to a single `gpt‑5` agent to write `research.md` with prioritized citations and “what’s next”.
     - Curation schema (per source):
       - `abstract` (trimmed to salient opening + high‑density sentences)
       - `contribution_one_liner` (exact or near‑verbatim when crisp)
       - `methodology_bullets` (3–5 bullets with key techniques, data, and constraints)
     - Fan‑in budgeting: cap per‑source note at B≈250–300 tokens, select up to M≈10 sources. Prefer top‑ranked; elide long tail with an “omitted” list to retain provenance signal without overwhelming context.
   - Ingest: write `sources.json`, `research.md`, and mini notes, then add graph edges Episode—(cites)→ResearchSource.

3) Determinism & budgets
   - Deterministic caching for ArXiv JSON responses; replay when offline.
   - Budget config (phase/local): token ceiling per mini, per synthesizer; watchdog for total wall‑clock; rate‑limit intervals (2s synth; 0.5s minis) are part of the deterministic schedule.
   - Fail‑closed: downrank or skip over budget; synthesize partial evidence with disclaimers.

4) Observability & UX
   - Footer: “agents X running” next to token usage.
   - Ctrl+A: agent status window with per‑agent model, elapsed, errors, worktree/branch, progress tail; quick keys to Reports/Trends/Episodes.
   - Show pacing in status: “synth in 2s” / “mini queue: next in 0.5s”; keep counts small and legible; do not emit rapid status churn.
   - Reports: day summaries show deltas; episode summary includes research synthesis and K/total sources observed.

5) Policy & provenance
   - Respect sandbox and approvals; redact PII in saved artifacts; log connectors and request counts.
   - Provenance metadata attached to `ResearchSource` nodes (URL, title, year, authors, normalizer version, cache key).

Implementation details
- Agent caps & queue
  - `AgentManager` keeps `caps_by_model` and a `queued` list.
  - `try_start_agent_now()` starts immediately if capacity allows; otherwise queues.
  - On completion/cancel/fail: `maybe_start_next_for_model(model)` starts next pending.
  - Status updates sent on every transition via `AgentStatusUpdateEvent` (now includes timestamps, errors, progress tails).

- Orchestrator API
  - `spawn_research_minis(cfg, topic, count)` — spawns ≤ 5 minis in read‑only.
  - `spawn_synthesizer(cfg, context, notes)` — spawns one `gpt‑5` with high reasoning.
  - Both non‑blocking; observed via the TUI Agent Status view.

- Connectors (ArXiv MVP)
  - Query builder supports keyword + optional year range; per‑query rate cap.
  - Cache to `harness/cache/research/<sha>.json` keyed by normalized query.
  - Normalizer outputs `url`, `title`, `year`, `authors`, `summary`, `score`.
  - Rank by score/recency; dedup by URL hash; cluster by title embeddings (optional) for sharding.

Testing & validation
- Unit tests: caps, queue scheduling, status payloads, dedup/ranking stability.
- Golden: connector normalizer; episodic graph ingestion of citations.
- E2E: topic → cached ArXiv → K minis with fake outputs → synthesizer → artifacts + memory edges; verify reports linkbacks.

Risks & mitigations
- Token blowups: hard caps + per‑stage ceilings + watchdog; minis use small models.
- Connector flakiness: backoff + caching; partial synthesis allowed.
- Nondeterminism: deterministic cache + stable sorting; doc includes reproducibility tips.

Rollout plan
- M7.0: caps + scheduler + ArXiv normalizer + end‑to‑end artifacts.
- M7.1: cluster sharding + per‑topic embeddings; improved reranking.
- M7.2: additional connectors; UI drill‑downs for citations.

Developer notes
- Keep caps enforced in code (not user‑editable); expose redacted status only.
- Use existing TUI (no new windows); rely on Ctrl+A for deep status.
- Prefer OAuth when available; fallback to OpenAI API key; record chosen path.
