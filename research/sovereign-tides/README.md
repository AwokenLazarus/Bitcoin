# Sovereign TIDES

A pool-side upgrade for DATUM pools on XBT that makes **who built a block's template**
checkable, scores pools on it in public, and can pay miners who build their own templates
a little more. No fork is needed. It is designed to sit next to the
[NTA soft fork draft](../node-template-consensus/), which answers a different question
(did every payee consent to be paid).

Research, published for discussion. **Nothing here is deployed**, and the bonus is off.

## How it works

1. **Template attestation (LZT1).** For each job, the gateway signs a 185-byte statement
   `A` with its DATUM identity key `G`: height, parent, the merkle branches (every
   transaction and its order), the coinbase value, and a digest of the coinbase outputs.
   The coinbase carries `OP_RETURN "LZT1" ‖ BLAKE2b(A) ‖ G` (70 bytes, under the RDTS
   83-byte cap); `A` and the signature go to the pool. Prime checks it once per job; anyone
   can check a published block. A pool can't copy a gateway's template onto its own payout
   (the output digest), replay it on another job, or forge it.
2. **Canary detector.** An attestation proves a key approved a template, not whose node
   chose the transactions. So the pool sends each registered gateway's node its own
   conflicting transaction (a *canary*) that the pool's node will never template. An
   honest gateway's templates carry it; a proxy of the pool's template never does.
   *Shared* canaries and *decoys* catch hybrids that mix the pool's template with their
   own mempool; *foreign* canaries catch many gateways fed by one third-party node.
3. **Public sovereignty score.** Per pool, over a window: attested blocks count fully,
   blocks claimed by a DATUM tag count half. Published with builder counts, effective
   builders and a **template Nakamoto coefficient** next to the usual pool one.
4. **TIDES bonus** (off). Attested DATUM work weighted by independence can get extra TIDES
   credit, funded from the stratum fee, conserving value; proxies earn 0.

Details: [design.md](design.md). How it was made safe for a production pool and the
rollout stages: [staging-summary.md](staging-summary.md).

## Status

| Part | Status |
|---|---|
| LZT1 format, verifier, gateway signing | built; the Rust gateway and a patch to the C gateway emit it; delivered over DATUM (`0x5A`) on regtest |
| Canary detector (own, shared, decoy, foreign) | built; regtest: honest gateways never flagged, proxy / mimic / both hybrids flagged 3/3, third-party-fed gateways clustered 3/3 |
| Pool-safe rework (S0) | built and verified on regtest (10 suites); 72 h soak in progress |
| Registration v1/v2 with BIP322 | built, cross-checked against Knots-signed vectors |
| NTA signing and attested coinbases | built (regtest), with gateway readiness detection |
| Public score | computed from real chain data (network score 15.3 over 7 days, 0 attested blocks: nobody emits LZT1 yet); not published by the pool |
| Bonus | designed and unit-tested; **off**, and a separate decision |

## Not proven

- **Mainnet calibration.** The detector was only run on regtest, where mempools diverge
  far more than on mainnet. Honest miss rates, cadence and thresholds need a shadow run on
  real gateways (stage S3).
- **Real network conditions.** Everything ran on loopback. Canary delivery over Tor is
  tested only at the framing level. The C gateway patch ran live only on regtest.
- **A farm that tailors each gateway's template on conflicts** is not caught.
- **Self-reported fields** in `A` (mempool digest, node tag) are not trusted by the
  detector, and prove nothing on their own.
- The score cannot see other pools' gateways beyond tags and attestations; they get no
  score.
- The bonus economics were checked by fuzzing conservation, not by running them with money.

## Contents

| Path | What |
|---|---|
| [TRY-IT.md](TRY-IT.md) | exact commands: apply the patches to `main`, build, test, run the regtest demos |
| [design.md](design.md) | the design and prototype record, with later updates inline |
| [staging-summary.md](staging-summary.md) | the pool-safe rework (S0) and the stages after it |
| [tools/](tools/) | Python: score over real chain data, LZT1 verifier, detector, RDTS test, unit tests |
| [demos/](demos/) | regtest harnesses: hybrid proxy (sov-004), canary sidecar (sov-008), foreign canaries (sov-015), the S0 verify driver (sov-010) |
| [results/](results/) | score tables, detector runs, demo evidence (regtest and public chain data only) |
| [patches/primed/](patches/primed/) | the Prime / gateway / sidecar changes as `git format-patch` series that apply to this repo's `main` at `f14bb4c` (MIT for `prime/` and `pool/`, AGPL-3.0 for `lazarus/`); see its README |

Task IDs (SOV-0xx, XBT-0xx) in these files name internal work items of the Lazarus pool.

## Try it

[TRY-IT.md](TRY-IT.md) has the commands. In short, on the commit that published this folder:

```sh
P=research/sovereign-tides/patches/primed
git am $P/mit/s0/*.patch && git am $P/agpl/s0/*.patch
(cd prime && cargo test --workspace) && (cd lazarus && cargo test --workspace)
WT=$PWD KNOTS=/path/to/knots/build/bin/bitcoind research/sovereign-tides/demos/sov-008/canary-sidecar-demo.sh
```

## License

Copyright (c) 2026 Mike Moore (AwokenLazarus). The documents and results are
[CC BY 4.0](https://creativecommons.org/licenses/by/4.0/). The tools and demos are MIT. Patches to
`prime/` and `pool/` are MIT, and patches to `lazarus/` are AGPL-3.0, because `lazarus/` is derived
from Ratum. Details and the attribution line to use: [LICENSE.md](LICENSE.md).
