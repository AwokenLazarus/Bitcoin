# XBT-NTA v1: spec decisions

*Decision record, 2026-09-25 (payee cap added 2026-09-26). Research, not a maintainer decision.
Task IDs such as SOV-001 or SOV-002 name internal work items; their findings are summarised in this folder.*

Scope: the four open questions in [NTA step-3 interaction](docs/step-3-interaction.md) §Activation path item 2, decided against the built patch (Knots 29.4.2 + `5164677`, GBT fix `8a153ba`; see [patches/knots-v29.4.2](patches/knots-v29.4.2/)) and the original draft ([docs/original-draft.md](docs/original-draft.md)).

**Summary.** v1 is what `5164677` enforces, unchanged:

| # | Question | v1 decision |
|---|---|---|
| 1 | Where the attestation lives | **Inline**: one 70-byte `OP_RETURN "NTA" 0x02 ‖ sig64` per distinct payee |
| 2 | `hashPrevBlock` in the digest | **Keep** it: sign per tip |
| 3 | IL and `extra` | **Out** of v1. Specified as a separate, later soft fork |
| 4 | Payee script types | **Taproot key-path only** (`OP_1 <32 bytes>`) |

No change to the patch is needed. Two open items for the maintainers (a cap on the number of payees, an extension hook) are at the end.

## Cost model used below

Serialized sizes, from the patch's exact encodings:

