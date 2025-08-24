#!/usr/bin/env bash
set -euo pipefail

# M6 — Relaunch: Restore a checkpoint into a fresh worktree
# Usage: scripts/relaunch.sh [<id>|latest] [--root <dir>] [--worktrees <dir>] [--profile <READ_ONLY|WORKSPACE_WRITE|GODMODE>]
# Defaults: --root orchestrator/checkpoints, --worktrees orchestrator/worktrees

ROOT="orchestrator/checkpoints"
WTROOT="orchestrator/worktrees"
PROFILE=""
ID="latest"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --root) ROOT="${2:-}"; shift 2;;
    --worktrees) WTROOT="${2:-}"; shift 2;;
    --profile) PROFILE="${2:-}"; shift 2;;
    -h|--help)
      echo "Usage: $0 [<id>|latest] [--root <dir>] [--worktrees <dir>] [--profile <mode>]"; exit 0;;
    *)
      ID="${1}"; shift;;
  esac
done

if [[ ! -d "$ROOT" ]]; then
  echo "No checkpoints found under: $ROOT" >&2
  exit 1
fi

if [[ "$ID" == "latest" ]]; then
  ID=$(ls -1 "$ROOT" | sort | tail -n1)
  if [[ -z "$ID" ]]; then echo "No checkpoints available" >&2; exit 1; fi
fi

SRC="$ROOT/$ID"
MAN="$SRC/manifest.json"
if [[ ! -d "$SRC" ]]; then
  echo "Checkpoint not found: $SRC" >&2
  exit 1
fi

mkdir -p "$WTROOT"
DEST="$WTROOT/$ID"
if [[ -e "$DEST" ]]; then
  echo "Worktree already exists: $DEST" >&2
  echo "Tip: remove it or choose a different ID." >&2
  exit 1
fi

mkdir -p "$DEST"

# Restore selected artifacts (best effort)
restore_if_exists() {
  local name="$1"; local src="$SRC/artifacts/$1"; local dst="$DEST/$1";
  if [[ -e "$src" ]]; then
    mkdir -p "$dst/.." >/dev/null 2>&1 || true
    cp -R "$src" "$DEST/" 2>/dev/null || cp -R "$src" "$dst" 2>/dev/null || true
  fi
}

restore_if_exists prompts
restore_if_exists skills
restore_if_exists configs
restore_if_exists memory
restore_if_exists harness

# Save git diff and status for context
if [[ -f "$SRC/git.diff" ]]; then
  mkdir -p "$DEST/.checkpoint"
  cp -f "$SRC/git.diff" "$DEST/.checkpoint/git.diff"
fi
if [[ -f "$MAN" ]]; then
  mkdir -p "$DEST/.checkpoint"
  cp -f "$MAN" "$DEST/.checkpoint/manifest.json"
fi

cat >"$DEST/RESTORED.md" <<MD
# Restored checkpoint: $ID

This directory contains artifacts restored from 

  $SRC

Included: prompts/, skills/, configs/, memory/, harness/ (when present).

Git context: see .checkpoint/git.diff and .checkpoint/manifest.json

Launching Code:

- From repo root: code
- Or from this worktree: (if binary in PATH) `code`

Recommended profile: ${PROFILE:-WORKSPACE_WRITE}

MD

echo "Restored checkpoint to: $DEST"
if [[ -n "$PROFILE" ]]; then
  echo "Recommended profile: $PROFILE"
fi

exit 0

