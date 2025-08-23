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
