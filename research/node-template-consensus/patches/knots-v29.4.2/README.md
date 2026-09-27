# XBT-NTA patches for Bitcoin Knots 29.4.2

Research patches, **regtest only**. They are not merged into any Knots branch and have not
been submitted upstream. The `nta` deployment has no height on any network; regtest sets
it with `-testactivationheight=nta@<h>`.

**Base:** tag `v29.4.2.knots20260508` (commit `58398baf33e588779685ead478e6397bb28ed3d6`,
"Update manpages, shell completion, and example bitcoin.conf").

| Patch | Commit | What |
|---|---|---|
| `0001-xbt-nta-payee-signed-coinbase-attestations-inline-va.patch` | `5164677` | Consensus rule `CheckNodeTemplateAttestations` (from `ContextualCheckBlock`), `DEPLOYMENT_NTA` / `NtaHeight`, `-testactivationheight=nta@h`, `getdeploymentinfo`; `test/functional/feature_xbt_nta.py`; `contrib/xbt-nta/` 3-node demo |
| `0002-xbt-nta-getblocktemplate-keeps-working-after-activat.patch` | `8a153ba` | Mining RPC only: after activation `getblocktemplate` checks the transaction set against a zero-value placeholder coinbase (without it an NTA chain has no templates) |
| `0003-xbt-nta-cap-distinct-coinbase-payees-at-512-bad-nta-.patch` | `61b07b8` | `MAX_NTA_PAYEES = 512`, reject reason `bad-nta-too-many`, checked before any signature; at-cap and over-cap tests |

## Apply and test

```sh
git clone https://github.com/bitcoinknots/bitcoin.git knots
cd knots
git checkout v29.4.2.knots20260508
git am /path/to/research/node-template-consensus/patches/knots-v29.4.2/*.patch
cmake -B build -DENABLE_WALLET=OFF && cmake --build build -j4 --target bitcoind bitcoin-cli
python3 test/functional/feature_xbt_nta.py --configfile build/test/config.ini
```

`feature_xbt_nta.py` builds and solves every block in Python on BLAKE2b v2
headers; it starts no miners.

The test vectors in [`../../vectors/`](../../vectors/) were judged by a node built from
all three patches. `vectors/check_vectors.py` re-checks them without Knots.

## License

MIT, the licence of Bitcoin Knots and Bitcoin Core. The changes are Copyright (c) 2026 Mike
Moore (AwokenLazarus). The patched files keep their upstream notices ("Copyright (c) The
Bitcoin Core developers" and the rest, under Knots' `COPYING`), and new files follow the same
convention. Knots' `COPYING` applies to the patched tree as a whole. See [../../LICENSE.md](../../LICENSE.md).
