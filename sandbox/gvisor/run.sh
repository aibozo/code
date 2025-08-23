#!/usr/bin/env bash
set -euo pipefail

# Stub gVisor wrapper. In production, this would "docker run --runtime=runsc".
echo "[sandbox:gvisor] launching container (stub) with runsc runtime..."
echo "[sandbox:gvisor] args: $@"
exit 0

