#!/bin/bash
# End-to-end test: a real C datum_gateway against primed on a BLAKE2b regtest node.
#
#   scripts/regtest-e2e.sh convoy|fte|iohzrd|startos|startos-convoy
#
# The startos lineages build the gateway commit the StartOS package pins as its submodule,
# resolved from the named branch or release tag; STARTOS_REF picks a specific one.
#
# Needs: a BLAKE2b Knots running with -regtest (cookie auth), cargo, cmake, and the
# gateway's build deps (libsodium libcurl4-openssl jansson libmicrohttpd). Clones and
# builds the named gateway lineage, starts primed and the gateway, and asserts that the
# gateway completed the handshake, took its configuration, and received a coinbaser
# reply — which proves the protocol path a stock gateway uses.
#
# Shares need real BLAKE2b work (a difficulty-1 share is ~2^32 hashes), so shares, block
# candidates and submitblock are only checked when MINER_CMD is set to a stratum miner
# command that will connect to 127.0.0.1:$STRATUM_PORT, e.g.
#   MINER_CMD='python miner.py --host 127.0.0.1 --port 19334 --user bcrt1q....rig'
#
# Environment (defaults suit ~/lazarus-regtest):
#   RPC_URL      http://127.0.0.1:18443
#   RPC_COOKIE   ~/lazarus-regtest/data/regtest/.cookie
#   BITCOIN_CLI  bitcoin-cli command with -regtest and datadir/conf baked in (for addresses)
#   PAYOUT       pool payout address; generated with BITCOIN_CLI if unset
#   WORKDIR      /tmp/primed-e2e
#   PRIME_PORT   19915   STATS_PORT 19916   STRATUM_PORT 19334   GW_API_PORT 19152
#   MINER_CMD    optional, see above
#   MINE_SECS    how long to let MINER_CMD run (default 120)
set -euo pipefail

LINEAGE="${1:-}"
# The StartOS package ships the gateway as a submodule, so the commit its newest build
# carries is the thing StartOS users actually run. Ask GitHub for it rather than pinning a
# copy here that would rot; STARTOS_REF overrides (a release tag, or pow-convoy).
startos_gateway_ref() {
  local ref="${STARTOS_REF:-$1}"
  curl -sSf --max-time 20 -H 'Accept: application/vnd.github+json' \
    "https://api.github.com/repos/Retropex/datum-gateway-startos/git/trees/$ref" |
    python3 -c 'import json,sys
for t in json.load(sys.stdin).get("tree") or []:
    if t["type"] == "commit" and t["path"] == "datum_gateway": print(t["sha"]); break'
}
case "$LINEAGE" in
  convoy) GW_REPO=https://github.com/CONVOYMining/datum_gateway; GW_REF=master ;;
  fte)    GW_REPO=https://github.com/FlyTheElephant1/datum_gateway; GW_REF=test/console-collapse-pr14-pr17 ;;
  # master carries the blake2b branch and two commits more; that is what its users run.
  iohzrd) GW_REPO=https://github.com/iohzrd/datum_gateway; GW_REF=master ;;
  # What the published StartOS "pow" package installs: OCEAN lineage, configure v1.
  startos)
    GW_REPO=https://github.com/OCEAN-xyz/datum_gateway
    GW_REF=$(startos_gateway_ref pow) || true
    [ -n "$GW_REF" ] || { echo "could not resolve the StartOS gateway commit" >&2; exit 2; }
    ;;
  # The unreleased pow-convoy branch. Its pin predates Convoy's ABW flag, so it rejects the
  # configure the newest Convoy requires; expect a configure error until it moves forward.
  startos-convoy)
    GW_REPO=https://github.com/CONVOYMining/datum_gateway
    GW_REF=$(startos_gateway_ref pow-convoy) || true
    [ -n "$GW_REF" ] || { echo "could not resolve the StartOS gateway commit" >&2; exit 2; }
    ;;
  *) echo "usage: $0 convoy|fte|iohzrd|startos|startos-convoy" >&2; exit 2 ;;
esac

HERE=$(cd "$(dirname "$0")/.." && pwd)
RPC_URL="${RPC_URL:-http://127.0.0.1:18443}"
RPC_COOKIE="${RPC_COOKIE:-$HOME/lazarus-regtest/data/regtest/.cookie}"
BITCOIN_CLI="${BITCOIN_CLI:-$HOME/lazarus-regtest/prefix/bin/bitcoin-cli -regtest -datadir=$HOME/lazarus-regtest/data -conf=$HOME/lazarus-regtest/etc/bitcoin.conf}"
WORKDIR="${WORKDIR:-/tmp/primed-e2e}"
PRIME_PORT="${PRIME_PORT:-19915}"
STATS_PORT="${STATS_PORT:-19916}"
STRATUM_PORT="${STRATUM_PORT:-19334}"
GW_API_PORT="${GW_API_PORT:-19152}"
MINE_SECS="${MINE_SECS:-120}"

