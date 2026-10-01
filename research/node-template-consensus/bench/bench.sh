#!/usr/bin/env bash
# SPDX-License-Identifier: MIT
# Copyright (c) 2026 Mike Moore (AwokenLazarus)
# Reproduce the NTA verify, block-check and signing numbers.
# Self-wraps in the CPU/memory cap. Writes results/bench.json and results/bench.md.
#
# Required:
#   KNOTS_DIR     Knots tree built from patches/knots-v29.4.2 on v29.4.2.knots20260508
#   PROTOCOL_DIR  protocol crate (lazarus/protocol) with the published primed/agpl series
# Optional:
#   PORT_BASE     first regtest port (default 32640; uses PORT_BASE..+59)
#   --quick       one run of each measurement (default is 5)
set -euo pipefail
cd "$(dirname "$0")"
ROOT=$(pwd)

QUICK=0
for arg in "$@"; do
  case "$arg" in
    --quick) QUICK=1 ;;
    *) echo "bench: unknown argument: $arg" >&2; exit 2 ;;
  esac
done
if [[ "$QUICK" == 1 ]]; then
  export NTA_BENCH_RUNS=1
else
  export NTA_BENCH_RUNS=5
fi

if [[ -z "${KNOTS_DIR:-}" ]]; then
  echo "bench: set KNOTS_DIR to a Knots tree built from patches/knots-v29.4.2" >&2
  exit 1
fi
if [[ -z "${PROTOCOL_DIR:-}" ]]; then
  echo "bench: set PROTOCOL_DIR to a protocol crate (lazarus/protocol) with the primed/agpl series" >&2
  exit 1
fi
KNOTS_DIR=$(cd "$KNOTS_DIR" && pwd)
PROTOCOL_DIR=$(cd "$PROTOCOL_DIR" && pwd)
PORT_BASE=${PORT_BASE:-32640}
export KNOTS_DIR PROTOCOL_DIR PORT_BASE
export NTA_PORT_BASE="$PORT_BASE"

port_hi=$((PORT_BASE + 59))
if (( PORT_BASE <= 32599 && port_hi >= 32300 )); then
  echo "bench: PORT_BASE $PORT_BASE overlaps the reserved range 32300-32599" >&2
  exit 1
fi

need() {
  if [[ ! -e "$1" ]]; then
    echo "bench: missing $2" >&2
    exit 1
  fi
}
need "$KNOTS_DIR/src/secp256k1/include/secp256k1.h" "secp256k1 headers under KNOTS_DIR"
need "$KNOTS_DIR/build/src/secp256k1/lib/libsecp256k1.a" "libsecp256k1.a (build Knots first)"
need "$KNOTS_DIR/build/bin/bitcoind" "bitcoind under KNOTS_DIR/build"
need "$KNOTS_DIR/build/test/config.ini" "Knots test config.ini"
need "$KNOTS_DIR/test/functional/feature_xbt_nta.py" "feature_xbt_nta.py"
need "$PROTOCOL_DIR/Cargo.toml" "protocol Cargo.toml"
need "$PROTOCOL_DIR/src/nta.rs" "protocol src/nta.rs"

if [[ -z "${BENCH_SCOPED:-}" ]]; then
  exec systemd-run --user --scope \
    -p CPUQuota=200% -p MemoryMax=4G \
    --working-directory="$ROOT" \
    --setenv=BENCH_SCOPED=1 \
    --setenv=KNOTS_DIR="$KNOTS_DIR" \
    --setenv=PROTOCOL_DIR="$PROTOCOL_DIR" \
    --setenv=PORT_BASE="$PORT_BASE" \
    --setenv=NTA_PORT_BASE="$PORT_BASE" \
    --setenv=NTA_BENCH_RUNS="$NTA_BENCH_RUNS" \
    nice -n 19 "$ROOT/bench.sh" "$@"
fi

SECP_INC="$KNOTS_DIR/src/secp256k1/include"
SECP_A="$KNOTS_DIR/build/src/secp256k1/lib/libsecp256k1.a"

