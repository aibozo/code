# PR: M1 — Harness v1 (golden gates)

Summary:
Provide a stable, CI-enforced harness with deterministic outputs. This PR creates the harness runner and adapters with local-only operations and a clean JSON report. Real checks will be added in follow-ups as discrete, reviewable PRs.

Scope for M1:
- Orchestrator script `harness/run.sh` with pluggable stages
- JSON result writer with versioned schema
- Local-only stubs for unit/integration/property/security/style
- SWE-bench adapter stub (no datasets required)

Implementation details:
1) Result schema
   - File: `harness/schema/result.v1.json` (add in this PR)
   - Fields: `timestamp`, `version`, `stages` (list), and per-stage metrics.
   - Keep schema small and forwards-compatible.

2) Orchestrator script
   - File: `harness/run.sh`
   - Stages executed in order: `unit -> integration -> property -> security -> style -> swebench`
   - Each stage runs a sub-script if present and captures exit/status/metrics.
   - Write a single JSON to `harness/results/YYYYMMDD-HHMMSS.json`.

3) Stage adapters (stubs)
   - `harness/stages/unit.sh` – returns `tests_run`, `tests_passed`.
   - `harness/stages/integration.sh` – returns `tests_run`, `tests_passed`.
   - `harness/stages/property.sh` – returns `cases_run`, `cases_passed`.
   - `harness/stages/security.sh` – returns `issues` grouped by `severity`.
   - `harness/stages/style.sh` – returns `issues` grouped by `rule`.
   - `harness/adapters/swebench/run.py` – returns `instances`, `resolved`, `score`.
   - All stubs must be networkless and write only to `harness/artifacts/`.

4) TUI/CLI integration (read-only for now)
   - CLI: `code /harness run` should shell out to `harness/run.sh`.
   - TUI: add a panel that detects the latest results JSON and renders deltas from the previous run.

5) Determinism & caching
   - Each stage must accept `--seed` and default to a fixed seed for deterministic behavior.
   - Cache previous results in `harness/.cache/` to allow delta computation without re-running unchanged stages (future PR M1.2).

Acceptance criteria:
- Running `./harness/run.sh` produces a JSON report compliant with `schema/result.v1.json`.
- All stages return zero or stubbed metrics without network/system writes.
- TUI panel can read and display the latest results file.

Follow-ups:
- M1.1: Wire unit/property tests on small local samples.
- M1.2: Add Semgrep/Bandit adapters (offline rules or vendored configs).
- M1.3: Add delta view and historical chart in TUI.
