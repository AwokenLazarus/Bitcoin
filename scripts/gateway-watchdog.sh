#!/usr/bin/env bash
# Lazarus gateway watchdog. Runs once a minute from gateway-watchdog.timer.
#
# Catches the failure that took the pool down on 2026-09-09: every gateway process was up
# and every miner stayed connected, but no gateway had published a job for 15 minutes
# (the node filled templates to the weight limit and the coinbase would not fit), so
# miners hashed a stale job that Prime does not credit. Nothing paged; the graph fell.
#
# For every gateway config in /home/umbrel/blake2b/etc/lazarus-*.json with an api_listen,
# read /audit and alert when:
#   * template_age_s > STALE_S            -- no job published; says whether Knots RPC is up
#   * the api does not answer but the process exists
#   * tx_trimmed > 0 for TRIM_STREAK runs -- the gateway is having to cut transactions on
#                                            every template: Knots' blockmaxweight is too
#                                            high again (see node/blake2b.conf)
# Alerts go to Home Assistant (phone push + persistent notification) using the token in
# /home/umbrel/blake2b/secrets/ha.env (HA_URL, HA_TOKEN). Re-alerts every REALERT_S while
# the condition holds, and sends one "recovered" when it clears. State in STATE_DIR.
#
#   gateway-watchdog.sh          normal run
#   gateway-watchdog.sh --test   send a test notification and exit
set -u
STALE_S="${STALE_S:-90}"
REALERT_S="${REALERT_S:-900}"
TRIM_STREAK="${TRIM_STREAK:-5}"
ETC="${ETC:-/home/umbrel/blake2b/etc}"
STATE_DIR="${STATE_DIR:-/home/umbrel/blake2b/logs/gateway-watchdog}"
LOG="${LOG:-/home/umbrel/blake2b/logs/gateway-watchdog.log}"
HA_ENV="${HA_ENV:-/home/umbrel/blake2b/secrets/ha.env}"
HA_NOTIFY="${HA_NOTIFY:-mobile_app_mike_phone}"
COOKIE_FILE="${COOKIE_FILE:-/home/umbrel/umbrel/app-data/bitcoin-knots/data/bitcoin/.cookie}"
RPC_URL="${RPC_URL:-http://127.0.0.1:9332/}"

