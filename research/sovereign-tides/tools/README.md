# Sovereign TIDES tools (Python)

| File | What |
|---|---|
| `sovereignty.py` | `fetch` (blocks and coinbase outputs from a mempool-style explorer API, read-only; `XBT_API` overrides the default public explorer), `score` (the per-pool sovereignty score, [design §4](../design.md#4-the-sovereignty-score)), `verify-fixture` (the LZT1 verifier, a Python mirror of Prime's Rust check) |
| `entropy.py` | the template-entropy detector features and score ([design §5](../design.md#5-template-entropy-detector-the-fake-datum-check)) |
| `entropy_regtest.py` | runs the detector on a 4-node isolated regtest network with honest, proxy, mimic and hybrid gateways |
| `rdts_opreturn_test.py` | the largest coinbase `OP_RETURN` Knots 29.4.2 accepts under RDTS ([design §2](../design.md#2-rdts-largest-coinbase-op_return-settled-on-regtest)) |
| `test_a2.py` | 13 unit tests: tag parsing on real coinbases, own-tag rule, score math, the Rust fixture's cases, detector separation |
| `fixture.json` | a deterministic LZT1 attestation signed by the Rust code (`prime/wire/examples/lzt1_fixture.rs` in the s0 patches) |
| `gateways.json`, `attestations.json` | the (empty) key directory and published attestations `score` reads |
| `xbtpow/` | Python port of the XBT BLAKE2b v2 header (Knots `CBlockHeader::GetHash`) and secp256k1 helpers |
| `demo.sh` | runs all of the above |

```sh
python3 test_a2.py
python3 sovereignty.py verify-fixture fixture.json
python3 sovereignty.py fetch --days 7 && python3 sovereignty.py score --days 7
PRIME=/path/to/Bitcoin/prime KNOTS=/path/to/bitcoind ./demo.sh [--quick]
```

`fetch` writes a `cache/` next to the script. The regtest scripts start isolated nodes on
loopback (ports 30801–30818 in `demo.sh`), start no miners, and stop the nodes on exit.
