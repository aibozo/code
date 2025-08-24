# Architecture

This document details how profiles, sandboxing, the harness, memory, and the orchestrator compose into a self-improving agent.

Design principle: Perpetual Agent
- The system runs as a long-lived, perpetual agent with dynamically compressed context.
- Improvement work happens continuously in the background and is surfaced in the TUI.
- Short "improvement sprints" are spawned as child instances when isolation or bounded runs are useful, but the core remains a single, persistent agent.

Core loops:
- Interactive loop (current Codex CLI): user-driven commands
- Perpetual improvement loop (new): supervisor-driven cycles gated by harness
  - Phases: Research → Plan → Code → Test → Summarize
  - Phase-specific prompts; exits via phase‑scoped tools (plain‑text inputs only)

Key components:
1) Profiles & Policy
   - Source: `configs/policy.yaml` → loaded into runtime
   - Mapped to sandbox runtimes (M2)
   - Logged to `logs/policy.jsonl`

2) Harness
   - Deterministic stages with versioned JSON schema
   - Read-only by default; writes to `harness/artifacts/` only

3) Memory
   - Vector + graph stores (pluggable)
   - Episode log with plan/diff/result/reflection/decision

4) Orchestrator
   - Providers: planner, actor, reviewer (existing agents)
   - Policy-gated execution and acceptance
   - Runs perpetually in-process; may spawn short-lived sprint instances for focused changes
   - Performs dynamic context compaction to stay within token budgets
   - Subagents: hierarchical summarization with high reasoning effort; OAuth preferred, API key fallback

Context format
- Agent context is always plain text; avoid JSON unless writing to on-disk results.

Data flows:
- Inputs: repo state, configs, optional research cache
- Outputs: diffs, harness results, memory episodes, checkpoints

Security posture:
- Default least privilege (READ_ONLY)
- Escalate only within containers/VMs (M2/M5)
- Full audit trail for sensitive actions
