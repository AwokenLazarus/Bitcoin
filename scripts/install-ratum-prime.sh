#!/bin/bash
# Write Lazarus Prime config from the existing DATUM payout address.
# Does not print the address. Safe to re-run (keeps the generated key).
set -euo pipefail
DATUM_JSON="${DATUM_JSON:-/home/umbrel/blake2b/etc/datum_gateway_config.json}"
CONF="${CONF:-/home/umbrel/blake2b/etc/ratum.toml}"
DATA="${DATA:-/home/umbrel/blake2b/ratum-prime}"
COOKIE="${COOKIE:-/home/umbrel/umbrel/app-data/bitcoin-knots/data/bitcoin/.cookie}"
mkdir -p "$DATA" "$(dirname "$CONF")"
ADDR=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["mining"]["pool_address"])' "$DATUM_JSON")
if [ -z "$ADDR" ]; then
  echo "missing mining.pool_address in $DATUM_JSON" >&2
  exit 1
fi
umask 077
cat > "$CONF" <<EOF
# Lazarus DATUM Prime (accept remote datum_gateway clients)
listen = "0.0.0.0:28915"
stats-listen = "127.0.0.1:28916"
advertise-address = "${ADVERTISE:-127.0.0.1:28915}"
data-dir = "$DATA"
motd = "Lazarus"
min-diff = 1024
payout-address = "$ADDR"
coinbase-tag = "Lazarus"
prime-id = 1
window = 8
min-payout = 546
fee-bps = 50
activation-height = 961640
headline = "8-30 NYPost Deride And Conquer"
rpc = "http://127.0.0.1:9332"
rpc-cookie = "$COOKIE"
poll = 0.5
EOF
chmod 600 "$CONF"
echo "wrote $CONF (payout address omitted from this log)"
