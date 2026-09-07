#!/bin/bash
# Prove Prime's empty-solo script-flip against a real stock DATUM gateway on regtest.
#
#   scripts/regtest-empty-solo.sh [convoy|fte|iohzrd]
#
# Four phases against one gateway identity:
#   1. learn      — first shares persist gateway-scripts.json
#   2. empty-solo — delayed coinbaser, 0-tx job pays the gateway; not in the window
#   3. full-solo  — delayed coinbaser on a mempool template (nTx>=2); GatewaySolo
#   4. split      — coinbaser on time; stay on pool; TIDES payees; lifetime_work grows
#
# Needs the same deps as regtest-e2e.sh plus ~/lazarus-regtest/cpu-miner.py (or MINER_CMD).
set -euo pipefail

LINEAGE="${1:-convoy}"
case "$LINEAGE" in
  convoy) GW_REPO=https://github.com/CONVOYMining/datum_gateway; GW_REF=master ;;
  fte)    GW_REPO=https://github.com/FlyTheElephant1/datum_gateway; GW_REF=test/console-collapse-pr14-pr17 ;;
  iohzrd) GW_REPO=https://github.com/iohzrd/datum_gateway; GW_REF=master ;;
  *) echo "usage: $0 convoy|fte|iohzrd" >&2; exit 2 ;;
esac

HERE=$(cd "$(dirname "$0")/.." && pwd)
RPC_URL="${RPC_URL:-http://127.0.0.1:18443}"
RPC_COOKIE="${RPC_COOKIE:-$HOME/lazarus-regtest/data/regtest/.cookie}"
BITCOIN_CLI="${BITCOIN_CLI:-$HOME/lazarus-regtest/prefix/bin/bitcoin-cli -regtest -datadir=$HOME/lazarus-regtest/data -conf=$HOME/lazarus-regtest/etc/bitcoin.conf}"
WORKDIR="${WORKDIR:-/tmp/primed-empty-solo}"
PRIME_PORT="${PRIME_PORT:-19915}"
STATS_PORT="${STATS_PORT:-19916}"
STRATUM_PORT="${STRATUM_PORT:-19334}"
GW_API_PORT="${GW_API_PORT:-19152}"
# CPU grind is ~40 MH/s. Diff-1 is 2^32 hashes, so 180s still misses ~16%
# of the time — enough to flake iohzrd after a clean Fly/Convoy pass.
LEARN_SECS="${LEARN_SECS:-360}"
SOLO_SECS="${SOLO_SECS:-360}"
SPLIT_SECS="${SPLIT_SECS:-360}"
FULL_SECS="${FULL_SECS:-360}"
# Diff-1 is ~2^32 hashes. The 2080 is usually busy (Laz); the CPU stand-in
# needs a long late-coinbaser window so it can actually land a share on it.
# Must outlast SOLO_SECS/FULL_SECS: if the delayed reply lands first, stock
# publishes a split and the grind finds that instead of the gateway-paid job.
SOLO_DELAY_MS="${SOLO_DELAY_MS:-300000}"
CPU_MINER="${CPU_MINER:-$HOME/lazarus-regtest/cpu-miner.py}"
GPU_MINER="${GPU_MINER:-$HOME/Bitcoin/miner/miner.py}"

mkdir -p "$WORKDIR"
# Drop a leftover listener from a previous run of this script (ports, not a name match).
fuser -k "$PRIME_PORT/tcp" "$STATS_PORT/tcp" "$STRATUM_PORT/tcp" "$GW_API_PORT/tcp" >/dev/null 2>&1 || true
GW_DIR="$WORKDIR/gw-$LINEAGE"
DATA="$WORKDIR/data"
# A leftover ledger/persist from a previous run would mix identities and window work.
rm -rf "$DATA" "$WORKDIR/phases"
mkdir -p "$DATA" "$WORKDIR/phases"
PIDS=()
MINER_PID=""
PRIME_PID=""
GW_PID=""
cleanup() {
  [ -n "${MINER_PID:-}" ] && kill "$MINER_PID" 2>/dev/null || true
  [ -n "${PRIME_PID:-}" ] && kill "$PRIME_PID" 2>/dev/null || true
  [ -n "${GW_PID:-}" ] && kill "$GW_PID" 2>/dev/null || true
  for p in "${PIDS[@]:-}"; do [ -n "$p" ] && kill "$p" 2>/dev/null || true; done
}
trap cleanup EXIT

