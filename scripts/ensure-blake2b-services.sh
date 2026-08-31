#!/bin/bash
# Keep header-v2 electrs and the local pools JSON server running.
if ! pgrep -f '/home/umbrel/blake2b/bin/electrs' >/dev/null; then
  nohup /home/umbrel/blake2b/bin/start-electrs.sh >> /home/umbrel/blake2b/electrs.log 2>&1 &
fi
if ! pgrep -f 'http.server 8765' >/dev/null; then
  nohup /home/umbrel/blake2b/bin/serve-pools.sh >> /home/umbrel/blake2b/pools-http.log 2>&1 &
fi
if ! pgrep -f '/home/umbrel/blake2b/prefix/bin/ratum-prime' >/dev/null; then
  nohup /home/umbrel/blake2b/bin/start-ratum-prime.sh >> /home/umbrel/blake2b/logs/ratum-prime.log 2>&1 &
fi
if ! pgrep -f 'datum_gateway -c /home/umbrel/blake2b/etc/datum_gateway_config.json' >/dev/null; then
  nohup /home/umbrel/blake2b/bin/datum-gateway.sh >> /home/umbrel/blake2b/logs/datum.console 2>&1 &
fi
if ! pgrep -f 'datum_gateway -c /home/umbrel/blake2b/etc/datum_gateway_gpu.json' >/dev/null; then
  nohup /home/umbrel/blake2b/bin/datum-gateway-gpu.sh >> /home/umbrel/blake2b/logs/datum-gpu.console 2>&1 &
fi
if ! pgrep -f '/home/umbrel/blake2b/lazarus-pool/server.py' >/dev/null; then
  nohup python3 /home/umbrel/blake2b/lazarus-pool/server.py >> /home/umbrel/blake2b/logs/pool-ui.log 2>&1 &
fi
