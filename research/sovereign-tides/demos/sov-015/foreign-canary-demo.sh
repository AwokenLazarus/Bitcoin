#!/usr/bin/env bash
# SPDX-License-Identifier: MIT
# Copyright (c) 2026 Mike Moore (AwokenLazarus)
# SOV-015: foreign canaries catch gateways fed by one shared third-party node, on Knots 29.4.2
# regtest, against rnd/sov-015. Per run: a fresh primed (publish, sovereignty-demo off), the pool
# node, 2 honest gateways on their own nodes, 3 gateways fed by one third-party node T (each
# registered a different port of T), SOV-004's proxy as control, and the real lazarus-canary sidecar
# with `[foreign] enabled`. Then a relay-race probe: how often an honest node takes C by relay
# before the twin the sidecar pushed it, by twin lag.
# Ports 32800-32899: primed 32815/32816, Knots 32820-32835, race probe 32880-32885. Datadirs under
# ./run. No miners. Nothing is pushed. Runs itself under a CPU/memory-capped scope (SOV-013 soak).
#
#   ./foreign-canary-demo.sh                        # 3 x 420 s (+ 55 s drain) + race probe
#   RUNS=1 MIN_RUNS=1 KNOTS_SECS=240 RACE_TRIALS=0 ./foreign-canary-demo.sh
set -euo pipefail
if [[ -z "${SOV015_SCOPED:-}" ]]; then
  exec systemd-run --user --scope -q -p CPUQuota=200% -p MemoryMax=4G env SOV015_SCOPED=1 nice -n 19 "$0" "$@"
fi
HERE=$(cd "$(dirname "$0")" && pwd)
WT=${WT:?set WT to a checkout of this repo with the primed patches applied}
PRIME=$WT/prime
RUN=${SOV015_RUN:-$HERE/run}
RUNS=${RUNS:-3}
MIN_RUNS=${MIN_RUNS:-3}
KNOTS_SECS=${KNOTS_SECS:-420}
RACE_TRIALS=${RACE_TRIALS:-20}
RACE_LAGS=${RACE_LAGS:-0,0.05,0.25,1,4}
LISTEN_PORT=32815
STATS_PORT=32816
BASE=32820
RACE_BASE=32880
KNOTS=${KNOTS:?set KNOTS to a Knots v29.4.2.knots20260508 bitcoind}
export PRIME_STATS="http://127.0.0.1:${STATS_PORT}" SOV015_RUN="$RUN" SIDECAR_PY="$WT/lazarus/canary/lazarus_canary.py"

[[ -x "$KNOTS" ]] || { echo "no Knots 29.4.2 at $KNOTS" >&2; exit 1; }
busy=$(ss -Hltn | awk '{print $4}' | sed 's/.*://' | awk '$1>=32800 && $1<=32899' | sort -u | tr '\n' ' ')
[[ -z "$busy" ]] || { echo "ports in 32800-32899 already in use: $busy (a stale run?); refusing to start" >&2; exit 1; }
PRIMED_PID=
cleanup() {
  if [[ -n "$PRIMED_PID" ]]; then
    kill "$PRIMED_PID" 2>/dev/null || true
    wait "$PRIMED_PID" 2>/dev/null || true
  fi
  PRIMED_PID=
}
trap cleanup EXIT
trap 'exit 130' INT TERM HUP

echo "== build primed ($(git -C "$WT" rev-parse --abbrev-ref HEAD) $(git -C "$WT" rev-parse --short HEAD)) and gwsim =="
(cd "$PRIME" && cargo build -j2 --offline -q --release --bin primed)
(cd "$HERE/gwsim" && cargo build -j2 --offline -q --release)
echo "== sidecar unit tests =="
(cd "$WT/lazarus/canary" && python3 -m unittest -q test_lazarus_canary 2>&1 | tail -1)

start_primed() {
  local dir="$RUN/prime-$1"
  rm -rf "$dir" && mkdir -p "$dir"
  cat > "$dir/prime.toml" <<TOML
listen = "127.0.0.1:${LISTEN_PORT}"
stats-listen = "127.0.0.1:${STATS_PORT}"
data-dir = "$dir"
payout-address = "bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4"
network = "mainnet"
rpc = "http://127.0.0.1:${BASE}"
rpc-user = "x"
rpc-password = "sov015"
min-diff = 1
fee-bps = 0
house-loopback = false
sovereignty-mode = "publish"
sovereignty-recompute-secs = 5
# production default for the check delay (45 s); the gap is the demo's (production 300 s)
sovereignty-canary-check-gap-secs = 10
headline = "SOV-015 rnd/sov-015"
TOML
  if curl -s -o /dev/null "$PRIME_STATS/healthz"; then
    echo "something already listens on :${STATS_PORT}; refusing to test against it" >&2
    exit 1
  fi
  (cd "$PRIME" && RUST_LOG=info,primed::session=debug exec ./target/release/primed -c "$dir/prime.toml" run \
    >"$dir/primed.log" 2>"$dir/primed.err") &
  PRIMED_PID=$!
  for _ in $(seq 1 50); do
    curl -sf "$PRIME_STATS/healthz" >/dev/null && [[ -s "$dir/sovereignty.token" && -s "$dir/prime.key" ]] && return 0
    kill -0 "$PRIMED_PID" 2>/dev/null || { echo "primed died:" >&2; cat "$dir/primed.err" >&2; exit 1; }
    sleep 0.2
  done
  echo "primed did not come up" >&2
  exit 1
}

