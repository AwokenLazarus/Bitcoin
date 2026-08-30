#!/bin/bash
cd /home/umbrel/blake2b/pools
exec python3 -m http.server 8765 --bind 0.0.0.0
