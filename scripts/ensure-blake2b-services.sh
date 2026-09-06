#!/bin/bash
# Keep header-v2 electrs and the local pools JSON server running.
listening() { ss -H -tln | awk '{print $4}' | grep -Eq ":$1$"; }

# Pick up an electrs upgrade: if bin/electrs was replaced after the running process started,
# stop it here and let the block below relaunch it. The process is root-owned when it was
# launched from an Umbrel hook, so this only takes effect when the hook runs us as root.
EPID=$(pgrep -of '/home/umbrel/blake2b/bin/electrs --conf' || true)
if [[ -n "$EPID" ]]; then
  bin_mtime=$(stat -c %Y /home/umbrel/blake2b/bin/electrs)
  started=$(( $(date +%s) - $(ps -o etimes= -p "$EPID" | tr -d ' ') ))
  if (( bin_mtime > started )) && kill -TERM "$EPID" 2>/dev/null; then
    for _ in $(seq 1 40); do kill -0 "$EPID" 2>/dev/null || break; sleep 0.5; done
    echo "electrs: restarted to pick up new binary"
  fi
fi
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
# Pooled GPU gateway (:3333) retired -- pooled mining is stratum-only via lazarus-asic.json.
#
# Solo stratum: one instance per miner class, both paying the finder directly (97.5%) with
# a 2.5% fee output. They hold no window state and never talk to Prime, so a solo restart
# cannot touch a pooled miner or a connected DATUM gateway.
#   solo-asic :23335 (api 7154, vardiff 1024/4096/131072)
#   solo-gpu  :3334  (api 7155, vardiff 1/1/131072)
for solo in asic gpu; do
  cfg=/home/umbrel/blake2b/etc/lazarus-solo-$solo.json
  [[ -f $cfg ]] || continue
  if ! pgrep -f "lazarus-gateway --config $cfg" >/dev/null; then
    nohup /home/umbrel/blake2b/bin/start-lazarus-gateway.sh "$cfg" >> "/home/umbrel/blake2b/logs/lazarus-solo-$solo.log" 2>&1 &
  fi
done
#
# Pool UI (pool/server.py): :8888 local, :8889 lan-edge, :8890 public NPM.
# Static files are read from disk per request, so pool/static/* deploys are live at once;
# a new server.py needs the processes relaunched. Same trick as electrs above: any instance
# older than server.py is TERMed here and relaunched below. When this script runs as root
# (Umbrel app hooks) the instances are started as umbrel via runuser, so later deploys from
# the umbrel timer can restart them without sudo. Only :8888 writes to pool.sqlite; the
# other two are read-only mirrors (POOL_UI_NO_WRITE=1) so they never fight over the lock.
POOL_PY=/home/umbrel/blake2b/lazarus-pool/server.py
POOL_LOGS=/home/umbrel/blake2b/logs
py_mtime=$(stat -c %Y "$POOL_PY")
for pid in $(pgrep -f "^python3 $POOL_PY$" || true); do  # anchored: skip the runuser wrappers
  started=$(( $(date +%s) - $(ps -o etimes= -p "$pid" | tr -d ' ') ))
  if (( py_mtime > started )); then
    if kill -TERM "$pid" 2>/dev/null; then
      for _ in $(seq 1 20); do kill -0 "$pid" 2>/dev/null || break; sleep 0.5; done
      echo "pool-ui: stopped pid $pid to pick up new server.py"
    else
      echo "pool-ui: pid $pid ($(ps -o user= -p "$pid")) is older than server.py; rerun as root to restart it"
    fi
  fi
done
pool_ui() { # port logfile [extra env]
  local port=$1 log=$2; shift 2
  if [[ $(id -u) -eq 0 ]]; then
    runuser -u umbrel -- env POOL_LISTEN_PORT="$port" "$@" nohup python3 "$POOL_PY" >> "$log" 2>&1 &
  else
    env POOL_LISTEN_PORT="$port" "$@" nohup python3 "$POOL_PY" >> "$log" 2>&1 &
  fi
}
listening 8888 || pool_ui 8888 "$POOL_LOGS/pool-ui.log"
listening 8889 || pool_ui 8889 "$POOL_LOGS/pool-ui-8889.log" POOL_UI_NO_WRITE=1
listening 8890 || pool_ui 8890 "$POOL_LOGS/pool-ui-8890.log" POOL_UI_NO_WRITE=1
