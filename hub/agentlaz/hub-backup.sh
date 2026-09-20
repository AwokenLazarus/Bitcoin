#!/bin/bash
# Pull an encrypted snapshot of the hub's pool data (ledger, credits, block log, gateway state) to AgentLaz.
# Runs from cron every 5 minutes. Key material (prime.key, identity keys, wallets, secrets) is NOT in these
# snapshots; keys are backed up once by hub-backup-keys. Encrypted at rest with ~/.config/lazarus-hub/backup.key.
set -euo pipefail
umask 077
DEST=~/backups/lazarus-hub/ledger; KEY=~/.config/lazarus-hub/backup.key; TS=$(date -u +%Y%m%dT%H%M%SZ)
OUT="$DEST/pool-$TS.tar.gz.enc"
ssh -o BatchMode=yes -o ConnectTimeout=20 lazarus-hub 'sudo tar -C /srv/pool --exclude="*.key" --exclude="secrets" --exclude="wallets" --exclude="lost+found" --exclude="cutover-*" --exclude="*.retired-*" --exclude="staging-retired" --exclude="*.pre-audit-*" --exclude="*.bak-*" --exclude="*.bak" --warning=no-file-changed -czf - . 2>/dev/null || true' \
  | openssl enc -aes-256-cbc -pbkdf2 -salt -pass file:"$KEY" -out "$OUT.part"
[ "$(stat -c %s "$OUT.part")" -gt 200 ] && mv "$OUT.part" "$OUT" || { rm -f "$OUT.part"; echo "$(date -u +%FT%TZ) backup FAILED (empty)" >> "$DEST/backup.log"; exit 1; }
echo "$(date -u +%FT%TZ) ok $(stat -c %s "$OUT") bytes" >> "$DEST/backup.log"
# retention: every snapshot for 48 h, then one per hour for 14 days
find "$DEST" -name 'pool-*.enc' -mmin +2880 ! -name 'pool-*T??0[0-4]*' -delete
find "$DEST" -name 'pool-*.enc' -mtime +14 -delete
# second copy on the TrueNAS (HexNAS) so one AgentLaz disk failure does not take the only backup with it.
# Snapshots are already encrypted; the key stays in ~/.config/lazarus-hub/backup.key (and Proton Pass).
# NAS is short on space (Mike): only one snapshot per hour goes there, kept 7 days, hard cap 2 GB.
NAS=/mnt/agent-workspace/lazarus-hub-backups
if [ "$(date -u +%M)" -lt 5 ] && [ -d "$NAS" ]; then
  cp "$OUT" "$NAS/ledger/" 2>/dev/null || echo "$(date -u +%FT%TZ) WARN: TrueNAS copy failed" >> "$DEST/backup.log"
  find "$NAS/ledger" -name 'pool-*.enc' -mtime +7 -delete 2>/dev/null
  while [ "$(du -sm "$NAS/ledger" 2>/dev/null | cut -f1)" -gt 2048 ]; do ls -t "$NAS/ledger"/pool-*.enc | tail -1 | xargs rm -f; done
fi
rsync -a ~/backups/lazarus-hub/keys/ /mnt/agent-workspace/lazarus-hub-backups/keys/ 2>/dev/null || true
