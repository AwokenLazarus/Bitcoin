#!/bin/bash
# Build the DATUM Prime (prime/) and install it as /home/umbrel/blake2b/prefix/bin/primed.
# Re-run after pulling; start-lazarus-prime.sh picks the new binary up on next start.
set -euo pipefail
REPO="${REPO:-$(cd "$(dirname "$0")/.." && pwd)}"
PREFIX="${PREFIX:-/home/umbrel/blake2b/prefix}"
CONF="${CONF:-/home/umbrel/blake2b/etc/lazarus-prime.toml}"
cd "$REPO/prime"
cargo build --release --locked 2>/dev/null || cargo build --release
cargo test --release -q
install -d "$PREFIX/bin"
install -m 0755 target/release/primed "$PREFIX/bin/primed.new"
mv -f "$PREFIX/bin/primed.new" "$PREFIX/bin/primed"
if [ -r "$CONF" ]; then
  "$PREFIX/bin/primed" -c "$CONF" check
  echo "pubkey: $("$PREFIX/bin/primed" -c "$CONF" pubkey 2>/dev/null)"
fi
echo "installed $PREFIX/bin/primed ($("$PREFIX/bin/primed" --version))"
