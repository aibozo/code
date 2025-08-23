#!/usr/bin/env bash
set -euo pipefail

# Stub Firecracker microVM wrapper. In production, this would start a VM
# with an ephemeral rootfs and vsock bridge.
echo "[sandbox:firecracker] starting microVM (stub)..."
echo "[sandbox:firecracker] args: $@"
exit 0

