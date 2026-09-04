#!/usr/bin/env bash
# Stage an official Bitcoin Knots (BLAKE2b) linux-gnu release into a prefix that the Umbrel
# hook bind-mounts over the app's bitcoind. Downloads the tarball + SHA256SUMS(+.asc) from
# bitcoinknots.org unless they are already in SRC_DIR, checks the sha256, and verifies the
# signature with Luke Dashjr's codesigning key if gpg has it (fetched from keys.openpgp.org
# when missing).
#
#   KNOTS_VER=29.4.1.knots20260508 node/install-knots.sh
#
# The running node is not touched: restart the Umbrel app afterwards
# (~/.local/bin/umbrel-app-control restart bitcoin-knots) to pick the new binary up.
set -euo pipefail

KNOTS_VER="${KNOTS_VER:-29.4.1.knots20260508}"
ARCH="${ARCH:-x86_64-linux-gnu}"
PREFIX="${PREFIX:-$HOME/blake2b/prefix}"
SRC_DIR="${SRC_DIR:-$HOME/blake2b/src/knots-$KNOTS_VER}"
BASE_URL="https://bitcoinknots.org/files/${KNOTS_VER%%.*}.x/$KNOTS_VER"
TARBALL="bitcoin-$KNOTS_VER-$ARCH.tar.gz"
LUKE_FPR="1A3E761F19D2CC7785C5502EA291A2C45D0C504A"

mkdir -p "$PREFIX" "$SRC_DIR"
cd "$SRC_DIR"
for f in "$TARBALL" SHA256SUMS SHA256SUMS.asc; do
  [[ -f "$f" ]] || curl -fsSL -o "$f" "$BASE_URL/$f"
done

sha256sum -c SHA256SUMS --ignore-missing

if command -v gpg >/dev/null; then
  gpg --list-keys "$LUKE_FPR" >/dev/null 2>&1 \
    || gpg --batch --keyserver hkps://keys.openpgp.org --recv-keys "$LUKE_FPR" || true
  verify_out="$(gpg --verify SHA256SUMS.asc SHA256SUMS 2>&1 || true)"
  if grep -q "Good signature from \"Luke Dashjr" <<<"$verify_out"; then
    echo "SHA256SUMS: good signature from Luke Dashjr ($LUKE_FPR)"
  else
    echo "WARNING: could not verify SHA256SUMS.asc with $LUKE_FPR" >&2
    [[ "${ALLOW_UNSIGNED:-0}" == 1 ]] || exit 1
  fi
fi

# Keep the previous Knots binaries (the prefix also holds DATUM/primed, so not the whole tree)
# so a rollback is a copy back from bin/backup/<version>/.
if [[ -x "$PREFIX/bin/bitcoind" ]]; then
  old="$("$PREFIX/bin/bitcoind" -version | head -1 | sed -E 's/.*version v//')"
  mkdir -p "$PREFIX/bin/backup/$old"
  for b in bitcoind bitcoin-cli bitcoin-tx bitcoin-util bitcoin-wallet bitcoin-qt test_bitcoin; do
    [[ -f "$PREFIX/bin/$b" ]] && cp -p "$PREFIX/bin/$b" "$PREFIX/bin/backup/$old/"
  done
  echo "previous $old kept in $PREFIX/bin/backup/$old"
fi

# Only the bitcoin-* binaries, lib/, share/ etc. -- never the DATUM/primed binaries beside them.
tar -xzf "$TARBALL" -C "$PREFIX" --strip-components=1
"$PREFIX/bin/bitcoind" -version | head -1
