# Self-Improvement Harness

This directory contains planning docs for the `self-improvement-harness` branch.

Scope:
- Capability profiles and sandbox wiring
- Evaluation harness (golden tests + gates)
- Perpetual agent loop and TUI commands
- Subagents (hierarchical summarization) – OAuth preferred, API key fallback
- Memory integration (vector + graph) roadmap

Primary docs:
- architecture.md — system overview and perpetual‑agent design
- cli-tui-integration.md — commands, headers, and wiring
- phases.md — Research → Plan → Code → Test → Summarize
- subagents.md — hierarchical summarization strategy
- reports.md — results, trends, and planned by‑context browsing
- phase-prompts.md — phase‑specific system prompt templates (plain text)
- phase-tools.md — phase completion tools and constraints
- milestones/ — milestone specs (M0–M5) with acceptance criteria
- roadmap.md — sequencing, dependencies, and risks

Graph memory (design deep‑dive):
- graph-memory.md — goals, constraints, phases of adoption
- graph-schema.md — entity/relation taxonomy and identity strategy
- graph-ingestion.md — ingestion from phases, code signals, and harness results
- graph-retrieval.md — query patterns and summarization pipelines
- graph-impl-plan.md — incremental implementation plan with current memory crate
- graph-risk-matrix.md — risks and mitigations

Guiding constraints:
- Agent context and prompts are plain text. Avoid JSON except for stored results.
- Phase exits are explicit tool calls available only in that phase.
- Subagents should run at high reasoning effort and be wrapped with validation.
