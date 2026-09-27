# SOV-010: S0 integration verify

`verify-all.sh` runs every Sovereign TIDES S0 verify against `rnd/sov-s0`
(a checkout of it, `WT=`). It exports each suite from that suite's own demo repo, and only three of
those (sov-004, sov-008, sov-015) are published here, so it is included as a record of what was
verified; the last full run is [../../results/sov-010/VERIFIED.md](../../results/sov-010/VERIFIED.md). The suites run one after another, all in ports
32300–32399, with datadirs under `./run`. No miners run, and nothing is pushed.

```
WT=/path/to/checkout RND=/path/to/demo-repos ./verify-all.sh
ONLY="enrol nodes-file" ./verify-all.sh      # a subset
SOV010_PORT_BASE=32400 ./verify-all.sh       # every suite in 32400-32499 instead
```

**Other branches and port ranges (SOV-012).**
- `SOV010_PORT_BASE` (default 32300) re-bases every suite into `BASE..BASE+99`: the port-base envs,
  the shifted literals, and every `must` pattern are all derived from it.
- The branch under test can be any descendant of `rnd/sov-s0` `d5278a2`. `WT` defaults to the Bitcoin
  worktree you run the script from when that is on a `rnd/sov-*` branch.
- Logs and datadirs go to `./run` for `rnd/sov-s0`, and to `$RND/sov-NNN/run/verify-all` for
  `rnd/sov-NNN` (`SOV010_RUN` overrides).
- `add-resolved.py` (below) and the sidecar suite's `nodes_source = "prime"` apply only where the
  branch has them; on `rnd/sov-s0` the suites run as before.

It prints one `PASS|FAIL` line per suite, then `RESULT PASS` only if every suite passed. Logs and
`summary.txt` go to `run/verify-<stamp>/`. After each suite it checks that nothing is still
listening in 32300–32399.

## Suites

| Suite | What |
|---|---|
| cargo-prime | `cargo test --release` for the prime workspace (with pipefail) |
| cargo-lazarus | `cargo test --release` for the lazarus workspace, plus the canary sidecar's `unittest` |
| bench | sov-006 `bench.sh` (in-process, no ports) |
| regress | sov-006 `regress.sh`: mode off vs the base without the series, the XBT-071 demo, and the SOV-004 hybrid demo (`HYBRID_RUNS`, default 1) |
| nta | sov-002 `nta-signing-demo.sh` |
| hybrid | sov-004 `hybrid-canary-demo.sh` (`HYBRID_STANDALONE_RUNS`, default 3, of 120 s each) |
| sidecar | sov-008 `canary-sidecar-demo.sh` (1 run of 420 s) |
| enrol | sov-009 `enrol-demo.sh` |
| nodes-file | the glue: the canary sidecar's own `gateways()` reads the `sovereignty-nodes.json` that the enrol suite's primed wrote. It must get exactly that file's nodes, every one belonging to an active key, none from the demo's revoked or superseded keys, and the file must be mode 0600 |
| lzt1 | sov-007 `lzt1-datum-demo.sh` (a non-loopback gateway on a second local address) |

## How the demos are re-based (the demo repos are never edited)

Each demo is exported with `git archive` into `run/suites/<id>/`, from the commit its task was
accepted at: sov-006 `59f7d51`, sov-002 `e57a828`, sov-004 `9c6741f`, sov-008 `5737c30`,
sov-009 `4e596f8` and sov-007 `545bb65`. The copy is then changed as follows:

| Demo | Ports | Other changes to the copy |
|---|---|---|
| sov-006 regress | literals 31900–31999 → 32300–32399 (its SOV-004 sed patterns for 317xx are left alone) | Its harness copies (XBT-071 `sov_nta_demo.py`, SOV-004 `hybrid_knots.py`) also go through `add-checks.py`. The XBT-071 demo runs with `XBT071_GAP_MIN/MAX=30/50`, and the hybrid step with `HYBRID_SECS=300` |
| sov-002 | `SOV002_PORT_BASE=32300` | `sovereignty-mode = "observe"`, and the ingest token on its POSTs. `rnd/sov-002` predates sov-006's mode switch, so `/sovereignty/credit` would otherwise be a 404 |
| sov-004 | literals 31700–31799 → 32300–32399 | What `regress.sh` already does to it (mode `publish`, the token on its POSTs), plus `add-checks.py`; 2 runs of 300 s |
| sov-008 | literals 32000–32099 → 32300–32399 | `gwsim` builds against `rnd/sov-s0`'s `datum-wire`. `sov008-enrol.py`: gateways enrol with v1 messages that name their nodes, and the sidecar gets `nodes_file = <primed data-dir>/sovereignty-nodes.json` instead of `[[gateways]]` |
| sov-009 | literals 32100–32199 → 32300–32399 | none |
| sov-007 | `SOV007_PORT_BASE=32300` | gw-A registers with `lazarus-gateway enrol --json` (a v1 message) on `POST /sovereignty/enrol`, not the old `LZT1 register G=…`, which is demo-only since sov-009 |

Every binary comes from the `rnd/sov-s0` worktree, through `PRIME=`/`WT=`.

## SOV-012: `add-resolved.py` and the sidecar's node source
- **`add-resolved.py`** patches the XBT-071 and SOV-004 harness copies (regress and hybrid suites) to
  measure fix 1 on their own run. At the point where each harness reads its verdicts, it reads
  `/sovereignty.json` (before), then posts every canary's settlement as the sidecar would
  (`POST /sovereignty/canary/resolved`, from `gettxout` on the pool node), then reads it again
  (after, which the harness then checks as before). It prints
  `SOV012 <demo> <gateway> own h/d -> h/d`. `verify-all.sh` collects those lines into
  `sov012.txt` and prints them after the summary.
- **The sidecar suite**, on a branch whose sidecar has `nodes_source`, runs the sidecar with
  `nodes_source = "prime"` and no `nodes_file` (`sov008-enrol.py <copy> prime`). Every node it
  canaries then came from primed's `GET /sovereignty/nodes`. The `nodes-file` suite still covers the
  file path (`nodes_source = "file"`).

## Why the pre-W8 harnesses need `add-checks.py`
Since sov-008 (SOV-P-002 W8), primed takes canary evidence only from template checks. The
XBT-071 and SOV-004 harnesses post job snapshots, so `add-checks.py` makes three changes to them:

1. **Checks.** It posts snapshots as checks on the demo-only `/sovereignty/check`, as sov-008
   did for `prime/scripts/rnd-a2-feed.py`.
2. **Fewer checks.** It sends at most one check per gateway every `SOV010_CHECK_EVERY` s
   (default 5). Primed keeps 256 checks per key. A check for every 0.25 s poll pushes each
   canary's 12–180 s window out within a minute, and then even honest gateways score 0.
3. **Slower blocks.** Blocks come 30–50 s apart instead of 8–14 s. A canary is judged only by
   jobs first seen 12 s or more after it is delivered, so with fast blocks an honest node has
   usually mined it by then, and that reads as a miss. sov-008 made the same change to
   `rnd-a2-knots.py`.

Without these three changes, both demos flag the honest gateways, or have no canary evidence
at all, on any build that includes W8.