mkdir -p "$ROOT/build" "$ROOT/results" "$ROOT/run" "$ROOT/sign-bench/.bench" "$ROOT/sign-bench/.cargo"
# Cargo will not follow a symlink back to the protocol crate's workspace
# (edition and version are inherited). The generated manifest path-depends
# on PROTOCOL_DIR itself. It is gitignored; the committed Cargo.toml has no
# machine path. .cargo/config.toml only caps the build.
cat > "$ROOT/sign-bench/.cargo/config.toml" << 'EOF'
[build]
jobs = 2
EOF
cat > "$ROOT/sign-bench/.bench/Cargo.toml" << EOF
[package]
name = "nta-sign-bench"
version = "0.1.0"
edition = "2021"
publish = false
description = "Time NtaAttestationHash + BIP340 sign the way the gateway does"
autobins = false

[[bin]]
name = "nta-sign-bench"
path = "../src/main.rs"

[dependencies]
lazarus-protocol = { path = "$PROTOCOL_DIR" }
EOF

cpu=$(awk -F: '/model name/ { gsub(/^[ \t]+/, "", $2); print $2; exit }' /proc/cpuinfo)
load_start=$(cut -d' ' -f1-3 /proc/loadavg)
knots_commit=$(git -C "$KNOTS_DIR" rev-parse --short HEAD)
protocol_commit=$(git -C "$PROTOCOL_DIR" rev-parse --short HEAD)
protocol_branch=$(git -C "$PROTOCOL_DIR" branch --show-current || true)

CMD_VERIFY="cc -O2 -o build/bench_verify src/bench_verify.c -I\"\$KNOTS_DIR/src/secp256k1/include\" \"\$KNOTS_DIR/build/src/secp256k1/lib/libsecp256k1.a\" && ./build/bench_verify"
CMD_SIGN="cargo build --release -j2 --manifest-path sign-bench/.bench/Cargo.toml && ./sign-bench/target/release/nta-sign-bench"
CMD_BLOCKS="PORT_BASE=\$PORT_BASE KNOTS_DIR=\$KNOTS_DIR NTA_BLOCKS_JSON=results/blocks.json python3 bench_blocks.py --configfile \"\$KNOTS_DIR/build/test/config.ini\" --tmpdir run/blocks"

cc -O2 -o "$ROOT/build/bench_verify" "$ROOT/src/bench_verify.c" -I"$SECP_INC" "$SECP_A"
"$ROOT/build/bench_verify" > "$ROOT/results/verify.json"

if [[ -f "$ROOT/sign-bench/Cargo.lock" ]]; then
  cp "$ROOT/sign-bench/Cargo.lock" "$ROOT/sign-bench/.bench/Cargo.lock"
fi
CARGO_TARGET_DIR="$ROOT/sign-bench/target" \
  cargo build --release -j2 --locked --manifest-path "$ROOT/sign-bench/.bench/Cargo.toml"
"$ROOT/sign-bench/target/release/nta-sign-bench" > "$ROOT/results/sign.json"

NTA_BLOCKS_JSON="$ROOT/results/blocks.json" \
  python3 "$ROOT/bench_blocks.py" \
    --configfile "$KNOTS_DIR/build/test/config.ini" \
    --tmpdir "$ROOT/run/blocks"

load_end=$(cut -d' ' -f1-3 /proc/loadavg)
python3 - > "$ROOT/results/host.json" << PY
import json, sys
json.dump({
  "cpu": """$cpu""",
  "loadavg_start": "$load_start",
  "loadavg_end": "$load_end",
  "cpu_quota": "200%",
  "memory_max": "4G",
  "nice": 19,
  "quick": "$QUICK" == "1",
  "runs": int("$NTA_BENCH_RUNS"),
  "knots_commit": "$knots_commit",
  "knots_build_type": "RelWithDebInfo",
  "secp_library": "KNOTS_DIR/build/src/secp256k1/lib/libsecp256k1.a",
  "protocol_commit": "$protocol_commit",
  "protocol_branch": "$protocol_branch",
  "port_base": int("$PORT_BASE"),
  "cmd_verify": """$CMD_VERIFY""",
  "cmd_sign": """$CMD_SIGN""",
  "cmd_blocks": """$CMD_BLOCKS""",
  "cmd_all": "KNOTS_DIR=<knots-tree> PROTOCOL_DIR=<protocol-crate> ./bench.sh",
}, sys.stdout, indent=2)
PY

python3 "$ROOT/write_report.py"
test -s "$ROOT/results/bench.json"
test -s "$ROOT/results/bench.md"
echo "wrote $ROOT/results/bench.json and bench.md"
