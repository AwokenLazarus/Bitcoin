#!/usr/bin/env bash
# Template divergence test: does a block found through a gateway carry the *gateway's* template
# when the pool's node has a different mempool? And what happens when the two nodes' tips drift?
#
#   MINER_CMD='python miner.py --host 127.0.0.1 --port 19334 --user bcrt1q....rig' \
#     scripts/regtest-divergence.sh fte
#
# Two BLAKE2b regtest nodes: A is the pool's node (primed submits there), B is the gateway's
# node (a fresh datadir synced from A). They are peered, then cut apart; each gets transactions
# the other never sees; primed's node A and the gateway's node B therefore build different
# templates at the same height. A block is then mined through the gateway and the script asserts
# that the block A accepted contains exactly B's transactions and none of A's own — i.e. the
# pool node accepted a block it could not have built itself, and the gateway's template survived
# primed's reassembly intact. It then puts A one block ahead of B and checks primed accepts a
# competing block within the stale grace (submitblock says `inconclusive`), that the next block
# on B's branch reorgs A, and that the `orphan:` label on the competing block clears once it is
# back in the main chain.
#
# Needs: a BLAKE2b Knots regtest node (node A) already running with cookie auth, its binaries
# (bitcoind/bitcoin-cli), cargo, cmake and the gateway build deps. Shares need real BLAKE2b work
# so MINER_CMD is required (a diff-1 share is ~2^32 hashes; every share is a block on regtest).
#
# Environment (defaults suit ~/lazarus-regtest):
#   BITCOIN_BIN     dir holding bitcoind and bitcoin-cli   (~/lazarus-regtest/prefix/bin)
#   A_DATADIR       node A datadir                         (~/lazarus-regtest/data)
#   A_CONF          node A bitcoin.conf                    (~/lazarus-regtest/etc/bitcoin.conf)
#   A_RPC_PORT      18443     A_P2P_PORT 18444
#   A_WALLET        wallet on A with spendable coins       (test)
#   B_RPC_PORT      28443     B_P2P_PORT 28444
#   WORKDIR         /tmp/primed-divergence
#   PRIME_PORT      19915     STATS_PORT 19916     STRATUM_PORT 19334     GW_API_PORT 19152
#   MINER_CMD       stratum miner that connects to 127.0.0.1:$STRATUM_PORT (required)
#   BLOCK_WAIT      seconds to wait for each block (default 180)

set -euo pipefail

LINEAGE="${1:-fte}"
case "$LINEAGE" in
  convoy) GW_REPO=https://github.com/CONVOYMining/datum_gateway; GW_REF=master ;;
  fte)    GW_REPO=https://github.com/FlyTheElephant1/datum_gateway; GW_REF=test/console-collapse-pr14-pr17 ;;
  # master carries the blake2b branch and two commits more; that is what its users run.
  iohzrd) GW_REPO=https://github.com/iohzrd/datum_gateway; GW_REF=master ;;
  # The commit the newest published StartOS "pow" package ships as its submodule.
  startos)
    GW_REPO=https://github.com/OCEAN-xyz/datum_gateway
    GW_REF=$(curl -sSf --max-time 20 -H 'Accept: application/vnd.github+json' \
      "https://api.github.com/repos/Retropex/datum-gateway-startos/git/trees/${STARTOS_REF:-pow}" |
      python3 -c 'import json,sys
for t in json.load(sys.stdin).get("tree") or []:
    if t["type"] == "commit" and t["path"] == "datum_gateway": print(t["sha"]); break') || true
    [ -n "$GW_REF" ] || { echo "could not resolve the StartOS gateway commit" >&2; exit 2; }
    ;;
  *) echo "usage: $0 convoy|fte|iohzrd|startos" >&2; exit 2 ;;
esac
[ -n "${MINER_CMD:-}" ] || { echo "MINER_CMD is required (see header)" >&2; exit 2; }

