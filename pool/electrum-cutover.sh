#!/bin/bash
# Flip public Electrum TLS nginx from Fulcrum :50001 to electrs when the indexer is at tip.
set -euo pipefail
TLS_SSH="${TLS_SSH:-root@electrum-tls}"
ELECTRS_PORT="${ELECTRS_PORT:-50011}"
OLD_PORT="${OLD_PORT:-50001}"
FORK_HEIGHT="${FORK_HEIGHT:-961640}"
# Set HEIGHTS_CMD to a command that prints: index=<n> tip=<n>
eval "$($HEIGHTS_CMD)"
if [[ -z "${index:-}" || -z "${tip:-}" ]]; then echo no_heights; exit 0; fi
if (( index < FORK_HEIGHT || index < tip - 2 )); then echo not_ready index=$index tip=$tip; exit 0; fi
ssh -o BatchMode=yes "$TLS_SSH" "sed -i s/:$OLD_PORT/:$ELECTRS_PORT/ /opt/electrum-tls/nginx.conf && docker restart electrum-tls"
echo cutover_ok index=$index tip=$tip
