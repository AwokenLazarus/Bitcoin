#!/bin/bash
set -e
export PATH=/home/umbrel/blake2b/llvm/bin:/home/umbrel/.cargo/bin:$PATH
export LIBCLANG_PATH=/home/umbrel/blake2b/llvm/lib
export LD_LIBRARY_PATH=/home/umbrel/blake2b/llvm/lib:/home/umbrel/blake2b/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}
cd /home/umbrel/blake2b/src/electrs
exec cargo build --release
