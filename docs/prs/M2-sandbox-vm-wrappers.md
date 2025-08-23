# PR: M2 — Sandbox & VM wrappers

Summary:
Add executable wrappers and integration hooks to run commands under controlled environments. M2 focuses on the executable interface and config; low-level details (kernel flags, images) arrive in M2.1+.

Execution modes:
- gVisor (container runtime: runsc) for READ_ONLY/WORKSPACE_WRITE
- Firecracker microVM for SYSTEM_WRITE/GODMODE

Deliverables:
1) Executable interfaces
   - `sandbox/gvisor/run.sh <mount> <cmd...>`
     - Expected behavior: run `<cmd>` in a container using `--runtime=runsc` with workspace bind mounted read-only or read-write per profile.
     - Flags: `--network=none|allowlist`, `--uid`, `--gid`, `--seccomp-profile`.
   - `sandbox/firecracker/start.sh <mount> <cmd...>`
     - Expected behavior: boot a microVM with an ephemeral rootfs, mount workspace, execute `<cmd>` via vsock, stream stdout/stderr.
     - Flags: `--ttl`, `--mem`, `--vcpus`, `--allowlist-hosts`.

2) Policy mapping
   - READ_ONLY → gVisor, `--network=none`, mount ro
   - WORKSPACE_WRITE → gVisor, `--network=allowlist`, mount rw
   - SYSTEM_WRITE → Firecracker, allowlist; privileged ops allowed inside VM
   - GODMODE → Firecracker, broad allowlist; requires explicit confirmation (M5)

3) Seccomp & capabilities
   - Start from Docker’s default seccomp profile; remove unnecessary capabilities.
   - Keep profiles under `sandbox/security/` for version control.

4) Integration hooks
   - Shell tool wrapper: before executing a command, select wrapper based on active profile and invoke it with the correct flags.
   - Log the selected runtime and flags to the policy log.

Acceptance criteria:
- Wrappers accept arguments and print the resolved plan (dry-run if runtimes unavailable).
- Shell tool uses the wrapper selection logic; `READ_ONLY` attempts result in no writes to host.
- Policy log records execution mode, profile, and allowlist selection.

Follow-ups:
- M2.1: Implement real gVisor integration (docker/containerd) and network allowlist.
- M2.2: Implement Firecracker with vsock bridge and ephemeral rootfs.
