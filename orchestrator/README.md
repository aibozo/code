# Orchestrator

Plan → Act → Observe → Reflect → Decide loop design.

Goals:
- Drive improvement cycles gated by the harness.
- Persist reflections and outcomes to memory services.
- Enforce policy profiles and approvals during actions.

Next steps:
- Implement supervisor with hooks for: plan(), act(), harness_run(), reflect(), decide().
- Persist episodes under `memory/episodes/` until external stores are available.