| Item | Bytes | Weight (coinbase is non-witness, ×4) |
|---|---:|---:|
| P2TR payee output (8 value + 1 length + 34 script) | 43 | 172 |
| P2WPKH payee output, for comparison | 31 | 124 |
| NTA attestation output (8 + 1 + 70) | 79 | 316 |
| **Added per payee by v1** (attestation + P2TR premium over P2WPKH) | **91** | **364** |
| 100-payee TIDES coinbase, added | 9.1 KB | 36,400 WU (0.9% of 4 MWU) |
| 512 payees (the draft's `MAX_ATTESTATIONS`) | 46.6 KB | 186,368 WU (4.7%) |

The research note's "about 104 bytes per payee" counts script bytes only (34 + 70). The serialized figure is 122 bytes per payee, of which 91 are new. Validation adds one BIP340 verification per distinct payee (single-threaded in `ContextualCheckBlock`, not counted as sigops). Signing adds one BIP340 signature per payee per tip.

---

## 1. Where the attestation lives

**Options**
- **A. Inline** (built). Each distinct payee gets a zero-value `OP_RETURN PUSH68("NTA" 0x02 ‖ sig64)`, in payee order, matched by exact bytes like the SegWit commitment.
- **B. 38-byte commitment + `vtxa` sidecar** (the original draft). One `OP_RETURN "NTA" 0x01 ‖ nta_commit`; attestations travel in a BIP144-style vector that old peers strip.
- **C. Reserved-slot `NTA1` leaf** (draft X2). `nta_commit` goes in a TLV leaf in the XBT header's 32-byte `m_mm_rhs` slot (unconstrained by Knots per XBT-050); the attestations still travel in a sidecar.

**Trade-offs**

| | A. Inline | B. Commitment + `vtxa` | C. Slot leaf |
|---|---|---|---|
| Coinbase bytes per payee | 79 (+12 for P2TR) | 0 per payee, 47 once | 0 |
| Where the signatures are | In the block; every peer and old node already relays them | Sidecar: new service bit, `block`/`cmpctblock`/`blocktxn` extensions, `getnta`/`nta` for IBD | Sidecar, same as B, plus slot-tree rules |
| Implementation in Knots | ~75 lines in `validation.cpp` + deployment plumbing (done, tested) | SegWit-class project: serialization, P2P, compact blocks, IBD, undo, relay topology of upgraded peers | B plus header-slot rules, and it spends part of the merge-mining slot |
| Implementation in gateway/Prime | Gateway signs 32 bytes per tip; coinbaser appends outputs | Same signing, plus Prime must hand the sidecar to every block relayer | Same as B, plus the slot root enters the job (every share then commits to it) |
| Pre-signing / latency | Unaffected by this choice (see §2) | Same | Same |
| Signer-without-node residual | Unaffected (see Limitations) | Only closed if the sidecar also carries IL/`extra` | Same as B |
| Data-carrier policy | 70 B OP_RETURN each; fits the RDTS 83 B limit; XBT-071 had 5 OP_RETURNs in one coinbase accepted | 38 B, one output | None |

**The inline cost is permanent.** A later soft fork can only add rules. Once v1 nodes require one inline signature per payee, a v2 that moves signatures to a sidecar still has to carry the v1 outputs, or v1 nodes reject its blocks. So choosing A accepts ~91 B per payee for as long as NTA exists. B or C are only cheaper if they are v1. The research notes read as if a later move to the commitment would save the bytes; it would not, short of a hard fork.

**Recommendation: A (inline).** At DATUM-scale payee counts the cost is under 1% of block weight, and it avoids a SegWit-sized P2P project whose only v1 payload would be 64-byte signatures. B and C only make sense for carrying large data (IL bodies, `extra`), which v1 leaves out (§3). If the IL extension ships, it brings its own sidecar and commitment, with a prefix distinct from both `0x01` (old draft) and `0x02` (v1).

## 2. `hashPrevBlock` in the signed digest

The digest is `TaggedHash("XBT-NTA/attestation", ser(script) ‖ height:i32 ‖ nBits:u32 ‖ hashPrevBlock)`.

**Options**
- **A. Keep it** (built): a signature is valid for one tip only.
- **B. Drop it**, keep height and nBits: a payee can pre-sign heights ahead.
- **C. Drop both `hashPrevBlock` and `nBits`**: a payee can pre-sign any number of heights.

**What dropping actually buys.** XBT keeps Bitcoin's 2016-block retarget (`pow.cpp`; `nPowTargetTimespan` two weeks), so `nBits` is known for the rest of a period once it starts. Under B a payee signs up to 2016 heights once per period (~2 weeks). Under C a payee signs once, ever, for as many heights as it likes.

**Trade-offs**

| | A. Keep prev | B. Height + nBits | C. Height only |
|---|---|---|---|
| Signer must be online | Every block | Once per retarget period, early in the period | Once |
| Payout key | **Hot** on the gateway. The P2TR output key *is* the signing key, so it can also spend the coinbase outputs | Can be cold (batch-sign 2016 digests from a hardware signer) | Cold |
| New-tip latency | The first jobs on a tip pay only already-signed payees (finder, pool); the rest are added as signatures arrive. SOV-002 measures p50/p95 | None | None |
| Offline TIDES members | Unpaid while offline; share carried | Paid if they pre-signed the period | Paid |
| What a stratum pool needs from a hasher | An always-on signer that signs the tip the pool sends | One batch signature every two weeks, e.g. a "sign this" page in a wallet | One signature at sign-up |
| Replay | A signature pays the same payee on a competing branch at the same height. Harmless: it only pays the signer | Same, across branches and within the period | Any height |
| IL extension (§3) | Needs a prev-bound, per-tip signature anyway (feasibility is computed against the parent's UTXO set) | Would reintroduce per-tip signing | Same |

**What prev is for.** It is not replay protection: every replay pays the key that signed. The research note is right that "the tip binding adds little" against replay. Its value is **liveness per block**. It is the only property in v1 that separates a DATUM gateway, which already follows every tip, from a stratum hasher, which does not. Under B or C a stratum pool's hashers satisfy step 3 with a signature every two weeks, or once, and the rule stops almost nothing.

**Recommendation: A (keep `hashPrevBlock`).** This is the built and tested digest, and SOV-002's gateway already targets it byte for byte. It is the only v1 discriminator between "runs tip-following software" and "hashes and waits to be paid", and the IL extension needs it. The costs are real, and they fall on honest miners:
- a hot payout key on the gateway (mitigation: a dedicated payout key, swept regularly);
- per-tip latency (to be measured in SOV-002);
- unpaid offline members (mitigation: carry).

Revisit only if SOV-002 shows p95 new-tip latency that costs meaningful payee coverage, and **only before activation**: dropping a field from the digest after activation makes blocks that v1 nodes reject valid (a hard fork). The same one-way rule applies to §4.

## 3. Inclusion list (IL) and `extra` in v1

**Options**
- **A. Out of v1** (built). v1 is payee consent only. IL, `extra`, carry window and `vtxa` become a separate, later soft fork.
- **B. In v1**: the full draft (IL bodies, `extra` valid against the previous UTXO set, 144-block carry window, `vtxa` sidecar and P2P, caps).
- **C. `extra` only**: the payee proves it had one valid transaction against the parent's UTXO set.

**Trade-offs**
- **B is what gives NTA teeth against template control.** Without an IL, a pool can pay a DATUM miner and still omit that miner's transactions (draft test 41, "pay-and-veto"). With it, paying a miner forces that miner's feasible transactions into the block.
- **B's cost is large and consensus-critical:**
  - a mempool-independent feasibility walk;
  - a 144-block catalog of IL bodies with undo on reorg;
  - `vtxa` relay and IBD for the window;
  - caps (`MAX_IL_WEIGHT` and the others);
  - IL bodies can't fit in an 83-byte OP_RETURN, so B forces §1 option B or C.

  None of this is built in Knots; it exists only in the Python model (55 + 10 tests). Asking maintainers to review this as the first step-3 mechanism is a much bigger ask.
- **C adds little.** A pool can hand its signer any valid transaction to put in `extra`, and `extra` needs the sidecar too.
- **Pre-signing and latency:** an IL signature has to be per tip and needs the payee's own mempool, so B also rules out §2 options B and C.
- **Signer-without-node:** B narrows it to "had a chain view and chose transactions". A pool-supplied signer that forwards the pool's IL still passes, as the draft already notes.

**Recommendation: A (out of v1).** Ship the payee rule first. It is small, built, tested and reviewable, and specify the IL as a follow-up soft fork. It can be added later because it only adds rules. Cost of deferring: the follow-up needs its own per-payee signature over the IL root, because the v1 digest can't be extended in place (see the extension-hook open item). v1 leaves template choice unconstrained, and says so.

## 4. Payee script types

**Options**
- **A. Taproot key-path only** (built): `OP_1 <K>`, with K a valid x-only key; the attestation is BIP340 by K.
- **B. Also P2WPKH.** The script holds only `HASH160(pubkey)`, so the attestation must reveal the key. BIP340 sig + 33-byte key is 6 + 64 + 33 = 103 bytes, over the RDTS 83-byte limit. The alternative is a 65-byte recoverable ECDSA signature (6 + 65 = 71 bytes, fits), which means a second signature scheme and public-key recovery in consensus.
- **C. Anything spendable, with a script-path proof.** Needs arbitrary script evaluation in a coinbase-output context. Out of scope.

**Trade-offs**
- **Migration.** A forces every coinbase payout address to `bc1p…` before activation. Wallet and gateway support for P2TR payout is a precondition; add a "taproot payout address" step to the P-001 timeline. B spares existing P2WPKH payees that migration.
- **Privacy / key exposure.** P2TR exposes the key in the output. With B the attestation reveals the key anyway (it is recoverable from an ECDSA signature and message), so B gives no key-hiding benefit.
- **Cost.** P2TR outputs are 12 B larger than P2WPKH. B's recoverable-ECDSA attestation is 1 B larger than A's.
- **Implementation.** A is one scheme and already built. B adds a second scheme, recovery, a second prefix, and a second test matrix in Knots and in every gateway.
- **Excluded by A.** Script-path-only outputs (NUMS internal key), bare multisig, P2WSH federations. Multi-party payees can still use a MuSig2 aggregate key.

**Recommendation: A (Taproot-only).** Note what this forecloses. The research note and the draft call P2WPKH "a possible later extension", but v1 nodes reject a P2WPKH payee (`bad-nta-payee`), so admitting one later **relaxes** a rule: a hard fork for v1 nodes, not a soft fork. **P2WPKH is "in v1 or never".** The recommendation stays A on that basis: the payees we serve (DATUM gateways) can change their payout address before activation, while a second signature scheme in consensus is permanent.

---

## What v1 does **not** stop

1. **A pool-supplied signer.** A stratum pool can ship a small signer, as a desktop app or inside the miner's control board or proxy. It holds the hasher's payout key and signs whatever tip the pool sends. The hasher runs no node and chooses no transactions, yet is paid in the coinbase. v1 proves "this payee's key was online and consented at this tip". It does not prove "ran a node" or "built the template". **This is the largest gap.** Step 3 under v1 raises the cost of stratum-with-coinbase-payouts (key custody + always-on software); it does not end it.
2. **Template control and censorship.** With no IL, a pool can pay every DATUM miner and still pick every transaction.
3. **Self-pay PPS.** A pool that pays only itself (custodial, off-chain settlement) is one payee and signs for itself: indistinguishable from a solo miner.
4. **Custody.** A pool holding a farm's payout key signs for it (test x06). The chain can't tell.
5. **Who found the block.** By design: a stratum-found block is valid if all its payees attested. That is what keeps DATUM TIDES shares from being forfeited.
6. **Non-signing payees are not paid on-chain.** Offline TIDES members, keyless stratum hashers and non-Taproot addresses are carried or paid off chain. The pool holds that value in the meantime, a custody cost that v1 creates.

## Open items for the maintainers (not decided here)

- **Payee cap.** `5164677` has no `MAX_NTA_PAYEES`. Block size bounds it at roughly 8,000 distinct payees (~1 MB of non-witness coinbase at 122 B each), each needing a BIP340 verification in `ContextualCheckBlock`. That is about 0.4 s single-threaded (an estimate at ~50 µs per verify, not yet measured) for a block that has already passed PoW. A cap of 512, as in the draft, would bound it. It is a one-line rule, but it changes the built patch. **Lean: add it before any public version.**
  **Done (SOV-014):** `MAX_NTA_PAYEES = 512`, reject reason `bad-nta-too-many`, checked before any signature; functional test and vector `x10`.
- **Extension hook.** BIP141 reserved a 32-byte value for future commitments. The v1 digest has none, so the IL follow-up must add a second per-payee signature (~79 B per payee when used). A reserved field would have to be carried per payee (more bytes now) or read from an optional output (more consensus code now). **Lean: no hook in v1.** Pay the extra signature if and when the IL ships.
- **Activation mechanism.** A buried height announced well ahead, like `blake2b`, or signalled. This is Knots' call. NTA can't activate on Lazarus hashrate alone (~28–30% of blocks).
