# PR: M5 — /godmode (ephemeral)

Summary:
Provide an explicit high-privilege mode usable only inside a Firecracker microVM with full audit. Requires typed confirmation and auto-expires after current task.

User flow:
1) User types `/godmode on`
2) TUI prompts: "Type ENABLE GODMODE to confirm"; shows risks and scope
3) On exact phrase, supervisor switches profile to GODMODE inside microVM, logs event
4) After task completes or on `/godmode off`, VM is torn down; profile reverts

Audit log format (JSONL):
```
{ "ts": "...", "event": "godmode_on", "reason": "...", "ticket": "optional" }
{ "ts": "...", "event": "exec", "cmd": ["apt-get","install","..."], "cwd": "..." }
{ "ts": "...", "event": "network", "dest": "...", "bytes": 1234 }
{ "ts": "...", "event": "godmode_off" }
```

Enforcement:
- GODMODE commands must route through Firecracker wrapper only
- Deny on host execution path; log attempted bypasses
- Auto-expire after `--ttl` (default: current command/task)

Acceptance criteria:
- UX, confirmation wording, and audit schema are specified
- Enforcement rules are documented; integration path via M2 wrappers
