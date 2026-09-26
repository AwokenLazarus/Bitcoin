# Fixtures

Recorded read-only from the public APIs on 2026-09-26 around 21:11 UTC (chain tip ~974,276), for `test/run.mjs`:

- `pool.json`, `coinbaser.json`, `miner-<address>.json`: `https://pool.lazarus-xbt.xyz/api/{pool?h=0,coinbaser,miner/<address>}`
  for the two miners audited on 2026-09-26 (bc1qesxz…c025a, bc1q7u80…kem20); `miner-unknown.json` is the answer for an address the pool has never seen.
- `tip.txt`, `height-<H>.txt`, `txids-<hash>.json`, `tx-<txid>.json`: `https://mempool.lazarus-xbt.xyz/api/…` for the blocks and
  make-good transactions the tests verify. `tx-3e62…4f82.json` is the explorer's 404 body for a make-good that is queued, not yet broadcast.

Public data only; nothing here is private. Re-record with the curl lines in the task log if the API shape changes.
