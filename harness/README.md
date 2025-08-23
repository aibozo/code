# Harness

Evaluation harness for regression and improvement tracking. This folder is immutable under CI; changes must land via PRs and be reviewed.

Layers:
- Golden tests (unit/integration/property)
- Security/style gates (e.g., Semgrep, Bandit) – optional locally
- Challenge adapters (e.g., SWE-bench subset)

Commands:
- `./harness/run.sh` – orchestrates all available checks and writes a summary under `harness/results/`.

Outputs:
- `harness/results/YYYYMMDD.json` – summarized metrics
- `harness/artifacts/` – logs and partial reports

