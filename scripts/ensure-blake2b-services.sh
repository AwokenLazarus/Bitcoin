#!/bin/bash
# Keep header-v2 electrs and the local pools JSON server running.
listening() { ss -H -tln | awk '{print $4}' | grep -Eq ":$1$"; }

if ! pgrep -f '/home/umbrel/blake2b/bin/electrs' >/dev/null; then
  nohup /home/umbrel/blake2b/bin/start-electrs.sh >> /home/umbrel/blake2b/electrs.log 2>&1 &
fi
if ! pgrep -f 'http.server 8765' >/dev/null; then
  nohup /home/umbrel/blake2b/bin/serve-pools.sh >> /home/umbrel/blake2b/pools-http.log 2>&1 &
fi
if ! pgrep -f '/home/umbrel/blake2b/prefix/bin/primed' >/dev/null; then
  nohup /home/umbrel/blake2b/bin/start-lazarus-prime.sh >> /home/umbrel/blake2b/logs/lazarus-prime.log 2>&1 &
fi
if ! pgrep -f 'lazarus-gateway --config /home/umbrel/blake2b/etc/lazarus-asic.json' >/dev/null; then
  nohup /home/umbrel/blake2b/bin/start-lazarus-gateway.sh /home/umbrel/blake2b/etc/lazarus-asic.json >> /home/umbrel/blake2b/logs/lazarus-asic.log 2>&1 &
fi
# GPU mining gateway (:3333) retired -- the pool is stratum-only via lazarus-asic.json.
# :8888 local, :8889 lan-edge, :8890 public NPM
if ! listening 8888; then
  nohup python3 /home/umbrel/blake2b/lazarus-pool/server.py >> /home/umbrel/blake2b/logs/pool-ui.log 2>&1 &
fi
if ! listening 8889; then
  nohup env POOL_LISTEN_PORT=8889 python3 /home/umbrel/blake2b/lazarus-pool/server.py >> /home/umbrel/blake2b/logs/pool-ui-8889.log 2>&1 &
fi
if ! listening 8890; then
  nohup env POOL_LISTEN_PORT=8890 python3 /home/umbrel/blake2b/lazarus-pool/server.py >> /home/umbrel/blake2b/logs/pool-ui-8890.log 2>&1 &
fi