say() { printf '\n== %s\n' "$*"; }
fail() { echo "FAIL: $*" >&2; exit 1; }

stats() { curl -fs "http://127.0.0.1:$STATS_PORT/stats.json"; }

wait_stats() {
  local secs="$1" py="$2"
  local i S
  for i in $(seq 1 "$secs"); do
    S=$(stats 2>/dev/null || echo '{}')
    if echo "$S" | python3 -c "$py"; then
      echo "$S"
      return 0
    fi
    sleep 1
  done
  echo "$S"
  return 1
}

say "node"
[ -r "$RPC_COOKIE" ] || fail "no RPC cookie at $RPC_COOKIE (is regtest Knots running?)"
INFO=$($BITCOIN_CLI getblockchaininfo) || fail "bitcoin-cli cannot reach the node"
echo "$INFO" | python3 -c 'import json,sys; d=json.load(sys.stdin); print("chain", d["chain"], "height", d["blocks"])'
$BITCOIN_CLI -rpcwallet=empty-solo getwalletinfo >/dev/null 2>&1 \
  || $BITCOIN_CLI createwallet empty-solo >/dev/null 2>&1 \
  || $BITCOIN_CLI loadwallet empty-solo >/dev/null
PAYOUT="${PAYOUT:-$($BITCOIN_CLI -rpcwallet=empty-solo getnewaddress "pool" bech32)}"
GATEWAY="${GATEWAY:-$($BITCOIN_CLI -rpcwallet=empty-solo getnewaddress "gateway" bech32)}"
echo "pool    $PAYOUT"
echo "gateway $GATEWAY"

say "build primed"
(cd "$HERE" && cargo build --release -q -p primed --bins)
PRIMED="$HERE/target/release/primed"

say "build $LINEAGE datum_gateway ($GW_REPO @ $GW_REF)"
if [ ! -d "$GW_DIR/.git" ]; then
  git clone -q --depth 1 --branch "$GW_REF" "$GW_REPO" "$GW_DIR"
fi
if [ ! -x "$GW_DIR/build/datum_gateway" ]; then
  (cd "$GW_DIR" && cmake -S . -B build -DCMAKE_BUILD_TYPE=Release >/dev/null && cmake --build build -j"$(nproc)" >/dev/null)
fi
GW="$GW_DIR/build/datum_gateway"
[ -x "$GW" ] || fail "gateway did not build"

write_prime_toml() {
  local delay="$1"
  mkdir -p "$DATA"
  cat > "$WORKDIR/prime.toml" <<EOF
listen = "127.0.0.1:$PRIME_PORT"
stats-listen = "127.0.0.1:$STATS_PORT"
advertise-address = "127.0.0.1:$PRIME_PORT"
data-dir = "$DATA"
motd = "primed empty-solo"
min-diff = 1
payout-address = "$PAYOUT"
coinbase-tag = "Lazarus"
prime-id = 7
window = 8
window-min-work = 1
min-payout = 546
fee-bps = 50
empty-solo-fee-bps = 250
coinbaser-delay-ms = $delay
network = "regtest"
rpc = "$RPC_URL"
rpc-cookie = "$RPC_COOKIE"
poll = 0.5
EOF
}

