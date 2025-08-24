# Enforcement Test Matrix — Profiles × Actions

Actions
- read: cat file
- list: ls directory
- search: rg string
- run-safe: echo hi
- run-tools: cargo build | npm install (simulated)
- write-ws: create/update file under cwd
- write-system: write under /usr/local (simulated)
- network: curl example.com (simulated)

Profiles
- READ_ONLY
- WORKSPACE_WRITE (net off)
- WORKSPACE_WRITE (net on)
- SYSTEM_WRITE (net on)
- GODMODE

Expectations (summary)
- READ_ONLY: allow read/list/search; deny run-tools/write/network.
- WORKSPACE_WRITE: allow run-safe, write-ws; deny write-system; network according to flag.
- SYSTEM_WRITE: allow tool runs; allow network per allowlist; deny write-system unless explicit.
- GODMODE: allow all; warn and log.

Notes
- Simulate unavailable adapters: tests should skip with clear message (seatbelt in CI).
- Denial messages should be human‑readable and include a remediation hint.
