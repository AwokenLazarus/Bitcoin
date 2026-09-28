# Sovereign TIDES and NTA changes to Prime, as patch files

These are the pool-side changes (Prime / `primed`, `lazarus-gateway`, the canary sidecar,
the reference NTA signer, the pool site's API) as `git format-patch` files. They are research
code: **not deployed** and **not merged into `main`**. The `main` branch of this repo builds
the production pool, so this code is published only as patches.

**They apply to this repo's `main` at `f14bb4c`** ("mempool-patches: apply-to-hub upgrades an
applied patch in place"), and build and pass `cargo test` there. [../../TRY-IT.md](../../TRY-IT.md)
has the exact commands.

## Two licence directories

Each directory holds exactly one licence, following the repo's own split (see the repo
README's [License](../../../../README.md#license) section):

| Directory | Touches | Licence |
|---|---|---|
| [`mit/`](mit/) | `prime/` and `pool/` only | **MIT**, Copyright (c) 2026 Mike Moore (AwokenLazarus). Same terms as [`prime/LICENSE`](../../../../prime/LICENSE) |
| [`agpl/`](agpl/) | `lazarus/` only | **AGPL-3.0**, Copyright (c) 2026 Mike Moore (AwokenLazarus). `lazarus/` is derived from [Ratum](https://github.com/iohzrd/ratum) and stays AGPL-3.0; full text in [`agpl/LICENSE`](agpl/LICENSE) |

A commit that touched both `prime/` and `lazarus/` is split in two by path
(`git format-patch -- prime pool` and `git format-patch -- lazarus`), so its halves appear under
the same subject in both directories. The two directories never touch the same file. Applying
all of a series' `mit/` patches and then all of its `agpl/` patches gives exactly the tree of
the source branch. Every series was checked that way with `git am`.

## Series

| Series | Applies on | `mit/` | `agpl/` | What |
|---|---|---:|---:|---|
| `s0` | `main` `f14bb4c` | 27 | 8 | The S0 stack: Sovereign TIDES made pool-safe, the canary sidecar, registration, NTA signing |
| `sov-011` | `f14bb4c` + `s0` | 6 | 3 | NTA activation safety: find and refuse gateways that can't carry attestations |
| `sov-015` | `f14bb4c` + `s0` | 2 | 1 | Foreign canaries: cluster gateways fed by one third-party node |
| `sov-017` | `f14bb4c` + `s0` | 4 | 3 | Direct NTA attestations for payees without a gateway (`nta-signer`) |

`sov-011`, `sov-015` and `sov-017` are independent of each other. Each applies on top of `s0`
alone. Pick one at a time.

```sh
git checkout -b try-sov f14bb4c        # or any main that still has f14bb4c's prime/, lazarus/, pool/
P=research/sovereign-tides/patches/primed
git am $P/mit/s0/*.patch  && git am $P/agpl/s0/*.patch
git am $P/mit/sov-015/*.patch && git am $P/agpl/sov-015/*.patch    # optional: one of the three
(cd prime && cargo test) && (cd lazarus && cargo test)
```

Checked results at each tip (`cargo test --workspace` in `prime/` and `lazarus/`, and the
sidecar's `python3 -m unittest test_lazarus_canary`): all pass.

| Tip | `prime/` tests | `lazarus/` tests | sidecar tests |
|---|---:|---:|---:|
| `s0` | 260 | 131 | 18 |
| `s0` + `sov-011` | 274 | 133 | 18 |
| `s0` + `sov-015` | 267 | 131 | 22 |
| `s0` + `sov-017` | 265 | 135 | 18 |

## How these differ from the internal branches

The research branches were built on an internal Prime base that has release commits not yet
published here. For this publication, every series was **rebased onto the public `f14bb4c`**.
Where a change only touched code around those unpublished commits, their lines were left out.
One change needed a real port:

- **`s0` `mit/0018` (W8, canary evidence).** Internally this change shares its plumbing with
  code that is not part of this publication. The published version is the canary check on its
  own: `maybe_check_canaries` asks for the newest
  job's transactions with `request_full_block`, and `on_validation` routes the reply to
  `note_check`. It has its own time-out (`CANARY_CHECK_TTL`, 120 s). If a found block on the
  same job has already asked for them, that one reply serves both. The detector, the canary
  rules and the sidecar are unchanged.

Default paths were also made generic in this publication only (the source branches keep
their own):
- the sidecar's example unit and `lazarus-canary.toml.example`: `/opt/lazarus-canary/...` and
  `/var/lib/primed/...`;
- the sidecar's and `pool/server.py`'s default token path: `/var/lib/primed/sovereignty.token`;
- the example node cookie path: `/var/lib/bitcoind/.cookie`.

The source branch of `s0` has three merges, and `git format-patch` drops merges. So the series
is **linearised**: each merge's side-branch commits are replayed onto the first parent, then a
"Merge resolution" patch brings the tree to the merge's tree. Intermediate trees inside `s0` may
not build. The tip of each series does. Authors, dates and messages are the originals, apart
from `mit/s0/0018`'s message, which describes the port. The `From` lines are zeroed.

## Patches

### `s0`

| `mit/` | `agpl/` | Change |
|---|---|---|
| 0001 | | sketch a signed template-authorship claim and a sovereignty bonus (rnd/a2) |
| 0002 | | move the sketch to the LZT1 attestation; weight the bonus by independence |
| 0003 | | wire Sovereign TIDES into primed (attest, score, bonus, detector) |
| 0004 | | judge fake DATUM by canaries, not by similarity to the pool |
| 0005 | | credited work takes the gateway's latest independence |
| 0006 | | weight the sovereignty score by independence, not just LZT1 |
| 0007 | | catch the hybrid proxy with shared canaries and decoys (SOV-004) |
| 0008 | | tides: index the template clusters and prepare snapshots once |
| 0009 | | make Sovereign TIDES pool-safe behind `sovereignty-mode` (SOV-006) |
| 0010 | | the A2 demo turns `sovereignty-mode` on and sends the ingest token |
| 0011 | 0001 | gateway signs XBT-NTA per tip; Prime builds attested coinbasers (SOV-002) |
| 0012 | | stratum-grind `--threads`; keep 32 tips of NTA latency |
| 0013 | | an attested coinbase is a Split, not a Partial |
| 0014 | | keep node-refused blocks; an NTA refusal is invalid now |
| | 0002 | lazarus-gateway serves the block its node last refused (`/rejected`) |
| 0015 | | merge resolution: the NTA signing work into the S0 rework |
| 0016 | | take LZT1 attestations over DATUM (`0x5A`) and verify them on the first share (SOV-007) |
| | 0003 | lazarus-gateway: attest every pooled job with LZT1; the split-only C gateway patch |
| | 0004 | lazarus-protocol: push the BIP34 height as Core writes it |
| 0017 | | rustfmt |
| 0018 | | canary evidence from canary checks (SOV-008, W8; ported, see above) |
| 0019 | 0005 | `lazarus-canary`: the canary sidecar (W9) |
| 0020 | | merge resolution: the canary work |
| 0021 | | wire: a BIP322 verifier for the registration's payout binding (SOV-009) |
| 0022 | | register gateways with LZT1 register v1/v2 and bind the payout by BIP322 |
| 0023 | 0006 | `lazarus-gateway enrol` and the public enrol route |
| 0024 | | merge resolution: the registration work |
| 0025 | 0007 | the sidecar reads `sovereignty-nodes.json` (SOV-010) |
| 0026 | | datum-wire: push the BIP34 height as Core writes it in `coinbase::build` |
| 0027 | 0008 | a canary mined before it is due is not due; nodes over loopback (SOV-012) |

### `sov-011`: NTA activation safety

Prime tells apart gateways that can carry NTA attestations and those that can't (the `nta-v1`
hello flag and the pre-activation probe). After activation it refuses unready gateways, and it
reports fleet readiness in `/nta.json`. This series includes the fix to the split-only C gateway
patch, which dropped zero-value outputs after its payees. See
[../../../node-template-consensus/gateway-readiness.md](../../../node-template-consensus/gateway-readiness.md)
and the demo in [../../demos/sov-011/](../../demos/sov-011/).

| `mit/` | `agpl/` | Change |
|---|---|---|
| 0001 | 0001 | find gateways that can't carry NTA attestations before activation |
| 0002 | | a share on any probe coinbaser still held decides, not only the newest |
| 0003 | | readiness weighs gateways by accepted share difficulty; refused reconnects log once per ten minutes |
| 0004 | 0002 | probe advertisers too; the C gateway keeps zero-value outputs after its payees |
| 0005 | | a gateway that **discards** 70-byte coinbasers is `nta-unready` after two pool-only strikes (added 2026-09-28) |
| 0006 | | after activation one unattested pool-only share makes a gateway `nta-unready` (added 2026-09-28) |
| | 0003 | the split-only C gateway patch, rebased onto FlyTheElephant1 master `a5f28aa` (added 2026-09-28) |

**Added 2026-09-28 (SOV-020, SOV-022).** The first publication had `mit/0001`–`0004` and
`agpl/0001`–`0002`. They judged a gateway only on a full-split share. Real XBT stock gateways
throw a coinbaser with a 70-byte script away whole and mine pool-only jobs, so they were never
judged and one mined an invalid block after activation (see gateway-readiness.md §1 and §5).
`mit/0005` and `0006` count pool-only shares on probed or attested coinbasers. `agpl/0003`
moves the C patch to a base that still exists upstream: its old base, FlyTheElephant1 `121edd0`,
was force-pushed away. The new patch builds and passes `./datum_gateway --test` on `a5f28aa`, and
its header lists how the port differs. Apply the whole series, as before: `mit/` then `agpl/`.

### `sov-015`: foreign canaries

This series clusters many gateways fed by one third-party node as one template source.

| `mit/` | `agpl/` | Change |
|---|---|---|
| 0001 | | foreign canaries cluster gateways on one template source |
| 0002 | 0001 | the sidecar's foreign mode fans each own canary's twin out to other gateways' nodes |

### `sov-017`: direct NTA attestations for payees without a gateway

| `mit/` | `agpl/` | Change |
|---|---|---|
| 0001 | | `POST /nta/attest`, the direct NTA path for payees that are not gateways |
| 0002 | | pool: `POST /api/nta/attest` forwards direct attestations to primed |
| | 0001 | `nta-signer`, the reference NTA signer for payees without a gateway |
| 0003 | | count direct NTA tries against the tip Prime is on, not the one claimed |
| 0004 | 0002 | a direct attestation waits up to 1 s for Prime's tip |
| | 0003 | post a tip again (with backoff) when Prime says the key is not a payee yet |

Task IDs (SOV-0xx) name internal work items; [../../staging-summary.md](../../staging-summary.md)
explains what each part is for.