start_primed() {
  local delay="$1"
  write_prime_toml "$delay"
  [ -n "${PRIME_PID:-}" ] && kill "$PRIME_PID" 2>/dev/null || true
  sleep 0.4
  RUST_LOG="${RUST_LOG:-info,primed::session=debug}" "$PRIMED" -c "$WORKDIR/prime.toml" run \
    > "$WORKDIR/primed.log" 2>&1 &
  PRIME_PID=$!
  for _ in $(seq 1 20); do
    curl -fs "http://127.0.0.1:$STATS_PORT/healthz" >/dev/null 2>&1 && return 0
    sleep 0.25
  done
  fail "primed did not come up: $(tail -8 "$WORKDIR/primed.log")"
}

start_gateway() {
  local pubkey
  pubkey=$("$PRIMED" -c "$WORKDIR/prime.toml" pubkey)
  EXTRA_MINING=""
  if [ "$LINEAGE" = fte ] || [ "$LINEAGE" = iohzrd ]; then
    ACT=$(grep -o 'testactivationheight=blake2b@[0-9]*' "$HOME/lazarus-regtest/etc/bitcoin.conf" 2>/dev/null | cut -d@ -f2 || true)
    EXTRA_MINING=", \"blake2b_activation_height\": ${ACT:-101}, \"blake2b_headline\": \"Lazarus\""
  fi
  cat > "$WORKDIR/gw.json" <<EOF
{
 "bitcoind": { "rpccookiefile": "$RPC_COOKIE", "rpcurl": "$RPC_URL", "work_update_seconds": 8, "notify_fallback": true },
 "stratum": { "listen_addr": "127.0.0.1", "listen_port": $STRATUM_PORT, "vardiff_min": 1 },
 "mining": { "pool_address": "bc1qt5praystcdle0nq04e3h02yjszha82uzhww85x6972lcy40k4eyqz9jfaq",
             "coinbase_tag_primary": "Lazarus", "coinbase_tag_secondary": "empty-solo-$LINEAGE"$EXTRA_MINING },
 "api": { "listen_port": $GW_API_PORT, "admin_password": "" },
 "logger": { "log_to_console": true, "log_to_file": false, "log_level_console": 0 },
 "datum": { "pool_host": "127.0.0.1", "pool_port": $PRIME_PORT, "pool_pubkey": "$pubkey",
            "pool_pass_workers": true, "pool_pass_full_users": true, "pooled_mining_only": true,
            "protocol_global_timeout": 60 }
}
EOF
  [ -n "${GW_PID:-}" ] && kill "$GW_PID" 2>/dev/null || true
  sleep 0.3
  "$GW" -c "$WORKDIR/gw.json" > "$WORKDIR/gw.log" 2>&1 &
  GW_PID=$!
}

start_miner() {
  [ -n "${MINER_PID:-}" ] && kill "$MINER_PID" 2>/dev/null || true
  local grind="$HERE/target/release/stratum-grind"
  # extra args (e.g. --skip-empty) go to stratum-grind only
  local extra=()
  if [ -n "${1:-}" ]; then
    extra+=("$@")
  fi
  if [ -n "${MINER_CMD:-}" ]; then
    bash -c "$MINER_CMD" > "$WORKDIR/miner.log" 2>&1 &
  elif [ -x "$grind" ]; then
    "$grind" --host 127.0.0.1 --port "$STRATUM_PORT" --user "$GATEWAY" "${extra[@]}" \
      > "$WORKDIR/miner.log" 2>&1 &
  elif [ "${USE_GPU:-0}" = 1 ] && [ -f "$GPU_MINER" ]; then
    python3 "$GPU_MINER" --host 127.0.0.1 --port "$STRATUM_PORT" --user "$GATEWAY" \
      > "$WORKDIR/miner.log" 2>&1 &
  else
    [ -f "$CPU_MINER" ] || fail "no cpu miner at $CPU_MINER (set MINER_CMD)"
    python3 "$CPU_MINER" --host 127.0.0.1 --port "$STRATUM_PORT" --user "$GATEWAY" \
      > "$WORKDIR/miner.log" 2>&1 &
  fi
  MINER_PID=$!
}

stop_miner() { [ -n "${MINER_PID:-}" ] && kill "$MINER_PID" 2>/dev/null || true; MINER_PID=""; }

