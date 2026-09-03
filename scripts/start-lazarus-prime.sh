#!/bin/bash
# Lazarus DATUM Prime — our pool listener for remote datum_gateway clients.
# Runs primed (prime/); the config it reads is the same lazarus-prime.toml as before.
set -euo pipefail
export RUST_LOG="${RUST_LOG:-info}"
CONF="${CONF:-/home/umbrel/blake2b/etc/lazarus-prime.toml}"
exec /home/umbrel/blake2b/prefix/bin/primed --config "$CONF" run