STAMP=$(date +%s)
RESULTS=()
FAILED=0
for i in $(seq 1 "$RUNS"); do
  echo "== run $i/$RUNS: ${KNOTS_SECS}s =="
  start_primed "$i"
  rm -rf "$RUN/sidecar-$i"
  if python3 "$HERE/foreign_knots.py" "$KNOTS" "$KNOTS_SECS" "$BASE" "127.0.0.1:${LISTEN_PORT}" "$RUN/prime-$i" \
      "$RUN/sidecar-$i" "$HERE/gwsim/target/release/gwsim" | tee "$RUN/run-$STAMP-$i.log"; then :; else FAILED=$((FAILED + 1)); fi
  RESULTS+=("$(grep '^result ' "$RUN/run-$STAMP-$i.log" | tail -1 | cut -d" " -f2 || true)")
  cleanup
done

RACE=
if [[ "$RACE_TRIALS" -gt 0 ]]; then
  echo "== relay-race probe: $RACE_TRIALS trials per twin lag ($RACE_LAGS s) =="
  if python3 "$HERE/race_probe.py" "$KNOTS" "$RACE_BASE" "$RUN/race" "$WT/lazarus/canary" "$RACE_TRIALS" "$RACE_LAGS" \
      | tee "$RUN/race-$STAMP.log"; then
    RACE=$(grep '^result ' "$RUN/race-$STAMP.log" | tail -1 | cut -d" " -f2 || true)
  else
    FAILED=$((FAILED + 1))
  fi
fi

echo "== summary over $RUNS runs =="
python3 - "$FAILED" "$MIN_RUNS" "$RACE" "${RESULTS[@]}" <<'PY'
import json, sys
failed, min_runs, race = int(sys.argv[1]), int(sys.argv[2]), sys.argv[3]
runs = [json.load(open(p)) for p in sys.argv[4:] if p]
honest, farm, proxy = ["knots-h1", "knots-h2"], ["tfarm-1", "tfarm-2", "tfarm-3"], "knots-proxy"
print("| gateway | flagged runs | clustered runs | own hit/due | foreign hit/due | independence per run | reasons |")
print("|---|---:|---:|---:|---:|---|---|")
for g in honest + farm + [proxy]:
    rows = [r["rows"].get(g) or {} for r in runs]
    fl = sum(bool(x.get("flagged")) for x in rows)
    cl = sum(any(g in c for c in r["clusters"]) for r in runs)
    s = lambda k: sum(x.get(k) or 0 for x in rows)
    reasons = sorted({q for x in rows for q in x.get("flag_reasons") or []})
    print(f"| {g} | {fl}/{len(rows)} | {cl}/{len(rows)} | {s('canary_hits')}/{s('canaries_due')} | {s('foreign_hits')}/{s('foreign_due')} | "
          f"{', '.join(str(x.get('independence')) for x in rows)} | {','.join(reasons)} |")
farm_ok = sum(sorted(farm) in r["clusters"] for r in runs)
honest_clustered = sum(any(h in c for c in r["clusters"]) for r in runs for h in honest)
honest_flagged = sum(bool((r["rows"].get(h) or {}).get("flagged")) for r in runs for h in honest)
proxy_flagged = sum(bool((r["rows"].get(proxy) or {}).get("flagged")) for r in runs)
fh = sum((r["rows"].get(h) or {}).get("foreign_hits") or 0 for r in runs for h in honest)
fd = sum((r["rows"].get(h) or {}).get("foreign_due") or 0 for r in runs for h in honest)
print(f"shared-source clusters per run: {[r['clusters'] for r in runs]}")
print(f"twin ack lag per run (s after C was sent): {[r['lags'] for r in runs]}")
print(f"own/shared leaks: {[r['leaks'] for r in runs]}; key isolation: {['OK' if r['key_ok'] else 'FAIL' for r in runs]}")
print(f"sidecar per run: {[r['sidecar'] for r in runs]}")
print(f"honest relay-race rate in the demo: {fh}/{fd} foreign canaries carried by honest gateways"
      f" ({(fh / fd if fd else 0):.4f})")
if race:
    rows = json.load(open(race))
    print("relay-race probe (an honest node holding C instead of its twin, by twin lag):")
    for r in rows:
        print(f"  lag {r['lag']:g} s: A's outbound peer {r['BO']}/{r['n']}, A's inbound peer {r['BI']}/{r['n']} "
              f"(ack lag median {r['ack_median']} s)")
n = len(runs)
ok = failed == 0 and n >= min_runs and farm_ok == n and honest_clustered == 0 and honest_flagged == 0 and proxy_flagged == n
print(f"T-fed gateways clustered {farm_ok}/{n}; honest false clusters {honest_clustered}/{2 * n}; "
      f"honest flagged {honest_flagged}/{2 * n}; proxy flagged {proxy_flagged}/{n}")
print("RESULT", "PASS" if ok else "FAIL")
sys.exit(0 if ok else 1)
PY
