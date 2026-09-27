# Node Template Attestation (XBT-NTA)

Draft **soft fork** for XBT (Bitcoin on BLAKE2b, Bitcoin Knots 29.4.x). After activation, a
block may pay a coinbase output only if the holder of that output's key signed for this
height, this tip and this difficulty. It is a candidate form of Knots' "step 3"
(withholding rewards from stratum-to-pool mining) that keys the rule to **payees**, not
to whoever found the block, so DATUM miners' TIDES shares are never forfeited.

Research, published for discussion. No BIP number, no activation parameters, not
submitted to the Knots maintainers.

## v1 (current)

| | |
| --- | --- |
| Spec | [bip-node-template-attestation.md](bip-node-template-attestation.md) (v1, updated 2026-09-26) |
| Decisions behind v1 | [v1-decisions.md](v1-decisions.md): inline attestations, keep `hashPrevBlock`, no inclusion list in v1, Taproot-only payees, 512-payee cap |
| Brief for the Knots maintainers | [knots-brief.md](knots-brief.md) (**draft for discussion**, not sent) |
| XBT specifics | [XBT-PROFILE.md](XBT-PROFILE.md): BLAKE2b header, RDTS 83-byte `OP_RETURN` limit, the unconstrained reserved header slot |
| Gateway readiness | [gateway-readiness.md](gateway-readiness.md): stock DATUM gateways truncate the coinbaser at 64 bytes and would mine invalid blocks; how to detect and refuse them before activation |
| Knots patches | [patches/knots-v29.4.2/](patches/knots-v29.4.2/): three `git format-patch` files on `v29.4.2.knots20260508` |
| Test vectors | [vectors/](vectors/): `nta-vectors.json` (judged by a regtest node), `check_vectors.py` (re-checks without Knots), `nta_vectors.py` (regenerates, needs the patched Knots) |

v1 rule, in short: every value-bearing coinbase output pays a key-path P2TR `OP_1 <K>`;
for each distinct payee, in order, the coinbase carries a zero-value
`OP_RETURN PUSH68("NTA" 0x02 ‖ sig64)`, a BIP340 signature by `K` over
`TaggedHash("XBT-NTA/attestation", script ‖ height ‖ nBits ‖ hashPrevBlock)`; at most 512
distinct payees per block.

```sh
python3 vectors/check_vectors.py      # every vector: ok
```

## Try it

Build Knots with the NTA patches, run the NTA functional test, and re-check the vectors. Knots'
build dependencies are in its `doc/build-unix.md`. Without system Boost and libevent, see the
`depends` route in [../sovereign-tides/TRY-IT.md](../sovereign-tides/TRY-IT.md#1-build-knots-for-the-demos).

```sh
git clone https://github.com/AwokenLazarus/Bitcoin.git
git clone https://github.com/bitcoinknots/bitcoin.git knots && cd knots
git checkout v29.4.2.knots20260508
git am ../Bitcoin/research/node-template-consensus/patches/knots-v29.4.2/*.patch
cmake -B build -DENABLE_WALLET=OFF && cmake --build build -j4 --target bitcoind bitcoin-cli
build/test/functional/feature_xbt_nta.py          # regtest; builds and solves blocks in Python, no miners
cd ../Bitcoin/research/node-template-consensus
python3 vectors/check_vectors.py                  # every vector: ok (no Knots needed)
python3 test_consensus.py && python3 test_world.py && python3 test_xbt.py   # the Python model
```

`feature_xbt_nta.py` covers activation, valid and invalid attestations, the 512-payee cap and
`getblocktemplate` after activation. The pool side (Prime signing attested coinbases, the
gateway readiness checks) is in [../sovereign-tides/TRY-IT.md](../sovereign-tides/TRY-IT.md).

## Research notes

- [docs/step-3-interaction.md](docs/step-3-interaction.md): the first Knots build, how NTA
  maps to step 3 per payee, what it does not stop, the activation path.
- [docs/sovereignty-nta-integrated.md](docs/sovereignty-nta-integrated.md): NTA and
  [Sovereign TIDES](../sovereign-tides/) on one 4-node regtest chain.
- [docs/original-draft.md](docs/original-draft.md): the original draft (inclusion list,
  `extra` transaction, `vtxa` sidecar) with its XBT profile. v1 keeps the inclusion list
  as a later extension.

## Python model (original draft)

The model files **model the original draft plus its XBT profile, not v1**: `spec.py`,
`validate.py`, `world.py`, `helpers.py`, `xbt_nta.py` (payee-key-signed attestations,
commitment in the reserved header slot), `xbt_pow.py` (BLAKE2b v2 header),
`xbt_secp.py` (independent secp256k1/BIP340, also used by `vectors/check_vectors.py`).

```sh
python3 test_consensus.py && python3 test_world.py && python3 test_xbt.py
```

`index.html` is rendered from the spec with `python3 generate_html.py`.

Task IDs in these documents (XBT-0xx, SOV-0xx) name internal work items of the Lazarus
pool; the findings they refer to are summarised here.

## License

Copyright (c) 2026 Mike Moore (AwokenLazarus). The documents (including the BIP) are
[CC BY 4.0](https://creativecommons.org/licenses/by/4.0/). The code and the Knots patches are
MIT. Details and the attribution line to use: [LICENSE.md](LICENSE.md).
