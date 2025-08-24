# Roadmap (M0–M5)

This roadmap clarifies scope, acceptance criteria, and dependencies per milestone.

M0 — Profiles & Policy (complete)
- YAML loader for `configs/policy.yaml` (no external deps)
- JSONL policy log; startup notice in TUI
- Pre‑exec warning/log when command violates the allowlist for current level
- Acceptance: visible notice + log entry; no execution blocks

M1 — Harness & Reports (complete)
- `harness/run.sh` orchestrates stages; deterministic `--seed` and schema v1
- CLI `code harness run`; TUI `/reports` picker views
- Store results under `harness/results/YYYYMMDD/<ts>.json`; daily summaries

M1.1 — In‑TUI Run & Autorun (complete)
- `/harness run` in TUI; `--startup-command` to autorun on launch
- Reports navigation: key hints; Esc to close; left/right to switch runs

M1.2 — Caching, Trends, Context, Phases (complete)
- Stage cache (`harness/cache`) and `cached` flags
- Trends view; Σ cached in summaries; context labels via `--context`
- Perpetual loop controls `/loop start|stop|once|status` (phase stubs)
- Phase docs + subagents strategy

M2 — Enforcement & Sandboxing (planned)
- Map profiles to real enforcement (gVisor/Firecracker, Landlock/Seatbelt)
- Network allowlists and fail‑closed defaults per profile
- Approval policy integration with enforcement state
- Minimal, auditable adapters for each platform; smoke tests

M3 — Memory & Episodes (planned)
- Persist episodes (research/plan/summary) to graph + vector stores
- Retrieval of prior episodes by topic/context; compaction strategies
- Embedder fallback and deterministic offline modes

M4 — Orchestrator (planned)
- Fully manage phases and tools; budgets and pacing per phase
- Subagent execution (OAuth preferred; API key fallback); retry/validate wrappers
- Rich summaries and run-to-run deltas integrated in reports

M5 — Security & Godmode (planned)
- MicroVM enforcement for GODMODE with explicit confirmation
- Resource budgets and exfiltration monitors; human‑visible audit trail

Cross‑cutting
- Plain‑text prompts/outputs; JSON only for storage
- Deterministic harness; caching for speed; context tagging for comparisons
- TUI remains responsive; background work stays visible; logs are durable
