#!/bin/bash
# Lazarus stratum. Pass ASIC or GPU json. Match on config path so the two
# profiles do not hide each other in pgrep.
set -euo pipefail
CONF="${1:?usage: start-lazarus-gateway.sh /path/to/asic-or-gpu.json}"
export RUST_LOG="${RUST_LOG:-info}"
# Soft nofile is 1024 on Umbrel; the gateway allows thousands of miners and each
# session is a socket + thread. Raise before exec so a restart is PH-ready.
# Does not affect an already-running process (use prlimit on the live pid).
ulimit -n 65536 || true
exec /home/umbrel/blake2b/prefix/bin/lazarus-gateway --config "$CONF"
