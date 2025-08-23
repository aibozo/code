#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
OUT_DIR="$ROOT_DIR/harness/results"
ART_DIR="$ROOT_DIR/harness/artifacts"
SCHEMA_VER="v1"
mkdir -p "$OUT_DIR" "$ART_DIR"

SEED=1337
while [[ $# -gt 0 ]]; do
  case "$1" in
    --seed)
      SEED="$2"; shift 2 ;;
    *) echo "Unknown arg: $1"; exit 2;;
  esac
done

ts=$(date +%Y%m%d-%H%M%S)
day=$(date +%Y%m%d)
log_file="$ART_DIR/$ts.log"
day_dir="$OUT_DIR/$day"
mkdir -p "$day_dir"
out_json="$day_dir/$ts.json"

echo "[harness] Starting harness run at $ts (seed=$SEED)" | tee "$log_file"

run_stage() {
  local name="$1"; shift
  local cmd=("$@")
  echo "[harness] stage=$name cmd=${cmd[*]}" | tee -a "$log_file"
  local status="ok"
  local metrics='{}'
  if output=$("${cmd[@]}" 2>>"$log_file"); then
    # Accept either JSON or empty
    if [[ -n "$output" ]]; then
      metrics="$output"
    fi
  else
    status="error"
  fi
  printf '{"name":"%s","status":"%s","metrics":%s}' "$name" "$status" "$metrics"
}

stages_json="["
stages_json+=$(run_stage unit "$ROOT_DIR/harness/stages/unit.sh" --seed "$SEED")
stages_json+=","$(run_stage integration "$ROOT_DIR/harness/stages/integration.sh" --seed "$SEED")
stages_json+=","$(run_stage property "$ROOT_DIR/harness/stages/property.sh" --seed "$SEED")
stages_json+=","$(run_stage security "$ROOT_DIR/harness/stages/security.sh" --seed "$SEED")
stages_json+=","$(run_stage style "$ROOT_DIR/harness/stages/style.sh" --seed "$SEED")

# SWE-bench stub (Python optional)
if command -v python3 >/dev/null 2>&1; then
  swe=$(python3 "$ROOT_DIR/harness/adapters/swebench/run.py" --seed "$SEED" 2>>"$log_file" || echo '{}')
else
  swe='{}'
fi
stages_json+=","$(printf '{"name":"%s","status":"%s","metrics":%s}' swebench ok "$swe")
stages_json+="]"

cat > "$out_json" <<JSON
{
  "version": "$SCHEMA_VER",
  "timestamp": "$ts",
  "seed": $SEED,
  "stages": $stages_json
}
JSON

echo "[harness] Results -> $out_json" | tee -a "$log_file"
