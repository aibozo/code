# PR: M6 — Checkpoints & self-restart

Summary:
Persist accepted iterations as checkpoints and allow relaunching from them in a fresh sandbox.

Artifacts to save:
- `prompts/`, `skills/` (agent policies, tools)
- `configs/` (policy, memory)
- Memory snapshots: pg dump + Neo4j export (when enabled)
- Harness metrics (latest JSON + history)
- Git ref and diff from previous checkpoint

Scripts:
1) `scripts/checkpoint.sh [--note <text>]`
   - Create `/state/checkpoints/<ts>/`
   - Capture git status, diffs, and copy artifacts
   - Write `manifest.json` with paths and notes
2) `scripts/relaunch.sh [<id>|latest]`
   - Restore artifacts into a fresh workspace clone
   - Launch in selected sandbox profile

Acceptance criteria:
- Script specs and manifest format are documented
- Path layout and retention policy (keep last N=10) defined

Implementation notes (current):
- Scripts
  - `scripts/checkpoint.sh [--note <text>] [--root <dir>] [--keep N]`
    - Creates `orchestrator/checkpoints/<ts>/` (UTC timestamp), copies artifacts (`prompts/`, `skills/`, `configs/`, `memory/`, selected harness JSON/reports), saves `git.diff`, and writes `manifest.json` with fields:
      - `id`, `created_at` (RFC3339 UTC), `cwd`, `note`
      - `git.ref`, `git.status`, `git.diff_file`
      - `artifacts.{prompts,skills,configs,memory,harness}` (bools)
      - `retention_keep` (number)
    - Retention: keeps last N (default 10), prunes older checkpoints.
  - `scripts/relaunch.sh [<id>|latest] [--root <dir>] [--worktrees <dir>] [--profile <mode>]`
    - Restores artifacts into `orchestrator/worktrees/<id>/`, preserves `git.diff` and `manifest.json` under `.checkpoint/`, writes `RESTORED.md` with usage.
- TUI
  - `/checkpoint [note|--note <text>]` runs the checkpoint script and surfaces a brief result.
  - `/relaunch [<id>|latest] [--profile <mode>]` runs the relaunch script and surfaces a brief result.
  - `/safety` shows Artifacts root summary and safety logs.

Retention policy details:
- Checkpoints are named by UTC timestamps `YYYYMMDD-HHMMSS` allowing lexical sort.
- Pruning keeps the newest N by name ordering; default N=10 (configurable via `--keep`).
