# Rust/codex-rs

In the codex-rs folder where the rust code lives:

- Crate names are prefixed with `codex-`. For examole, the `core` folder's crate is named `codex-core`
- When using format! and you can inline variables into {}, always do that.
- Never add or modify any code related to `CODEX_SANDBOX_NETWORK_DISABLED_ENV_VAR` or `CODEX_SANDBOX_ENV_VAR`.
  - You operate in a sandbox where `CODEX_SANDBOX_NETWORK_DISABLED=1` will be set whenever you use the `shell` tool. Any existing code that uses `CODEX_SANDBOX_NETWORK_DISABLED_ENV_VAR` was authored with this fact in mind. It is often used to early exit out of tests that the author knew you would not be able to run given your sandbox limitations.
  - Similarly, when you spawn a process using Seatbelt (`/usr/bin/sandbox-exec`), `CODEX_SANDBOX=seatbelt` will be set on the child process. Integration tests that want to run Seatbelt themselves cannot be run under Seatbelt, so checks for `CODEX_SANDBOX=seatbelt` are also often used to early exit out of tests, as appropriate.

Completion/build step

- On completion of work, run only `./build-fast.sh` from the repo root. This script verifies file integrity and is the single required check right now.
- Do not run additional format/lint/test commands on completion (e.g., `just fmt`, `just fix`, `cargo test`) unless explicitly requested for a specific task.

When making individual changes prefer running tests on individual files or projects first if asked, but otherwise rely on `./build-fast.sh` at the end.

## Command Execution Architecture

The command execution flow in Codex follows an event-driven pattern:

1. **Core Layer** (`codex-core/src/codex.rs`):
   - `on_exec_command_begin()` initiates command execution
   - Creates `EventMsg::ExecCommandBegin` events with command details

2. **TUI Layer** (`codex-tui/src/chatwidget.rs`):
   - `handle_codex_event()` processes execution events
   - Manages `RunningCommand` state for active commands
   - Creates `HistoryCell::Exec` for UI rendering

3. **History Cell** (`codex-tui/src/history_cell.rs`):
   - `new_active_exec_command()` - Creates cell for running command
   - `new_completed_exec_command()` - Updates with final output
   - Handles syntax highlighting via `ParsedCommand`

This architecture separates concerns between execution logic (core), UI state management (chatwidget), and rendering (history_cell).

## Ephemeral Context Injection

Codex automatically injects fresh context at the end of each model request:

- **Location**: Ephemeral items are appended as the last items in the request via `get_formatted_input()`
- **Content**: System status (cwd, git branch, reasoning level), browser info (URL, type), screenshots
- **Generation**: `build_turn_ephemeral_items()` creates fresh context for each retry attempt
- **Storage**: Stored in `Prompt.ephemeral_items` field, ensuring they're regenerated per-request

This design ensures the model always receives current environment context without complex refresh logic or state management.

## Runtime Profiles & Safety (M5)

- Profiles: `READ_ONLY`, `WORKSPACE_WRITE`, `GODMODE` (danger-full-access).
  - `READ_ONLY`: read-only disk, no network. Safe introspection tasks only.
  - `WORKSPACE_WRITE`: write inside the project; network optional per policy.
  - `GODMODE`: high-privilege; isolated via microVM/container wrapper when available; host fallback blocked by default.
- Consent: entering GODMODE requires one-time typed confirmation (`GODMODE`). Consent is logged.
- Isolation: exec is routed to `sandbox/firecracker/start.sh` (or `sandbox/gvisor/run.sh`) with policy args (cwd, writable roots, network). The shim enforces read-only outside the project and network-off by default.
- Approvals: risky commands (installs, writes outside project, escalations) go through the approval flow. Clear dependency installs convert to async after ~2 minutes; manage from `/approvals` (Ctrl+P).
- Policy gate: allowlist/blocklist gate sourced from `configs/policy.yaml` (`allowed_commands`, `allowed_hosts`, with inheritance). Clearly dangerous commands are blocked; unknowns will ask unless policy is `Never`.
- Auditing: safety decisions and GODMODE actions append JSONL entries to `logs/policy.jsonl`. Approvals lifecycle logs to `.code/approvals/requests.jsonl`.
- Safety status: use `/safety` to view active profile, network status, writable roots, and recent safety logs. Exfil warnings surface as footer hints and are logged.

Notes for agents:

- Prefer `apply_patch` for edits; don’t bypass approvals or sandboxes.
- For long installs, request approval once and keep working; rely on async approvals to proceed later.
- Use concise preambles; group related actions.
- On completion, only run `./build-fast.sh` (no additional lint/test unless asked).

## Self‑Deploy Guardrails (M6 prep)

- Explicit scope: Any self‑redeploy or service restart must be requested or confirmed by the user; do not assume permission.
- Gate controls: Treat redeploy, service restarts, and env changes as “risky actions” — require approvals; block when policy denies.
- Budget/timeout: Keep redeploy actions bounded by wall‑time; fail fast and surface logs rather than looping.
- Rollback: Always prepare a “last known good” pointer (tag or commit SHA) before attempting changes; surface `/restore` as the recovery path.
- Audit: Log each phase (build, package, deploy, restart) with clear artifacts and outcomes.
- Artifacts: Use `orchestrator/episodes` as the canonical artifacts root. Each episode should contain plan.md, changes.md, reflection.md, and summary.md.
- Network & hosts: Respect `allowed_hosts`; any calls to registries or artifact stores must be on the allowlist or approved.
 - Idempotence: Plan actions to be re‑runnable safely (e.g., check current version before restart; skip if already applied).

