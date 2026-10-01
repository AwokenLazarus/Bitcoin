```
BIP: unassigned
Title: Node Template Attestation (payee-signed coinbase outputs)
Author: Mike Moore <mrmoore27@pm.me>
Comments-Summary: No comments yet.
Comments-URI:
Status: Draft
Type: Standards Track
Layer: Consensus (soft fork)
Created: 2026-09-07
Updated: 2026-10-01
License: CC-BY-4.0
Post-History:
```

# Node Template Attestation

A **draft** soft fork for XBT (Bitcoin on BLAKE2b, Bitcoin Knots 29.4.x). It has no
BIP number and no activation parameters. It is published for discussion and has not
been submitted to the Knots maintainers. A working Knots patch is included as patch files
(see [Reference implementation](#reference-implementation)).

This revision (v1) specifies what that patch enforces. The original draft's inclusion
list, `extra` transaction and `vtxa` sidecar are kept as a separate, later extension in
[Follow-up: inclusion-list extension](#follow-up-inclusion-list-extension-not-part-of-v1).

## Abstract

After activation, every coinbase output that carries value must pay a key-path Taproot
script `OP_1 <K>`. For each distinct such script, the coinbase must also carry a
zero-value attestation output

```
OP_RETURN PUSH68( "NTA" 0x02 || sig64 )
```

in the same order as the payees. `sig64` is a BIP340 signature by `K` over a tagged
hash of the payee script, the block height, the block's `nBits` and its
`hashPrevBlock`.

A block may therefore pay a script only if the holder of that script's key signed
for this height, this difficulty and this tip. The rule is about **payees**, not about
who found the block or which protocol delivered the work. Old nodes see ordinary
`OP_RETURN` outputs and accept the blocks: this is a soft fork.

## Copyright

This document is licensed under the Creative Commons Attribution 4.0 International License
([CC-BY-4.0](https://creativecommons.org/licenses/by/4.0/)). Copyright (c) 2026 Mike Moore
(AwokenLazarus).

## Motivation

### Step 3, per payee

The Knots maintainers have circulated a "step 3" that would withhold block rewards
from blocks produced via stratum-to-pool. At the time of writing it has no
specification, patch or activation height. Any consensus version of it has one
problem: **the chain cannot see a mining protocol.** Once a block is found, a
share delivered over Stratum V1 and one delivered by a DATUM gateway look the same.

What the chain can see is who the coinbase pays. This draft restates step 3 as a rule
about payees:

> After activation, a block may pay a script only if the holder of that script's key
> signed for this height, this tip and this difficulty.

How that plays out for each way of mining:

| Who mines how | Under this rule |
|---|---|
| DATUM miner: own node and gateway, own payout key | Valid, provided the payout address is P2TR and the gateway signs each tip |
| Stratum hasher paid in the coinbase (FPPS/PPLNS in coinbase) | **Cannot be paid in the coinbase** unless it runs a signer. It has no key the pool can use, and the pool cannot sign for it |
| Stratum pool that pays only itself | Valid. The pool signs for its own key; to the chain it is one large miner |
| Pool holding its farms' payout keys | Valid, but the pool holds keys that spend the farms' coinbase outputs |

### DATUM TIDES shares are never forfeited

A rule keyed on the **finder** ("this block came from stratum, so its reward is
withheld") would punish every DATUM miner whose TIDES share sits in a block that a
stratum hasher happened to find. This rule is keyed on the **payees**. A block is
valid whoever found it, provided every script it pays has attested. The pool's
coinbase builder controls that list, not the hasher:
- it lists only payees whose signature for the current tip has arrived;
- it carries everyone else's share forward (TIDES carry).

The block stays valid, and no honest miner's share is lost to a stratum find. This was
shown end to end on regtest (XBT-071: a four-node chain with TIDES coinbases and
`nta@139`).

### What changed from the original draft

The original draft accepted **gifted** attestations: a pool with a node could grind an
attestation for a customer's script. v1 closes that by making the payee's own key sign.
v1 also drops the inclusion list, the `extra` transaction and the attestation grind
(`SHARE_K`). These need a sidecar and new P2P messages, and are specified separately as
a follow-up. What v1 does not stop is listed under [Limitations](#limitations).

## Specification

The rules below apply to full nodes validating a block at height `h ≥ NtaHeight`
(the `nta` deployment is active for the block being validated). Before that height,
nothing changes.

### Definitions

- **Attestation output.** A coinbase output whose `scriptPubKey` is exactly 70 bytes
  and begins with the 6 bytes `6a 44 4e 54 41 02`: `OP_RETURN`, `OP_PUSHBYTES_68`, then
  `"NTA"` and version byte `0x02`. The remaining 64 bytes are `sig64`. Matching is on
  exact bytes, as for the BIP141 witness commitment. Any other encoding of the same data
  (a different push opcode, a truncated signature, extra bytes) is **not** an
  attestation output. It is an ordinary `OP_RETURN`.
- **Payee output.** A coinbase output that is not an attestation output, has
  `nValue > 0`, and whose `scriptPubKey` is not provably unspendable. Provably
  unspendable means it begins with `OP_RETURN` or is larger than `MAX_SCRIPT_SIZE`
  (`CScript::IsUnspendable`). Zero-value outputs of any script are not payees, and
  neither are value-carrying `OP_RETURN`s (burns).
- **Payee list.** The distinct payee `scriptPubKey`s, ordered by first occurrence.
  Two outputs with the same script are one payee.
- **`MAX_NTA_PAYEES` = 512.** The most entries a payee list may have.

### Rules

The coinbase outputs are scanned once, in order. The first failure ends validation
with the given reject reason. All failures are `BLOCK_CONSENSUS`: the block is
invalid.

1. If an output is an attestation output and its `nValue ≠ 0`: **`bad-nta-att-value`**.
2. If an output is a payee output and its `scriptPubKey` is not exactly
   `0x51 0x20 <32 bytes>` (`OP_1 OP_PUSHBYTES_32 K`): **`bad-nta-payee`**.
3. If `K` is not a valid BIP340 x-only public key (no curve point with that x):
   **`bad-nta-payee-key`**.
4. If the output adds a new entry to the payee list and the list already has
   `MAX_NTA_PAYEES` entries: **`bad-nta-too-many`**. Repeating a script already in the
   list never triggers this.

After the scan:

5. If the number of attestation outputs differs from the number of entries in the payee
   list: **`bad-nta-count`**. A block with no payees must have no attestation outputs.
6. For each `i`, the `i`-th attestation output (in output order) must hold a valid BIP340
   signature by the `i`-th payee's key `K_i` over `NtaAttestationHash(payee_i, h, nBits,
   hashPrevBlock)`; otherwise **`bad-nta-sig`**.

Attestation outputs may appear anywhere among the coinbase outputs. Only their order
relative to each other matters. The recommended layout is: payees, then attestations in
payee order, then the witness commitment, then any other zero-value commitments.

### Attestation digest

```
NtaAttestationHash(script, h, nBits, prev) =
    TaggedHash("XBT-NTA/attestation", ser(script) || h || nBits || prev)
```

- `TaggedHash(tag, m) = SHA256(SHA256(tag) || SHA256(tag) || m)`, as in BIP340, with
  `tag` the 19 ASCII bytes `XBT-NTA/attestation`.
- `ser(script)`: CompactSize length, then the script bytes. For a P2TR payee that is
  `0x22 0x51 0x20 || K`, 35 bytes.
- `h`: the height of the block being validated, `int32` little-endian.
- `nBits`: the block header's `nBits`, `uint32` little-endian.
- `prev`: the block header's `hashPrevBlock`, 32 bytes in serialized (internal) byte order,
  i.e. the reverse of the usual hex display.

The preimage is 75 bytes. `sig64` is a BIP340 signature by the secret key of `K` over
this 32-byte message. The attestation is not a transaction signature: no sighash, no
annex, no script execution.

### Deployment parameters

A buried deployment named `nta` (`Consensus::DEPLOYMENT_NTA`, height `NtaHeight`).
In the reference implementation it is **unscheduled on every network**
(`NtaHeight = INT_MAX`). Regtest sets it with `-testactivationheight=nta@<height>`, and
`getdeploymentinfo` reports it as a buried deployment. This document proposes no mainnet
height. See [Deployment](#deployment).

### Mining and templates (non-normative)

- A node holds no payee keys, so it cannot build a valid coinbase after activation
  unless the coinbase pays nothing. `generate*` blocks that pay the node's script are
  refused on submission.
- `getblocktemplate` keeps working. It validates the transaction set against a
  zero-value placeholder coinbase, and `coinbasevalue` is unchanged. Without this, no
  template can be produced after activation, and every pool and gateway that builds
  from GBT stops. This is Knots commit `8a153ba`, not part of the first patch. It is
  not consensus.
- The pool (or a solo miner's gateway) builds the coinbase:
  1. collect a signature from each payee for the new tip;
  2. list only payees whose signature has arrived;
  3. append their attestation outputs in payee order;
  4. carry every other share forward.
- The first jobs on a new tip can pay only payees already signed, typically the finder's
  gateway and the pool, and add more as signatures arrive.
- A payee signs once per tip, not per job: the digest does not cover the transaction
  set, so one signature serves every job built on that tip.
- A builder with more than `MAX_NTA_PAYEES` attested payees lists 512 and carries the
  rest, exactly as it carries unattested payees.
- The payee key is the Taproot **output** key `K`, used untweaked. A BIP86 wallet
  address is `OP_1 <P + t·G>`, so the gateway needs the tweaked secret (`d + t`), which
  wallets do not normally export. A release needs a way to export it, or a
  non-BIP86 payout key (see [Limitations](#limitations)).

## Rationale

**Why inline signatures, not a commitment and a sidecar.** A 38-byte commitment
with a `vtxa` sidecar (the original draft), or a leaf in the XBT header's reserved
slot, saves about 79 bytes per payee. Both need a new service bit, block and
compact-block relay extensions, and IBD support: SegWit-sized work, whose only v1
payload would be 64-byte signatures.

Inline, the attestation is 79 bytes serialized (316 WU) per payee: 0.9% of block weight
for a 100-payee coinbase. It fits the RDTS 83-byte `OP_RETURN` limit, and every node
already relays it. The cost is **permanent**: a later soft fork can add a sidecar but
cannot remove the v1 outputs.

**Why the digest includes `hashPrevBlock`.** Not for replay protection. Replaying an
attestation on a competing branch pays the same key that signed, which harms nobody.
The tip binding forces a **fresh signature on every block**. That is the only thing
in v1 that separates a DATUM gateway (which already follows every tip) from a stratum
hasher (which does not).

XBT retargets every 2016 blocks, so without `prev` a payee could pre-sign a whole period
in one batch, and a stratum pool could satisfy the rule with one wallet signature per
hasher every two weeks. The inclusion-list extension needs per-tip signatures anyway.
The cost falls on honest miners:
- the payout key is hot on the gateway (the P2TR output key is the signing key);
- new-tip latency: on regtest (SOV-002, loopback, real gateways and Prime), every
  payee's signature reached the pool **~41 ms** after a mined tip (p50 40.6, p95 40.7 ms)
  and the full payee set was in the coinbaser at 40.8 ms p95. Tips the gateways learn of
  only by polling took up to **0.3 s** (p95 298 ms). Mainnet adds propagation to each
  gateway's node. Signing itself is ~14 µs (`bench/bench.sh`; Ryzen 9 9950X3D, capped at 2 CPUs, load ~2–3), better than the earlier ~25 µs estimate;
- offline payees go unpaid until they return.

**Why a cap of 512 payees.** Without one, only block size bounds the payee list: about
8,000 distinct payees in a maximal non-witness coinbase (122 bytes each), so about 8,000
BIP340 verifications, single-threaded in `ContextualCheckBlock`, for a block that has
already passed proof of work. Measured (`bench/bench.sh`; Ryzen 9 9950X3D, capped at 2 CPUs, load ~2–3): one verify is ~21 µs and 8,000 verifies are ~0.17 s; a 512-payee block check is ~17 ms, of which the verifies are ~10 ms. All better than the earlier estimates (50 µs, 0.4 s, 26 ms). The
coinbase grows by at most 46.6 KB over P2WPKH payouts (4.7% of block weight).
512 is the original draft's `MAX_ATTESTATIONS`; a TIDES window at DATUM scale pays tens
to low hundreds of payees, and a builder carries anyone past the cap. The cap is
checked during the scan, before any signature, so an over-cap block costs no
verification at all.

**Why a new reject reason (`bad-nta-too-many`, not `bad-nta-count`).** `bad-nta-count`
means "a payee is missing its attestation (or there is a spare one)"; the fix is to add
or drop an attestation. Over the cap, every attestation may be present and valid, and
the fix is to drop payees. Pools act on the reason (SOV-002's Prime releases its books
on any `bad-nta-*` and logs which), so one reason per fix is worth one more string.
Reject reasons are not consensus; this changes no block's validity.

**Why Taproot key-path only.** One signature scheme, 70-byte attestations, and the
payee key is the script. A P2WPKH payee would need the attestation to reveal its key:
103 bytes with BIP340, over the RDTS limit, or 71 bytes with recoverable ECDSA, which
means a second signature scheme in consensus. Revealing the key removes P2WPKH's
key-hiding benefit anyway.

Adding P2WPKH later would **relax** a v1 rule, so it would be a hard fork. It is in v1 or
not at all. Multi-party payees can use a MuSig2 aggregate key. Script-path-only outputs
cannot be paid.

**Why one attestation per distinct script.** A TIDES split may pay one miner in several
outputs. Tying one signature to one script keeps the count check simple and the
coinbase small. Payee order (first occurrence) fixes the pairing without an index field.

**Why exact bytes.** As with the witness commitment, one canonical encoding means no
parser ambiguity. A malformed attestation is simply an `OP_RETURN`, and its payee then
fails the count check.

**Why fail closed.** A payee with no attestation makes the block invalid. Anything
softer ("skip unattested payees") would let a pool keep building today's coinbases
forever.

**Why no attestation grind.** In the original draft the grind (`SHARE_K`) stopped a
copied attestation from riding another script. The payee signature does that
directly.

## Backwards compatibility

- **Old full nodes.** Attested blocks contain only `P2TR` and `OP_RETURN` outputs, which
  they already accept. Blocks that v1 nodes reject (unattested payees) remain valid to
  old nodes. As with every soft fork, old nodes follow the rules only while most
  hashrate enforces them.

  Evidence, all regtest on Knots 29.4.2 + NTA:
  - `feature_xbt_nta.py` (`nta@120`): the non-enforcing node accepts an unattested block
    at the activation height. When the enforcing side extends an attested chain, it
    **reorgs onto it** and reports the unattested block as `valid-fork`. The enforcing
    node rejects it.
  - The 3-node demo (`contrib/xbt-nta/nta_demo.py`: A and B enforce, C does not). Before
    activation all three follow an unattested block. After it, B rejects an unattested
    block at 120 with `bad-nta-count`. C accepts and relays it, and A and B keep refusing
    it. A mines attested blocks 120 and 121, and C reorgs onto them.
  - XBT-071 (4 nodes, `nta@139`, TIDES coinbases with 3 payees, 3 attestations, a witness
    commitment and a pool commitment, RDTS on). Enforcing nodes reject the old coinbaser
    (`bad-nta-payee`) and the all-P2TR-but-unsigned coinbaser (`bad-nta-count`). The
    old-rules node reorgs onto the attested chain. Three consecutive runs passed.
- **SPV clients.** No change. They never validated coinbase structure.
- **Wallets.** No new address type. Coinbase payout addresses must be P2TR (`bc1p…`)
  by the activation height.
- **Pools and gateways.** A new message path (payee signature per tip) and a coinbase
  builder that lists only attested payees. Stock DATUM gateways (`v0.4.1-beta`) can
  neither sign nor carry an attestation: after activation they would mine invalid
  blocks without noticing. A gateway release is a precondition of activation; see
  [Gateway readiness](#gateway-readiness).
- **Mining RPCs.** `getblocktemplate` needs the placeholder change above. `generate*`
  cannot mine value-bearing blocks after activation.

## Deployment

The reference implementation defines `nta` as a buried deployment with no height on any
network. The choice of mechanism belongs to the Knots maintainers. It could be:
- a buried height announced well in advance, as was done for `blake2b`;
- a signalled deployment.

Suggested order, none of which this document authorizes:

1. Review of v1 as specified here.
2. A gateway release (signing, and carrying 70-byte attestations) and a pool coinbase
   builder that lists only attested payees.
3. Signet or a private test network with real gateways and a stratum hasher, measuring
   new-tip signature latency and payee coverage.
4. Activation parameters, in a separate document.
5. A "Taproot payout address" deadline for every coinbase payee ahead of the height.

No single pool can activate this. Blocks from non-enforcing hashrate stay valid to old
nodes.

### Gateway readiness

**Stock DATUM gateways would mine invalid blocks after activation, silently.** The
attestation output is 70 bytes and follows the payees, and no released gateway carries
a coinbaser script over 64 bytes. Upstream has two behaviours, and both lose every
attestation:

- **Truncate.** OCEAN `datum_gateway` v0.4.1 (`5b06123`) stops parsing the coinbaser at
  the first script longer than 64 bytes (`src/datum_coinbaser.c:795`,
  `if ((slen < 2) || (slen > 64)) { break; }`; the buffer is `output_script[64]`,
  `src/datum_stratum.h:115`). It keeps the payees before that script, drops everything
  after it without an error, and the undistributed value goes to the pool address.
- **Discard.** OCEAN master (`dbc3b14`, `:801–804`) and every XBT lineage checked throw
  the **whole** coinbaser away instead: `Script length (%d) is invalid. Using
  default/empty`, zero outputs, and the job pays only the pool. They are the StartOS pin
  iohzrd `7491a50` (`:832`), iohzrd `c031568` (`:772`), FlyTheElephant1 `a5f28aa`
  (`:831`) and CONVOY `b9ea7dc` (`:806`). These are the gateways XBT miners run.

Either way, from the activation height every block such a gateway finds is
`bad-nta-count`. Nothing tells the operator, the miners keep hashing, and the loss is
every block that gateway finds. This is not about signing: the gateway cannot even carry
attestations that the pool signed. On regtest, unmodified `7491a50` and `c031568` builds
discarded every probed coinbaser (146 of 146 and 206 of 206), and one of them found a
post-activation block that the node refused as `bad-nta-count` (SOV-020).

So activation needs three things that are not in consensus:

1. **A gateway release** that accepts the exact 70-byte attestation shape in the
   coinbaser (and, for a signing gateway, signs each tip). The lazarus-gateway
   prototype and a split-only patch to the C gateway (SOV-007) accept exactly
   `6a44 "NTA" 02 ‖ sig64` with value 0 and nothing else over 64 bytes.
2. **A readiness signal** the pool can measure per gateway (SOV-011):
   - an updated gateway advertises `nta-v1` in the DATUM handshake, in a field stock
     v0.4.1 ignores;
   - the pool probes every gateway before activation, advertised or not: it puts a
     0-value 70-byte NTA-shaped output at the end of one coinbaser per gateway, and
     checks whether the next share's coinbase still has it. Before activation that
     output is an ordinary `OP_RETURN` and changes no payout;
   - the probe must also count **pool-only** shares. A discarding gateway never returns
     a share that carries the probed payees, so a rule that judges only such shares
     never reaches a verdict: in SOV-020 both real XBT builds stayed "unknown" and were
     still served after activation. A pool-only share at the probed height, on a job
     with transactions, first seen after the probed coinbaser, is a strike; two strikes
     make the gateway unready (one is not enough, because stock also sends one
     pool-only job at each new height). After activation a single pool-only share
     without the pool's attestation is enough. For such a gateway the probe is not free:
     its jobs pay only the pool until its next coinbaser, which one probe an hour keeps
     to about one job window per hour;
   - after activation the pool refuses pooled work to a gateway that is not ready, with
     a message the operator sees, instead of letting it hash on invalid blocks.
3. **A readiness threshold.** Pools publish the share of their gateway hashrate that is
   ready. **Proposal:** set the activation height only once the pools that serve DATUM
   gateways report at least 95% of that hashrate ready, sustained for a full retarget
   period, and give the remaining operators the announcement lead time to upgrade. This
   is a coordination rule, not a consensus rule, and it is the maintainers' call.

The same lead time covers the Taproot payout deadline and the untweaked-key export
([Limitations](#limitations)).

## Test vectors

Derived from the XBT-065 functional test (`test/functional/feature_xbt_nta.py`, Knots
`5164677`): same keys, same digest function, same signer. Each vector was submitted to
an enforcing regtest node (`nta@120`), and the verdict below is the node's.

Vectors 1–3 are unchanged from the first revision; vector 4 was added with the cap and
has its own parent (the setup chain has wall-clock timestamps, so each generator run
gets a new `hashPrevBlock`). The digests and signatures were also re-checked with the
research model's independent BIP340 code. Full data, including coinbase transactions: `vectors/nta-vectors.json`.
Regenerate with `vectors/nta_vectors.py`; check with `vectors/check_vectors.py`.

Setup:
- Secret keys: `sk = SHA256("xbt-nta/" || name)`.
- Signatures: BIP340 with `aux_rand` = 32 zero bytes (deterministic).
- Regtest `nBits = 0x207fffff`.
- `prev` is shown in RPC (display) order; the preimage holds it reversed.

### Vector 1: one signed payee (valid)

```
height          120
nBits           207fffff
hashPrevBlock   3d7482abfafe2abd5509a1344ca9f4d7cb58c4fcff49048347c8b3a623751bba
payee (alice)   sk  0ac199acc7329a6ca13218a5deb6fbdd668dbd0e609c4d5a0ad090b86bb0931e
                K   50f677dabf7111d39d09da91310631c765396bb2b86d770bc8e15343492664ed
outputs
  0  5000000000  512050f677dabf7111d39d09da91310631c765396bb2b86d770bc8e15343492664ed
  1           0  6a444e54410286af4385ba3bf4fb5d711099c6c8e8a60728bd763f16af2672dd89ae90b1
                 5b122cf75c225f95a0033a15c31da8bf2be4e6d7675ae10f3b98a85569714aea45bd
preimage        22512050f677dabf7111d39d09da91310631c765396bb2b86d770bc8e15343492664ed
                78000000 ffff7f20
                ba1b7523a6b3c847830449fffcc458cbd7f4a94c34a10955bd2afefaab82743d
digest          196724702dcf9266a83672e0d27d16c86605af1a6c56949d5c04cae05769075d
sig64           86af4385ba3bf4fb5d711099c6c8e8a60728bd763f16af2672dd89ae90b15b12
                2cf75c225f95a0033a15c31da8bf2be4e6d7675ae10f3b98a85569714aea45bd
result          valid
```

### Vector 2: split, repeated script shares one attestation (valid)

```
height          121
nBits           207fffff
hashPrevBlock   3b9589aefb047c6383a9e12b4acc78456be408739f7c58245b6bc1be38b90687
payee 0 (alice) K 50f677dabf7111d39d09da91310631c765396bb2b86d770bc8e15343492664ed
payee 1 (bob)   sk 7974de48046f00ca3cbe4f8b1da2e1b09edd94fe87ea921b4fb993ac74aab162
                K  0353ea2c4cf26758fc7e168641351fd9a9dd39635db1ec3f34c2ff7430aaabde
outputs
  0  1250000000  512050f677dabf7111d39d09da91310631c765396bb2b86d770bc8e15343492664ed  (alice)
  1  2500000000  51200353ea2c4cf26758fc7e168641351fd9a9dd39635db1ec3f34c2ff7430aaabde  (bob)
  2  1250000000  512050f677dabf7111d39d09da91310631c765396bb2b86d770bc8e15343492664ed  (alice again)
  3           0  6a444e5441023ecdcae6fb08209352e30314f1eed7f5c70a69a063839e0281caefb780b4
                 c773d9eafdf0f023030d74883eb154f448c2ed30445d3ee1f79dd6a65a0ed6778d67  (alice)
  4           0  6a444e5441024527434fd9dec3c255f640a3e69b072f12474c17e54a05772d8e32d6e451
                 d304aa172d30633e4323037a97934cad0c25250aadc63caf80aaacef5552bdfd87f9  (bob)
digest alice    4cd758888977d9113b0e25b17dcc52e40d23c505fe6931439ee080967b48e9ec
digest bob      43d9f93f38846058ee6a7274c55e4a8dc6fafc67a9ed90c8d208e593b7cc4e3c
result          valid
```

### Vector 3: gifted attestation (invalid)

Same parent as vector 1. The payee is `customer`
(`K = bcbbcdeb177829fc3a6db3c8876bb3bed5f54a0564ae3d91f5473baeccf8f587`), but the
signature over its digest (`d6c4a74190fc9ccbacc6673163a89edee27b217a3648bb01b880ac590af5659a`)
is by the `pool` key:

```
  0  5000000000  5120bcbbcdeb177829fc3a6db3c8876bb3bed5f54a0564ae3d91f5473baeccf8f587
  1           0  6a444e5441024f851cd5397b899f1542eeeb82b26e14b8ae19e4a9acdba358d72625d677
                 b914bcd13b8fe19e31eb815e89790c06f8d339519882ba628f20f45fa076e9a77fd5
result          bad-nta-sig
```

### Vector 4: over the cap (invalid)

`x10` in `vectors/nta-vectors.json`, judged by the capped node (branch `rnd/sov-014`).
513 distinct P2TR payees, each paid 1,000,000 sats, with keys
`sk_i = SHA256("xbt-nta/" || "cap/" || i)` for `i = 0…512` (decimal `i`), then 513
attestations, each a valid signature by its payee for this tip. Every signature
verifies, so the cap alone rejects the block. The full data (all 513 preimages, digests
and signatures) is in the JSON.

```
height          120
nBits           207fffff
hashPrevBlock   49d75917ba2b37707fa9c787bb79ab6fb3357e15a8bccc4881b86f3458d8b58e
outputs         513 payees, then 513 attestations (1,026 outputs; coinbase tx 62,641 bytes)
result          bad-nta-too-many
```

### Other rejections (from the functional test)

| Case | Reject reason |
|---|---|
| All-P2TR coinbase, no attestation | `bad-nta-count` |
| Anyone-can-spend (`OP_TRUE`) payee | `bad-nta-payee` |
| P2WPKH payee, with a placeholder attestation | `bad-nta-payee` |
| `OP_1 <x>` with `x` not on the curve | `bad-nta-payee-key` |
| Signature for height+1, `nBits`−1 or another `prev` | `bad-nta-sig` |
| Two payees, attestations in the wrong order | `bad-nta-sig` |
| Two payees, one attestation; or one payee, two attestations | `bad-nta-count` |
| Attestation output with `nValue = 1` | `bad-nta-att-value` |
| Attestation truncated to 63 signature bytes | `bad-nta-count` |
| Pool holds the farm's key and signs (custody) | valid |
| Zero-value P2TR output plus a data `OP_RETURN`, no attestation | valid |
| 512 distinct signed payees (one paid twice), 512 attestations: at the cap | valid |
| 513 distinct signed payees, 513 valid attestations: over the cap | `bad-nta-too-many` |
| 513 distinct payees, 513 all-zero attestations: over the cap | `bad-nta-too-many` (no signature checked) |

## Reference implementation

Bitcoin Knots `v29.4.2.knots20260508` plus three research commits, published as
`git format-patch` files in [patches/knots-v29.4.2/](patches/knots-v29.4.2/) (not merged
into any Knots branch):

| Commit | What |
|---|---|
| `5164677` (branch `rnd/xbt-065-nta`) | Consensus: `CheckNodeTemplateAttestations` in `validation.cpp` (called from `ContextualCheckBlock`), `DEPLOYMENT_NTA` / `NtaHeight`, `-testactivationheight=nta@h`, `getdeploymentinfo`; `feature_xbt_nta.py`; `contrib/xbt-nta` 3-node demo |
| `8a153ba` (branch `rnd/a2-nta`) | Mining RPC only: `getblocktemplate` validates against a zero-value placeholder coinbase after activation |
| `61b07b8` (branch `rnd/sov-014`) | Consensus: `MAX_NTA_PAYEES = 512`, `bad-nta-too-many`; at-cap and over-cap cases in `feature_xbt_nta.py` |

About 80 lines of consensus code. The Python model in this folder (`spec.py`,
`validate.py`, `xbt_nta.py`) still describes the [original draft](docs/original-draft.md)
plus the XBT profile, including the IL and slot variants that are not ported.

## Limitations

v1 proves that **each payee's key was online and consented at this tip**. It does not
prove that the payee ran a node or chose the template. These gaps are real and are
stated so reviewers can weigh them:

1. **A pool-supplied signer.** A stratum pool can ship a small signer, as a desktop app,
   a proxy, or inside the miner's control board. It holds the hasher's payout key and
   signs whatever tip the pool sends. The hasher then runs no node, chooses no
   transactions, and is still paid in the coinbase. v1 raises the cost of paying stratum
   hashers in the coinbase (key custody and always-on software); it does not end it.
2. **Template control.** With no inclusion list, a pool can pay every DATUM miner and
   still choose every transaction (the original draft's "pay-and-veto" case).
3. **Self-pay PPS.** A pool that pays only itself and settles off chain is one miner.
4. **Custody.** A pool that holds a farm's payout key signs for it. The chain cannot
   tell that apart from the farm signing.
5. **The finder is not identified**, by design. That is what keeps DATUM shares safe.
6. **Non-signing payees are not paid on chain.** Offline members, keyless hashers and
   non-Taproot addresses are carried or paid off chain. The pool holds that value
   meanwhile.
7. **At most 512 payees per block.** A pool with more attested payees carries the rest
   to a later block.
8. **Per-tip latency.** Payees join a new tip's coinbase as their signatures arrive:
   ~41 ms after a mined tip on regtest, up to ~0.3 s when a gateway learns of the tip only
   by polling (one sample of ~1 s right after a pool restart). Until then the first jobs
   pay only already-signed payees. Mainnet adds propagation.
9. **Untweaked payout keys.** The gateway signs with the secret of the output key `K`
   itself. A standard BIP86 wallet derives `K` by tweaking an internal key and does not
   export the tweaked secret, so either wallets export it or payees use a dedicated,
   non-BIP86 payout key. It is hot on the gateway either way (sweep it regularly).
10. **Stock gateways fail silently.** See [Gateway readiness](#gateway-readiness): an
    un-upgraded DATUM gateway (OCEAN v0.4.1, which truncates, or any XBT lineage, which
    discards) mines `bad-nta-count` blocks after activation unless its pool detects it
    and refuses it work.

## Follow-up: inclusion-list extension (not part of v1)

> **Status: design only, not implemented in Knots, not proposed for activation with v1.**
> Kept from the original draft (Python model on `rnd/a1-nta`, 55 + 10 tests) so the
> path is on record. It would be a **separate soft fork** on top of v1. It only adds
> rules, so v1 nodes stay in consensus.

**Goal.** Close limitations 1 and 2 as far as consensus can: if a block pays you, it must
include the transactions your node wanted, whenever they are still feasible.

**Packaging.**
- Attestations gain an inclusion list (IL: full transaction bodies, capped) and an
  `extra` transaction valid against the parent's UTXO set.
- These are too large for the coinbase. They travel in an attestation vector `vtxa`:
  not hashed into `hashMerkleRoot`, committed in one coinbase `OP_RETURN`, and stripped
  by old peers, as BIP141/144 did for witnesses.
- The commitment needs a new version byte. The original draft's `"NTA" 0x01` 38-byte
  commitment must not be reused, and neither may v1's `0x02`.
- Alternatively, the commitment can be a leaf `("NTA1", nta_commit)` in the XBT header's
  reserved 32-byte slot (`m_mm_rhs`). XBT-050 showed that Knots does not constrain the
  slot, so using it is also a soft fork, and every share of a job then commits to the
  attestation root.
- The v1 attestation outputs stay required. The v1 digest has no reserved field, so the
  extension needs a **second per-payee signature** over the IL root and `extra`.

**Attestation object** (per payee):
- `scriptPubKey`, `nHeight`, `nBits`;
- `extra` (a non-coinbase transaction valid against the previous UTXO set, not applied);
- `il` (full bodies, identity = wtxid);
- the payee's BIP340 signature over all of the above plus `hashPrevBlock`.

**Feasible inclusion list** (`ConnectBlock` reads only chain and block, never the
mempool):
- **Catalog.** Every IL body in this block's `vtxa` plus those in the last `W` blocks,
  indexed by wtxid. Two different bodies under one wtxid make the block invalid.
- **Walk.** Drop confirmed entries. Sort the rest by wtxid. Start from the computed
  coinbase weight. Take each body that is valid against the previous UTXO set, does not
  conflict with one already taken, and fits `MAX_BLOCK_WEIGHT`.
- **Satisfy.** Every taken body must appear in `vtx` byte-identical (by wtxid).
- The walk ignores `vtx`, so stuffing the block or conflict-replacing cannot evict an IL
  transaction. Claimed weights are never used.

**Caps** (consensus):

| Name | Value |
|---|---|
| `MAX_ATTESTATIONS` | 512 |
| `MAX_IL_TXIDS` | 256 |
| `MAX_IL_WEIGHT` | 32,000 WU per attestation |
| `MAX_EXTRA_WEIGHT` | 32,000 WU |
| `MAX_NTA_SERIALIZED` | 131,072 bytes |
| `W` (window) | 144 blocks |

**Connect, undo, P2P.**
- Connecting a block appends to the confirmed-wtxid set and to the IL window.
- Reorgs undo both.
- IBD must fetch `vtxa` for the last `W` blocks.
- Peers need a service bit, `vtxa`-carrying `block`/`cmpctblock`/`blocktxn`, and a
  `getnta`/`nta` pair.

**Why bodies, not txids, and why previous-UTXO feasibility.**
- If the IL were a list of txids resolved from each node's mempool, two honest nodes
  could reach two verdicts on one block (model test 42).
- If the IL were "whatever still fits", stuffing would evict it (model tests 24, 25).

**What the extension still does not stop.** A pool-supplied signer that forwards the
pool's own IL still passes. The proof becomes "had a chain view and chose
transactions", not "ran `bitcoind`". Self-pay PPS and custody are unchanged.

## XBT notes: what this is not

This is not non-outsourceable proof of work. The A1 study (XBT-050) ruled that out on
XBT hardware. Sia-class ASICs hash an 80-byte job whose root is computed upstream once
per job, so any per-job key-bound value is computed by the job builder: it proves
custody, not authorship. Per-share keyed wins (2P-PoW, VRF) push every candidate to the
pool, tax solo miners the same, and let old nodes accept phase-1-only chains more
cheaply. NTA constrains payees instead of hashers.

## Comparison

| Design | Template author | Unattested payee in coinbase | Gifted payee | Pay-and-veto |
|---|---|---|---|---|
| Stratum V1 today | Pool | Yes | n/a | Yes |
| DATUM / SV2 JD | Miner node, by protocol | Yes | n/a | Yes (not consensus) |
| Original draft (grind + IL) | Payee's committed IL | No | **Yes** | No |
| **v1 (this document)** | Unconstrained | **No** | **No** (payee key signs) | Yes |
| v1 + IL extension | Payee's committed IL | No | No | No, if that payee is paid |

## Changes

2026-10-01: the cost numbers are now **measured** (`bench/bench.sh`). Earlier versions published unmeasured estimates (50 µs per verify, 0.4 s, 26 ms, 25 µs signing) as if they were figures. They happened to be conservative, but that was luck, not caution, and they should not have been published unmeasured. All numbers so far come from a desktop CPU; **a measurement on modest hardware (Pi-class or small VPS) is pending.** Run `bench/bench.sh` on yours and tell us.

## References

- BIP340: Schnorr signatures and tagged hashes.
- BIP341: Taproot outputs (payee script form).
- BIP141 / BIP144: the witness commitment layout and relay pattern reused by the
  extension.
- Ethereum FOCIL: inclusion lists. The extension is stricter on stuffing.
- DATUM / OCEAN TIDES: the pool protocol and payout scheme this is meant to work with,
  not replace.
