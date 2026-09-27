# Sovereignty + NTA integrated

*Research note, 2026-09-25. Sovereign TIDES and NTA on one regtest chain. Task IDs (XBT-0xx, SOV-0xx) name internal work items; the demo harness is described below and the primed changes are in [../../sovereign-tides/patches/primed](../../sovereign-tides/patches/primed/).*

Builds on [Sovereign TIDES](../../sovereign-tides/design.md) (canary detector `9c69abf`) and [NTA step-3 interaction](step-3-interaction.md) (Knots `5164677`).

**Result: both phases PASS** on Knots 29.4.2 + NTA, with `sovereignty-bonus` on in regtest only. Getting there took two small fixes, both on local branches named `rnd/a2-nta`. Without the Knots fix, an NTA chain has no block templates at all.

## Attestation roles: can one attestation serve both?
**No. They answer different questions and are signed by different keys, so a TIDES coinbase carries both.**

| | A2 **LZT1** | **XBT-NTA** (inline v0) |
|---|---|---|
| Question it answers | *Who built this template?* | *Did everyone this block pays agree to be paid, on this tip?* |
| How many per block | **1**: the builder's gateway | **1 per distinct payee script** |
| Signer | gateway key `G` (ed25519, the DATUM session key), registered to a payout address | payee key `K` (BIP340, the P2TR output key itself) |
| Signed message | `A`: height, prev, **branches_hash (every tx and its order)**, **payout_digest (every coinbase output)**, node view | `script ‖ height ‖ nBits ‖ prev`, and nothing about the transactions |
| On chain | 70 B `OP_RETURN "LZT1" ‖ BLAKE2b(A) ‖ G`; `A` and its signature go off chain to Prime | 70 B `OP_RETURN "NTA"02 ‖ sig64` per payee, plus P2TR payees |
| Checked by | Prime (per job), public verifier, explorer | **consensus** (every enforcing node) |
| Stops | a pool lifting a gateway's template onto its own payout; stratum blocks passing as gateway-built | paying anyone whose key didn't sign (stratum hashers, gifted payouts) |
| Doesn't stop | a proxy running its own `G` (**the detector's job**) | a proxy or "signer without a node" holding its own `K` |