mkdir -p "$WORKDIR"
GW_DIR="$WORKDIR/gw-$LINEAGE"
PIDS=()
cleanup() {
  for p in "${PIDS[@]:-}"; do [ -n "$p" ] && kill "$p" 2>/dev/null || true; done
}
trap cleanup EXIT

say() { printf '\n== %s\n' "$*"; }
fail() { echo "FAIL: $*" >&2; exit 1; }

say "node"
[ -r "$RPC_COOKIE" ] || fail "no RPC cookie at $RPC_COOKIE (is regtest Knots running?)"
INFO=$($BITCOIN_CLI getblockchaininfo) || fail "bitcoin-cli cannot reach the node"
echo "$INFO" | python3 -c 'import json,sys; d=json.load(sys.stdin); print("chain", d["chain"], "height", d["blocks"])'
if [ -z "${PAYOUT:-}" ]; then
  $BITCOIN_CLI -rpcwallet=e2e getwalletinfo >/dev/null 2>&1 || $BITCOIN_CLI createwallet e2e >/dev/null 2>&1 || $BITCOIN_CLI loadwallet e2e >/dev/null
  PAYOUT=$($BITCOIN_CLI -rpcwallet=e2e getnewaddress "" bech32)
fi
echo "payout $PAYOUT"

say "build primed"
(cd "$HERE" && cargo build --release -q)
PRIMED="$HERE/target/release/primed"

say "build $LINEAGE datum_gateway ($GW_REPO @ $GW_REF)"
if [ ! -d "$GW_DIR/.git" ]; then
  if [[ "$GW_REF" =~ ^[0-9a-f]{40}$ ]]; then
    # A submodule pin is not on any branch, but GitHub serves it by hash.
    git init -q "$GW_DIR"
    git -C "$GW_DIR" fetch -q --depth 1 "$GW_REPO" "$GW_REF"
    git -C "$GW_DIR" checkout -q FETCH_HEAD
  else
    git clone -q --depth 1 --branch "$GW_REF" "$GW_REPO" "$GW_DIR"
  fi
fi
(cd "$GW_DIR" && cmake -S . -B build -DCMAKE_BUILD_TYPE=Release >/dev/null && cmake --build build -j"$(nproc)" >/dev/null)
GW="$GW_DIR/build/datum_gateway"
[ -x "$GW" ] || fail "gateway did not build"

say "primed"
cat > "$WORKDIR/prime.toml" <<EOF
listen = "127.0.0.1:$PRIME_PORT"
stats-listen = "127.0.0.1:$STATS_PORT"
advertise-address = "127.0.0.1:$PRIME_PORT"
data-dir = "$WORKDIR/data-$LINEAGE"
motd = "primed e2e"
min-diff = 1
payout-address = "$PAYOUT"
coinbase-tag = "Lazarus"
prime-id = 7
window = 8
window-min-work = 64
min-payout = 546
fee-bps = 50
network = "regtest"
rpc = "$RPC_URL"
rpc-cookie = "$RPC_COOKIE"
poll = 0.5
EOF
mkdir -p "$WORKDIR/data-$LINEAGE"
"$PRIMED" -c "$WORKDIR/prime.toml" check
PUBKEY=$("$PRIMED" -c "$WORKDIR/prime.toml" pubkey)
RUST_LOG="${RUST_LOG:-info,primed::session=debug}" "$PRIMED" -c "$WORKDIR/prime.toml" run > "$WORKDIR/primed-$LINEAGE.log" 2>&1 &
PIDS+=($!)
sleep 2
curl -fs "http://127.0.0.1:$STATS_PORT/healthz" >/dev/null || fail "primed did not come up: $(tail -3 "$WORKDIR/primed-$LINEAGE.log")"

say "gateway"
# The fte and iohzrd forks want the activation height and headline. Convoy and the gateway
# the StartOS package ships read activation from the node's GBT rules instead and reject
# unknown keys, so only add them where they are understood.
EXTRA_MINING=""
if [ "$LINEAGE" = fte ] || [ "$LINEAGE" = iohzrd ]; then
  ACT=$(grep -o 'testactivationheight=blake2b@[0-9]*' "$HOME/lazarus-regtest/etc/bitcoin.conf" 2>/dev/null | cut -d@ -f2 || true)
  EXTRA_MINING=", \"blake2b_activation_height\": ${ACT:-101}, \"blake2b_headline\": \"Lazarus\""
