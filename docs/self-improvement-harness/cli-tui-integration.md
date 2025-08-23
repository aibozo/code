# CLI & TUI Integration

Goals:
- Surface active profile, sandbox mode, and approvals in the header
- Provide slash-commands: `/sandbox`, `/harness run`, `/improve`, `/godmode`
- Keep behavior deterministic and auditable

Header spec:
- Layout: `[Profile: WORKSPACE_WRITE] [Approvals: on-request] [Sandbox: gVisor] [Harness: last pass +Δ]`
- Color coding per profile

Commands:
1) `/sandbox <PROFILE>`
   - Validates transition; logs event; re-renders header
2) `/harness run [--seed N]`
   - Invokes `harness/run.sh`; opens results panel on completion
3) `/improve <goal> [--max-attempts K] [--no-approval]`
   - Starts supervisor loop (M4)
4) `/godmode on|off`
   - Triggers confirmation flow and microVM enforcement (M5)

Implementation notes:
- Rust TUI: add a global `StatusBarState { profile, approvals, sandbox, last_harness }`
- Update on events via `CodexEvent::PolicyChanged` and `HarnessEvent::Completed`

