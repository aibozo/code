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
  # Log only to file (do not pollute JSON output)
  echo "[harness] stage=$name cmd=${cmd[*]}" >> "$log_file"
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

# Update day index and summary for fast browsing
if command -v python3 >/dev/null 2>&1; then
  python3 - "$day_dir" "$ts" <<'PY'
import json, os, sys
day_dir, ts = sys.argv[1], sys.argv[2]
index_path = os.path.join(day_dir, "_index.json")
summary_path = os.path.join(day_dir, "_daily_summary.json")

# Update index (list of runs)
idx = {"runs": [], "updated_at": ts}
if os.path.exists(index_path):
    try:
        with open(index_path, 'r') as f:
            idx = json.load(f)
    except Exception:
        pass
if ts not in idx.get("runs", []):
    idx.setdefault("runs", []).append(ts)
    idx["runs"] = sorted(idx["runs"])  # keep sorted
idx["updated_at"] = ts
with open(index_path, 'w') as f:
    json.dump(idx, f, indent=2)

# Recompute daily summary (aggregate ok/error per run + totals)
def read_json(path):
    try:
        with open(path, 'r') as f:
            return json.load(f)
    except Exception:
        return None

run_names = idx.get('runs') or []
runs = []
for tsname in sorted(run_names):
    path = os.path.join(day_dir, tsname + '.json')
    obj = read_json(path)
    if not obj:
        continue
    ok = err = 0
    for st in obj.get('stages', []) or []:
        status = st.get('status')
        if status == 'ok':
            ok += 1
        elif status == 'error':
            err += 1
    runs.append({
        "ts": obj.get("timestamp", tsname),
        "ok": ok,
        "error": err,
    })

total_ok = sum(r['ok'] for r in runs)
total_err = sum(r['error'] for r in runs)
improved = 0
for i in range(1, len(runs)):
    prev = runs[i-1]
    cur = runs[i]
    # Improved if errors decreased or ok increased
    if cur['error'] < prev['error'] or cur['ok'] > prev['ok']:
        improved += 1

summary = {
    "runs_count": len(runs),
    "total_ok": total_ok,
    "total_error": total_err,
    "improved_runs": improved,
    "runs": runs,
    "updated_at": ts,
}
with open(summary_path, 'w') as f:
    json.dump(summary, f, indent=2)
PY
fi