HERE=$(cd "$(dirname "$0")/.." && pwd)
BITCOIN_BIN="${BITCOIN_BIN:-$HOME/lazarus-regtest/prefix/bin}"
A_DATADIR="${A_DATADIR:-$HOME/lazarus-regtest/data}"
A_CONF="${A_CONF:-$HOME/lazarus-regtest/etc/bitcoin.conf}"
A_RPC_PORT="${A_RPC_PORT:-18443}"; A_P2P_PORT="${A_P2P_PORT:-18444}"
A_WALLET="${A_WALLET:-test}"
B_RPC_PORT="${B_RPC_PORT:-28443}"; B_P2P_PORT="${B_P2P_PORT:-28444}"
WORKDIR="${WORKDIR:-/tmp/primed-divergence}"
PRIME_PORT="${PRIME_PORT:-19915}"; STATS_PORT="${STATS_PORT:-19916}"
STRATUM_PORT="${STRATUM_PORT:-19334}"; GW_API_PORT="${GW_API_PORT:-19152}"
BLOCK_WAIT="${BLOCK_WAIT:-180}"

A="$BITCOIN_BIN/bitcoin-cli -regtest -datadir=$A_DATADIR -conf=$A_CONF"
AW="$A -rpcwallet=$A_WALLET"
B_DIR="$WORKDIR/nodeb"
B="$BITCOIN_BIN/bitcoin-cli -regtest -datadir=$B_DIR -conf=$B_DIR/bitcoin.conf"

mkdir -p "$WORKDIR" "$B_DIR"
PIDS=()
cleanup() {
  for p in "${PIDS[@]:-}"; do [ -n "$p" ] && { kill -CONT "$p" 2>/dev/null; kill "$p" 2>/dev/null; } || true; done
  $B stop >/dev/null 2>&1 || true
}
trap cleanup EXIT
say() { printf '\n== %s\n' "$*"; }
fail() { echo "FAIL: $*" >&2; exit 1; }
py() { python3 -c "$@"; }
count() { $1 getblockcount; }
# wait until node A's tip reaches height $1
wait_block() {
  local want=$1 i
  for i in $(seq 1 "$BLOCK_WAIT"); do
    sleep 1
    [ "$(count "$A")" -ge "$want" ] && return 0
  done
  return 1
}

say "node A"
INFO=$($A getblockchaininfo) || fail "cannot reach node A"
echo "$INFO" | py 'import json,sys; d=json.load(sys.stdin); print("chain", d["chain"], "height", d["blocks"])'
$AW getwalletinfo >/dev/null || fail "wallet $A_WALLET not loaded on A"
ACT=$(grep -o 'testactivationheight=blake2b@[0-9]*' "$A_CONF" | cut -d@ -f2 || echo 101)

say "node B (fresh datadir, peered to A)"
if ! $B getblockcount >/dev/null 2>&1; then
  cat > "$B_DIR/bitcoin.conf" <<EOF
regtest=1
server=1
listenonion=0
discover=0
natpmp=0
upnp=0
fallbackfee=0.0002
txindex=1
blake2b_headline=Lazarus

[regtest]
listen=1
bind=127.0.0.1
rpcbind=127.0.0.1
rpcallowip=127.0.0.1
rpcport=$B_RPC_PORT
port=$B_P2P_PORT
testactivationheight=blake2b@$ACT
addnode=127.0.0.1:$A_P2P_PORT
EOF
  "$BITCOIN_BIN/bitcoind" -regtest -datadir="$B_DIR" -conf="$B_DIR/bitcoin.conf" -daemon >/dev/null
fi
for _ in $(seq 1 60); do
  sleep 1
  [ "$($B getbestblockhash 2>/dev/null || true)" = "$($A getbestblockhash)" ] && break
done
[ "$($B getbestblockhash)" = "$($A getbestblockhash)" ] || fail "B did not sync to A"
echo "A and B both at $(count "$A")"