## Self‑Improvement Harness Architecture

Overview
- Perpetual agent loop focused on measurable improvements. Core phases: Research → Plan → Code → Test → Summarize. Plain‑text prompts and artifacts; JSON only for stored results.
- Orchestrator runs in‑process and writes human‑readable episode artifacts under `orchestrator/episodes/<ts>/`. Harness runs produce JSON summaries under `harness/results/YYYYMMDD/<ts>.json` with daily indexes.

Key Components
- Orchestrator: `codex-rs/core/src/orchestrator.rs`
  - Drives bounded improvement cycles with budgets and acceptance gates.
  - Steps: diagnose target → research → plan → attempt loop → harness run → reflect → accept/continue → summarize.
  - Acceptance: prefer fewer errors (Δerr ≤ 0) and more passes (Δok ≥ 0); deny on new high‑severity security failures.
  - Writes `assessment.md`, `coder-output.md`, `reflection.md`, `summary.md` in the episode dir and ingests episode + last harness run into graph memory.
  - Pause/resume: `pause()/resume()/is_paused()` control loop pacing. Context labels tag runs via `--context`.

- Phase Tools: `codex-rs/core/src/phase_tools.rs`
  - Enum `Phase` with cyclical order; one finish tool exposed per phase: `research_finish`, `plan_finish`, `code_finish`, `test_finish`, `summary_finish`.
  - Each finish tool accepts `text` (plain‑text), validates minimal length and rejects JSON blocks, writes `<phase>.md` to the active episode, then advances the phase.
  - After `summary_finish`, best‑effort ingestion into the graph store runs.

- Harness Runner: `harness/run.sh`
  - Deterministic stages: `unit`, `integration`, `property`, `security`, `style`, plus a SWE‑bench stub when Python is present.
  - Per‑stage caching keyed by script hash + workspace git state + seed; outputs `results/YYYYMMDD/<ts>.json` and updates daily `_index.json` and `_daily_summary.json`.
  - Flags: `--seed N`, `--no-cache`, `--context <label>`. Logs to `harness/artifacts/<ts>.log`.

- Memory: Graph Store (file‑backed JSONL)
  - Paths: `codex-rs/memory/src/graph/` (`mod.rs`, `ingest.rs`, `retrieve.rs`).
  - Ingestion: `ingest_run_file` adds Run/Stage nodes; `ingest_episode_dir` adds Episode and cites ResearchSource nodes (from `sources.json`) and edges.
  - Retrieval helpers summarize recent runs/episodes and filter by `context`; orchestrator uses ingestion to preserve history across runs.

- Enforcement & Policy
  - Gate: `codex-rs/core/src/enforcement_gate.rs` provides deterministic allow/deny decisions from `SandboxPolicy` and the command vector (READ_ONLY vs WORKSPACE_WRITE vs GODMODE), with human‑readable messages.
  - Advisor: `codex-rs/core/src/enforcement_advisor.rs` surfaces non‑blocking hints for likely denials.
  - Policy source: `configs/policy.yaml` selects active level; environment context advertises `policy_level`, wrapper availability (`firecracker`/`gvisor`/`none`), artifacts root.

- CLI/TUI Integration
  - Header shows profile, approvals, sandbox wrapper, and last harness delta; see `docs/self-improvement-harness/cli-tui-integration.md`.
  - Slash‑commands (design/implementation): `/sandbox`, `/harness run [--seed N] [--context L]`, `/loop start|stop|status|sprint`, `/godmode`.
  - Event‑driven UI: core emits events; TUI manages `RunningCommand` and history cells for rendering (see sections above).

Data & Artifacts
- Episodes: `orchestrator/episodes/<ts>/`
  - `assessment.md`, `research.md`, `plan.md`, `changes.md`, `test.md`, `summary.md`, optional `sources.json`.
- Harness results: `harness/results/YYYYMMDD/<ts>.json`, `_index.json`, `_daily_summary.json`; logs in `harness/artifacts/`.
- Graph store: `graph/nodes.jsonl`, `graph/edges.jsonl` at repo root.

Flows
- Loop (orchestrator): diagnose → research (may spawn minis for notes, synthesize) → plan → code attempt(s) → run harness → reflect → accept gate → summarize → ingest.
- Manual phases (tools): agent calls `*.finish(text)` to exit each phase; TUI/CLI expose only the current phase’s finish tool.

Constraints & Conventions
- Plain‑text only for prompts and phase outputs; avoid JSON except for stored results and graph records.
- Deterministic, cacheable harness; results are append‑only and indexed by day.
- Safety first: start in READ_ONLY; escalate to WORKSPACE_WRITE with approvals; prefer isolated wrappers (Firecracker/gVisor) where available; log all sensitive actions.

Quick References
- Orchestrator entry: `run_improve()` in `codex-rs/core/src/orchestrator.rs`.
- Phase tools wiring: `finish_tool_for_phase()` and `handle_finish_for_phase()` in `codex-rs/core/src/phase_tools.rs`.
- Harness driver: `harness/run.sh`; stages under `harness/stages/` and adapters under `harness/adapters/`.
- Graph ingest/retrieve: `codex-rs/memory/src/graph/`.