wait_client() {
  say "waiting for gateway session"
  if ! wait_stats 60 '
import json,sys
d=json.load(sys.stdin)
c=d.get("clients",[]); t=d.get("totals",{})
live=[x for x in c if not x.get("offline")]
sys.exit(0 if live and t.get("handshake_failures",0)==0 else 1)
'; then
    tail -20 "$WORKDIR/gw.log" >&2 || true
    tail -20 "$WORKDIR/primed.log" >&2 || true
    fail "gateway never completed handshake"
  fi
  echo "gateway connected"
}

wait_handshake() {
  wait_client
  say "waiting for coinbaser"
  if ! wait_stats 60 '
import json,sys
d=json.load(sys.stdin)
t=d.get("totals",{})
sys.exit(0 if t.get("coinbasers",0)>=1 else 1)
'; then
    fail "gateway never received a coinbaser reply"
  fi
  echo "coinbaser ok"
}

snapshot_phase() {
  local name="$1"
  mkdir -p "$WORKDIR/phases/$name"
  cp -f "$WORKDIR/primed.log" "$WORKDIR/phases/$name/primed.log" 2>/dev/null || true
  cp -f "$WORKDIR/miner.log" "$WORKDIR/phases/$name/miner.log" 2>/dev/null || true
  cp -f "$WORKDIR/gw.log" "$WORKDIR/phases/$name/gw.log" 2>/dev/null || true
  stats > "$WORKDIR/phases/$name/stats.json" 2>/dev/null || echo '{}' > "$WORKDIR/phases/$name/stats.json"
}

block_ntx() {
  $BITCOIN_CLI getblock "$1" 1 | python3 -c 'import json,sys; print(json.load(sys.stdin)["nTx"])'
}

mature_wallet() {
  local trusted
  trusted=$($BITCOIN_CLI -rpcwallet=empty-solo getbalances | python3 -c 'import json,sys; print(json.load(sys.stdin)["mine"]["trusted"])')
  python3 -c "import sys; sys.exit(0 if float('$trusted')>=1 else 1)" && return 0
  say "maturing wallet coinbases (node generate, gateway is not up)"
  local fund
  fund=$($BITCOIN_CLI -rpcwallet=empty-solo getnewaddress "fund" bech32)
  $BITCOIN_CLI generatetoaddress 101 "$fund" >/dev/null
}

fund_mempool() {
  local dest="${1:-$GATEWAY}"
  say "funding mempool so the next job is a full template"
  $BITCOIN_CLI -rpcwallet=empty-solo sendtoaddress "$dest" 0.01 >/dev/null
  local i
  for i in $(seq 1 20); do
    local n
    n=$($BITCOIN_CLI getmempoolinfo | python3 -c 'import json,sys; print(json.load(sys.stdin)["size"])')
    [ "${n:-0}" -ge 1 ] && { echo "mempool size=$n"; return 0; }
    sleep 0.25
  done
  fail "sendtoaddress did not land in the mempool"
}

wait_full_template() {
  local mark secs="${1:-40}"
  mark=$(wc -l < "$WORKDIR/gw.log")
  say "waiting for gateway GBT with txn_count>=1"
  local i
  for i in $(seq 1 "$secs"); do
    if tail -n +"$mark" "$WORKDIR/gw.log" | grep -qE 'txn_count: [1-9]'; then
      tail -n +"$mark" "$WORKDIR/gw.log" | grep -E 'txn_count: [1-9]' | tail -1
      return 0
    fi
    sleep 1
  done
  tail -20 "$WORKDIR/gw.log" >&2 || true
  fail "gateway never published a full template in ${secs}s"
}

# --- phase 1: learn ------------------------------------------------------------------
mature_wallet
say "phase 1: learn gateway identity"
start_primed 0
"$PRIMED" -c "$WORKDIR/prime.toml" check
start_gateway
wait_handshake
start_miner
if ! wait_stats "$LEARN_SECS" '
import json,sys
d=json.load(sys.stdin)
t=d.get("totals",{})
sys.exit(0 if t.get("shares_accepted",0)>=1 else 1)
'; then
  tail -30 "$WORKDIR/miner.log" >&2 || true
  tail -30 "$WORKDIR/primed.log" >&2 || true
  fail "phase 1: no shares accepted in ${LEARN_SECS}s"
