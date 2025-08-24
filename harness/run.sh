#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
OUT_DIR="$ROOT_DIR/harness/results"
ART_DIR="$ROOT_DIR/harness/artifacts"
CACHE_DIR="$ROOT_DIR/harness/cache"
SCHEMA_VER="v1"
mkdir -p "$OUT_DIR" "$ART_DIR" "$CACHE_DIR"

SEED=1337
NO_CACHE=0
CONTEXT=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --seed)
      SEED="$2"; shift 2 ;;
    --no-cache|--force)
      NO_CACHE=1; shift ;;
    --context)
      CONTEXT="$2"; shift 2 ;;
    *) echo "Unknown arg: $1"; exit 2;;
  esac
done

ts=$(date +%Y%m%d-%H%M%S)
day=$(date +%Y%m%d)
log_file="$ART_DIR/$ts.log"
day_dir="$OUT_DIR/$day"
mkdir -p "$day_dir"
out_json="$day_dir/$ts.json"

echo "[harness] Starting harness run at $ts (seed=$SEED, cache=$((1-NO_CACHE)))" | tee "$log_file"

# Workspace key for cache invalidation
HEAD_HASH=$(git -C "$ROOT_DIR" rev-parse HEAD 2>/dev/null || echo nogit)
DIRTY_HASH=$(git -C "$ROOT_DIR" status --porcelain 2>/dev/null | sha1sum | awk '{print $1}')
WS_KEY="$HEAD_HASH-$DIRTY_HASH"

run_stage() {
  local name="$1"; shift
  local cmd=("$@")
  # Log only to file (do not pollute JSON output)
  echo "[harness] stage=$name cmd=${cmd[*]}" >> "$log_file"
  local status="ok"
  local metrics='{}'
  # Stage-specific script version
  local stage_script="$ROOT_DIR/harness/stages/$name.sh"
  local stage_ver
  stage_ver=$(sha1sum "$stage_script" 2>/dev/null | awk '{print $1}')
  local cache_key="$stage_ver|$name|$WS_KEY|$SEED"
  local cache_file="$CACHE_DIR/$name.json"
  if [[ $NO_CACHE -eq 0 ]] && [[ -f "$cache_file" ]] && grep -q "$cache_key" "$cache_file" 2>/dev/null; then
    echo "[harness] cache hit for $name" >> "$log_file"
    if command -v python3 >/dev/null 2>&1; then
      read_status_and_metrics=$(python3 - "$cache_file" <<'PY'
import json,sys
path=sys.argv[1]
with open(path,'r') as f:
    obj=json.load(f)
st=obj.get('status','ok')
mx=obj.get('metrics',{})
print(st)
print(json.dumps(mx))
PY
)
      status=$(printf "%s" "$read_status_and_metrics" | sed -n '1p')
      metrics=$(printf "%s" "$read_status_and_metrics" | sed -n '2p')
      printf '{"name":"%s","status":"%s","metrics":%s,"cached":true}' "$name" "$status" "$metrics"
      return
    fi
  fi

  if output=$("${cmd[@]}" 2>>"$log_file"); then
    # Accept either JSON or empty
    if [[ -n "$output" ]]; then
      metrics="$output"
    fi
  else
    status="error"
  fi
  # Store cache
  echo "[harness] cache store for $name" >> "$log_file"
  if command -v python3 >/dev/null 2>&1; then
    python3 - "$cache_file" <<PY
import json,sys
path=sys.argv[1]
obj={"cache_key": "$cache_key", "status": "$status", "metrics": $metrics}
with open(path,'w') as f:
    json.dump(obj,f)
PY
  fi
  printf '{"name":"%s","status":"%s","metrics":%s,"cached":false}' "$name" "$status" "$metrics"
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

# Optional context key
CONTEXT_JSON=""
if [[ -n "$CONTEXT" ]]; then
  CONTEXT_JSON=",\n  \"context\": \"$CONTEXT\""
fi

cat > "$out_json" <<JSON
{
  "version": "$SCHEMA_VER",
  "timestamp": "$ts",
  "seed": $SEED$CONTEXT_JSON,
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
    cached = 0
    for st in obj.get('stages', []) or []:
        status = st.get('status')
        if status == 'ok':
            ok += 1
        elif status == 'error':
            err += 1
        if st.get('cached'):
            cached += 1
    runs.append({
        "ts": obj.get("timestamp", tsname),
        "ok": ok,
        "error": err,
        "cached": cached,
    })

total_ok = sum(r['ok'] for r in runs)
total_err = sum(r['error'] for r in runs)
total_cached = sum(r.get('cached', 0) for r in runs)
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
    "total_cached": total_cached,
    "improved_runs": improved,
    "runs": runs,
    "updated_at": ts,
}
with open(summary_path, 'w') as f:
    json.dump(summary, f, indent=2)
PY
fi
