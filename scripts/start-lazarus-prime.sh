#!/bin/bash
# Lazarus DATUM Prime — our pool listener for remote datum_gateway clients.
# Runs primed (prime/); the config it reads is the same lazarus-prime.toml as before.
set -euo pipefail
export RUST_LOG="${RUST_LOG:-info}"
CONF="${CONF:-/home/umbrel/blake2b/etc/lazarus-prime.toml}"
# Soft nofile is 1024 on Umbrel; each DATUM session is a socket. Raise before exec.
# Does not affect an already-running process (use prlimit on the live pid).
ulimit -n 65536 || true
exec /home/umbrel/blake2b/prefix/bin/primed --config "$CONF" run
