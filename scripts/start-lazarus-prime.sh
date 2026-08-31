#!/bin/bash
# Lazarus DATUM Prime — our pool listener for remote datum_gateway clients.
set -euo pipefail
export RUST_LOG="${RUST_LOG:-info}"
exec /home/umbrel/blake2b/prefix/bin/lazarus-prime \
  --config /home/umbrel/blake2b/etc/lazarus-prime.toml