fi
stop_miner
# identity is persisted on the first share; give the write a moment
sleep 0.3
[ -s "$DATA/gateway-scripts.json" ] || fail "phase 1: gateway-scripts.json was not written"
python3 - <<PY
import json
m=json.load(open("$DATA/gateway-scripts.json"))
assert m, "empty persist"
ident=next(iter(m.values()))["identity"]
print("persisted", ident)
assert ident.lower()=="$GATEWAY".lower(), (ident, "$GATEWAY")
PY
LEARN=$(stats)
echo "$LEARN" | python3 -c '
import json,sys
d=json.load(sys.stdin); t=d["totals"]; w=d["window"]
print("phase1 accepted=%s window_work=%s solo_empty=%s solo_full=%s" % (
    t["shares_accepted"], w["work"], t.get("solo_empty_shares",0), t.get("solo_full_shares",0)))
'
LEARN_WORK=$(echo "$LEARN" | python3 -c 'import json,sys; print(json.load(sys.stdin)["window"]["work"])')
snapshot_phase 1-learn

# --- phase 2: empty-solo (0-tx, late coinbaser) --------------------------------------
say "phase 2: delayed coinbaser — empty 0-tx job must pay the gateway"
# restart primed so it loads persist and sends configure(gateway) at handshake
start_primed "$SOLO_DELAY_MS"
# gateway reconnects on its own. Do not wait for the delayed coinbaser — the
# miner has to be hashing the empty/late job *during* that window.
sleep 2
wait_client
grep -q 'configure gateway' "$WORKDIR/primed.log" \
  || fail "phase 2: primed never sent configure(gateway); see $WORKDIR/primed.log"
start_miner
if ! wait_stats "$SOLO_SECS" '
import json,sys
d=json.load(sys.stdin)
t=d.get("totals",{})
solo=t.get("solo_empty_shares",0)+t.get("solo_full_shares",0)
kinds=[b.get("kind") for b in d.get("blocks",[])]
sys.exit(0 if solo>=1 or "solo" in kinds else 1)
'; then
  tail -40 "$WORKDIR/primed.log" >&2 || true
  echo "$(stats)" >&2
  fail "phase 2: no empty-solo / gateway-solo share in ${SOLO_SECS}s"
fi
stop_miner
SOLO=$(stats)
echo "$SOLO" | python3 -c '
import json,sys
d=json.load(sys.stdin); t=d["totals"]; w=d["window"]
print("phase2 accepted=%s window_work=%s solo_empty=%s solo_full=%s" % (
    t["shares_accepted"], w["work"], t.get("solo_empty_shares",0), t.get("solo_full_shares",0)))
solos=[b for b in d.get("blocks",[]) if b.get("kind")=="solo"]
assert t.get("solo_empty_shares",0)+t.get("solo_full_shares",0) >= 1, "no solo shares counted"
assert solos, "no kind=solo block record (regtest every share is a candidate)"
for b in solos:
    print("solo block height=%s hash=%s owed=%s pool_sats=%s submit=%s" % (b.get("height"), b.get("hash"), b.get("owed_sats"), b.get("pool_sats"), b.get("submit")))
    assert int(b.get("owed_sats") or 0)==0, b
