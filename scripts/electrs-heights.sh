#!/bin/bash
idx=$(curl -sS -m 3 http://127.0.0.1:4225/ | awk '/^electrs_index_height/{print $NF; exit}')
COOKIE=$(tr -d '\n' < /home/umbrel/umbrel/app-data/bitcoin-knots/data/bitcoin/.cookie)
tip=$(curl -sS --max-time 8 --user "$COOKIE" --data-binary '{"jsonrpc":"1.0","id":"p","method":"getblockcount","params":[]}' -H 'content-type:text/plain' http://127.0.0.1:9332 | python3 -c 'import sys,json; print(json.load(sys.stdin).get("result"))')
echo "$idx $tip"