mkdir -p "$STATE_DIR"
now=$(date +%s)
log() { printf '%s %s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$*" >> "$LOG"; }

notify() { # notify <title> <message>
  local title="$1" msg="$2"
  log "ALERT: $title -- $msg"
  [ -r "$HA_ENV" ] || { log "no $HA_ENV; alert not sent"; return 1; }
  # shellcheck disable=SC1090
  . "$HA_ENV"
  [ -n "${HA_URL:-}" ] && [ -n "${HA_TOKEN:-}" ] || { log "HA_URL/HA_TOKEN missing"; return 1; }
  local body
  body=$(python3 -c 'import json,sys; print(json.dumps({"title": sys.argv[1], "message": sys.argv[2]}))' "$title" "$msg")
  curl -sS -m 10 -o /dev/null -H "Authorization: Bearer $HA_TOKEN" -H "Content-Type: application/json" \
    -X POST "$HA_URL/api/services/notify/$HA_NOTIFY" -d "$body" || log "phone notify failed"
  local pbody
  pbody=$(python3 -c 'import json,sys; print(json.dumps({"title": sys.argv[1], "message": sys.argv[2], "notification_id": "lazarus-gateway-watchdog"}))' "$title" "$msg")
  curl -sS -m 10 -o /dev/null -H "Authorization: Bearer $HA_TOKEN" -H "Content-Type: application/json" \
    -X POST "$HA_URL/api/services/persistent_notification/create" -d "$pbody" || log "persistent notify failed"
}

if [ "${1:-}" = "--test" ]; then
  notify "Lazarus pool watchdog" "Test notification from $(hostname) at $(date -u +%H:%M:%SZ). Watchdog is installed."
  exit $?
fi

knots_up() {
  [ -r "$COOKIE_FILE" ] || return 1
  curl -s -m 5 --user "$(cat "$COOKIE_FILE")" -H 'Content-Type: text/plain' \
    --data-binary '{"jsonrpc":"1.0","id":"wd","method":"getblockcount","params":[]}' "$RPC_URL" \
    | grep -q '"result":[0-9]'
}

# raise <key> <title> <message>: alert now if new or REALERT_S since the last one.
raise() {
  local key="$1" title="$2" msg="$3" last=0
  local f="$STATE_DIR/$key.alerting"
  [ -f "$f" ] && last=$(cat "$f" 2>/dev/null || echo 0)
  if [ "$last" -eq 0 ] || [ $((now - last)) -ge "$REALERT_S" ]; then
    notify "$title" "$msg" && echo "$now" > "$f"
  fi
}
# clear <key> <message>: if it was alerting, say it recovered.
clear_alert() {
  local key="$1" msg="$2"
  local f="$STATE_DIR/$key.alerting"
  if [ -f "$f" ]; then
    rm -f "$f"
    notify "Lazarus pool: recovered" "$msg"
  fi
}

shopt -s nullglob
for cfg in "$ETC"/lazarus-*.json; do
  name=$(basename "$cfg" .json); name=${name#lazarus-}
  api=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1])).get("api_listen",""))' "$cfg" 2>/dev/null)
  [ -n "$api" ] || continue
  pgrep -f "lazarus-gateway --config $cfg" >/dev/null || continue   # not a running gateway (ensure-* owns that)
  audit=$(curl -s -m 5 "http://$api/audit")
  if [ -z "$audit" ]; then
    raise "$name-api" "Lazarus pool: $name gateway API down" "lazarus-gateway ($cfg) is running but http://$api/audit does not answer."
    continue
  fi
  clear_alert "$name-api" "$name gateway API answering again."
  read -r age trimmed height <<<"$(printf '%s' "$audit" | python3 -c 'import json,sys
d=json.load(sys.stdin); print(d.get("template_age_s",-1), d.get("tx_trimmed",0), d.get("height",0))' 2>/dev/null || echo "-1 0 0")"
  if [ "$age" = "-1" ]; then
    # no job yet (just started) or unparseable; leave it to the next run
    continue
  fi
  if [ "$age" -gt "$STALE_S" ]; then
    if knots_up; then
      raise "$name-stale" "Lazarus pool: $name gateway STALE" \
        "No job published for ${age}s at height $height while Knots RPC is up. Miners are hashing stale work that Prime will not credit. Check $LOG and logs/lazarus-$name.log for 'not publishing'."
    else
      raise "$name-stale" "Lazarus pool: $name gateway STALE (Knots down)" \
        "No job published for ${age}s and Knots RPC at $RPC_URL is not answering. Check the bitcoin-knots app."
    fi
  else
    clear_alert "$name-stale" "$name gateway publishing again (template ${age}s old, height $height)."
  fi
  # Constant trimming is a config smell, not an outage: say so once per REALERT_S.
  sf="$STATE_DIR/$name.trimstreak"
  if [ "${trimmed:-0}" -gt 0 ]; then
    n=$(( $(cat "$sf" 2>/dev/null || echo 0) + 1 )); echo "$n" > "$sf"
    if [ "$n" -ge "$TRIM_STREAK" ]; then
      raise "$name-trim" "Lazarus pool: $name trimming every template" \
        "Gateway is dropping $trimmed txs per template to fit the coinbase. Knots blockmaxweight is too close to the 800000 limit; it should be 736000 (node/blake2b.conf, app settings.json)."
    fi
  else
    rm -f "$sf"
    clear_alert "$name-trim" "$name templates fit without trimming."
  fi
done
exit 0