say "build primed and $LINEAGE gateway"
(cd "$HERE" && cargo build --release -q)
PRIMED="$HERE/target/release/primed"
GW_DIR="$WORKDIR/gw-$LINEAGE"
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
GW="$GW_DIR/build/datum_gateway"; [ -x "$GW" ] || fail "gateway did not build"

say "cut A and B apart, give each its own transactions"
$B addnode "127.0.0.1:$A_P2P_PORT" remove >/dev/null 2>&1 || true
$B disconnectnode "127.0.0.1:$A_P2P_PORT" >/dev/null 2>&1 || true
sleep 1
[ "$($A getpeerinfo | py 'import json,sys; print(len(json.load(sys.stdin)))')" = 0 ] || fail "A still has peers"
# B-only: raw transactions signed by A's wallet, broadcast to B only (A never sees them)
B_TXIDS=()
while read -r txid vout amt; do
  dest=$($AW getnewaddress "" bech32)
  out=$(py "print(f'{float($amt)-0.0005:.8f}')")
  raw=$($A createrawtransaction "[{\"txid\":\"$txid\",\"vout\":$vout}]" "{\"$dest\":$out}")
  signed=$($AW signrawtransactionwithwallet "$raw" | py 'import json,sys; d=json.load(sys.stdin); assert d["complete"]; print(d["hex"])')
  $AW lockunspent false "[{\"txid\":\"$txid\",\"vout\":$vout}]" >/dev/null
  B_TXIDS+=("$($B sendrawtransaction "$signed")")
