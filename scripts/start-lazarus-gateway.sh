#!/bin/bash
# Lazarus stratum. Pass ASIC or GPU json. Match on config path so the two
# profiles do not hide each other in pgrep.
set -euo pipefail
CONF="${1:?usage: start-lazarus-gateway.sh /path/to/asic-or-gpu.json}"
export RUST_LOG="${RUST_LOG:-info}"
exec /home/umbrel/blake2b/prefix/bin/lazarus-gateway --config "$CONF"
