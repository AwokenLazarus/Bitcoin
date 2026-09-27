#!/usr/bin/env bash
# SPDX-License-Identifier: MIT
# Copyright (c) 2026 Mike Moore (AwokenLazarus)
# SOV-004: the hybrid proxy (pool template + its own node's mempool) against primed's canary
# detector with own, shared and decoy canaries (branch rnd/sov-004), on Knots 29.4.2 regtest.
# Honest x2, proxy, mimic and three hybrids. RUNS runs (default 3), each against a fresh primed.
# Ports 31700-31799: primed 31715/31716, Knots 31720-31729. Datadirs under ./run. No miners.
# Nothing is pushed.
#
#   ./hybrid-canary-demo.sh              # 2 x 300 s
#   RUNS=5 KNOTS_SECS=180 ./hybrid-canary-demo.sh
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
PRIME=${PRIME:?set PRIME to the prime/ directory of a checkout with the s0 series of patches/primed applied}
RUN=${SOV004_RUN:-$HERE/run}
RUNS=${RUNS:-2}
KNOTS_SECS=${KNOTS_SECS:-300}
STATS_PORT=31716
LISTEN_PORT=31715
KNOTS=${KNOTS:?set KNOTS to a Knots v29.4.2.knots20260508 bitcoind}
export PRIME_STATS="http://127.0.0.1:${STATS_PORT}" SOV004_RUN="$RUN"

[[ -x "$KNOTS" ]] || { echo "no Knots 29.4.2 at $KNOTS" >&2; exit 1; }
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

echo "== build primed ($(git -C "$PRIME" rev-parse --abbrev-ref HEAD) $(git -C "$PRIME" rev-parse --short HEAD)) =="
(cd "$PRIME" && nice -n 10 cargo build -j4 --offline -q --release --bin primed)

start_primed() {
  local dir="$RUN/prime-$1"
  rm -rf "$dir" && mkdir -p "$dir"
  cat > "$dir/prime.toml" <<EOF
listen = "127.0.0.1:${LISTEN_PORT}"
stats-listen = "127.0.0.1:${STATS_PORT}"
data-dir = "$dir"
payout-address = "bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4"
network = "mainnet"
rpc = "http://127.0.0.1:31798"
poll = 60
min-diff = 1
fee-bps = 0
stratum-fee-bps = 2500
datum-rebate-bps = 1000
house-loopback = false
sovereignty-mode = "publish"
sovereignty-bonus = true
sovereignty-demo = true
sovereignty-bonus-bps = 500
sovereignty-min-key-age-secs = 0
headline = "SOV-004 rnd/sov-004"
EOF
  if curl -s -o /dev/null "$PRIME_STATS/healthz"; then
    echo "something already listens on :${STATS_PORT}; refusing to test against it" >&2
    exit 1
  fi
  (cd "$PRIME" && RUST_LOG=info exec nice -n 10 ./target/release/primed -c "$dir/prime.toml" run \
    >"$dir/primed.log" 2>"$dir/primed.err") &
  PRIMED_PID=$!; for _ in $(seq 1 50); do [[ -s "$dir/sovereignty.token" ]] && break; sleep 0.1; done; export SOV_TOKEN=$(cat "$dir/sovereignty.token")
  for _ in $(seq 1 50); do
    curl -sf "$PRIME_STATS/healthz" >/dev/null && return 0
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
  if python3 "$HERE/hybrid_knots.py" "$KNOTS" "$KNOTS_SECS" 31720 | tee "$RUN/run-$STAMP-$i.log"; then :; else FAILED=$((FAILED + 1)); fi
  RESULTS+=("$(grep '^result ' "$RUN/run-$STAMP-$i.log" | tail -1 | cut -d" " -f2 || true)")
  cleanup
done

echo "== summary over $RUNS runs =="
python3 - "$FAILED" "${RESULTS[@]}" <<'EOF'
import json, sys
failed, paths = int(sys.argv[1]), [p for p in sys.argv[2:] if p]
runs = [json.load(open(p)) for p in paths]
names = ["knots-h1", "knots-h2", "knots-hybrid-forced", "knots-proxy", "knots-mimic", "knots-hybrid", "knots-hybrid-filt"]
print("| gateway | flagged runs | own hit rate | shared hit rate | decoys carried | reasons |")
print("|---|---:|---:|---:|---:|---|")
agg = {}
for g in names:
    rows = [r["rows"].get(g, {}) for r in runs]
    fl = sum(bool(x.get("flagged")) for x in rows)
    agg[g] = fl
    def s(k, d):
        return sum(x.get(k) or 0 for x in rows), sum(x.get(d) or 0 for x in rows)
    oh, od = s("canary_hits", "canaries_due"); sh, sd = s("shared_hits", "shared_due"); dc, dd = s("decoys_carried", "decoys_due")
    reasons = sorted({r for x in rows for r in x.get("flag_reasons") or []})
    print(f"| {g} | {fl}/{len(rows)} | {oh}/{od} = {oh/od if od else 0:.2f} | {sh}/{sd} = {sh/sd if sd else 0:.2f} | {dc}/{dd} | {','.join(reasons)} |")
print("forced hybrid: share of its txs its own node templated within 6 s:", [r["forced_in_own"] for r in runs],
      "share of its txs from a pool template its own node never templated:", [r["forced_pool_only"] for r in runs])
print("own/shared leaks into pool templates:", [r["leaks"] for r in runs],
      "decoys in pool templates:", [f'{r["decoys_in_pool"]}/{r["decoys"]}' for r in runs],
      "decoys held by honest nodes:", [r["honest_decoys"] for r in runs])
n = len(runs)
honest_fp = agg["knots-h1"] + agg["knots-h2"]
ok = (failed == 0 and n >= 1 and honest_fp == 0 and agg["knots-hybrid-forced"] == 0
      and all(agg[g] == n for g in ("knots-proxy", "knots-mimic", "knots-hybrid", "knots-hybrid-filt")))
print(f"honest false positives {honest_fp}/{2*n}; hybrid detection {agg['knots-hybrid']}/{n}; "
      f"filtered hybrid {agg['knots-hybrid-filt']}/{n}; proxy {agg['knots-proxy']}/{n}; mimic {agg['knots-mimic']}/{n}")
print("RESULT", "PASS" if ok else "FAIL")
sys.exit(0 if ok else 1)
EOF
