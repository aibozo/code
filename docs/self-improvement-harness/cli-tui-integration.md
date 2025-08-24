# CLI & TUI Integration

Goals:
- Surface active profile, sandbox mode, and approvals in the header
- Provide slash-commands: `/sandbox`, `/harness run`, `/loop start|stop|status|sprint`, `/godmode`
- Keep behavior deterministic and auditable

Header spec:
- Layout: `[Profile: WORKSPACE_WRITE] [Approvals: on-request] [Sandbox: gVisor] [Harness: last pass +Δ]`
- Color coding per profile

Commands:
1) `/sandbox <PROFILE>`
   - Validates transition; logs event; re-renders header
2) `/harness run [--seed N]`
   - Invokes `harness/run.sh`; opens results panel on completion
3) `/loop <start|stop|status|sprint ...>`
   - Perpetual loop controls (M1.2):
     - `start`: begin Research→Plan→Code→Test→Summarize cycles (agent-controlled time)
       - Options: `--interval SECS` (optional cadence), `--context NAME` (label runs), `--once`
     - `stop`: pause the loop (state preserved)
     - `status`: show current loop status
     - `sprint <goal> [--seed N]`: run a short, focused background sprint
4) `/godmode on|off`
   - Triggers confirmation flow and microVM enforcement (M5)

Implementation notes:
- Rust TUI: add a global `StatusBarState { profile, approvals, sandbox, last_harness, loop_state }`
- Update on events via `CodexEvent::PolicyChanged`, `HarnessEvent::Completed`, and background loop events
- Loop compacts context automatically (Op::Compact) to maintain headroom
- Phase exits are tool calls exposed by the core; TUI does not gate or force transitions.
