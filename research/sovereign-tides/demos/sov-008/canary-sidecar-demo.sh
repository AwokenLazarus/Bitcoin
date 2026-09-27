#!/usr/bin/env bash
# SPDX-License-Identifier: MIT
# Copyright (c) 2026 Mike Moore (AwokenLazarus)
# SOV-008: the lazarus-canary sidecar (W9) against primed's template-check canary evidence (W8),
# rnd/sov-008, on Knots 29.4.2 regtest: pool node, 2 honest gateways, a proxy, the naive and the
# filtered hybrid. The gateways are real DATUM sessions (gwsim); primed runs in `publish` with
# sovereignty-demo OFF (no HTTP snapshots or checks); the sidecar runs as it would in production,
# with the demo's fast cadence. RUNS runs (default 1), each against a fresh primed and fresh nodes.
# Ports 32000-32099: primed 32015/32016, Knots 32020-32031. Datadirs under ./run. No miners.
# Nothing is pushed.
#
#   ./canary-sidecar-demo.sh                  # 1 x 420 s (+ 55 s drain)
#   RUNS=2 KNOTS_SECS=300 ./canary-sidecar-demo.sh
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
WT=${WT:?set WT to a checkout of this repo with the primed patches applied}
PRIME=$WT/prime
RUN=${SOV008_RUN:-$HERE/run}
RUNS=${RUNS:-1}
KNOTS_SECS=${KNOTS_SECS:-420}
LISTEN_PORT=32015
STATS_PORT=32016
BASE=32020
KNOTS=${KNOTS:?set KNOTS to a Knots v29.4.2.knots20260508 bitcoind}
export PRIME_STATS="http://127.0.0.1:${STATS_PORT}" SOV008_RUN="$RUN" SIDECAR_PY="$WT/lazarus/canary/lazarus_canary.py"

[[ -x "$KNOTS" ]] || { echo "no Knots 29.4.2 at $KNOTS" >&2; exit 1; }
busy=$(ss -Hltn | awk '{print $4}' | sed 's/.*://' | awk '$1>=32000 && $1<=32099' | sort -u | tr '\n' ' ')
[[ -z "$busy" ]] || { echo "ports in 32000-32099 already in use: $busy (a stale run?); refusing to start" >&2; exit 1; }
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
(cd "$PRIME" && nice -n 10 cargo build -j4 --offline -q --release --bin primed)
(cd "$HERE/gwsim" && nice -n 10 cargo build -j4 --offline -q --release)
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
rpc-password = "sov008"
min-diff = 1
fee-bps = 0
house-loopback = false
sovereignty-mode = "publish"
sovereignty-recompute-secs = 5
# production default for the check delay (45 s); the gap is the demo's (production 300 s)
sovereignty-canary-check-gap-secs = 10
headline = "SOV-008 rnd/sov-008"
TOML
  if curl -s -o /dev/null "$PRIME_STATS/healthz"; then
    echo "something already listens on :${STATS_PORT}; refusing to test against it" >&2
    exit 1
  fi
  (cd "$PRIME" && RUST_LOG=info,primed::session=debug exec nice -n 10 ./target/release/primed -c "$dir/prime.toml" run \
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
  if python3 "$HERE/sidecar_knots.py" "$KNOTS" "$KNOTS_SECS" "$BASE" "127.0.0.1:${LISTEN_PORT}" "$RUN/prime-$i" \
      "$RUN/sidecar-$i" "$HERE/gwsim/target/release/gwsim" | tee "$RUN/run-$STAMP-$i.log"; then :; else FAILED=$((FAILED + 1)); fi
  RESULTS+=("$(grep '^result ' "$RUN/run-$STAMP-$i.log" | tail -1 | cut -d" " -f2 || true)")
  echo "primed canary checks sent: $(grep -c 'canary check on job' "$RUN/prime-$i/primed.log" "$RUN/prime-$i/primed.err" 2>/dev/null | awk -F: '{s+=$2} END {print s}')"
  cleanup
done

echo "== summary over $RUNS runs =="
python3 - "$FAILED" "${RESULTS[@]}" <<'PY'
import json, sys
failed, paths = int(sys.argv[1]), [p for p in sys.argv[2:] if p]
runs = [json.load(open(p)) for p in paths]
names = ["knots-h1", "knots-h2", "knots-proxy", "knots-hybrid", "knots-hybrid-filt"]
print("| gateway | flagged runs | checks | own hit rate | shared hit rate | decoys carried | reasons |")
print("|---|---:|---:|---:|---:|---:|---|")
agg = {}
for g in names:
    rows = [r["rows"].get(g) or {} for r in runs]
    fl = sum(bool(x.get("flagged")) for x in rows)
    agg[g] = fl
    def s(k, d):
        return sum(x.get(k) or 0 for x in rows), sum(x.get(d) or 0 for x in rows)
    oh, od = s("canary_hits", "canaries_due"); sh, sd = s("shared_hits", "shared_due"); dc, dd = s("decoys_carried", "decoys_due")
    ck = sum(x.get("checks") or 0 for x in rows)
    reasons = sorted({r for x in rows for r in x.get("flag_reasons") or []})
    print(f"| {g} | {fl}/{len(rows)} | {ck} | {oh}/{od} = {oh/od if od else 0:.2f} | {sh}/{sd} = {sh/sd if sd else 0:.2f} | {dc}/{dd} | {','.join(reasons)} |")
print("own/shared leaks into pool templates:", [r["leaks"] for r in runs],
      "decoys in pool templates:", [f'{r["decoys_in_pool"]}/{r["decoys"]}' for r in runs],
      "decoys held by honest nodes:", [r["honest_decoys"] for r in runs])
print("canary key isolation:", ["OK" if r["key_ok"] else "FAIL" for r in runs])
for r in runs:
    f = r["fees"]
    print(f"fees: {f['rounds']} rounds, placed {f['placed']}, planned {f['fees_planned']} sat ({f['planned_per_round']}/round), "
          f"resolved {f['resolved']} paid {f['fees_paid']} sat by kind {f['paid_by_kind']} (resolved {f['resolved_by_kind']}), own confirmed as {f['own_confirmed_as']}")
n = len(runs)
honest_fp = agg["knots-h1"] + agg["knots-h2"]
ok = (failed == 0 and n >= 1 and honest_fp == 0 and all(r["key_ok"] and r["leaks"] == 0 for r in runs)
      and all(agg[g] == n for g in ("knots-proxy", "knots-hybrid", "knots-hybrid-filt")))
print(f"honest false positives {honest_fp}/{2*n}; proxy {agg['knots-proxy']}/{n}; hybrid {agg['knots-hybrid']}/{n}; "
      f"filtered hybrid {agg['knots-hybrid-filt']}/{n}")
print("RESULT", "PASS" if ok else "FAIL")
sys.exit(0 if ok else 1)
PY
