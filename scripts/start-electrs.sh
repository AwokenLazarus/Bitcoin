#!/bin/bash
set -e
export PATH=/home/umbrel/blake2b/bin:$PATH
exec /home/umbrel/blake2b/bin/electrs --conf /home/umbrel/blake2b/electrs.toml
