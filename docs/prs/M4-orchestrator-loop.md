# PR: M4 — Orchestrator loop & policy gate

Summary:
Implement the Plan → Act → Observe → Reflect → Decide loop, gated by the harness and policy profiles. The supervisor runs improvement cycles and accepts changes only when golden metrics improve and gates are clean.

Supervisor flow (pseudo-code):
```
loop attempt in 1..N:
  plan = planner.propose_changes(goal, constraints)
  diffs = actor.generate_diffs(plan)
  fs.apply(diffs)  # workspace_write only
  results = harness.run(profile=READ_ONLY)
  reflection = reviewer.reflect(plan, results)
  decision = decider.accept_if(results.improved && !gates_broken)
  memory.append_episode(plan, diffs, results, reflection, decision)
  if decision.accept:
    checkpoint.save()
    break
  else:
    fs.revert(diffs)
```

Implementation details:
1) Entry point and commands
   - CLI: `code /improve <goal> [--max-attempts 3] [--no-approval]`
   - TUI: `/plan`, `/act`, `/harness run`, `/reflect`, `/accept|/revert` executed automatically when allowed by policy.

2) Planner/Actor/Reviewer roles (can reuse providers)
   - Planner drafts a high-level plan and task list; cites sources if research mode active.
   - Actor produces diffs using repo-aware tools (existing apply_patch integration).
   - Reviewer summarizes outcomes and suggests adjustments.

3) Policy gates
   - Respect current profile; escalate to SYSTEM_WRITE or GODMODE only through explicit user confirmation (M5).
   - Enforce approval policy for write actions unless `--no-approval`.

4) Harness integration
   - Always run harness in READ_ONLY or WORKSPACE_WRITE; never SYSTEM_WRITE/GODMODE.
   - Parse JSON result, compute delta versus last successful run.
   - Accept only if delta >= 0 and no new high-severity issues.

5) Memory integration
   - Persist plan, diffs, results, reflection, and decision via `memory/service.*` once available (M3.1+).

Acceptance criteria:
- Supervisor spec and flow are documented with commands and policies.
- No code yet; implementation to land in M4.1.
