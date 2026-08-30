#!/usr/bin/env bash
exec /home/umbrel/blake2b/prefix/bin/bitcoin-cli -datadir=/home/umbrel/blake2b/bitcoin -conf=/home/umbrel/blake2b/etc/bitcoin.conf "$@"
