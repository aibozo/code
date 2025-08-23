# Architecture

This document details how profiles, sandboxing, the harness, memory, and the orchestrator compose into a self-improving agent.

Core loops:
- Interactive loop (current Codex CLI): user-driven commands
- Improvement loop (new): supervisor-driven cycles gated by harness

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

Data flows:
- Inputs: repo state, configs, optional research cache
- Outputs: diffs, harness results, memory episodes, checkpoints

Security posture:
- Default least privilege (READ_ONLY)
- Escalate only within containers/VMs (M2/M5)
- Full audit trail for sensitive actions

