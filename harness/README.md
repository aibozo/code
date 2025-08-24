# Harness

Evaluation harness for regression and improvement tracking. This folder is immutable under CI; changes must land via PRs and be reviewed.

Layers:
- Golden tests (unit/integration/property)
- Security/style gates (e.g., Semgrep, Bandit) – optional locally
- Challenge adapters (e.g., SWE-bench subset)

Commands:
- `./harness/run.sh` – orchestrates all available checks and writes a summary under `harness/results/`.
  - Flags:
    - `--seed N` – set RNG seed for deterministic behavior
    - `--no-cache` – disable stage cache and force fresh execution

Outputs:
- `harness/results/YYYYMMDD.json` – summarized metrics
- `harness/artifacts/` – logs and partial reports
 - `harness/cache/` – per-stage cache entries (keyed by script hash + git state + seed)

Schema notes:
- Each stage record includes `name`, `status`, `metrics`, and `cached`.
- Daily summaries (`_daily_summary.json`) aggregate `total_ok`, `total_error`, `total_cached`, and per-run counts.
