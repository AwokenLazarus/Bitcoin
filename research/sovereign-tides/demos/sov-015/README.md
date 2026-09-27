# SOV-015: foreign canaries catch many gateways fed by one shared third-party node

Code: branch `rnd/sov-015` (`0708b7b`, on top of `s0`; series `sov-015` of [../../patches/primed](../../patches/primed/)): the
detector in `prime/tides/src/entropy.rs`, primed in `prime/primed/src/{sovereignty,stats}.rs`, the
sidecar's `[foreign]` mode in `lazarus/canary/`. Set `WT` to a checkout with the s0 and sov-015
series applied and `KNOTS` to a Knots v29.4.2 `bitcoind`. Evidence: [../../results/sov-015/](../../results/sov-015/).

```
./foreign-canary-demo.sh                        # 3 runs x 420 s (+ 55 s drain) + race probe; RESULT PASS|FAIL
RUNS=1 MIN_RUNS=1 KNOTS_SECS=300 RACE_TRIALS=0 ./foreign-canary-demo.sh   # quick look
python3 race_probe.py <bitcoind> 32880 run/race $WT/lazarus/canary 40   # the full race curve
```

- The script re-runs itself under `systemd-run --user --scope -p CPUQuota=200% -p MemoryMax=4G nice -n 19`
  (a soak was running on the same host) and builds with `cargo -j2`.
- Ports 32800-32899: primed 32815/32816, Knots 32820-32831 (P, H1, H2, X, T, signer S) and
  32833-32835 (T's three extra addresses), race probe 32880-32885. Datadirs, logs and results
  under `run/` (git-ignored). No miners: blocks come from `generatetodescriptor` every 60-120 s.
- primed runs in `publish` with `sovereignty-demo` off. Gateways enrol v1 naming their nodes; the
  sidecar reads them from primed's `GET /sovereignty/nodes`. `gwsim/` is SOV-013's DATUM gateway
  simulator built against this branch's `datum-wire`.
- The sidecar runs for real with `[foreign] enabled = true`, `fanout = 0` (every other gateway),
  `order = "own-ack"`, `max_lag_secs = 0.05`, 15 s rounds.

| gateway | registered node | template | expected |
|---|---|---|---|
| knots-h1, knots-h2 | its own (H1, H2) | its own node's getblocktemplate | never flagged, never clustered |
| tfarm-1, -2, -3 | T on three different ports | T's getblocktemplate | one `shared-template-source` cluster, independence ÷ 3 |
| knots-proxy | X (runs, ignored) | the pool node's template | flagged (SOV-004 control) |

`RESULT PASS` needs, in every one of ≥ 3 runs: the three T-fed gateways exactly one cluster, the
honest gateways in no cluster and not flagged, the proxy flagged and not clustered, 0 own/shared
leaks into pool templates, key isolation OK, no failed delivery. It prints the honest relay-race
rate (foreign canaries honest gateways carried / judged), the twins' ack lags, and the race probe:
how often an honest node took C by relay rather than its twin, by twin lag, for a node G's node
connected to and a node that connected to G's node.

Files: `foreign-canary-demo.sh` (driver), `foreign_knots.py` (one run), `race_probe.py` (relay race),
`knots_lab.py` (Knots helpers), `gwsim/`.
