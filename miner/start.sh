#!/bin/bash
set -euo pipefail
cd "$(dirname "$0")"
exec python3 miner.py --host "${STRATUM_HOST:-127.0.0.1}" --port "${STRATUM_PORT:-23334}" --intensity "${INTENSITY:-22}"
