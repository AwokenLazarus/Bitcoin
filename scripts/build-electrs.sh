#!/bin/bash
# Build the header-v2 (BLAKE2b, 164-byte headers) electrs. Source tree: romanz/electrs at the
# 0.11.1 release tag with patches/electrs-0.11.1-header-v2.patch applied
# (git clone https://github.com/romanz/electrs -b v0.11.1 ~/blake2b/src/electrs && git apply ...).
# The running binary is bin/electrs; ensure-blake2b-services.sh restarts it when that file is
# replaced (cp target/release/electrs bin/electrs).
set -e
export PATH=/home/umbrel/blake2b/llvm/bin:/home/umbrel/.cargo/bin:$PATH
export LIBCLANG_PATH=/home/umbrel/blake2b/llvm/lib
export LD_LIBRARY_PATH=/home/umbrel/blake2b/llvm/lib:/home/umbrel/blake2b/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}
cd /home/umbrel/blake2b/src/electrs
exec cargo build --release
