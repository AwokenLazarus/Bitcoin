# Shared by the regtest-*.sh scripts: a test miner must never outlive the script (HL-015).
#
# cpu-miner.py forks daemon multiprocessing workers. Killing only its PID (or the `bash -c`
# wrapper) leaves every worker running, which once left 46 miners pinning AgentLaz's CPUs.
# So miners run under GNU `timeout`, which puts them in their own process group and signals
# the whole group on kill or when MINER_MAX_SECS runs out. MINER_PID holds timeout's PID, which
# is also the group id: pause/resume the miner with `kill -STOP/-CONT -- -"$MINER_PID"`.

MINER_MAX_SECS="${MINER_MAX_SECS:-1800}"   # hard cap on any one miner, even if cleanup never runs

# run_miner <log> <cmd...>   start a miner in the background; its PID is in $!
run_miner() {
  local log=$1; shift
  timeout -k 10 "$MINER_MAX_SECS" "$@" > "$log" 2>&1 &
}

# kill_miner <pid>   stop a miner started by run_miner, workers included
kill_miner() {
  [ -n "${1:-}" ] || return 0
  kill -CONT -- -"$1" 2>/dev/null || true
  kill -TERM -- -"$1" 2>/dev/null || true
  kill "$1" 2>/dev/null || true
  return 0
}

# reap_miners   last-resort sweep for cpu-miner.py workers (killminer.py matches exact argv)
reap_miners() {
  local k="$HOME/lazarus-regtest/killminer.py"
  [ -f "$k" ] && python3 "$k" >/dev/null 2>&1 || true
  return 0
}

# Make INT/TERM/HUP (Ctrl-C, a closed Herdr tab, patrol) run the script's EXIT trap too.
trap 'exit 130' INT
trap 'exit 143' TERM
trap 'exit 129' HUP
