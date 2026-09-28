# Try it

This page covers two things: building and testing the Sovereign TIDES and NTA changes to the pool
software from this repo, and running the regtest demos. Everything runs on loopback regtest
nodes. Nothing touches mainnet, and no miners are started.

For the NTA consensus rule on its own (Knots patches, functional test, vectors), see
[../node-template-consensus/README.md#try-it](../node-template-consensus/README.md#try-it).

## What you need

- Linux with `git`, `ss` (iproute2) and a C++ toolchain.
- Rust (stable `cargo`) and Python 3.11 or later. The scripts use only the standard library.
- A Bitcoin Knots `v29.4.2.knots20260508` `bitcoind` **with the wallet** for the demos: they create
  regtest wallets for the pool node and the canary signer. The NTA-patched build from step 1
  works too. NTA has no activation height unless `-testactivationheight=nta@<h>` is given, so the
  demos see stock Knots behaviour. The sov-011 readiness demo **needs** the NTA-patched build,
  and the Python `cryptography` package (`apt install python3-cryptography`).
- A git identity, because `git am` records commits: `git config --global user.name "…"` and
  `user.email "…"` on a fresh machine (or `git -c user.name=… -c user.email=… am …`).
- On Debian or Ubuntu, this is enough for everything below, Knots included:
  `apt install build-essential cmake pkgconf git python3 python3-cryptography libboost-dev libevent-dev libsqlite3-dev`.
  The C gateway in the sov-011 demo also needs `libcurl4-openssl-dev libjansson-dev libsodium-dev libmicrohttpd-dev`.

## 1. Build Knots (for the demos)

```sh
git clone https://github.com/bitcoinknots/bitcoin.git knots && cd knots
git checkout v29.4.2.knots20260508
git am /path/to/Bitcoin/research/node-template-consensus/patches/knots-v29.4.2/*.patch   # optional here
cmake -B build && cmake --build build -j4 --target bitcoind bitcoin-cli     # wallet on (needs SQLite)
export KNOTS=$PWD/build/bin/bitcoind
cd ..
```

A build with `-DENABLE_WALLET=OFF` is enough for the NTA functional test, but not for these demos.
Knots' build dependencies are listed in its `doc/build-unix.md`. Without system Boost and
libevent, build them with Knots' `depends` system and point CMake at its toolchain:

```sh
make -C depends -j4 NO_QT=1 NO_ZMQ=1 NO_USDT=1
cmake -B build --toolchain depends/$(./depends/config.guess)/toolchain.cmake
cmake --build build -j4 --target bitcoind bitcoin-cli
```

## 2. Apply the pool-side patches

The patches apply to this repo's `main` at `f14bb4c`. Apply them on the commit that published
them: that is `f14bb4c` plus `research/`, so the demos find their scripts and the patched
`prime/` in one checkout. `main` has moved on since (its `pool/` no longer matches), so start
from that commit and bring in the current `research/` from `main`, which has later fixes to the
patches and demos (for example the `sov-011` discard fix and the sov-011 demo).

```sh
git clone https://github.com/AwokenLazarus/Bitcoin.git && cd Bitcoin
C=$(git log --format=%H --diff-filter=A -1 -- research/sovereign-tides/TRY-IT.md)
git diff --quiet f14bb4c "$C" -- prime lazarus pool && echo "base ok"      # same code as f14bb4c
git checkout -b try-sov "$C"
git checkout main -- research && git commit -qm "research/ from main"   # the latest patches and demos
P=research/sovereign-tides/patches/primed
git am $P/mit/s0/*.patch && git am $P/agpl/s0/*.patch
```

To try one of the optional series, apply it on top of `s0`. Use one at a time: they are
independent, and each needs its own branch.

```sh
git checkout -b try-sov-015 try-sov
git am $P/mit/sov-015/*.patch && git am $P/agpl/sov-015/*.patch      # or sov-011, sov-017
```

`git am` warns about whitespace in the patch files that carry a C-gateway patch
(`lazarus/patches/*.patch`). The warnings are expected.

## 3. Build and test

```sh
(cd prime && cargo test --workspace)
(cd lazarus && cargo test --workspace)
(cd lazarus/canary && python3 -m unittest -q test_lazarus_canary)
```

Expected on `s0`: 260 passing `prime/` tests, 131 `lazarus/` tests and 18 sidecar tests, with 0
failures. The numbers for the other tips are in [patches/primed/README.md](patches/primed/README.md).

The demo scripts build with `cargo build --offline`, so fetch their dependencies once:

```sh
(cd prime && cargo fetch)
for g in research/sovereign-tides/demos/sov-0*/gwsim; do (cd $g && cargo fetch); done
```

If `git am` stops with "Committer identity unknown", set the git identity (above), run
`git am --abort`, and apply the series again.

## 4. Run the regtest demos

Each demo builds `primed --release`, starts its own Knots nodes on a fixed loopback port range,
refuses to start if that range is in use, and prints `RESULT PASS` or `RESULT FAIL` at the end.
Datadirs, logs and results go under the demo's `run/` directory.

| Demo | Branch | Command (from the repo root) | Ports | Time |
|---|---|---|---|---|
| Hybrid proxy vs the canary detector ([sov-004](demos/sov-004/)) | `try-sov` | `PRIME=$PWD/prime research/sovereign-tides/demos/sov-004/hybrid-canary-demo.sh` | 31700–31799 | 2 × 300 s |
| Canary sidecar on real DATUM sessions ([sov-008](demos/sov-008/)) | `try-sov` | `WT=$PWD research/sovereign-tides/demos/sov-008/canary-sidecar-demo.sh` | 32000–32099 | 420 s + 55 s |
| Foreign canaries ([sov-015](demos/sov-015/)) | `try-sov-015` | `WT=$PWD RUNS=1 MIN_RUNS=1 KNOTS_SECS=300 RACE_TRIALS=0 research/sovereign-tides/demos/sov-015/foreign-canary-demo.sh` | 32800–32899 | ~6 min (quick look; the defaults do 3 runs + the race probe) |
| NTA gateway readiness ([sov-011](demos/sov-011/)) | `try-sov-011` | `WT=$PWD research/sovereign-tides/demos/sov-011/readiness-demo.sh` (NTA-patched `KNOTS`) | 32600–32699 | 15–40 min (CPU miners find diff-1 shares) |

All of them need `KNOTS` from step 1, and the harnesses are already adapted to the `s0` tip (see
each demo's README). Each demo's README says what its gateways do and what
`PASS` requires. The evidence from the accepted runs is in [results/](results/).

The sov-011 demo clones FlyTheElephant1's `datum_gateway` at `a5f28aa` and builds it with the
split-only patch, so it needs network access once. It can also run real, unmodified stock
gateways next to its own (`STOCK_S`, `STOCK_I`, `OCEAN`; see [its README](demos/sov-011/)).

**"nodes did not sync".** About one run in ten, the Knots nodes of a demo fail to peer during
setup and the run stops with `nodes did not sync` before any canary is sent. It is a setup flake,
not a detector result: run the demo again. The sov-015 driver counts such a run as failed, so a
multi-run `RESULT` is `FAIL` after one; rerun it, or check that the failed run's log ends in
setup.

The sov-015 driver re-runs itself under `systemd-run --user --scope` with a CPU and memory cap.
On a host without systemd user sessions, set `SOV015_SCOPED=1` to skip that.

## Off loopback: canary settings for WAN and Tor

The demos run on loopback, and the sidecar's defaults (`[foreign] order = "own-ack"`,
`max_lag_secs = 0.05`) are loopback settings. **On any other link they switch foreign-canary
detection off without an error**: `max_lag_secs` is compared with the twin's *pong* lag (C sent
until the target's `pong` for C′ returns, at least one round trip), not with the real arrival
skew at the target node. At 20 ms one-way every twin was already "late"; over Tor the pong lag is
at least ~0.65 s. The only sign is `status` → `foreign.late`.

For foreign canaries off loopback:
- set `[foreign] order = "pipelined"`;
- size `max_lag_secs` to the link: **0.5** at up to ~80 ms one-way (about 2 × RTT + 0.1 s), and
  **2–3** over Tor (`[delivery] mode = "socks5"`, `proxy = "127.0.0.1:9050"`);
- alert when `foreign.late / foreign.twins` is above 0.1.

SOV-020's measurements behind these numbers are in [design.md](design.md#canary-delivery-off-loopback-sov-020-2026-09-28).
W8 canary checks on real gateways also need **several shares a minute per gateway** (a real
gateway shows Prime a new job only together with a share).

On an isolated lab box, note that `apt install tor` (Debian/Ubuntu) starts `tor@default`, a client
on the public Tor network, immediately. Run `sudo systemctl disable --now tor@default` straight
after installing and start only your own `tor` instances.

## Also runnable without the patches

- [tools/](tools/): `python3 test_a2.py` (unit tests), and `sovereignty.py` for the score over
  public chain data (see [tools/README.md](tools/README.md)).
- [tools/demo.sh](tools/demo.sh): the original A2 demo. It needs `PRIME` (patched) and `KNOTS`,
  and reads the public explorer.

## What these demos do not show

These are regtest runs on one host. What they cannot tell you (mainnet calibration, real
hashrate, and more) is listed in [README.md#not-proven](README.md#not-proven).
