#!/usr/bin/env bash
export LD_LIBRARY_PATH=/home/umbrel/blake2b/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}
exec /home/umbrel/blake2b/prefix/bin/datum_gateway -c /home/umbrel/blake2b/etc/datum_gateway_gpu.json "$@"
