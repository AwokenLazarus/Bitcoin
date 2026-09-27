# NTA step-3 interaction

*Research note, 2026-09-25. The first Knots build of NTA and how it maps to Knots' "step 3". Later decisions are in [../v1-decisions.md](../v1-decisions.md); the patch is in [../patches/knots-v29.4.2](../patches/knots-v29.4.2/). Task IDs (XBT-0xx, SOV-0xx) and plan names (P-001, P-003) name internal work items of the Lazarus pool.*

Model: this folder's Python files (`xbt_nta.py`, `test_xbt.py`).

## What was built (Knots 29.4.2 patch, inline variant)

Research branch `rnd/xbt-065-nta` on Knots `v29.4.2.knots20260508`, commit `5164677` ([patch 0001](../patches/knots-v29.4.2/)). About 90 lines of C++.

- **Deployment:** a buried deployment `nta` (`Consensus::DEPLOYMENT_NTA`, `NtaHeight`). It is **unscheduled on every network**. Regtest sets it with `-testactivationheight=nta@<h>` and `getdeploymentinfo` reports it.
- **Rule** (`CheckNodeTemplateAttestations`, called from `ContextualCheckBlock` once `nta` is active):
  1. Every coinbase output with `nValue > 0` that is not provably unspendable must pay a key-path taproot script `OP_1 <K>` (else `bad-nta-payee`), with `K` on the curve (else `bad-nta-payee-key`).
  2. For each distinct payee script, in output order, the coinbase carries one zero-value attestation output `OP_RETURN PUSH68("NTA" 0x02 || sig64)`, in the same order. The count must match (else `bad-nta-count`) and the attestation output must carry no value (else `bad-nta-att-value`). The encoding is exact bytes, like the SegWit commitment: a truncated or re-encoded push is just an OP_RETURN, so its payee counts as unattested.
  3. `sig64` is BIP340 by `K` over `TaggedHash("XBT-NTA/attestation", ser(script) || height:i32 || nBits:u32 || hashPrevBlock)` (else `bad-nta-sig`).
- **Size:** the attestation output is 70 bytes, inside the RDTS 83-byte OP_RETURN limit. P2TR payees are 34 bytes, inside the RDTS 34-byte output limit. The demo and the test run with RDTS on (`-rdtsexpiry`).
- **Not in this variant:** the draft's `extra` transaction, the inclusion list (IL), the `vtxa` sidecar with its P2P and IBD messages, and the reserved-slot commitment. The task allowed a minimal version ("carry the attestation in the coinbase OP_RETURN variant first"). A signature carried inline has no room for an IL. What that costs is in [Residuals](#residuals-what-nta-does-not-stop).
- **Mining:** `getblocktemplate`/`generate*` do not build attested coinbases. After activation a node's built-in miner only works for zero-value coinbases. Prime or the gateway builds the coinbase (see [DATUM and Prime changes](#what-datum-and-prime-must-change)).

### Test evidence
The demo script (model tests, then `feature_xbt_nta.py`, then the 3-node regtest) exited 0 on 2026-09-25, with no miners and no leftover listeners on its ports. Blocks are built and solved in Python on BLAKE2b v2 headers (`blake2b@101`, `nta@120`), and there are no CPU miners.

| Model vector (`test_xbt.py`) | C++ reason in `feature_xbt_nta.py` |
|---|---|
| x01 signed payee accepts (OP_RETURN placement) | accepted, beside a SegWit commitment |
| x02 split, each payee signs | accepted; a repeated script shares one attestation |
| x03 gifted attestation (pool signs for a customer's script) | `bad-nta-sig` (the model's `att_key_script_mismatch`/`att_bad_sig`) |
| x04 non-taproot / keyless payee | `bad-nta-payee`, `bad-nta-payee-key` (`att_no_payee_key`) |
| x05 signature doesn't move | wrong height, nBits, tip or payee order → `bad-nta-sig` |
| x06 custody residual | accepted (a pool holding the farm key can sign) |
| x09 commitment count errors | missing or extra attestation → `bad-nta-count`; valued → `bad-nta-att-value` |
| base 03/18/39 unattested payee | `bad-nta-count`; anyone-can-spend → `bad-nta-payee` |
| x07, x08, x10 (slot placement), IL tests (19, 24, 25, 38, …) | not ported: they need the slot/`vtxa` variant |

**3-node demo** (`contrib/xbt-nta/nta_demo.py`, p2p 30601–30603, rpc 30611–30613). A and B enforce `nta@120`. C runs the old rules.
- **Before activation:** B accepts an unattested block (height 118) and all three nodes follow it. A mines an attested block 119.
- **After activation:** B rejects an unattested block at 120 (`bad-nta-count`). C accepts it, relays it, and A and B still refuse it (A marks it `invalid`). A mines attested blocks 120 and 121. **C reorgs off the unattested block onto the attested chain**, where it shows as `valid-fork`. This is the soft-fork property.

## How NTA implements Knots step 3

Step 3, as circulated, "withholds rewards from blocks produced via stratum-to-pool." It has no text, PR or activation height. The chain cannot see a protocol: stratum and DATUM shares look the same once a block is found. What it can see is the **coinbase payees**. NTA turns step 3 into a rule about payees:

> After activation, a block may pay a script only if the holder of that script's key signed for this height, this tip and this difficulty.

Consequences for each way of mining:

| Who mines how | Coinbase today | Under NTA (this patch) |
|---|---|---|
| **DATUM miner** (own node + gateway, own payout key) | Pays the miner's address directly (TIDES) | Valid, **provided the payout address is P2TR and the gateway signs** for each tip. |
| **Stratum hasher, paid in the coinbase** (Lazarus hosted stratum, FPPS/PPLNS in coinbase) | Pool puts the hasher's address in the coinbase | **Cannot be paid.** The hasher has no signer, and the pool doesn't have the hasher's key. This is the "withhold rewards" part, per payee. |
| **Stratum pool that pays only itself** (custodial PPS) | One pool output | Valid: the pool signs for its own key. To the chain it looks like a large solo miner (residual). |
| **Pool holding its farms' keys** | Farm addresses in the coinbase | Valid, but the pool holds keys that can spend the farms' coinbase outputs (custody residual, test x06). |

### Why DATUM is not penalized
- **The rule is about payees, not the finder.** Whoever finds a block, stratum hasher or DATUM gateway, it is valid as long as every paid script is attested. So the worst case the migration plan feared, where one stratum-found block forfeits every DATUM miner's TIDES share, **can't happen** as long as the coinbase builder only lists attested payees. Lazarus controls that (the Prime coinbaser), not the hasher.
- **An unattested payee is fixable before publishing.** The coinbaser leaves a non-signing identity out and carries its TIDES share forward (see TIDES payouts and carry). The block stays valid.
- **DATUM miners already hold their payout keys** (non-custodial by design), so signing is a gateway feature, not a trust change. Stratum customers are the ones who have to change something: run a signer (or a gateway), accept custody, or be paid off-chain.

## Residuals: what NTA does not stop

1. **Custody** (model x06, kept on purpose). A pool that holds a farm's key signs for it. The cost is real: the pool can spend that coinbase output, farms can see this, and it carries regulatory weight. The chain cannot tell it apart from the farm signing.
2. **Self-pay PPS.** A stratum pool that pays only itself is one solo miner. No consensus rule on payees can target it. Step 3 under NTA stops coinbase payouts to hashers who don't sign. It does not stop pooled hashing as such.
3. **Signer without a node** (specific to this inline variant). The signature binds only (script, height, nBits, prev), so a tiny daemon that follows headers and signs every tip is enough. It proves the payee's key is online, not that the payee ran a node or chose a template. The full draft closes most of this with `extra` (a real transaction valid against the previous UTXO set) and the **IL** (the payee's node names transactions that must appear, bodies carried in `vtxa`). Even then the proof is "had a chain view and chose transactions", not "ran `bitcoind`" (draft, *Limitations*).
4. **Offline TIDES members** can't sign fresh attestations, so they aren't paid until they come back. Their carry grows (open item from the dossier).
5. **Taproot-only payees.** Every paid address must be `bc1p…`. Payout addresses that aren't taproot (`bc1q…`, legacy) have to migrate before activation. P2WPKH with an even-y key is a possible later extension.

## What DATUM and Prime must change
- **Gateway:** sign `NtaAttestationHash` for every new tip with the miner's payout key, and send the 64-byte signature to Prime with the next share or frame. Stock `v0.4.1-beta` gateways (most of the Lazarus fleet) can't do this, so they need a gateway release.
- **Prime coinbaser:** build the TIDES split only from identities whose signature for the current tip has arrived, and append their attestation outputs in payee order. Everyone else's share carries forward.
- **Latency on a new tip:** the first jobs on a new tip can pay only the finder's gateway key and the pool key, adding TIDES payees as signatures arrive. Dropping `hashPrevBlock` from the signed digest in v1 (keeping height and nBits) would let payees **pre-sign the next N heights** and remove the per-tip round trip. Without an IL, the tip binding adds little: a signature replayed on a competing branch at the same height pays the same payee.
- **Coinbase size:** 34 + 70 bytes per payee, about 104 bytes. A 100-payee TIDES coinbase grows by about 7 KB of attestations. That is fine for weight, but it is the reason to move to the draft's 38-byte commitment with a `vtxa` sidecar, or the reserved-slot `NTA1` leaf, which costs 0 coinbase bytes and commits every share to the attestation root.

## Activation path
1. **Now (done, regtest only):** the buried `nta` deployment with `-testactivationheight=nta@<h>`, the functional test and the 3-node demo. Unscheduled on mainnet.
2. **v1 spec decisions:** inline vs commitment + `vtxa` vs slot leaf, whether the digest keeps `hashPrevBlock`, IL and `extra` in or out, taproot-only or also P2WPKH. Needs review against the draft (`bip-node-template-attestation.md`).
3. **xbt-lab / signet:** Prime and gateway support (signing, coinbaser), then a mixed run in which a stratum hasher finds blocks with an attested coinbase. Also measure the new-tip signature latency.
4. **Proposal to Knots as the step-3 mechanism:** it replaces "detect stratum" (impossible on chain) with "payees must sign". Not yet sent; the [Knots brief](../knots-brief.md) is the draft.
5. **Activation:** a buried height announced well ahead, like `blake2b`, or a signalled soft fork. As with any soft fork, old nodes follow only if most of the hashrate enforces. Lazarus is about 28–30% of blocks, so it cannot activate this alone. Every paid address must be P2TR by the activation height (the pool's migration timeline gains a "taproot payout address" step).

## Reproduce
Apply [the Knots patches](../patches/knots-v29.4.2/) to `v29.4.2.knots20260508`, build `bitcoind` and `bitcoin-cli` (wallet off; we used `depends`), then run `python3 test/functional/feature_xbt_nta.py` and `contrib/xbt-nta/demo.sh` from the Knots tree, and `python3 test_xbt.py` in this folder.
