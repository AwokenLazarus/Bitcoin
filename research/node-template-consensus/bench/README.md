# NTA performance bench

One command measures the numbers in the BIP, the Knots brief and `v1-decisions.md`.

```sh
KNOTS_DIR=/path/to/knots PROTOCOL_DIR=/path/to/protocol ./bench.sh
```

`bench.sh` caps itself at 2 CPUs and 4 GB (`systemd-run --user --scope -p CPUQuota=200% -p MemoryMax=4G nice -n 19`) and writes `results/bench.json` and `results/bench.md`. `--quick` runs each measurement once (the default is 5) so a check can finish without a full spread.

## What you point the variables at

| Variable | Required | What it is |
|---|---|---|
| `KNOTS_DIR` | yes | A Bitcoin Knots tree built from [`patches/knots-v29.4.2`](../patches/knots-v29.4.2/) on tag `v29.4.2.knots20260508`, with `bitcoind`, `build/test/config.ini` and `build/src/secp256k1/lib/libsecp256k1.a`. |
| `PROTOCOL_DIR` | yes | A `lazarus/protocol` crate with the published primed/agpl series applied (`Cargo.toml` and `src/nta.rs`). This is the crate whose `Signer::sign` the gateway calls per tip. |
| `PORT_BASE` | no | First regtest port. Default **32640** (the node uses `PORT_BASE`..+59). Do not overlap **32300–32599**. |

`sign-bench/Cargo.toml` does not path-depend on a machine directory. The protocol crate inherits `edition` and `version` from its own workspace, and Cargo does not follow a symlink when it looks for that workspace, so `bench.sh` writes a gitignored manifest `sign-bench/.bench/Cargo.toml` whose `lazarus-protocol` path is `$PROTOCOL_DIR`, plus `sign-bench/.cargo/config.toml` (the job cap). The build is:

```sh
cargo build --release -j2 --manifest-path sign-bench/.bench/Cargo.toml
```

## What each number is

1. **BIP340 verify.** `src/bench_verify.c` links the Knots tree's `libsecp256k1.a` (MIT) and times `secp256k1_schnorrsig_verify` on a 32-byte message, the length `ContextualCheckBlock` passes. It also times 512 and 8,000 verifies back to back. The 8,000 figure is the verify loop behind the old "~0.4 s at the block-size bound" sentence. A real block never does that many: the cap refuses payee 513 before any signature.

2. **Block check at 1, 100 and 512 payees.** `bench_blocks.py` builds blocks with the Knots test helpers and times `submitblock` only. Python signing is outside the timer. An accepted `submitblock` runs `CheckBlock`, `ContextualCheckBlock` and `ConnectBlock` of a coinbase-only block, so the 512-payee time is an **upper bound** on the consensus check. The verifies inside that check are the separate 512-verify number (about 10 ms on the machine below; the block check was about 17 ms).

3. **Why 513 is cheap.** Payee 513 returns `bad-nta-too-many` during the payee scan, before BIP340. The bench still submits it (with a fresh `nTime`, which is not in the attestation digest, so the invalid-block cache cannot skip the scan). That path does not connect the block.

4. **Per-tip signing.** `sign-bench` times `Signer::sign` in the protocol crate: attestation hash plus BIP340 with zero aux randomness, and a new signing context per call, which is what the gateway pays.

## The published numbers

Measured 2026-10-01 on an AMD Ryzen 9 9950X3D, capped at 2 CPUs, load about 2–3. All four were better than the earlier estimates (50 µs, 0.4 s, 26 ms, 25 µs).

| Claim | Earlier estimate | Measured |
|---|---|---|
| one BIP340 verify | 50 µs | ~21 µs |
| 8,000 verifies | 0.4 s | ~0.17 s |
| 512-payee block check | 26 ms | ~17 ms (verifies ~10 ms) |
| per-tip signing | 25 µs | ~14 µs |

Re-run `./bench.sh` to replace those with your own medians. The host, the command, the run count and the spread are in `results/bench.md`.

## Modest hardware

The published numbers come from a Ryzen 9 9950X3D capped at 2 CPUs. A run on modest hardware (a Raspberry Pi 4/5 or a small VPS) is pending. If you run `bench.sh` on one, please open an issue with `results/bench.md` and your CPU model.
