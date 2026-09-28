#!/usr/bin/env bash
# SPDX-License-Identifier: MIT
# Copyright (c) 2026 Mike Moore (AwokenLazarus)
# SOV-011: Prime finds gateways that cannot carry XBT-NTA attestations before activation,
# refuses them work after it, and reports fleet readiness in /nta.json. Two NTA-patched Knots
# 29.4.2 regtest nodes (nta@ a few hundred blocks ahead), primed, three gateways:
#   gw-A   lazarus-gateway (advertises nta-v1, signs NTA for its miner)
#   gw-C   datum_gateway, FlyTheElephant1 a5f28aa + lazarus/patches/datum-gateway-split-only.patch
#          (advertises nta-v1)
#   stock  lazarus-gateway with LAZARUS_GATEWAY_EMULATE_STOCK_V041=1: reads coinbasers exactly as
#          OCEAN v0.4.1 datum_coinbaser.c:795 (cut at the first script over 64 B), no nta-v1
# Optional (SOV-020): REAL unmodified datum_gateway builds, observed rather than checked. Set any of
#   STOCK_S  the StartOS pin, iohzrd/datum_gateway 7491a50
#   STOCK_I  iohzrd/datum_gateway c031568
#   OCEAN    OCEAN-xyz/datum_gateway 5b06123 (v0.4.1; SHA256d only, so it gets no work on XBT)
# to the binary. See sov011_demo.py for the steps and checks. Prints RESULT PASS|FAIL.
#
# The whole run lives in one user scope capped at 200% CPU and 4 GB, at nice 19; builds use -j2
# (SOV011_SCOPED=1 skips the scope on a host without systemd user sessions). Ports
# SOV011_PORT_BASE (32600) .. +99, loopback only. Datadirs and results under ./run.
# Miners: stratum-grind --threads 2, one at a time, each under `timeout` in its own process group.
set -euo pipefail
if [[ -z "${SOV011_SCOPED:-}" ]]; then
  exec systemd-run --user --scope -q -p CPUQuota=200% -p MemoryMax=4G \
    env SOV011_SCOPED=1 nice -n 19 "$0" "$@"
