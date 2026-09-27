# SOV-004: the hybrid proxy vs the canary detector

The detector code is branch `rnd/sov-004` (`bc21f79`, series `s0` of [../../patches/primed](../../patches/primed/)): `prime/tides/src/entropy.rs` plus primed's plumbing. Set `PRIME` to the `prime/` directory of a checkout with that series applied, and `KNOTS` to a Knots v29.4.2 `bitcoind`. Evidence from the accepted run: [../../results/sov-004/](../../results/sov-004/).

**Adapted to the `s0` tip.** As published, this harness runs primed with
`sovereignty-mode = "publish"` and sends the ingest token (`X-Sovereignty-Token`, read from
primed's `sovereignty.token`) on its POSTs. The pool-safe rework (SOV-006, in `s0`) requires both.
It has also been through [`../sov-010/add-checks.py`](../sov-010/add-checks.py) (each job snapshot is
also posted as a check, because since W8 canaries are read from checks; blocks are 30–50 s apart for
W8's 12 s due point) and [`../sov-010/add-resolved.py`](../sov-010/add-resolved.py) (canary
settlements, SOV-012). These are the same changes the S0 verify made to its copy, and the reason
the defaults are now 2 runs of 300 s. It needs a Knots with the wallet.

```
./hybrid-canary-demo.sh                  # 2 runs x 300 s, fresh primed each run, prints RESULT PASS|FAIL
RUNS=3 KNOTS_SECS=420 ./hybrid-canary-demo.sh
```

- Ports 31700–31799: primed 31715/31716, Knots 31720–31729.
- Datadirs and results go under `run/` (git-ignored).
- It uses no miner processes: blocks come from `generatetodescriptor`.

`hybrid_knots.py` runs 5 Knots 29.4.2 nodes: P (the pool), H1, H2, X and Y. The gateways are:
- honest ×2;
- a proxy and a mimic of P's template;
- three hybrids that combine P's template `T` with their own node Y's mempool `L`.

| gateway | template | caught by |
|---|---|---|
| knots-hybrid | T ∪ (L∖T), own canaries win conflicts | decoys (pool-only txs) |
| knots-hybrid-filt | (T∩L) ∪ (L∖M), where M is P's mempool | shared canaries (P's exclusions) |
| knots-hybrid-forced | (T∩L) ∪ (L∖T), which is L | nothing. It *is* its own node's tx set (the bound) |
