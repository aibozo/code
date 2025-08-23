# PR: M1 — Harness v1 (golden gates)

Summary:
Add a stable, minimal harness that produces a JSON summary and provides
stubs for unit/integration/property/security/style checks and SWE-bench.

Changes included:
- `harness/run.sh` orchestration script (writes to `harness/results/`).
- `harness/adapters/swebench/run.py` stub runner.
- `harness/README.md` docs.

Acceptance criteria:
- Running `./harness/run.sh` creates a timestamped results JSON.
- No external network or system writes are required.

Follow-ups:
- Wire property tests and static analysis in separate PRs (M1.1+).