fi
# pool_address is only the gateway's fallback and must parse as a mainnet address in the
# forks; consensus only ever sees the scriptPubKeys primed issues.
cat > "$WORKDIR/gw-$LINEAGE.json" <<EOF
{
 "bitcoind": { "rpccookiefile": "$RPC_COOKIE", "rpcurl": "$RPC_URL", "work_update_seconds": 10, "notify_fallback": true },
 "stratum": { "listen_addr": "127.0.0.1", "listen_port": $STRATUM_PORT, "vardiff_min": 1 },
 "mining": { "pool_address": "bc1qt5praystcdle0nq04e3h02yjszha82uzhww85x6972lcy40k4eyqz9jfaq",
             "coinbase_tag_primary": "Lazarus", "coinbase_tag_secondary": "e2e-$LINEAGE"$EXTRA_MINING },
 "api": { "listen_port": $GW_API_PORT, "admin_password": "" },
 "logger": { "log_to_console": true, "log_to_file": false, "log_level_console": 0 },
 "datum": { "pool_host": "127.0.0.1", "pool_port": $PRIME_PORT, "pool_pubkey": "$PUBKEY",
            "pool_pass_workers": true, "pool_pass_full_users": true, "pooled_mining_only": true,
            "protocol_global_timeout": 60 }
}
EOF
"$GW" -c "$WORKDIR/gw-$LINEAGE.json" > "$WORKDIR/gw-$LINEAGE.log" 2>&1 &
PIDS+=($!)

say "waiting for handshake + configure + coinbaser"
for _ in $(seq 1 60); do
  sleep 1
  S=$(curl -fs "http://127.0.0.1:$STATS_PORT/stats.json" 2>/dev/null || echo '{}')
  OK=$(echo "$S" | python3 -c '
import json,sys
d=json.load(sys.stdin)
c=d.get("clients",[]); t=d.get("totals",{})
print(1 if c and t.get("coinbasers",0)>=1 and t.get("handshake_failures",0)==0 else 0)')
  [ "$OK" = 1 ] && break
done
if [ "$OK" != 1 ]; then
  # A gateway older than Convoy's ABW flag refuses a non-zero flags byte, so it rejects the
  # configure this pool must send and abandons the connection. Say so rather than leaving it
  # to look like a regression here.
  if grep -q 'Invalid data structure in configuration' "$WORKDIR/gw-$LINEAGE.log" 2>/dev/null; then
    echo "note: the gateway rejected the configure outright:" >&2
    grep -m1 'Invalid data structure in configuration' "$WORKDIR/gw-$LINEAGE.log" >&2
    echo "note: builds older than Convoy's ABW_DISABLED flag (2026-09-02) do this, and need" >&2
    echo "      a gateway update; this pool cannot drop the flag without stalling newer ones." >&2
  fi
  fail "gateway never completed the DATUM handshake/coinbaser exchange; see $WORKDIR/gw-$LINEAGE.log and primed-$LINEAGE.log"
fi
echo "$S" | python3 -c '
import json,sys
d=json.load(sys.stdin)
for c in d["clients"]:
    print("client gen=%s ua=%s" % (c["generation"], c["user_agent"][:48]))
print("coinbasers issued:", d["totals"]["coinbasers"])'
grep -q 'Coinbaser length is invalid' "$WORKDIR/gw-$LINEAGE.log" && fail "gateway rejected a coinbaser reply"
echo "PASS: handshake, configure, coinbaser"

if [ -n "${MINER_CMD:-}" ]; then
  say "mining for ${MINE_SECS}s with: $MINER_CMD"
  bash -c "$MINER_CMD" > "$WORKDIR/miner-$LINEAGE.log" 2>&1 &
  PIDS+=($!)
  sleep "$MINE_SECS"
  S=$(curl -fs "http://127.0.0.1:$STATS_PORT/stats.json")
  echo "$S" | python3 -c '
import json,sys
d=json.load(sys.stdin); t=d["totals"]
print("accepted", t["shares_accepted"], "rejected", t["shares_rejected"], "candidates", t["block_candidates"], "submitted", t["blocks_submitted"])
assert t["shares_accepted"] > 0, "no shares accepted"
assert t["shares_rejected"] == 0, "primed rejected shares from a stock gateway"
for b in d["blocks"][:3]:
    print("block", b["height"], b["kind"], b["submit"])
    assert b["submit"] in ("accepted", "duplicate"), b["submit"]
print("PASS: shares verified, blocks assembled and submitted")'
fi