Why they can't merge:
- In a TIDES coinbase **many payees sign, but only one party built the template**. A non-builder payee's NTA signature can't attest to a template it never saw, and one LZT1 signature can't stand in for the payees' consent to be paid.
- NTA's digest leaves out the transaction set on purpose, so signatures can be gathered once per tip and reused on every job for that tip. LZT1 has to cover the transactions, so it is per job.
- A partial fold is possible later. In NTA v1, the **builder's** payee signature could carry an optional authorship root (the draft's slot/`extra` direction), which would make the LZT1 output redundant for the finder. That saves 70 B and a key type, but it puts pool policy into consensus. Not recommended for v1 (see [the path to a Knots proposal](#path-to-a-knots-proposal)).

**They do compose cleanly, and the order matters:**
1. Per tip, Prime collects an NTA signature from each payee it will pay.
2. It lays out outputs: payees (P2TR), then the NTA attestations in payee order, then the SegWit commitment.
3. The builder's gateway computes `A` with `payout_digest` over **all of those** and appends its LZT1 output.

So `G`'s signature also binds the NTA signatures and the payout, and NTA ignores LZT1 (a zero-value OP_RETURN is not a payee). Measured coinbase: 3 payees × 34 B + 3 × 70 B NTA + 38 B witness commitment + 70 B LZT1. **RDTS accepts all five OP_RETURN outputs**, each ≤ 83 B.

## How Prime emits NTA-valid coinbases (regtest pool)
The demo plays Prime's coinbaser in Python (`sov_nta_demo.py:build`), with primed doing the scoring and bonus:
- **Payees must be P2TR.** Sovereign `bcrt1p…` (its gateway's payout key), fake `bcrt1p…`, and the pool (`payout-address` = the pool's P2TR). The stratum hasher is P2WPKH with no signer.
- **Split per block:** the TIDES base split, the stratum fee (25%), the DATUM rebate (10 points) and the **bonus credits returned by primed's `/sovereignty/apply-bonus`**. The pool takes the remainder.
- **Once NTA is active for the next block** (`getdeploymentinfo`), the coinbaser drops every payee without a key or a signature and **carries** its share (held by the pool output: custodial carry). It then signs `NtaAttestationHash` with each remaining payee's key and appends the attestations in output order. In production each gateway signs the new tip with its payout key and sends the 64 bytes to Prime (a new DATUM sub-command). Prime only relays them.
- **Needs Knots `8a153ba`:** `getblocktemplate` must survive activation (see [Fixes](#what-had-to-change)).

## The demo
`./sovereignty-nta-demo.sh` (the XBT-071 harness; not included here, it drives the same primed branch as the S0 patches).

- **Nodes:** four NTA Knots nodes on loopback (rpc/p2p 31100–31107), RDTS on, `blake2b@101`, `nta@139` on P, S and X:
  - **P**: the pool's node;
  - **S**: the sovereign gateway's node;
  - **X**: the fake gateway's registered node, which the fake ignores;
  - **O**: an old-rules node.
- **primed** from `rnd/a2-nta` on 31115/31116: `network = "regtest"`, `sovereignty-bonus = true`, `sovereignty-demo = true`, bonus 500 bps, stratum fee 2,500 bps, rebate 1,000 bps.
- **No miner processes.** Blocks are built from each builder's own `getblocktemplate` and solved in Python on BLAKE2b v2 headers.
- **Gateways:**
  - **sovereign** templates from S;
  - **fake** serves P's template 20–80 ms later (a proxy).
  - Both register `G → payout` with a signed `LZT1 register` message, and both put **valid LZT1 attestations** in the blocks they build.
- **Detector input:** templates are polled every 0.25 s and posted live as snapshots. A conflict-pair canary goes to each gateway's node every 2 s (C to S or X, C′ +1 sat to P, and P told `prioritisetransaction C −1e8`). All traffic enters at P, so honest mempools converge on the pool's: the hard case from XBT-064.
- **Work credited per block:** sovereign 300, fake 300, stratum hasher 400.
- **Results:** `results/<UTC timestamp>/` holds `summary.json`, `blocks.json` (every coinbase output), `checks.json`, `sovereignty.json` (primed's final view), `canaries.json`, `demo.log`, `prime.toml` and `versions.txt`. `results/latest` links to the newest.

### Phase 1: pre-activation (blocks 132–138)
The builders rotate sovereign, pool, fake. Figures are from run `20260925T041942Z`:

| Check | Result |
|---|---|
| All 7 blocks accepted by P, S and X | PASS |
| LZT1 verifies in primed for every gateway-built block, **the fake's too** | PASS (5/5, score 100) |
| Canary detector | **sovereign 30/30** (independence 1.0, not flagged); **fake 0/30** (independence 0, **flagged**, identical to pool 1.00) |
| Bonus | sovereign **≈100.0M sats per block** (5% of the stratum-work value of a 50 XBT regtest block); **fake 0 in every block** |
| Coinbase | hasher P2WPKH 15.0 XBT · sovereign 17.0 (15 base + 1 rebate + 1 bonus) · fake 16.0 (15 + 1 rebate) · pool 2.0 (fee 5 − rebate 2 − bonus 1) |

### Phase 2: from `nta@139` (blocks 139–142)
| Check | Result |
|---|---|
| Pool-built block from the **old coinbaser** (pays the hasher's P2WPKH) | P, S, X: **`bad-nta-payee`** |
| Pool-built block, all-P2TR but **no NTA attestations** | P, S, X: **`bad-nta-count`** |
| O (old rules) accepts the old-coinbaser block, relays it | P, S and X keep refusing it |
| 4 attested blocks (sovereign, pool, fake, sovereign): 3 payees, 3 NTA attestations, and LZT1 where gateway-built | all accepted by P, S and X |
| Bonus intact | sovereign ≈100.0M sats in **every** block, fake 0; the fake stays flagged (0/56), the sovereign clear (56/56) |
| Hasher | never paid on chain after activation; **60.0 XBT carried** over 4 blocks, held by the pool's output |
| Soft fork | O reorgs onto the attested chain; on O the un-attested block is `valid-fork`, on P it is `invalid` |
| Canary leaks into pool templates | 0 |

### Stability
Three consecutive runs of the exact verify command, all exit 0, `scoped miners left: none`, and nothing left listening on 31100–31199:

| Run | Result | Phase 1 canaries sov / fake | Phase 2 canaries sov / fake | Sovereign bonus, block 138 (sats) | Enforcing nodes' rejections | Hasher carried (sats) | Final height |
|---|---|---|---|---:|---|---:|---:|
| 20260925T041942Z | PASS | 30/30 / 0/30 | 56/56 / 0/56 | 100,002,076 | bad-nta-payee, bad-nta-count | 6,000,100,030 | 142 |
| 20260925T042208Z | PASS | 29/29 / 0/29 | 58/58 / 0/58 | 100,002,004 | bad-nta-payee, bad-nta-count | 6,000,135,073 | 142 |
| 20260925T042426Z | PASS | 30/30 / 0/30 | 58/58 / 0/57 | 100,000,736 | bad-nta-payee, bad-nta-count | 6,000,115,391 | 142 |

An earlier run (`20260925T041603Z`) passed every property check, with the sovereign at 27/32. It failed only on a peer-count assertion that was too strict, since removed; the sync checks cover connectivity.

## What had to change
1. **Knots `8a153ba` (`rnd/a2-nta`, on `rnd/xbt-065-nta`): `getblocktemplate` keeps working after activation.**
   - After `nta@<h>`, every template failed `CreateNewBlock`'s `TestBlockValidity` with `bad-nta-payee`. The placeholder coinbase pays the node's own script, and the node holds no payee key. Prime and every DATUM gateway build from GBT, so **an NTA chain had no templates at all**. The XBT-065 demo missed this because it built its blocks without GBT.
   - Fix: when NTA is active, check the transaction set against a **zero-value placeholder coinbase**. `coinbasevalue` doesn't change. `generate*` blocks are still refused on submission (`block not accepted`).
   - `feature_xbt_nta.py` asserts both. It passes (ports 31140+).
2. **primed `608e926` (`rnd/a2-nta`, on `rnd/a2`): credited work takes the gateway's latest independence.** `credit_authored` kept the independence seen at a gateway's *first* credit. Work credited before the canary evidence arrived stayed at 0 forever, and a gateway flagged later kept its old weight. It now refreshes on every credit, like `note_share`. There's a new unit test, `credited_work_takes_the_latest_independence`.

## Residuals
1. **NTA and LZT1 both let the proxy through; only the detector catches it.** The fake holds its own `K` and `G`, so its blocks are consensus-valid and "attested", and its base TIDES share is paid. The three layers depend on each other: NTA says the payees consented, LZT1 names who built the template, and the canary detector shows whether that builder's node chose the transactions. Only the bonus is withheld from a proxy, never its base pay.
2. **The public score counts the flagged fake as verified.** The score was **100** with a flagged fake among the builders. The score should discount builders the detector flags or clusters with the pool, or at least report them. Tie `score::summarize` to the detector's clusters.
3. **The coinbaser is still Python.** Primed has no NTA coinbaser yet. It needs:
   - per-tip payee-signature collection (DATUM sub-command);
   - P2TR-only payout identities;
   - carry for non-signers;
   - the first jobs on a new tip paying only payees already signed.
   Tip latency is unmeasured (the NTA note suggests dropping `prev` from the digest so payees can pre-sign).
4. **Canary issuance is still harness-driven** (the Python plays Prime), and live DATUM jobs still lack per-job txid lists: the same two blockers as XBT-064.
5. **Custodial carry for stratum hashers.** After activation a keyless hasher can only be paid off chain, or by running a signer or a gateway. Its carry (60 XBT here) sits in the pool's coinbase output. This is step 3's "withhold rewards" working as intended, but it's a custody and trust cost for the hasher, and it needs an off-chain payout path.
6. **Registration is signed by `G` only.** BIP322 binding to the payout address is still missing (XBT-064 hardening). `min-key-age` is 0 in the demo.
7. **Demo simplifications:**
   - `apply-bonus` is priced on P's template value; the pool output absorbs the difference.
   - The sovereign's regtest miss rate is 0–16% (blocks every 8–12 s).
   - Regtest subsidy makes the bonus look huge (1 XBT per block). On mainnet it is the same 5% of the stratum-work value.

## Path to a Knots proposal
Only NTA goes to Knots. A2 (LZT1, the score, the detector and the bonus) is pool policy and stays in primed. What this task adds to the case from [NTA's activation path](step-3-interaction.md#activation-path):
1. **The GBT fix is part of the patch** (`8a153ba`), with its functional test. Without it no pool can mine an NTA chain. Also document that `generate*` can't mine after activation (nodes hold no payee keys).
2. **Evidence that a DATUM/TIDES pool can live under NTA.**
   - A multi-payee coinbase with payee signatures, the pool's own commitments (LZT1, SegWit) and RDTS limits works on a real Knots node.
   - Non-signers are carried, not lost.
   - Honest DATUM miners are never penalized: the sovereign kept its pay and its bonus.
   That is the "step 3 without hurting DATUM" argument, now shown end to end.
3. **Spec questions this sharpens for v1:**
   - Keep the transaction set out of the NTA digest (it lets signatures batch per tip; authorship stays pool-side).
   - Drop `hashPrevBlock`, or keep it?
   - Taproot-only payees, or also P2WPKH?
   - Should the builder's signature optionally commit to an authorship root (see [the merge question](#attestation-roles-can-one-attestation-serve-both))?
4. **Next evidence:**
   - move the coinbaser and payee-signature collection into primed, and add a gateway signing release;
   - a mixed run in **xbt-lab** or signet with real `datum_gateway`s and a stratum hasher, measuring the new-tip signature latency;
   - a mainnet shadow run of the detector (the XBT-064 gate).
5. **The proposal has not been sent.** The [Knots brief](../knots-brief.md) is the draft.
