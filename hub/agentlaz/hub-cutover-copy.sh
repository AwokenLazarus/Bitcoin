#!/bin/bash
# Phase 8 step 3: final copy of the production prime state home -> hub, with checksum gate.
# Run ONLY after the home prime is stopped (refuses otherwise). Streams through AgentLaz; nothing lands on local disk.
set -euo pipefail
POOL=/home/mike/lazarus-ops/pool-ssh
FILES="ledger.json credits.bin identities.txt gateway-scripts.json blocks.jsonl lazarus-prime.key"

if $POOL -c 'pgrep -x primed >/dev/null' 2>/dev/null; then
  echo "REFUSING: home prime is still running"; exit 1
fi
echo "home prime is down; copying $(date -u +%FT%TZ)"
$POOL -c 'tar -C ~/blake2b -czf - lazarus-prime lazarus-pool/pool.sqlite lazarus-pool/owed-settlements.json makegood-queue 2>/dev/null' 2>/dev/null \
 | ssh -o BatchMode=yes lazarus-hub 'sudo rm -rf /srv/pool/cutover-final && sudo mkdir -p /srv/pool/cutover-final && sudo tar -C /srv/pool/cutover-final -xzf - && sudo rm -rf /srv/pool/prime-production && sudo mv /srv/pool/cutover-final/lazarus-prime /srv/pool/prime-production && echo "hub: $(sudo du -sh /srv/pool/prime-production | cut -f1) in /srv/pool/prime-production"'

HOME_SUM=$($POOL -c "cd ~/blake2b/lazarus-prime && sha256sum $FILES" 2>/dev/null)
HUB_SUM=$(ssh -o BatchMode=yes lazarus-hub "cd /srv/pool/prime-production && sudo sha256sum $FILES")
if [ "$HOME_SUM" = "$HUB_SUM" ]; then
  echo "CHECKSUMS MATCH ($(echo "$HUB_SUM" | wc -l) files)"; echo "$HUB_SUM" | cut -c1-16,65-
else
  echo "CHECKSUM MISMATCH"; diff <(echo "$HOME_SUM") <(echo "$HUB_SUM") || true; exit 2
fi
