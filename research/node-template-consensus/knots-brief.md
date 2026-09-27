# Step 3 per payee: a brief on XBT-NTA

*Draft for discussion. Not yet sent to the Knots maintainers. Mike Moore, 2026-09-26.*

**What step 3 needs.** Withhold block rewards from stratum-to-pool mining without
hurting miners who build their own templates (DATUM). But consensus can't see a
protocol: a found stratum share and a DATUM share look the same. A finder-based rule
would also forfeit every DATUM miner's TIDES share whenever a stratum hasher finds the
block.

**How NTA meets it.** Key the rule to **payees** instead of the finder. After
activation:
- every value-bearing coinbase output must be a key-path P2TR `OP_1 <K>`;
- each distinct payee needs a 70-byte `OP_RETURN "NTA"02 ‖ sig64` beside it: a BIP340
  signature by `K` over `TaggedHash("XBT-NTA/attestation", script ‖ height ‖ nBits ‖
  prevhash)`.

A pool can pay only miners whose keys sign each tip. Non-signers are left out and their
share carried, so the block is valid whoever found it.

**Evidence** (Knots 29.4.2 + [patch](patches/knots-v29.4.2/), regtest only):
- A buried `nta` deployment, unscheduled everywhere: ~80 lines in
  `ContextualCheckBlock`, plus a `getblocktemplate` fix. A functional test covers gifted
  signatures, wrong tip, count, value, encoding, custody and the payee cap.
- Upgraded nodes reject an unattested block; an old node that accepted it **reorgs onto
  the attested chain** (soft fork).
- Real gateways signing each tip, Prime building TIDES coinbases: stratum-found and
  gateway-found blocks were valid, non-signers were carried, no DATUM miner lost pay.
  The BIP draft has node-judged vectors.

**Costs and risks.**
- 79 bytes per payee in the coinbase, permanently: about 0.9% of block weight at 100
  payees.
- Payout addresses must be Taproot by activation. P2WPKH needs a second signature scheme
  to fit RDTS, and adding it later is a hard fork.
- Payout keys are hot on the gateway and used untweaked (BIP86 wallets must export the
  tweaked secret). Payees join a new tip ~41 ms after it is mined (≤0.3 s worst,
  regtest); offline payees are paid when they return.
- **Every DATUM gateway needs a release before activation.** Stock v0.4.1 silently drops
  coinbaser scripts from the first one over 64 bytes (`datum_coinbaser.c:795`), so it
  would mine invalid blocks unnoticed. Activation therefore needs a gateway
  release, a per-gateway readiness signal (handshake flag plus a pre-activation probe)
  and a readiness threshold (we propose 95% of DATUM hashrate).
- It needs most of the hashrate; no single pool can activate it.

**What it doesn't stop.**
- A pool can ship its hashers a signer that signs whatever tip the pool sends: no node,
  still paid.
- A pool can pay DATUM miners and still pick every transaction (no inclusion list).
- Self-pay PPS and pools holding their farms' keys look like solo miners.

v1 raises the cost of stratum-with-coinbase-payouts; it does not end it. An
inclusion-list extension is specified as a later soft fork, but it is much larger work.

**Open questions for the maintainers.**
1. Is "payees must sign" an acceptable form of step 3, or does step 3 aim at something
   else (e.g. template choice)? If so, is the inclusion list a precondition?
2. Should the digest keep the previous block hash (per-tip freshness, but hot keys and
   latency) or allow pre-signing (cold keys, but a pool gets hashers' signatures once
   per retarget)? We recommend keeping it.
3. Is Taproot-only acceptable, given it is permanent? We propose capping payees at 512
   per block (`bad-nta-too-many`, checked before any signature): at most ~26 ms of
   verification instead of ~0.4 s; pools carry the rest. Patch and test included.
4. Activation: a buried height like `blake2b`, or signalled?

**The ask.** Would you look at the patch and draft and tell us whether this fits step 3
as you intend it? If it does, we'll do the rest: the gateway release,
signet with real gateways and a stratum hasher, and any changes you want before
anything is public. Patch, tests and draft: [patches/knots-v29.4.2](patches/knots-v29.4.2/), [the BIP draft](bip-node-template-attestation.md).
