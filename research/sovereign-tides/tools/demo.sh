#!/usr/bin/env bash
# SPDX-License-Identifier: MIT
# Copyright (c) 2026 Mike Moore (AwokenLazarus)
# Sovereign TIDES (A2) demo: Sovereign TIDES attestations, sovereignty score, entropy detector.
# Read-only against the public explorer; regtest nodes are isolated, loopback-only, on ports
# 30801-30803 (RDTS pair) and 30811-30818 (entropy net), and stopped
# on exit; no miner processes are started. Usage: ./demo.sh [--refetch] [--quick]
set -euo pipefail
cd "$(dirname "$0")"
# PRIME: the prime/ directory of this repo with the s0 series of patches/primed applied (steps 1-2).
# KNOTS: a stock Knots v29.4.2.knots20260508 bitcoind (steps 5-6; bitcoin-cli beside it).
PRIME=${PRIME:?set PRIME=/path/to/Bitcoin/prime (patched)}
KNOTS=${KNOTS:?set KNOTS=/path/to/knots/build/bin/bitcoind}
QUICK=0; REFETCH=0
for a in "$@"; do case $a in --quick) QUICK=1;; --refetch) REFETCH=1;; esac; done

echo "== 1. rnd/a2 unit tests (datum-wire authorship + tides sovereignty) =="
(cd "$PRIME" && git log --oneline -1 && cargo test --offline -q -p datum-wire authorship 2>&1 | grep "test result" \
  && cargo test --offline -q -p tides sovereignty 2>&1 | grep "test result")

echo "== 2. Rust signs an LZT1 attestation; the Python chain verifier checks it and 13 tampers =="
(cd "$PRIME" && cargo run --offline -q -p datum-wire --example lzt1_fixture) > fixture.json
python3 sovereignty.py verify-fixture fixture.json

echo "== 3. Python unit tests =="
python3 test_a2.py 2>&1 | tail -1

echo "== 4. Sovereignty score over real chain data (public explorer, read-only) =="
if [ "$REFETCH" = 1 ] || [ ! -s cache/outputs.json ]; then python3 sovereignty.py fetch --days 7; fi
python3 sovereignty.py score --days 7 --json score-7d.json | tee score-7d.md
python3 sovereignty.py score --days 1 --json score-1d.json > score-1d.md; grep Network score-1d.md

if [ "$QUICK" = 0 ]; then
  echo "== 5. RDTS: largest coinbase OP_RETURN Knots 29.4.2 accepts (isolated regtest pair) =="
  python3 rdts_opreturn_test.py "$KNOTS" 30801 -rdtsexpiry=4102444800 | grep -E "node:|control|coinbase OP_RETURN +(70|83|84) "
  echo "== 6. Template-entropy detector on a 4-node regtest network (180 s) =="
  python3 entropy_regtest.py "$KNOTS" 180 30811 | tail -12
fi

echo "demo OK"
