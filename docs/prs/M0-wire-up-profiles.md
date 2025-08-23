# PR: M0 — Wire up profiles

Summary:
Introduce capability profiles and initial configs. No behavior change beyond
recognizing profiles and displaying them in the UI (follow-up wiring).

Changes included:
- Add `configs/policy.yaml` defining READ_ONLY, WORKSPACE_WRITE, SYSTEM_WRITE, GODMODE.
- Add `configs/memory.yaml` with compaction and store defaults.
- Add docs under `docs/self-improvement-harness/`.

Acceptance criteria:
- Config files loadable and documented.
- TUI/CLI can read and display the current profile (stub OK).
- Profile transitions logged (stub acceptable, to be wired in M0.1).

Testing notes:
- Run `./build-fast.sh` (no tests required yet).