done < <($AW listunspent 1 9999999 | py 'import json,sys
u=sorted(json.load(sys.stdin), key=lambda x:-x["amount"])[:3]
print("\n".join("%s %d %s" % (x["txid"], x["vout"], x["amount"]) for x in u))')
# A-only: ordinary wallet sends on A
A_TXIDS=()
for i in 1 2; do A_TXIDS+=("$($AW sendtoaddress "$($AW getnewaddress "" bech32)" 1.$i)"); done
echo "B-only: ${B_TXIDS[*]}"; echo "A-only: ${A_TXIDS[*]}"
$A getblocktemplate '{"rules":["segwit","blake2b"]}' > "$WORKDIR/gbt-A.json"
$B getblocktemplate '{"rules":["segwit","blake2b"]}' > "$WORKDIR/gbt-B.json"
HEIGHT=$(py 'import json; print(json.load(open("'"$WORKDIR"'/gbt-B.json"))["height"])')
py '
import json,sys
a=json.load(open(sys.argv[1])); b=json.load(open(sys.argv[2]))
ta={t["txid"] for t in a["transactions"]}; tb={t["txid"] for t in b["transactions"]}
assert a["height"]==b["height"] and a["previousblockhash"]==b["previousblockhash"], "nodes not at the same tip"
assert ta and tb and not (ta & tb), "templates do not diverge"
print("templates at height", a["height"], "differ: A has", len(ta), "txs, B has", len(tb), "none shared; coinbasevalue A", a["coinbasevalue"], "B", b["coinbasevalue"])' "$WORKDIR/gbt-A.json" "$WORKDIR/gbt-B.json"

say "primed -> node A"
PAYOUT=$($AW getnewaddress "" bech32)
cat > "$WORKDIR/prime.toml" <<EOF
listen = "127.0.0.1:$PRIME_PORT"
stats-listen = "127.0.0.1:$STATS_PORT"
advertise-address = "127.0.0.1:$PRIME_PORT"
data-dir = "$WORKDIR/data"
motd = "primed divergence"
min-diff = 1
payout-address = "$PAYOUT"
coinbase-tag = "Lazarus"
prime-id = 7
window = 8
window-min-work = 64
min-payout = 546
fee-bps = 50
network = "regtest"
rpc = "http://127.0.0.1:$A_RPC_PORT"
rpc-cookie = "$A_DATADIR/regtest/.cookie"
poll = 0.5
stale-grace-secs = 30
EOF
mkdir -p "$WORKDIR/data"
PUBKEY=$("$PRIMED" -c "$WORKDIR/prime.toml" pubkey)
RUST_LOG="${RUST_LOG:-info}" "$PRIMED" -c "$WORKDIR/prime.toml" run > "$WORKDIR/primed.log" 2>&1 &
PIDS+=($!)
sleep 2
curl -fs "http://127.0.0.1:$STATS_PORT/healthz" >/dev/null || fail "primed did not come up: $(tail -3 "$WORKDIR/primed.log")"

say "gateway -> node B"
EXTRA_MINING=""
# Only fte and iohzrd know these keys; Convoy and the StartOS build read activation from GBT.
if [ "$LINEAGE" = fte ] || [ "$LINEAGE" = iohzrd ]; then
  EXTRA_MINING=", \"blake2b_activation_height\": $ACT, \"blake2b_headline\": \"Lazarus\""
fi
cat > "$WORKDIR/gw.json" <<EOF
{
 "bitcoind": { "rpccookiefile": "$B_DIR/regtest/.cookie", "rpcurl": "http://127.0.0.1:$B_RPC_PORT", "work_update_seconds": 10, "notify_fallback": true },
 "stratum": { "listen_addr": "127.0.0.1", "listen_port": $STRATUM_PORT, "vardiff_min": 1 },
 "mining": { "pool_address": "bc1qt5praystcdle0nq04e3h02yjszha82uzhww85x6972lcy40k4eyqz9jfaq",
             "coinbase_tag_primary": "Lazarus", "coinbase_tag_secondary": "divergence-gw"$EXTRA_MINING },
 "api": { "listen_port": $GW_API_PORT, "admin_password": "" },
 "logger": { "log_to_console": true, "log_to_file": false, "log_level_console": 0 },
 "datum": { "pool_host": "127.0.0.1", "pool_port": $PRIME_PORT, "pool_pubkey": "$PUBKEY",
            "pool_pass_workers": true, "pool_pass_full_users": true, "pooled_mining_only": true,
            "protocol_global_timeout": 60 }
}
EOF
"$GW" -c "$WORKDIR/gw.json" > "$WORKDIR/gw.log" 2>&1 &
PIDS+=($!)
for _ in $(seq 1 60); do
  sleep 1
  curl -fs "http://127.0.0.1:$STATS_PORT/stats.json" 2>/dev/null | py 'import json,sys; d=json.load(sys.stdin); sys.exit(0 if d.get("clients") and d["totals"].get("coinbasers",0)>=1 else 1)' && break
done

say "mine through the gateway until A accepts block $HEIGHT"
bash -c "exec $MINER_CMD" > "$WORKDIR/miner.log" 2>&1 &
PIDS+=($!)
wait_block "$HEIGHT" || fail "no block within ${BLOCK_WAIT}s; see $WORKDIR/miner.log"
sleep 2
BH=$($A getblockhash "$HEIGHT")
$A getblock "$BH" 2 | py '
import json,sys
blk=json.load(sys.stdin); b_only=set(sys.argv[1].split()); a_only=set(sys.argv[2].split())
txs={t["txid"] for t in blk["tx"]}
cb=blk["tx"][0]; tag=bytes.fromhex(cb["vin"][0]["coinbase"])
print("block", blk["height"], blk["hash"][:16], "size", blk["size"], "txs", len(txs)-1)
print("  B-only txs present %d/%d   A-only txs present %d/%d" % (len(txs & b_only), len(b_only), len(txs & a_only), len(a_only)))
print("  coinbase tag", tag[4:].split(b"\x00")[0])
print("  coinbase outs", [(v["scriptPubKey"].get("address","?")[:14], v["value"]) for v in cb["vout"]])
assert txs >= b_only, "block is missing the gateway node'"'"'s transactions"
assert not (txs & a_only), "block carries the pool node'"'"'s transactions: template was not preserved"
assert b"divergence-gw" in tag, "secondary tag missing"' "${B_TXIDS[*]}" "${A_TXIDS[*]}"
grep -q "submitblock $BH .*: accepted" "$WORKDIR/primed.log" || fail "primed did not submit $BH to A as accepted"
[ "$($B getblock "$BH" 1 | py 'import json,sys; print(json.load(sys.stdin)["height"])')" = "$HEIGHT" ] || fail "B does not have the block"
echo "PASS: pool node accepted a block built from the gateway node's template"

say "gateway node one block behind: competing block, reorg, orphan label clears"
MINER_PID=${PIDS[-1]}
kill -STOP "$MINER_PID"
sleep 2
TIP=$(count "$A")
$A generatetoaddress 1 "$($AW getnewaddress "" bech32)" >/dev/null
[ "$(count "$A")" = $((TIP+1)) ] && [ "$(count "$B")" = "$TIP" ] || fail "could not put A one block ahead"
MARK=$(wc -l < "$WORKDIR/primed.log")
sleep 3
kill -CONT "$MINER_PID"
for _ in $(seq 1 $((BLOCK_WAIT*10))); do
  tail -n +"$MARK" "$WORKDIR/primed.log" | grep -q "inconclusive" && { kill -STOP "$MINER_PID"; break; }
  sleep 0.1
done
tail -n +"$MARK" "$WORKDIR/primed.log" | grep -q "inconclusive" || fail "no competing block within grace; see $WORKDIR/primed.log"
COMP=$(tail -n +"$MARK" "$WORKDIR/primed.log" | grep -oE "submitblock [0-9a-f]{64} .*inconclusive" | head -1 | cut -d' ' -f2)
echo "competing block $COMP at $((TIP+1)) submitted: inconclusive (accepted by A within stale grace)"
for _ in $(seq 1 60); do
  tail -n +"$MARK" "$WORKDIR/primed.log" | grep -q "$COMP at $((TIP+1)) is not in the main chain" && break
  sleep 1
done
tail -n +"$MARK" "$WORKDIR/primed.log" | grep -q "$COMP.*is not in the main chain" || fail "confirm pass never labelled the competing block"
echo "labelled orphan while A's own block was the tip"
kill -CONT "$MINER_PID"
for _ in $(seq 1 $((BLOCK_WAIT+60))); do
  tail -n +"$MARK" "$WORKDIR/primed.log" | grep -q "$COMP at $((TIP+1)) is back in the main chain" && break
  sleep 1
done
tail -n +"$MARK" "$WORKDIR/primed.log" | grep -q "$COMP.*back in the main chain" || fail "orphan label did not clear after the reorg"
curl -fs "http://127.0.0.1:$STATS_PORT/stats.json" | py '
import json,sys
d=json.load(sys.stdin); r=[b for b in d["blocks"] if b["hash"]==sys.argv[1]][0]
print("record:", r["height"], r["kind"], r["submit"], "settled" if r["settled"] else "unsettled")
assert r["kind"]=="split" and r["settled"], r' "$COMP"
$A getchaintips | py 'import json,sys; [print("A tip", t["height"], t["hash"][:16], t["status"]) for t in json.load(sys.stdin) if t["status"] in ("active","valid-fork") and t["height"]>=int(sys.argv[1])]' "$TIP"
echo "PASS: reorg onto the gateway's branch; competing block settled as split"

say "reconnect A and B"
$B addnode "127.0.0.1:$A_P2P_PORT" add >/dev/null
for _ in $(seq 1 30); do sleep 1; [ "$($A getbestblockhash)" = "$($B getbestblockhash)" ] && break; done
[ "$($A getbestblockhash)" = "$($B getbestblockhash)" ] || fail "A and B did not reconverge"
echo "A and B agree at $(count "$A")"
echo
echo "ALL PASS"
