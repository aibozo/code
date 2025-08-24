#!/usr/bin/env bash
set -euo pipefail

# M6 — Checkpoints: Persist accepted iterations as checkpoints
# Usage: scripts/checkpoint.sh [--note "text"] [--root <dir>] [--keep N]
# Defaults: --root orchestrator/checkpoints, --keep 10

ROOT="orchestrator/checkpoints"
KEEP=10
NOTE=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    --note)
      NOTE="${2:-}"; shift 2;;
    --root)
      ROOT="${2:-}"; shift 2;;
    --keep)
      KEEP="${2:-}"; shift 2;;
    -h|--help)
      echo "Usage: $0 [--note <text>] [--root <dir>] [--keep N]"; exit 0;;
    *)
      echo "Unknown arg: $1" >&2; exit 2;;
  esac
done

mkdir -p "$ROOT"

ts_utc() { date -u +%Y%m%d-%H%M%S; }
now_rfc3339() { date -u +%Y-%m-%dT%H:%M:%SZ; }

TS="$(ts_utc)"
DEST="${ROOT%/}/$TS"
ART="$DEST/artifacts"
mkdir -p "$ART"

esc_json() { echo -n "$1" | sed -e 's/\\/\\\\/g' -e 's/"/\\"/g' -e ':a;N;$!ba;s/\n/\\n/g'; }

# Capture git state (best effort)
GIT_REF=""
GIT_STATUS=""
GIT_DIFF_FILE="$DEST/git.diff"
if git rev-parse --is-inside-work-tree >/dev/null 2>&1; then
  GIT_REF="$(git rev-parse HEAD 2>/dev/null || true)"
  GIT_STATUS="$(git status --porcelain=v1 -b 2>/dev/null || true)"
  git diff --patch --no-color >"$GIT_DIFF_FILE" || :
else
  : >"$GIT_DIFF_FILE"
fi

# Copy selected artifacts if present
copy_if_exists() {
  local src="$1"; local dst="$ART/$(basename "$src")";
  if [[ -e "$src" ]]; then
    mkdir -p "$dst/.." >/dev/null 2>&1 || true
    cp -R "$src" "$ART/" 2>/dev/null || cp -R "$src" "$dst" 2>/dev/null || true
  fi
}

copy_if_exists prompts
copy_if_exists skills
copy_if_exists configs
copy_if_exists memory

# Harness metrics (best effort — copy common locations)
if [[ -d harness ]]; then
  # Copy common result files to provide broader context without copying binaries/large artifacts
  mkdir -p "$ART/harness"
  find harness -maxdepth 4 -type f \
    \( -name '*.json' -o -name '*.md' -o -name '*.html' -o -name '*.log' -o -name '*.txt' -o -name '*.csv' \
       -o -name '*metrics*' -o -name '*report*' \) -print0 \
    | while IFS= read -r -d '' f; do \
        dst="$ART/harness/${f#harness/}"; \
        mkdir -p "$(dirname "$dst")"; \
        cp -f "$f" "$dst"; \
      done
fi

# Write manifest.json
mkdir -p "$DEST"
MANIFEST="$DEST/manifest.json"
NOTE_ESC="$(esc_json "$NOTE")"
GIT_STATUS_ESC="$(esc_json "$GIT_STATUS")"
cat >"$MANIFEST" <<JSON
{
  "id": "$TS",
  "created_at": "$(now_rfc3339)",
  "cwd": "$(pwd)",
  "note": "${NOTE_ESC}",
  "git": {
    "ref": "${GIT_REF}",
    "status": "${GIT_STATUS_ESC}",
    "diff_file": "git.diff"
  },
  "artifacts": {
    "prompts": $( [[ -e "$ART/prompts" ]] && echo true || echo false ),
    "skills": $( [[ -e "$ART/skills" ]] && echo true || echo false ),
    "configs": $( [[ -e "$ART/configs" ]] && echo true || echo false ),
    "memory": $( [[ -e "$ART/memory" ]] && echo true || echo false ),
    "harness": $( [[ -e "$ART/harness" ]] && echo true || echo false )
  },
  "retention_keep": ${KEEP}
}
JSON

echo "Created checkpoint: $DEST"

# Retention: keep last N by directory name (timestamp ordering)
mapfile -t EXISTING < <(ls -1 "$ROOT" | sort)
COUNT=${#EXISTING[@]}
if [[ "$COUNT" -gt "$KEEP" ]]; then
  TO_DELETE=$((COUNT - KEEP))
  for ((i=0; i<TO_DELETE; i++)); do
    OLD="$ROOT/${EXISTING[$i]}"
    echo "Pruning old checkpoint: $OLD"
    rm -rf -- "$OLD"
  done
fi

exit 0
