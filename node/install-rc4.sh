#!/usr/bin/env bash
# Extract official Knots 29.4.1rc4 (BLAKE2b) into a prefix. Does not download
# blindly — put the linux-gnu tarball + SHA256SUMS + SHA256SUMS.asc next to
# this script or pass TARBALL=.
#
# Official files: https://bitcoinknots.org/files/29.x/29.4.1.knots20260508rc4/
# Verify SHA256SUMS.asc before extract.
set -euo pipefail

PREFIX="${PREFIX:-$HOME/blake2b/prefix}"
SRC_DIR="${SRC_DIR:-$HOME/blake2b/src}"
TARBALL="${TARBALL:-$SRC_DIR/bitcoin-29.4.1.knots20260508rc4-x86_64-linux-gnu.tar.gz}"
SUMS="${SUMS:-$SRC_DIR/SHA256SUMS}"

mkdir -p "$PREFIX" "$SRC_DIR"

if [[ ! -f "$TARBALL" ]]; then
  echo "Missing $TARBALL" >&2
  echo "Download bitcoin-29.4.1.knots20260508rc4-x86_64-linux-gnu.tar.gz and SHA256SUMS(+.asc) into $SRC_DIR" >&2
  exit 1
fi

if [[ -f "$SUMS" ]]; then
  (cd "$(dirname "$TARBALL")" && sha256sum -c SHA256SUMS --ignore-missing)
else
  echo "WARNING: no SHA256SUMS next to the tarball; extract anyway" >&2
fi

tar -xzf "$TARBALL" -C "$PREFIX" --strip-components=1
"$PREFIX/bin/bitcoind" -version | head -1
# expect: Bitcoin Knots daemon version v29.4.1.knots20260508rc4