fi
HERE=$(cd "$(dirname "$0")" && pwd)
WT=${WT:?set WT to a checkout of this repo with the s0 and sov-011 series applied}
KNOTS=${KNOTS:?set KNOTS to an NTA-patched Knots v29.4.2 bitcoind (patches/knots-v29.4.2)}
KNOTS_FUNC=${KNOTS_FUNC:-$(cd "$(dirname "$KNOTS")/../.." && pwd)/test/functional}
FTE_REPO=${FTE_REPO:-https://github.com/FlyTheElephant1/datum_gateway.git}
FTE_COMMIT=a5f28aa873bd241f60402259a5ef93bcd34aa4e1
BASE=${SOV011_PORT_BASE:-32600}
END=$((BASE + 99))
TS=$(date -u +%Y%m%dT%H%M%SZ)
RUN=$HERE/run/demo
RESULTS=$HERE/run/results/$TS
CGW=$HERE/run/cgw
PY_PID=

cleanup() {
  if [[ -n "$PY_PID" ]]; then kill "$PY_PID" 2>/dev/null || true; wait "$PY_PID" 2>/dev/null || true; fi
  pkill -f -- "--config $RUN/" 2>/dev/null || true
  pkill -f -- "-c $RUN/" 2>/dev/null || true
  pkill -f -- "-datadir=$RUN/knots-" 2>/dev/null || true
  pkill -f -- "stratum-grind --host 127.0.0.1 --port $((BASE / 100))" 2>/dev/null || true
}
trap cleanup EXIT
trap 'exit 130' INT TERM HUP

busy=$(ss -Hltn | awk '{print $4}' | sed 's/.*://' | awk -v lo="$BASE" -v hi="$END" '$1>=lo && $1<=hi' | sort -u | tr '\n' ' ')
[[ -z "$busy" ]] || { echo "ports in ${BASE}-${END} already in use: $busy (a stale run?); refusing to start" >&2; exit 1; }
[[ -x $KNOTS ]] || { echo "no bitcoind at $KNOTS" >&2; exit 1; }
[[ -f $KNOTS_FUNC/test_framework/key.py ]] || { echo "no Knots test/functional at $KNOTS_FUNC (set KNOTS_FUNC)" >&2; exit 1; }
python3 -c "import cryptography" 2>/dev/null || { echo "needs the Python cryptography package (apt install python3-cryptography)" >&2; exit 1; }

echo "== build $(git -C "$WT" rev-parse --abbrev-ref HEAD) @ $(git -C "$WT" rev-parse --short HEAD) (primed, stratum-grind, lazarus-gateway)"
(cd "$WT/prime" && cargo build -j2 --offline --release -q -p primed)
(cd "$WT/lazarus" && cargo build -j2 --offline --release -q -p lazarus-gateway)

# The split-only C gateway: FTE a5f28aa + the patch from $WT, rebuilt when the patch changes.
PATCH=$WT/lazarus/patches/datum-gateway-split-only.patch
stamp=$(sha256sum "$PATCH" | cut -c1-16)
if [[ "$(cat "$CGW/.stamp" 2>/dev/null)" != "$stamp" ]]; then
  echo "== build datum_gateway (FTE ${FTE_COMMIT:0:7} + split-only patch $stamp)"
  if [[ ! -d $CGW/.git ]]; then rm -rf "$CGW" && git clone -q "$FTE_REPO" "$CGW"; fi
  git -C "$CGW" reset -q --hard && git -C "$CGW" clean -qfdx
  git -C "$CGW" -c advice.detachedHead=false checkout -q "$FTE_COMMIT"
  (cd "$CGW" && patch -p1 -s < "$PATCH" && cmake . >/dev/null && make -j2 -s >/dev/null 2>&1)
  (cd "$CGW" && ./datum_gateway --test >/dev/null 2>&1) || { echo "datum_gateway --test failed" >&2; exit 1; }
  echo "$stamp" > "$CGW/.stamp"
fi

rm -rf "$RUN" && mkdir -p "$RUN" "$RESULTS"
real=
for pair in "stock-S:${STOCK_S:-}:startos" "stock-I:${STOCK_I:-}:iohzrd" "ocean:${OCEAN:-}:ocean"; do
  IFS=: read -r name bin lineage <<<"$pair"
  [[ -z $bin ]] && continue
  [[ -x $bin ]] || { echo "$name: no binary at $bin" >&2; exit 1; }
  real+="${real:+, }\"$name\": [\"$bin\", \"$lineage\"]"
done
cat > "$RESULTS/cfg.json" <<JSON
{"bitcoind": "$KNOTS", "func": "$KNOTS_FUNC", "run": "$RUN", "results": "$RESULTS", "base": $BASE,
 "primed": "$WT/prime/target/release/primed", "grind": "$WT/prime/target/release/stratum-grind",
 "gateway": "$WT/lazarus/target/release/lazarus-gateway", "cgw": "$CGW/datum_gateway",
 "real": {$real}}
JSON
{
  echo "wt $(git -C "$WT" rev-parse HEAD)"
  echo "knots $("$KNOTS" -version | head -1)"
  echo "cgw fte $FTE_COMMIT + split-only patch sha256 $stamp"
  for b in "${STOCK_S:-}" "${STOCK_I:-}" "${OCEAN:-}"; do [[ -n $b ]] && echo "$b sha256 $(sha256sum "$b" | cut -c1-16)"; done
} > "$RESULTS/versions.txt"

set +e
python3 -u "$HERE/sov011_demo.py" "$RESULTS/cfg.json" > >(tee "$RESULTS/demo.log") 2>&1 &
PY_PID=$!
wait "$PY_PID"
rc=$?
PY_PID=
set -e
ln -sfn "$TS" "$HERE/run/results/latest"
[[ $rc -eq 0 ]] || { echo "demo FAILED (rc=$rc): $RESULTS" >&2; exit "$rc"; }
echo "demo OK: $RESULTS"
