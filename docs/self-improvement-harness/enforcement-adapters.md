# Enforcement Adapters — Platforms & Mapping

Goals
- Tie capability profiles to real OS enforcement with clear, auditable behavior.
- Keep adapters thin and deterministic; degrade gracefully when unavailable.

Profiles → enforcement
- READ_ONLY: sandboxed read/list/search only; deny write/exec/network.
- WORKSPACE_WRITE: read/list/run inside project; write within workspace; optional network.
- SYSTEM_WRITE: package install/build tools; elevated workspace writes; network allowlist.
- GODMODE: full host; explicit consent; session banner; microVM preferred.

Linux
- Landlock + seccomp for READ_ONLY / WORKSPACE_WRITE (default)
- gVisor (runsc) for SYSTEM_WRITE (optional) and GODMODE (preferred, explicit)
- Notes: per‑profile seccomp filters; bind‑mounts to restrict paths; network namespaces.

macOS
- Seatbelt profiles for READ_ONLY / WORKSPACE_WRITE; custom SBPL rules
- SYSTEM_WRITE via container or prompt; GODMODE by explicit user consent

Diagnostics
- On denial: concise human‑readable error with next steps (“profile X blocks Y; switch to Z or request approval”).
- Append to policy logs.

Integration points
- codex-core exec path maps current SandboxPolicy to chosen adapter.
- Respect approval policy before escalating profile/sandbox.
