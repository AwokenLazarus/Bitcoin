# XBT profile: what NTA looks like on Bitcoin BLAKE2b

XBT is Bitcoin on BLAKE2b, run by Bitcoin Knots 29.4.x. This page collects the
XBT-specific facts the NTA v1 draft ([bip-node-template-attestation.md](bip-node-template-attestation.md))
depends on: the header and proof of work, the RDTS output limits, the reserved header
slot, and why NTA constrains payees instead of hashers.

All of it was measured on isolated regtest nodes (loopback, no peers outside the test)
against Knots `v29.4.2.knots20260508`, or read from its source.

## 1. Header and proof of work

The v2 header hashes in stages. Only the last stage runs on the ASIC:

| Stage | Computed by | Inputs |
|---|---|---|
| H1 = tagged SHA256 | job builder | version, prev, height, merkle root, time, nBits, tx count, flags, clear bits, H(xor_key) |
| H2 = tagged SHA256("Merge-mining hook") | job builder | H1 ‖ 0³² ‖ **rhs** (the reserved slot, header bytes 132–163) |
| root = BLAKE2b(0⁴ ‖ H2 ‖ extranonce16) | host or pool, **once per job** | Sia-style `coinb1` + extranonce |
| **ASIC**: BLAKE2b over 80 bytes | ASIC inner loop | hidden_prev(32) ‖ nonce(4) ‖ nonce2(4) ‖ time_offset(4) ‖ nonce3(4) ‖ root(32) |
| hash = that ⊕ mask(xor_key) | whoever has the mask | lets a pool hide wins from its hashers |

A Python port of `CBlockHeader::GetHash` is in [`xbt_pow.py`](xbt_pow.py); it matches the
vectors in [`block_header_v2_vector.json`](block_header_v2_vector.json).

**Consequence: no non-outsourceable PoW on this hardware.** The inner loop holds no key.
A key can enter only per job (the slot, the root or the mask), and the job builder, which
is the pool, computes those once. That proves custody of a key, not authorship of a
template. A per-candidate keyed win (two-phase PoW or a VRF over each phase-1 solution)
only makes the pool collect every phase-1 candidate, which is what a share already is;
it taxes solo miners the same; and as a soft fork it lets old nodes and headers-first
sync accept phase-1-only chains 2^k times more cheaply. We studied it and dropped it.
NTA constrains the **payees** instead.

Rough numbers from the study (network 37 PH/s, pool 10 PH/s, ~10 TH/s per box, ECVRF
~60 µs per core, both estimates):

| k | candidates/s per box | VRF cores per box | pool candidates/s | pool WAN, binary / JSON |
|---|---:|---:|---:|---|
| 20 | 0.47 | 0.00003 | 472 | 0.09 / 0.45 Mb/s |
| 28 | 121 | 0.007 | 121k | 23 / 116 Mb/s |
| 34 | 7.7k | 0.46 | 7.7M | 1.5 / 7.4 Gb/s |

## 2. RDTS: output-script limits apply to the coinbase

Knots' RDTS rules cap an output's scriptPubKey. They are **consensus** and they apply to
coinbase outputs too. On regtest they are **off unless `-rdtsexpiry` is set**, so a test
without it passes blocks that mainnet refuses.

Measured by splicing an N-byte `OP_RETURN` into a coinbase, re-grinding the v2 header,
submitting the block and checking that a second node received it over P2P:

| Coinbase output scriptPubKey | RDTS on (`-rdtsexpiry` set) | RDTS off |
|---|---|---|
| Controls: P2PK 35 B, bare multisig 37 B | refused `bad-txns-vout-script-toolarge` | accepted |
| `OP_RETURN` 38, 70, 77, 80, 81, 82, **83** B | accepted, became tip, relayed | accepted |
| `OP_RETURN` **84**, 85, 103 B | refused `bad-txns-vout-script-toolarge` | accepted |
| Mempool tx with the same `OP_RETURN` (`testmempoolaccept`) | ≤ 83 allowed, ≥ 84 refused | same |

So the limit is **83 bytes of `OP_RETURN` scriptPubKey** (`OP_RETURN OP_PUSHDATA1 80
<80 bytes>`, 80 bytes of payload), and **34 bytes** for other outputs.

What NTA v1 fits into that:
- a P2TR payee `OP_1 <32 bytes>` is 34 bytes, exactly the output limit;
- the attestation `OP_RETURN PUSH68("NTA" 0x02 ‖ sig64)` is 70 bytes, 13 under the cap;
- a P2WPKH payee would need its key revealed: BIP340 signature + 33-byte key is 103
  bytes (over the cap), and a 65-byte recoverable ECDSA signature fits (71 bytes) but
  means a second signature scheme in consensus. This is why v1 is Taproot-only.

A demo coinbase with 3 P2TR payees, 3 NTA attestations, the witness commitment and a
70-byte pool commitment (five `OP_RETURN` outputs) was accepted by RDTS-enforcing nodes.

## 3. The reserved header slot (`m_mm_rhs`)

The XBT header carries a 32-byte slot at bytes 132–163, meant as a merge-mining hook.
**Knots 29.4.x does not constrain it.**
- In the source (`v29.4.2.knots20260508`), the slot is serialized, hashed into H2 and
  shown by RPC. `CheckBlockHeader` and the contextual header checks never read it.
  `AreHeaderV2FieldsNull` applies only to v1 headers.
- On isolated regtest, blocks re-ground with non-zero slots (`0xabac…`, `0xff…`) were
  accepted by `submitblock` and became the tip, on Knots 29.4.1rc4 and 29.4.2rc2. The
  29.4.2 final source matches.

So **any rule that requires content in the slot is a soft fork**, and any rule that
changes the PoW is a hard fork.

The original NTA draft plus its XBT profile ([docs/original-draft.md](docs/original-draft.md),
section "XBT profile") proposed putting the NTA commitment in the slot as a tagged leaf:

```
slot     = TaggedHash("XBT slot", MerkleRoot(sorted leaves))
leaf     = TaggedHash("XBT slot leaf", tag(4) || value(32))
NTA leaf = ("NTA1", nta_commit)
```

Because the slot feeds H2, every share of a job would commit to the attestation root, at
no coinbase cost. **NTA v1 does not use the slot.** It carries one signature per payee
inline (see [v1-decisions.md](v1-decisions.md) §1). The slot stays available for the
inclusion-list extension, which needs a sidecar anyway. The model code for the slot
variant is in [`xbt_nta.py`](xbt_nta.py) and [`test_xbt.py`](test_xbt.py).

## 4. Other XBT facts the draft uses

- **Retarget:** XBT keeps Bitcoin's 2016-block retarget, so `nBits` is known for a whole
  period once it starts. That is why dropping `hashPrevBlock` from the digest would let a
  payee pre-sign two weeks of heights at once (see v1-decisions §2).
- **Activation precedent:** the `blake2b` fork itself was a buried height announced ahead.
  NTA's reference patch defines `nta` as a buried deployment with no height on any network.
- **Regtest:** the patch adds `-testactivationheight=nta@<h>`; the demos use `blake2b@101`
  and `nta@120` (or later).
- **Signatures:** the NTA signature is not a transaction signature, so whether Knots uses
  `SIGHASH_UNIFIED` (0x21) for transactions does not affect it.