print("PASS phase 2: empty-solo shares accepted, owed_sats=0")
print(solos[0]["hash"])
' | tee /tmp/phase2-out.txt
EMPTY_HASH=$(tail -1 /tmp/phase2-out.txt)
echo "$EMPTY_HASH" > "$WORKDIR/phases/2-empty.hash"
# window must not have grown from the solo work (phase 1 work may still be there)
SOLO_WORK=$(echo "$SOLO" | python3 -c 'import json,sys; print(json.load(sys.stdin)["window"]["work"])')
python3 - <<PY
learn, solo = int("$LEARN_WORK"), int("$SOLO_WORK")
# a reconnect starts a new primed process; the ledger file keeps phase-1 work.
# solo shares must not have added to it.
assert solo == learn, "window grew during solo phase: learn=%s solo=%s" % (learn, solo)
print("window work unchanged through empty-solo phase:", solo)
PY
EMPTY_HASH=$(cat "$WORKDIR/phases/2-empty.hash")
EMPTY_NTX=$(block_ntx "$EMPTY_HASH")
echo "empty-solo nTx=$EMPTY_NTX hash=$EMPTY_HASH"
[ "$EMPTY_NTX" = 1 ] || fail "phase 2: expected a 0-tx empty-solo block (nTx=1), got nTx=$EMPTY_NTX"
grep -q 'configure gateway' "$WORKDIR/primed.log" \
  || fail "phase 2: no mid-session configure(gateway) in primed.log"
snapshot_phase 2-empty

# --- phase 3: full-solo (mempool template, late coinbaser) ---------------------------
say "phase 3: delayed coinbaser on a FULL template must pay the gateway"
fund_mempool
start_primed "$SOLO_DELAY_MS"
sleep 2
wait_client
grep -q 'configure gateway' "$WORKDIR/primed.log" \
  || fail "phase 3: primed never sent configure(gateway)"
