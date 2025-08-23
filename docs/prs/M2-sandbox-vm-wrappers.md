# PR: M2 — Sandbox & VM wrappers

Summary:
Introduce stubs for gVisor- and Firecracker-based execution wrappers. No real
container/VM is started yet; these scripts serve as integration points.

Changes included:
- `sandbox/gvisor/run.sh` stub.
- `sandbox/firecracker/start.sh` stub.

Acceptance criteria:
- Scripts print usage info and exit 0; no privileges required.

Follow-ups:
- Integrate with actual runtimes and configs; gate behind capability profiles.

