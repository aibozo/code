# PR: M0 — Wire up profiles

Summary:
Introduce formal capability profiles with clear behaviors and surface them in CLI/TUI. Profiles gate command execution, network egress, and require approvals depending on level. This PR focuses on wiring and UX, not deep sandboxing (that’s M2).

Profiles:
- READ_ONLY: No writes; network default-deny; only safe read commands allowed.
- WORKSPACE_WRITE: Writes allowed under repo; network allowlist (GitHub, npm, PyPI).
- SYSTEM_WRITE: Writes allowed to system, but only inside VM (M2); broader allowlist.
- GODMODE: Full capabilities, only inside ephemeral microVM (M5), requires explicit confirm.

Changes included:
- Add `configs/policy.yaml` defining profiles, allowed_commands, allowed_hosts, and logging path.
- Add `configs/memory.yaml` with retention defaults (consumed later by memory/orchestrator).
- Add status chip in TUI header and CLI flag mapping (implementation notes below).
- Add JSONL policy log writer stub.

Implementation (step-by-step):
1) Config ingestion
   - File: `codex-cli/src/config.ts` (or equivalent config loader in this repo)
   - Add `PolicyConfig` interface matching `configs/policy.yaml` and a loader that merges defaults + file contents.
   - Resolve inheritance keys (`inherit: READ_ONLY`) into concrete lists at load time.

2) CLI flag mapping
   - File: `codex-cli/src/cli.ts`
   - Add flag `--profile <READ_ONLY|WORKSPACE_WRITE|SYSTEM_WRITE|GODMODE>`; default from `policy.yaml.level`.
   - If `--sandbox` flag already exists, map levels as: `READ_ONLY -> read_only`, `WORKSPACE_WRITE -> workspace_write`, `SYSTEM_WRITE -> system_write`, `GODMODE -> godmode`.
   - Persist the active profile in the runtime state (e.g., `AppState.profile`).

3) TUI header status chip
   - File: `codex-rs/codex-tui/src/app.rs` and/or `codex-rs/codex-tui/src/header.rs` (where header is drawn)
   - Add a compact chip showing: `Profile: WORKSPACE_WRITE | Approvals: on-request | Sandbox: workspace_write`.
   - Colors: READ_ONLY (yellow), WORKSPACE_WRITE (green), SYSTEM_WRITE (blue), GODMODE (red).
   - Update on profile change events.

4) Policy log writer
   - File: `codex-rs/core/src/codex.rs`
   - Add a small helper: `fn log_policy_event(event: &str, details: serde_json::Value)` that appends JSONL to `logs/policy.jsonl`.
   - Log on: startup (profile), profile change, approval mode changes.
   - Note: Do not modify existing sandbox env var logic; this is orthogonal logging only.

5) Profile transitions command
   - TUI command palette: `/sandbox <profile>` triggers a state change and re-render.
   - CLI: `code --profile WORKSPACE_WRITE` sets initial profile.
   - Validation: reject illegal transitions (e.g., to GODMODE) without confirmation flow (M5).

6) Command gate (lightweight, no VM yet)
   - In the shell tool wrapper (repo: `codex-cli`), add a preflight check that warns (but does not block yet) when a command is outside `allowed_commands[profile]`.
   - Emit a policy log entry with `action: "warn_disallowed_command"` and `profile`.
   - Hard enforcement arrives in M2 when VM wrappers land.

Acceptance criteria:
- Profile can be set via CLI flag and reflected in TUI header.
- Profile changes are logged to `logs/policy.jsonl` as JSONL.
- Disallowed commands emit a warning and a policy log entry.
- No changes to deep sandboxing behavior yet.

Manual test plan:
- Run `code --profile READ_ONLY` and verify header shows current profile and approvals mode.
- Attempt a write action in READ_ONLY; observe a warning and policy log entry.
- Switch to WORKSPACE_WRITE via `/sandbox WORKSPACE_WRITE`; header updates and logs entry.