wait_full_template 40
# Hash type-00 (gateway-paid, subsidy_only=false) on the full job. Skip N…ff:
# stock marks those subsidy_only and Prime rejects a full+subsidy_only coinbase.
start_miner --skip-n
FULL=""
for _ in $(seq 1 "$FULL_SECS"); do
  S=$(stats 2>/dev/null || echo '{}')
  HASH=""
  FULL_N=$(echo "$S" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("totals",{}).get("solo_full_shares",0))')
  while read -r h; do
    [ -z "$h" ] && continue
    NTX=$(block_ntx "$h" 2>/dev/null || echo 0)
    if [ "${NTX:-0}" -ge 2 ] && [ "${FULL_N:-0}" -ge 1 ]; then
      HASH="$h"
      echo "full-solo candidate nTx=$NTX solo_full=$FULL_N"
      break
    fi
  done < <(echo "$S" | python3 -c '
import json,sys
d=json.load(sys.stdin)
for b in d.get("blocks",[]):
    if b.get("kind")=="solo" and int(b.get("owed_sats") or 0)==0 and b.get("hash"):
        print(b["hash"])
')
  if [ -n "$HASH" ]; then
    FULL="$S"
    echo "$HASH" > "$WORKDIR/phases/3-full.hash"
    break
  fi
  sleep 1
done
[ -n "$FULL" ] || { echo "$(stats)" >&2; tail -40 "$WORKDIR/primed.log" >&2; fail "phase 3: no GatewaySolo find with nTx>=2 in ${FULL_SECS}s"; }
stop_miner
echo "$FULL" | python3 -c '
import json,sys
d=json.load(sys.stdin); t=d["totals"]; w=d["window"]
print("phase3 accepted=%s window_work=%s solo_empty=%s solo_full=%s" % (
    t["shares_accepted"], w["work"], t.get("solo_empty_shares",0), t.get("solo_full_shares",0)))
solos=[b for b in d.get("blocks",[]) if b.get("kind")=="solo"]
assert t.get("solo_full_shares",0) >= 1, "solo_full_shares stayed 0"
for b in solos:
    print("full-solo block height=%s hash=%s owed=%s pool_sats=%s" % (b.get("height"), b.get("hash"), b.get("owed_sats"), b.get("pool_sats")))
    assert int(b.get("owed_sats") or 0)==0, b
print("PASS phase 3: GatewaySolo shares, owed_sats=0")
'
FULL_WORK=$(echo "$FULL" | python3 -c 'import json,sys; print(json.load(sys.stdin)["window"]["work"])')
python3 - <<PY
learn, full = int("$LEARN_WORK"), int("$FULL_WORK")
assert full == learn, "window grew during full-solo: learn=%s full=%s" % (learn, full)
print("window work unchanged through full-solo phase:", full)
PY
FULL_HASH=$(cat "$WORKDIR/phases/3-full.hash")
FULL_NTX=$(block_ntx "$FULL_HASH")
echo "full-solo nTx=$FULL_NTX hash=$FULL_HASH"
[ "$FULL_NTX" -ge 2 ] || fail "phase 3: expected a full block (nTx>=2), got nTx=$FULL_NTX"
snapshot_phase 3-full

# --- phase 4: split ------------------------------------------------------------------
say "phase 4: coinbaser on time — shares credit the window, finds are split"
start_primed 0
sleep 2
wait_handshake
# Stock's class 00 / N…ff jobs pay only the pool (section 0) even after a
# coinbaser. Skip those so the first find is a type-2+ TIDES split.
start_miner --skip-empty
if ! wait_stats "$SPLIT_SECS" '
import json,sys
d=json.load(sys.stdin)
# Phase-1 split (empty window) has split=[]. A real TIDES find carries payees.
# Window work may not grow once target_work=1 is already full.
paid=any(b.get("kind")=="split" and b.get("split") for b in d.get("blocks",[]))
life=d.get("totals",{}).get("lifetime_work",0)
sys.exit(0 if paid and life > int("'"$LEARN_WORK"'") else 1)
'; then
  echo "$(stats)" >&2
  tail -30 "$WORKDIR/primed.log" >&2 || true
  fail "phase 4: no split find / window credit in ${SPLIT_SECS}s"
fi
stop_miner
echo "$(stats)" | python3 -c '
import json,sys
d=json.load(sys.stdin); t=d["totals"]; w=d["window"]
print("phase4 accepted=%s window_work=%s lifetime_work=%s splits=%s" % (
    t["shares_accepted"], w["work"], t["lifetime_work"],
    sum(1 for b in d.get("blocks",[]) if b.get("kind")=="split")))
assert any(b.get("kind")=="split" and b.get("split") for b in d.get("blocks",[])), "no TIDES split find"
assert t["lifetime_work"] > int("'"$LEARN_WORK"'"), "split share was not credited"
print("PASS phase 4: window credited, split find recorded")
'
grep -q 'configure pool' "$WORKDIR/primed.log" \
  || fail "phase 4: no mid-session configure(pool) before coinbaser"
# After the first configure(pool) on a tip they must stay on pool. The old
# bug restored configure(gateway) ~1s after every coinbaser.
python3 - <<PY || fail "phase 4: flipped back to solo after coinbaser (see primed.log)"
import re
from datetime import datetime
log = open("$WORKDIR/primed.log")
ts_re = re.compile(r"^\[(\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z)")
last_pool = None
n_pool = 0
for line in log:
    m = ts_re.match(line)
    if not m:
        continue
    t = datetime.strptime(m.group(1), "%Y-%m-%dT%H:%M:%SZ")
    if "tip height=" in line or "BLOCK CANDIDATE" in line:
        last_pool = None
    elif "configure pool" in line:
        last_pool = t
        n_pool += 1
    elif "configure gateway" in line and last_pool is not None:
        dt = (t - last_pool).total_seconds()
        raise SystemExit("configure(gateway) %.0fs after configure(pool) on the same tip" % dt)
if n_pool < 1:
    raise SystemExit("no configure(pool)")
print("PASS: stayed on pool after coinbaser until next tip (%d configure pool)" % n_pool)
PY
snapshot_phase 4-split

echo
echo "PASS: empty-solo script-flip against stock $LINEAGE gateway"
echo "  empty 0-tx → gateway, not in window, kind=solo owed=0"
echo "  late full template → gateway, solo_full, owed=0"
echo "  after coinbaser → stay on pool / split until next tip"
echo "  mid-session configure honoured (no gateway upgrade)"
echo "  per-phase logs: $WORKDIR/phases/"
