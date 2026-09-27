# SOV-008: canaries from canary checks (W8) + the lazarus-canary sidecar (W9)

Code: branch `rnd/sov-008` (`17a8ab5`, series `s0` of [../../patches/primed](../../patches/primed/)):
primed W8 in `prime/`, the sidecar in `lazarus/canary/`. Set `WT` to a checkout with that series
applied and `KNOTS` to a Knots v29.4.2 `bitcoind` with the wallet.

**Adapted to the `s0` tip.** As published, this harness has already been through
[`../sov-010/sov008-enrol.py`](../sov-010/sov008-enrol.py) with `prime`, as the S0 verify ran it.
Each gateway enrols with a v1 message naming its node (`POST /sovereignty/enrol`; the older
`register` route is demo-only since sov-009). `gwsim` gains a `sign` command for that. The sidecar
reads the nodes from primed (`nodes_source = "prime"`) instead of a `[[gateways]]` list. Do not run
`sov008-enrol.py` on it again.

```
./canary-sidecar-demo.sh                   # 1 run: 420 s + 55 s drain, prints RESULT PASS|FAIL
RUNS=2 KNOTS_SECS=300 ./canary-sidecar-demo.sh
```

- Ports 32000-32099: primed 32015/32016, Knots 32020-32031 (P, H1, H2, X, Y, signer S).
- Datadirs, logs and results under `run/` (git-ignored). No miners: blocks come from
  `generatetodescriptor` every 60-120 s.
- `gwsim/` is a DATUM gateway simulator (Rust, on the branch's `datum-wire`): the real handshake
  under a registered G, one job (job + coinbase sections) per template, and answers to
  `request_full_block`. Its shares carry no work (a diff-1 BLAKE2b share is ~2^32 hashes), so
  primed refuses them after taking their job sections, which is all a canary check needs.
- primed runs in `publish` with `sovereignty-demo` **off**: gateways register with a G signature,
  and nothing posts snapshots or checks. The check delay is primed's default (45 s); the per-gateway
  gap is 10 s here (default 300 s) so a 15 s round cadence gets a check each round.
- The sidecar runs for real (`lazarus_canary.py run`) with a 15 s round cadence.

| gateway | template | expected |
|---|---|---|
| knots-h1, knots-h2 | its own node's getblocktemplate | never flagged |
| knots-proxy | the pool node's template T | flagged (all three kinds) |
| knots-hybrid | T ∪ (L∖T), its node's side of conflicts wins | flagged: decoys carried |
| knots-hybrid-filt | (T∩L) ∪ (L∖M) | flagged: shared canaries missed |

`RESULT PASS` also needs 0 own/shared canaries in any pool template, and the zero-leak key check:
the sidecar's `check-isolation`, the pool wallet's `getaddressinfo` (not mine, not watch-only),
`listdescriptors true` without the WIF, no canary UTXO in the pool wallet, the WIF nowhere in the
pool node's datadir, and the signer with no peers and an empty mempool.
