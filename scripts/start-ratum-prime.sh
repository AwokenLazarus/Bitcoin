#!/bin/bash
# Lazarus DATUM Prime — pool-side listener for remote DATUM gateways.
# This is OUR Prime, not iohzrd's public pool.
set -euo pipefail
export RUST_LOG="${RUST_LOG:-info}"
exec /home/umbrel/blake2b/prefix/bin/ratum-prime \
  --config /home/umbrel/blake2b/etc/ratum.toml
