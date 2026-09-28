# SOV-011: NTA gateway readiness (probe, refusal, readiness metric)

Code: series `sov-011` of [../../patches/primed](../../patches/primed/), on top of `s0`: readiness in
`prime/primed/src/readiness.rs` and `session.rs`, the `nta-v1` hello flag in `prime/wire`, and
`lazarus/patches/datum-gateway-split-only.patch` for the C gateway. Set `WT` to a checkout with
the `s0` and `sov-011` series applied, and `KNOTS` to an **NTA-patched** Knots v29.4.2 `bitcoind`
([../../../node-template-consensus/patches/knots-v29.4.2](../../../node-template-consensus/patches/knots-v29.4.2/)).
What it tests: [../../../node-template-consensus/gateway-readiness.md](../../../node-template-consensus/gateway-readiness.md) §5.

```
./readiness-demo.sh                                          # prints RESULT PASS|FAIL
STOCK_S=/path/to/datum_gateway-7491a50 ./readiness-demo.sh   # plus a real stock gateway (observed)
```

- The script re-runs itself under `systemd-run --user --scope -p CPUQuota=200% -p MemoryMax=4G
  nice -n 19` and builds with `cargo -j2` (`SOV011_SCOPED=1` skips the scope).
- Needs: `KNOTS`; Knots' `test/functional` (found next to `build/bin/bitcoind`, or set
  `KNOTS_FUNC`); Python 3 with `cryptography`; the C gateway's build dependencies. On first run it
  clones FlyTheElephant1's `datum_gateway` (`FTE_REPO` overrides the URL), checks out `a5f28aa`,
  applies the split-only patch from `$WT`, builds it and runs `./datum_gateway --test`.
- Ports 32600–32699 on loopback: Knots 32600–32603 (P, G), primed 32615/32616, gateway stratum and
  API ports from 32620. Datadirs, logs and results under `run/` (git-ignored), results in
  `run/results/<ts>/` (`summary.json`, `checks.json`, `readiness-{a,b,c}.json`,
  `observations.json`, every log).
- Miners are `stratum-grind --threads 2`, one at a time, each under `timeout`. A diff-1 BLAKE2b
  share takes a few minutes on two CPU threads, so a run takes 15–40 minutes.

| gateway | what | hello |
|---|---|---|
| gw-A | lazarus-gateway with `nta_key_file` (signs NTA for its miner) | `nta-v1` |
| gw-C | datum_gateway, FlyTheElephant1 `a5f28aa` + the split-only patch | `nta-v1` |
| stock ("emu") | lazarus-gateway with `LAZARUS_GATEWAY_EMULATE_STOCK_V041=1`: parses coinbasers as OCEAN v0.4.1 (`datum_coinbaser.c:795`, cut at the first script over 64 bytes) | none |

primed runs with `nta-height` 300 blocks past the setup height (420), `nta-probe-every-secs = 10`
(the default is 3600) and `nta-unready-policy = "refuse"`. The steps (a)–(e) are in the
[`sov011_demo.py`](sov011_demo.py) docstring. `RESULT PASS` needs: both advertisers ready, the
emulated stock gateway unknown, then `nta-unready` after its first probed share, refused at
activation with Prime's DATUM server message and the `0x4e` notice in its log, and gw-A and gw-C
each mining a valid post-activation block with one attestation per payee.

**Real stock gateways (optional, SOV-020).** Set any of these to an unmodified build:

| Variable | Build | Source |
|---|---|---|
| `STOCK_S` | the StartOS `pow_0.4.1_23` pin | `https://github.com/iohzrd/datum_gateway` at `7491a5099dd5d887a027c812f71de63e0d5986a3` |
| `STOCK_I` | iohzrd master | same repo at `c0315682aef40dc7e0674a7bb8fb523b27a6886d` |
| `OCEAN` | OCEAN v0.4.1 | `https://github.com/OCEAN-xyz/datum_gateway` at `5b061233a3d3323771b2be98e17f543e59346619` |

Build each with `cmake . && make`. The demo starts them against node G and primed, mines on
`STOCK_S` and `STOCK_I` until a verdict or `SOV011_STOCK_SHARES` (3) shares or
`SOV011_STOCK_SECS` (900 s), and, if `STOCK_S` is still served after activation, mines on it
again. Their results are **observations** (`OBS` lines, `observations.json`), not checks: the
point is to see what the real binaries do. With the full `sov-011` series they end `nta-unready
(coinbaser discarded)` and are refused; with only `mit/0001`–`0004` they stayed `unknown` and
were served (gateway-readiness.md §5). OCEAN v0.4.1 connects but never gets work, because its
`getblocktemplate` lacks the `blake2b` rule.

Files: `readiness-demo.sh` (driver), `sov011_demo.py` (the run). It uses `../sov-015/knots_lab.py`.
