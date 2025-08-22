# Semantic Compression Roadmap (codex-rs)

This directory contains an implementation plan to add semantic compression to Codex with minimal disruption and clear integration boundaries. It is split into three phases and multiple sprints with concrete PR slices, file paths, type names, and acceptance criteria to avoid ambiguity and merge conflicts.

Contents

1. 00-architecture.md — End-to-end data flow and insertion points
2. 01-phase1-summaries.md — Summaries-only MVP (lowest complexity)
3. 02-phase2-embeddings.md — Optional embedding memory and retrieval
4. 03-phase3-codebase-index.md — Optional codebase index + hybrid recall
5. 20-config.md — Config shape, defaults, and CLI/TUI surfacing
6. 30-testing.md — Unit/integration/UX testing strategy and fixtures
7. 40-migration.md — Rollout, compatibility, and failure modes
8. 50-pr-plan.md — Sprint plan and PR slicing
9. 70-security-privacy.md — Sensitive data handling and opt-in controls

Goals

- Keep Phase 1 shippable in a small set of PRs by reusing existing Compact summarization, history, and prompt plumbing.
- Ensure Phases 2–3 are opt-in, additive, and scoped behind clean traits and config flags.
- Preserve current UX by default; when enabled, inject only a tiny, bounded slice of compressed memory.

Non‑Goals

- Changing approval/sandbox behavior.
- Changing existing ephemeral context logic.
- Introducing external vector databases or hard dependencies on heavy local models by default.

Conventions

- New crate names must be prefixed with `codex-` (e.g., `codex-memory`).
- Do not add or modify any code related to `CODEX_SANDBOX_NETWORK_DISABLED_ENV_VAR` or `CODEX_SANDBOX_ENV_VAR`.
- Follow existing module and test placement patterns. Prefer small, focused PRs as outlined in 50-pr-plan.md.

