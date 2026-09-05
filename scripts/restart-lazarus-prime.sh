#!/bin/bash
# Graceful primed restart: persist the live TIDES window, then reload it.
# Never SIGKILL unless the process ignores TERM for 30s.
set -euo pipefail
BIN=/home/umbrel/blake2b/prefix/bin/primed
START=/home/umbrel/blake2b/bin/start-lazarus-prime.sh
STATS=/home/umbrel/blake2b/lazarus-prime/stats.json
LOG=/home/umbrel/blake2b/logs/lazarus-prime.log
PAT='^/home/umbrel/blake2b/prefix/bin/primed'
REPORT=${REPORT:-/tmp/primed-restart-report.txt}

exec > >(tee "$REPORT") 2>&1

if [ ! -x "$BIN" ]; then
  echo "missing $BIN"
  exit 1
fi

snapshot() {
  python3 - "$1" <<'PY'
import json, sys
d = json.load(open(sys.argv[1]))
w = d.get("window") or {}
ms = sorted(w.get("miners") or [], key=lambda m: -int(m.get("work") or 0))[:12]
print(f"work={w.get('work')} target={w.get('target_work')} miners={len(w.get('miners') or [])}")
for m in ms:
    ident = (m.get("identity") or "")[:42]
    share = float(m.get("share_percent") or 0)
    print(f"  {ident} work={m.get('work')} share={share:.3f}%")
PY
}

echo "=== before ==="
if curl -sf --max-time 3 http://127.0.0.1:28916/stats.json -o /tmp/prime-stats-before.json; then
  snapshot /tmp/prime-stats-before.json
else
  echo "stats not answering"
fi

PID=$(pgrep -f "$PAT" | head -1 || true)
echo "target pid: ${PID:-none}"
T0=$(date -u +%s.%N)
if [ -n "${PID:-}" ]; then
  if [ "$PID" = "$$" ] || [ "$PID" = "$PPID" ]; then
    echo "ABORT: pid $PID is this script"
    exit 1
  fi
  kill -TERM "$PID"
  for _ in $(seq 1 300); do
    kill -0 "$PID" 2>/dev/null || break
    sleep 0.1
  done
  if kill -0 "$PID" 2>/dev/null; then
    echo "did not exit on TERM after 30s; sending KILL (window may not have persisted)"
    kill -KILL "$PID"
    sleep 0.5
  fi
fi
echo "old process gone"

mkdir -p "$(dirname "$LOG")"
setsid nohup "$START" >>"$LOG" 2>&1 </dev/null &

UP=""
for _ in $(seq 1 120); do
  if curl -sf --max-time 2 http://127.0.0.1:28916/stats.json -o /tmp/prime-stats-after.json; then
    UP=yes
    break
  fi
  sleep 0.25
done
T1=$(date -u +%s.%N)
echo "listener answering: ${UP:-NO}"
echo "downtime_seconds: $(echo "$T1 - $T0" | bc)"
echo "=== after ==="
if [ -f /tmp/prime-stats-after.json ]; then
  snapshot /tmp/prime-stats-after.json
  if [ -f /tmp/prime-stats-before.json ]; then
    python3 - <<'PY'
import json
b=json.load(open("/tmp/prime-stats-before.json"))
a=json.load(open("/tmp/prime-stats-after.json"))
bm={m["identity"]:int(m.get("work") or 0) for m in (b.get("window") or {}).get("miners") or []}
am={m["identity"]:int(m.get("work") or 0) for m in (a.get("window") or {}).get("miners") or []}
cliffs=[]
for ident,prev in bm.items():
    if prev < 1_000_000:
        continue
    now=am.get(ident,0)
    if now + prev//10 < prev:
        cliffs.append((ident, prev, now))
if cliffs:
    print("WINDOW CLIFF after restart:")
    for ident,prev,now in cliffs:
        print(f"  {ident} {prev} -> {now}")
    raise SystemExit(2)
print("no window cliff vs pre-restart stats")
PY
  fi
fi
echo "done at $(date -u +%H:%M:%S)"
