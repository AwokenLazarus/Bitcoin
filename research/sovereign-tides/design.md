# A2 Sovereign TIDES: template attestations, sovereignty score, entropy detector, TIDES bonus

*Design and prototype record, 2026-09-25, with the SOV-004 (2026-09-25) and SOV-015 (2026-09-26) updates inline. Task IDs (XBT-0xx, SOV-0xx) and plan names (P-001, P-003, A1–A4) name internal work items of the Lazarus pool. How this became hub-safe code is in [staging-summary.md](staging-summary.md); the code is in [patches/primed/](patches/primed/).*

Builds on items C.2 #2 and #5 of an internal research note on template sovereignty (2026-09), and merges the "verifiable TIDES" idea of a share-chain study (A4). Inputs: the pool's DATUM rebate and OCEAN's TIDES payout scheme.

**Verdict: keep. It's a pool-only upgrade.** Every piece runs on today's chain with no fork:
- a 70-byte coinbase commitment;
- a Prime-side check;
- a public score;
- a detector that turns Prime's job telemetry into an independence number;
- a TIDES bonus paid from the stratum fee.

The data format is chosen so it can later be promoted to an NTA-style consensus rule (the note's idea 8 / Knots "step 3") unchanged.

## 1. What changed from the note's sketch (and why)
| Note C.2 #2 | This design | Why |
|---|---|---|
| `OP_RETURN PUSH68 "LZT1" ‖ BLAKE2b(A) ‖ G`, 70 B | **Same** | Measured: under RDTS, Knots 29.4.2 accepts a coinbase OP_RETURN scriptPubKey of **≤ 83 bytes** and refuses 84+ (§2). My first cut put a 64-byte signature on chain (103 B). **That would be an invalid block**, so the signature stays off chain as the note had it. |
| `A` = {v, G, height, prev, branches_hash, coinbase_value, n_tx, mempool_digest, node_tag, t_gbt} | **Adds `payout_digest`** (SHA256d of the coinbase outputs minus the commitment) | Without it, a pool that sees a gateway's `A` (Prime does) can copy the template *and* `A` into a coinbase paying itself, and the block still "verifies". With it, the template can be copied but the payout can't. Test: `lifting_a_gateways_attestation_onto_another_payout_fails`. |
| `G` x-only, Schnorr | `G` = the gateway's **DATUM identity key (ed25519)** | Prime already authenticates every session with this key, so "attestation key = session key" costs nothing new. It's the same 32 bytes on chain. BIP340 is a crate swap if the note's choice wins. The BIP322 binding of `G` to a payout address is unchanged (registration, §3). |
| one `A` per job | one `A` per **job × coinbase variant** | A gateway publishes up to 6 coinbase size classes per job, each with different outputs, so each needs its own `payout_digest`. |
| TIDES multiplier ×(1+β) | A **pot**: β = pot ÷ attested work's value, pot ≤ stratum fee − rebate | A raw multiplier inflates attested work inside the window, so every other miner pays for it. A pot is funded by the stratum fee alone, as the note intends, and conservation is testable. |

## 2. RDTS: largest coinbase OP_RETURN (settled on regtest)
[`tools/rdts_opreturn_test.py`](tools/rdts_opreturn_test.py):
- Two isolated Knots **v29.4.2.knots20260508** nodes (temp datadirs, loopback only, spare ports 19581–19583, no miners).
- Each block is built with `generateblock … submit=false`, an N-byte OP_RETURN is spliced into the coinbase, the BLAKE2b header v2 is re-ground (XBT-050's `xbtpow`), then the block is submitted to A and B is checked for it over P2P.

| Coinbase output scriptPubKey | RDTS on (`-rdtsexpiry` set) | Default regtest (RDTS off) |
|---|---|---|
| **Controls**: P2PK 35 B, bare multisig 37 B | refused `bad-txns-vout-script-toolarge` | accepted |
| OP_RETURN 38, 70 (**LZT1**), 77, 80, 81, 82, **83** B | accepted, became tip, **relayed to B** | accepted |
| OP_RETURN **84**, 85, 103 B | refused `bad-txns-vout-script-toolarge` (not relayed) | accepted |
| Mempool tx with the same OP_RETURN (`testmempoolaccept`) | ≤ 83 allowed, ≥ 84 `scriptpubkey` | same |

So the RDTS output cap is **consensus and applies to coinbase outputs**. The limit is **83 bytes of scriptPubKey** (`OP_RETURN OP_PUSHDATA1 80 <80 bytes>`), which leaves 80 bytes of payload. LZT1 uses 70 and has 13 to spare. Watch out: on regtest, RDTS is **off unless `-rdtsexpiry` is set**, so a test without it passes things mainnet refuses.

**Magic collision with A4.** A4's verifiable-TIDES sketch also calls its window-root output `LZT1`. Proposal:
- `LZT1` = gateway attestation (this doc).
- `LZW1 ‖ window_root(32) ‖ epoch(8)` = Prime's window commitment: 44 B payload, a 46 B script.

Both fit in one coinbase: 70 + 46 bytes of script, about 134 bytes of outputs. Better still, carry the window root inside `A`, since Prime puts it in the coinbaser and the gateway signs over it via `payout_digest`, so one output does both jobs.

## 3. The commitment
**Registration (off chain, once per key).** The gateway operator sends Prime:
- `G`;
- a BIP322 message signed by the payout address: `"LZT1 register G=<hex> pool=lazarus"`;
- a node tag.

Prime publishes a directory of `G → {payout address, first_seen, house?}`. House keys (lazarus-gateway, any gateway the pool runs) are **listed publicly and never score or earn**. A key earns the bonus only after `min_key_age` (e.g. 1 TIDES window), so a fresh key can't hit and run.

**Per job and coinbase variant (gateway).** The gateway builds `A`:
```
v(1) | G(32) | height(4) | prev_hash(32) | branches_hash(32) = BLAKE2b(merkle branches)
| coinbase_value(8) | n_tx(4) | mempool_digest(16) | node_tag(16) | t_gbt(8)
| payout_digest(32) = SHA256d(outputs without the commitment)            (185 bytes)
```
It then:
- signs `"XBT-SOVEREIGN-TIDES/1\0" ‖ A` with `G`;
- adds `OP_RETURN 0x44 "LZT1" ‖ BLAKE2b(A) ‖ G` (a 0-value output, anywhere) to that variant's coinbase;
- sends `A` and the signature to Prime with the job. That needs a new DATUM sub-command: a gateway patch, like `lazarus-split`.

**Prime.** Once per job and variant, when it absorbs the coinbase, Prime checks, in this order:
1. The commitment is present and well formed.
2. `BLAKE2b(A)` and `G` match it.
3. `G` is this session's identity key.
4. The signature is valid.
5. `prev_hash`, `height` and `branches_hash` equal the job's.
6. `payout_digest` equals this coinbase's outputs.

The cost is one ed25519 verify per job, not per share. Every share on that job is then **attested work**. The coinbase still classifies as a full `Split`: the commitment is a 0-value OP_RETURN, which `classify_coinbase` already tolerates (test `prime_checks_a_real_share_on_an_attested_job`). On a block, Prime publishes `A` and the signature, and the explorer shows them.

**Public verifier.** Anyone can check a block from the block alone:
- rebuild the coinbase merkle branches from its txids;
- run checks 1, 2 and 4–6;
- look `G` up in the directory.

`sovereignty.py` does this and cross-checks against the Rust implementation (14/14 fixture cases, §8).

**What a valid attestation proves.** Whoever holds `G`, a key registered to payout address P, approved *exactly* this tx set, in this order, on this parent, with this payout, before the block existed.

**What it cannot prove:**
- **Whose node chose the tx set.** A farm or pool can run the gateway and node "on behalf of" P (gifted attestations). The detector (§5) is the answer, plus the observation that running a real node is the outcome we want anyway.
- **That `mempool_digest` and `node_tag` are honest.** They're self-reported. They help the detector, and lying about them costs nothing but also buys nothing: the detector's strong features (lead, containment) don't trust them.
- Anything about blocks from other pools unless they adopt it. Their score stays tag-based (§4).

**Attacks**

| Attack | Result |
|---|---|
| Replay `A` onto another job | Bound to prev, height and branches → `WrongTemplate` |
| Pool lifts gateway's `A` + template onto its own payout | `payout_digest` → `WrongPayout` |
| Edit `A` after signing (e.g. node_tag) | `BLAKE2b(A)` ≠ commitment → `CommitmentMismatch` |
| Forge `A` naming someone else's `G` | Signature fails |
| Put someone else's `G` in your coinbase | Prime: `WrongKey` (session key); public: signature fails |
| Gifted attestations (farm/pool runs node + key for P) | Not preventable. Detector scores it ~0 if it tracks the pool's node (§5) |
| One `G` shared by many farms | Optional per-`G` cap on the bonus pot, plus clustering in the score |
| Many `G`s on one node (Sybil) | Bonus is linear and pro rata: no gain. Score counts one builder per detector cluster |
| Key hopping to dodge a bad independence score | `min_key_age`, and unknown keys score 0 until they have ≥ 20 comparable snapshots |
| Privacy | `G` is new public data. The gateway name is already public via the DATUM secondary tag |

## 4. The sovereignty score
**Per block**, a template's builder is classified by rule `f47db50`, the same rule as XBT-047's recount:
- **gateway**: a DATUM secondary tag naming someone other than the pool;
- **pool**: no secondary tag, or one naming the pool itself (its name, slug, primary tag, matcher regexes, or containing one of them ≥ 5 chars, e.g. `buy hashrate @ pow.re`, `xorpool.com`; generic `DATUM`/`DATUM User` excepted);
- **unattributed**: the explorer can't name the pool.

A block is **verified** when it carries a valid LZT1 attestation by a non-house key.

**Per pool over a window:** `score = 100 × (verified + ½·(claimed − verified)) ÷ blocks`.
- *claimed* = gateway-built blocks. The tag is unauthenticated, so a claim counts half.
- A pool with no attestations maxes out at 50. Only attestations reach 100.

It is published alongside:
- builder count;
- **effective builders** (1/HHI over builders, the pool's own builds counting as one);
- top-builder share;
- median explorer `matchRate` for pool-built vs gateway-built blocks. This is an independent hint: low match means a template the explorer's node wouldn't have built.

**Network:** the same score, plus the **template Nakamoto coefficient** (fewest builders holding > 50% of blocks) next to the usual pool Nakamoto coefficient. That makes the effect of DATUM visible.

**Data sources**, in order of strength:
1. LZT1 attestations (coinbase + published `A`);
2. DATUM secondary tags in the coinbase scriptSig;
3. explorer attribution (pool tags and addresses);
4. explorer `matchRate`;
5. (Lazarus only) Prime's detector output per gateway.

### Today's baseline (real chain, read-only)
`sovereignty.py fetch` ran GETs against `mempool.lazarus-xbt.xyz/api`: `/v1/blocks`, `/block/:hash/txs/0` for **every** coinbase's outputs, and `/v1/mining/pool/:slug`, at 0.15–0.2 s spacing, on 2026-09-25 ~02:00–02:15 UTC. **No LZT1 commitment exists on chain** (all 1,307 coinbases scanned), so today's verified share is 0 everywhere and every score is tag-based.

**7 days, blocks 972,713–974,019 (1,307 blocks):**

| Pool | Blocks | Gateway-built | Pool-built | Claimed % | **Score** | Builders | Eff. builders | Top builder % | matchRate pool / gw |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---|
| AlphaPool | 453 | 76 | 377 | 16.8 | **8.4** | 8 | 1.43 | 83.2 | 98 / 96 |
| Lazarus | 333 | 238 | 95 | 71.5 | **35.7** | 33 | 5.54 | 28.5 | 100 / 98 |
| DxPool | 161 | 1 | 160 | 0.6 | **0.3** | 2 | 1.01 | 99.4 | 98 / 100 |
| B2Pool | 57 | 1 | 56 | 1.8 | **0.9** | 2 | 1.04 | 98.2 | 96 / — |
| Bitcoin Xor | 51 | 16 | 35 | 31.4 | **15.7** | 3 | 1.79 | 68.6 | 97 / 96 |
| CONVOY | 40 | 9 | 31 | 22.5 | **11.2** | 8 | 1.64 | 77.5 | 96 / 99 |
| PyBLOCK | 30 | 7 | 23 | 23.3 | **11.7** | 6 | 1.66 | 76.7 | 97 / 89 |
| Quai Network | 27 | 0 | 27 | 0.0 | **0.0** | 1 | 1.00 | 100.0 | 99 / — |
| Omega Pool | 21 | 3 | 18 | 14.3 | **7.1** | 3 | 1.34 | 85.7 | 96 / 95 |
| RIPTIDE | 19 | 14 | 5 | 73.7 | **36.8** | 6 | 4.35 | 31.6 | 100 / 100 |
| Catbus | 11 | 11 | 0 | 100.0 | **50.0** | 2 | 1.42 | 81.8 | — / 69 |
| 30 more pools (2 unattributed) | 100 | 36 | 62 | | | | | | |

**Network (7 d): score 15.3.**
- Gateway-built 401/1,307 (30.7%), pool-built 904, unattributed 2, attested 0.
- 41 pools: pool Nakamoto **2**, effective 4.84.
- 108 template builders: template Nakamoto **4**, effective 8.65.

**Last 24 h (973,866–974,019, 154 blocks): score 32.1.**
- Gateway-built 99/154 (64.3%); template Nakamoto **7**, effective 15.8; pool Nakamoto still 2.
- AlphaPool jumped to 50/53 gateway-built (score 47.2), with one gateway, `Tofu Toes`, building 26 of AlphaPool's 53 blocks.
- Lazarus: 30/41 (36.6).

**Readings**
- DATUM already doubles the template Nakamoto coefficient compared with pools (4 vs 2 over a week; 7 vs 2 today). This is the headline a public dashboard should show.
- Tags can't tell `Tofu Toes` (26 of AlphaPool's blocks in a day) from AlphaPool's own infrastructure under another name. That is exactly the case the detector and attestations address.
- `DATUM User` (the stock default tag) collapses many gateways into one "builder". That under-counts diversity, which is the conservative direction.
- Catbus blocks carry the primary tag `dxpool` with gateway names `xxxbtcb2`/`dxgw`, and their `matchRate` is the lowest (69). These are blocks the explorer's node would not have built, a hint of independent policy. Attribution there needs a human look.

Tables: [`results/score-7d.md`](results/score-7d.md), [`results/score-1d.md`](results/score-1d.md) (JSON beside them).

## 5. Template-entropy detector (the "fake DATUM" check)
Prime already sees every gateway's jobs: prev hash, merkle branches and timing. It polls its own node too. From those it computes, per gateway, against the pool node's templates:

| Feature | What it catches |
|---|---|
| identical: branches equal a pool template (same parent, last 3 s) | a straight proxy |
| branch_sim (b10c weighting 2^i) and tx-set Jaccard | near-copies (reported) |
| switch lag and lockstep with the pool's parent switches | proxies on a WAN (reported, **not scored**: on a LAN, honest nodes also switch in lockstep) |
| tx-inclusion lag | a copy is always behind (reported) |
| **lead**: share of its txs it templated before the pool's node did | **a copy can never be ahead of its source** |
| **containment**: share of each template's txs the pool's recent templates already had | a copy has nothing of its own |

The score:
- `follower = containment × (1 − min(1, lead/0.25))`
- `independence = 1 − max(identical, follower)`
- Fewer than 20 comparable snapshots scores 0: an unknown gateway earns no bonus.

Clustering (identical ≥ 80% both ways) groups gateways into builders for the public score.

**Regtest verification (the note's "verify").** `entropy_regtest.py` ran 4 isolated Knots 29.4.2 nodes with RDTS on:
- **P**, the pool node, which finds a block every 12–20 s;
- **H1** (default policy), **H2** (`-datacarriersize=0`) and **H3** (`-blockmintxfee` 5 sat/vB), peered P–H1–H2–H3–P.

Traffic was P2WSH-OP_TRUE spends at mixed fees, 30% carrying an OP_RETURN, sent to random nodes. Every gateway was polled every 0.5 s for 180 s (360 snapshots each, 13 blocks). Three dishonest gateways were derived from P:
- **PROXY**: P's template, 20–80 ms later;
- **MIMIC**: P's template shuffled, 0.3–1.5 s later;
- **HYBRID**: P's template plus up to 2 txs from H1's mempool, 0.2–0.6 s later.

| Gateway | Identical | Branch sim | Jaccard | Lockstep | Lead | Containment | **Independence** |
|---|---:|---:|---:|---:|---:|---:|---:|
| H1 honest | 0.03 | 0.09 | 0.42 | 1.00 | 0.37 | 0.68 | **0.97** |
| H2 honest, strict | 0.03 | 0.06 | 0.30 | 1.00 | 0.50 | 0.61 | **0.97** |
| H3 honest, high min fee | 0.03 | 0.07 | 0.35 | 0.91 | 0.38 | 0.66 | **0.97** |
| PROXY | 1.00 | 1.00 | 1.00 | 1.00 | 0.00 | 1.00 | **0.00** |
| MIMIC (reorder + delay) | 0.33 | 0.31 | 0.95 | 0.00 | 0.00 | 1.00 | **0.00** |
| HYBRID (copy + own txs) | 0.07 | 0.36 | 0.64 | 0.00 | 0.16 | 0.84 | **0.69** |

This table is the logged run in [`results/entropy_result.md`](results/entropy_result.md). Across three 180 s runs, honest nodes scored **0.91–0.97**, PROXY and MIMIC **0.00** every time, and HYBRID **0.51–0.69**.
Clusters: `[POOL, PROXY]`, and every other gateway alone.

**Limits, stated plainly**
- **HYBRID half-evades (0.51–0.69).** A proxy that also runs a mempool node and slips in a few of its own transactions looks partly independent. It had to run a node to do it, which is partial success for the program, but the scoring should not reward it this much. The next step is calibration on **mainnet Prime job logs**; lab mempools diverge far more than mainnet ones do, so tuning `lead_ref` and the containment scaling here would overfit.

**Update (SOV-004, 2026-09-25):** superseded for pool-fed hybrids.

> **Hybrid proxy (residual #1): closed for pool-fed templates (SOV-004).**
>
> A hybrid runs the registered node L but builds from the pool's template T merged with L's mempool. It carries its own canaries, because it takes its node's side of the conflict, so own canaries alone pass it. Two more canary kinds, each one tx per round for all gateways, close that gap:
> - **Shared canaries.** A tx sits in the pool node's mempool and relays on its base fee. The pool node pre-prioritises it under its `-blockmintxfee` and runs `-blockprioritysize=0`, so the tx never enters T. Every honest node templates it. A hybrid that follows the pool's exclusions misses it.
> - **Decoys.** A zero-fee tx is prioritised on the pool node alone, so it is in T and below every peer's relay floor. No honest node ever has it, and any template carrying it came from T.
>
> Together the three kinds force a hybrid's tx set to T ⊆ L and T ⊇ L, which is its own node's set. What is left for it to follow is tx order, and fewer than 75% of the pool's exclusions.
>
> Regtest, Knots 29.4.2, converged mempools, 3 × 120 s:
> - honest gateways flagged 0/6;
> - merge hybrid 3/3 (decoys);
> - exclusion-following hybrid 3/3 (shared canaries);
> - proxy and mimic 3/3;
> - a hybrid forced to its own node's set was never flagged.
>
> Flagged gateways now earn independence 0. Still open: a hybrid fed by a **third party's** node, where Prime can't place decoys or exclusions. It isn't the pool's template; if many registered gateways share one source, cross-gateway ("foreign") canaries and clustering are the next step. **Update (SOV-015, 2026-09-26):** multiple gateways fed by one third-party node are now caught with *foreign* canaries. A gateway's canary twin is fanned out to every other registered node within 50 ms; a gateway that carries another gateway's own canary shares its template source. Such gateways are clustered (`shared-template-source`) and split one builder's independence (÷ n); they are not flagged. Regtest: 3/3 clustered, honest 0/6 false clusters, 0/630 foreign canaries carried by honest gateways, relay race lost 0/240 at lags ≤ 50 ms (Knots 29.4.2). Code `rnd/sov-015` 0708b7b ([patches/primed/](patches/primed/), series `sov-015`); demo [`demos/sov-015`](demos/sov-015/). Still open: a farm that tailors each gateway's template on conflicts. Also open: raising the shared-canary bar once mainnet misses (≈ 2%) are measured. Code `rnd/sov-004` bc21f79 (in [patches/primed/](patches/primed/), series `s0`); demo [`demos/sov-004`](demos/sov-004/).
- On loopback every honest node switches in lockstep with P, so timing features can't be scored in a lab. On mainnet they add evidence against WAN proxies.
- Honest Jaccard with the pool node is ~0.3 here and will be far higher on mainnet, where mempools converge. `lead` doesn't depend on convergence: on a random relay graph an honest node is first on a share of txs either way.
- The detector sees only Lazarus's own gateways. Other pools are scored on tags and attestations alone.

## 6. The TIDES bonus
Attested DATUM work gets extra TIDES credit. Like the rebate, it is credited **as carry when a block is found** and reversed on orphan. Two funding sources, both closed-form on window totals (V = block value, T/S = total/stratum work, D_u = unattested DATUM work):
```
from_stratum = floor(V·S/T) × b          b ≤ stratum_fee − rebate  (clamp)
from_diff    = V·D_u/T × u               optional differential on unattested DATUM work
credit_i     = pot × Σ_g (work_ig × independence_g) / Σ (work × independence), capped per G
```
**Interaction with the DATUM rebate** (DATUM rebate mechanics). The rebate stays as it is, spread over all DATUM work. The bonus is the *next slice* of the same stratum fee, spread over attested work only:
- the pool nets `fee − rebate − bonus ≥ 0`, checked by fuzz over 5,000 random windows (`conservation_and_pool_net_hold_across_random_windows`), using the same floored stratum value so rounding can't overspend;
- the advertised uplift becomes `rebate%·S/D + bonus%·S/D_attested`.

**Worked numbers at today's fees.** Stratum 25%, rebate 12.5 points, stratum ≈ 23% of work, DATUM ≈ 77% (pool figures, 2026-09-24). The rebate uplift for all DATUM is 12.5 × 23/77 ≈ **+3.7%**. A bonus of b = 5 points gives a pot of 1.15% of V:

| Attested share of DATUM work | Bonus uplift on attested work | Total uplift (rebate + bonus) | Pool nets on stratum |
|---:|---:|---:|---:|
| 10% | +14.9% | +18.6% | 7.5 pts |
| 25% | +6.0% | +9.7% | 7.5 pts |
| 50% | +3.0% | +6.7% | 7.5 pts |
| 100% | +1.5% | +5.2% | 7.5 pts |

The bonus is largest for early adopters and shrinks as attestation spreads, the same shape as the rebate.

**After stratum is gone (P-001),** S → 0 and so does the stratum-funded pot. The **differential** keeps the incentive alive without any pool income. Unattested DATUM work pays `u` (e.g. 1%), all of it credited to attested work, and the pool keeps none of it: at u = 1% and 20% attested, the uplift is +4%. With nobody attested it is **waived, not owed**.

**Anti-gaming**
- **No deferral.** Unlike `rebate_owed`, a pot with nobody attested goes back to the pool, or isn't charged. An owed pot would grow and hand the first signer a windfall.
- **Linear pro rata**: splitting into identities or keys gains nothing (`splitting_into_sybils_gains_nothing`).
- **Independence weighting**: a proxy (independence 0) earns 0; half-independent work earns half per unit (`a_gateway_that_copies_the_pool_template_gets_nothing`).
- **Per-G cap**, optional: one key can't take more than `cap` of the pot, and the excess goes to the pool, not to rivals, so nobody gains by pushing a rival over the cap (`cap_per_gateway_returns_the_excess_to_the_pool`).
- **House keys never earn; key age; orphan reversal via `carry_delta`.**

## 7. Consensus class and path
- **Now:** pool-only. A 0-value OP_RETURN of 70 B (RDTS-valid), a Prime check, a DATUM sub-command for `A`, explorer publication.
- **Policy:** other pools can adopt LZT1 as is. The score rewards them publicly (reputation, as with b10c/miningpool.observer).
- **Later:** the format promotes to an NTA-style soft fork: "value-bearing payees need an attestation signed by the payee key", with the root in a coinbase OP_RETURN or the reserved header slot (XBT-050's slot findings). That is a candidate text for Knots step 3's "detect stratum-to-pool blocks": a block with no valid attestation is detectable from block data alone.

## 8. Prototype and evidence
- **`rnd/a2`** (the first commits of series `s0` in [patches/primed/](patches/primed/)):
  - `prime/wire/src/authorship.rs`: LZT1 `Attestation` encode/decode, `attest`, `find`, `verify`. 9 tests, including an end-to-end share through Prime's real `verify_with_target` (still a `Split`, attestation checks against the job and session key) and every tamper case.
  - `prime/tides/src/sovereignty.rs`: bonus pot, clamp, differential, independence weighting, per-G cap, no deferral. 8 tests, including the 5,000-window conservation fuzz.
  - `prime/wire/examples/lzt1_fixture.rs`: a deterministic signed fixture.
  - The full `datum-wire` (72) and `tides` (41) suites are green. Nothing in `primed` calls the new modules.
- **[`tools/`](tools/)**:
  - `sovereignty.py`: fetch, score and LZT1 verifier, a Python mirror of the Rust check;
  - `entropy.py` + `entropy_regtest.py`: the detector;
  - `rdts_opreturn_test.py`;
  - `test_a2.py`: 13 tests (real-coinbase tag parsing, own-tag rule, score math, the Rust fixture's 14 cases, detector separation);
  - `demo.sh`.
- **Verify:** `tools/demo.sh` (see [tools/README.md](tools/README.md)). `demo.sh --quick` skips the two regtest runs (~6 min).

## 9. Next steps

*Status 2026-09-27: items 2 and 5 are built in the S0 series (the `0x5A` sub-command, LZT1 in both gateways, BIP322 registration); see [staging-summary.md](staging-summary.md).*

1. **Calibrate the detector on mainnet**: log Prime's per-gateway jobs (prev, branches, txids via `request_short_ids`, time) for 24 h on the production pool (read-only telemetry). Tune `lead_ref` and containment, and decide HYBRID's fate.
2. Specify the DATUM sub-command that carries `A` + sig. Patch `lazarus-gateway` first (house, so it gets a listed house key), then `datum-gateway-split-only.patch`. Run e2e in xbt-lab with a non-loopback gateway, because loopback gateways skip template checks on the rig.
3. Settle the `LZT1`/`LZW1` split with A4's verifiable-TIDES window root, or put the root inside `A`.
4. Explorer: publish `A` per block and add a sovereignty dashboard (per-pool score, template Nakamoto) to the XBT-047 pie/galaxy work.
5. Registration UX: BIP322 signing in the gateway-in-a-box (A3) flow.
