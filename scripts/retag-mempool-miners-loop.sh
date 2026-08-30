#!/bin/bash
# retag-mempool-miners-loop
while true; do
  /home/umbrel/blake2b/bin/retag-mempool-miners.sh >> /home/umbrel/blake2b/retag.log 2>&1 || true
  sleep 90
done
