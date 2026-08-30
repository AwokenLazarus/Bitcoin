#!/usr/bin/env bash
set -euo pipefail
BIN=/home/umbrel/blake2b/prefix/bin/bitcoind
if pgrep -f "$BIN" >/dev/null; then echo already_running; exit 0; fi
exec "$BIN" -datadir=/home/umbrel/blake2b/bitcoin -conf=/home/umbrel/blake2b/etc/bitcoin.conf
