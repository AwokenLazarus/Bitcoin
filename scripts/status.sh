#!/usr/bin/env bash
CLI=/home/umbrel/blake2b/bin/bitcoin-cli.sh
echo === knots ===
if pgrep -f /home/umbrel/blake2b/prefix/bin/bitcoind >/dev/null; then
  $CLI getblockchaininfo
else
  echo bitcoind not running
fi
ss -lnt | grep -E ':18332|:18333|:23334|:7152' || true
