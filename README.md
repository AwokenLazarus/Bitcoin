# Bitcoin (BLAKE2b) — Lazarus Pool

Code for [Lazarus Pool](https://pool.awokenlazarus.xyz) on the BLAKE2b fork of Bitcoin (Knots, header v2, forked from SHA-256d at height 961640): the DATUM Prime, the stratum/DATUM gateway, the pool website and explorer front ends, the Electrum and node hooks, and a GPU miner. Copy the **design**; hosts, keys, firewall rules and operating details are deliberately not in this repo (see [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) for the general shape).

Since 2026-09-20 the pool runs on a dedicated bare-metal hub with two Knots nodes (one serves templates, one serves Electrum, the explorer and the hot wallet). Before that it ran on a homelab Umbrel node; the Umbrel-era files under `node/`, `scripts/` and `systemd/` are kept as reference.

## Layout

| Path | What |
|------|------|
| `prime/` | **The DATUM Prime we run** (`primed`, Rust, MIT): pool side of the DATUM protocol, TIDES window and coinbase split, share verification against the node's chain, block booking (deferred earnings, DATUM rebate, stale balances), operator requests as files (`hold`, `paid`, `release`, `credit`) |
| `lazarus/gateway/` | `lazarus-gateway`: DATUM gateway that also serves stratum (ASIC and GPU profiles, solo mode, the overflow relay that hands hashrate above the pool's self-cap to partner pools). The rest of `lazarus/` is the retired first Prime and the protocol crates (AGPL-3.0, see Credits) |
| `pool/` | Pool website backend: `server.py` (stdlib Python, one writer + read-only replicas over SQLite) and the static site; `config.example.json` |
| `cloudflare/` | The public sites as Cloudflare Pages projects: `pool-site/` and `mempool-site/` (static snapshot + a worker that proxies `/api/*` to the hub through an Access-guarded tunnel), `deploy.sh`, verify scripts |
| `docs/` | [`ARCHITECTURE.md`](docs/ARCHITECTURE.md), the operator [playbook](docs/blake2b-mining-pool-playbook.md), the [DATUM rebate plan](docs/datum-rebate-plan.md), the [solo plan](docs/solo-mining-plan.md), the unsplit-coinbase advisory |
| `node/` | Knots install script, `blake2b.conf`, explorer pool-list merge and Lazarus theme, Umbrel-era app config and hooks |
| `scripts/`, `systemd/` | Umbrel-era helpers and user units (reference only) |
| `miner/` | OpenCL BLAKE2b GPU miner |
| `patches/` | DATUM PROP / coinbaser wiring notes |
| `laz-agent/` | Tools for an operator agent (node, Electrum and pool lookups) |

## How mining works here

DATUM, as OCEAN designed it: the miner runs their own Knots and a DATUM gateway, the gateway builds every block template from the miner's node, and Prime only tracks shares and dictates the coinbase split. Remote gateways point at `stratum.awokenlazarus.xyz:28915` with the pool's public key (`primed pubkey`). Any gateway that speaks the protocol is accepted: the house `lazarus-gateway`, [FlyTheElephant1's BLAKE2b fork](https://github.com/FlyTheElephant1/datum_gateway), iohzrd's and Ratum's. Prime answers every coinbaser request with the current TIDES split, verifies each share by rebuilding the BLAKE2b header, and only accepts a coinbase that pays the split it issued; work a gateway submits on a pool-only or unsplit coinbase is credited safely rather than refused (`docs/blake2b-unsplit-coinbase-advisory.md`).

Miners without a gateway use the pool's public stratum (`:23334` ASIC, `:3333` GPU), served by the house gateway, itself a DATUM client of the same Prime. Solo ports (`:23335`, `:3334`) pay the finder alone.

Fees: DATUM work 0%; public stratum 15%, of which 7.5 points is credited to DATUM miners pro rata as carry (see the rebate plan); the pool keeps the other 7.5 points. Solo 0%. A found block pays the TIDES window in the coinbase; what a coinbase cannot place (below the payout floor, over the size budget) becomes carry and rides on later coinbases out of the pool's remainder. A miner who leaves with a small balance is paid by the coinbase after a stale period, or by hand through Prime's `hold`/`paid` requests. Coinbase tag is `Lazarus`; a gateway may add its own secondary tag, which is how the site learns operator names.

## The website

`pool/server.py` is a thin view over `primed`'s `stats.json` plus Knots RPC and the gateway client API: *If a block is found right now* (the exact coinbase Prime hands to every gateway, as a ring and a table with each output's fee path), Connect, address lookup, miners, gateways, blocks and payouts. Endpoints: `/api/pool`, `/api/coinbaser`, `/api/gateways`, `/api/miners`, `/api/miner/<address>`, `/api/payouts`, `/api/blocks`. It runs as one writer (scrapes and records) and read-only replicas (`POOL_UI_NO_WRITE=1`) that serve traffic.

The public site is a Cloudflare Pages project built from a running backend: `cloudflare/deploy.sh pool` snapshots the HTML, deploys, and verifies the deployed site against the origin before it returns; the Pages worker proxies `/api/*` to the hub's replicas through a Cloudflare Tunnel behind Access (service token only), with short edge caching. The explorer (`mempool.awokenlazarus.xyz`, a mempool.space fork with the Lazarus theme) uses the same pattern with `/ws` passed through. Both wordmarks lead with the Chi Rho (`pool/static/chi-rho.svg`), which is also the pool's icon in the explorer.

```bash
# local development
cd pool && cp config.example.json config.json   # cookie_file, stratum_host, explorer_url, datum_prime_stats
python3 server.py
```

## Building and running Prime

`cd prime && cargo build --release` gives `primed`; `primed -c prime.toml check`, `pubkey`, `window`, `import-ledger <ledger.json>` (rows may carry a `source`, 1 stratum / 2 DATUM), `run`. The gateway is `cargo build --release -p lazarus-gateway` in `lazarus/`. `primed` has run the pool since 2026-09-03, cut over live with the previous window imported. Operator actions while it runs are files in `<data-dir>/payouts/` (see `prime/primed/src/payouts.rs`).

## Share validation invariants

Four rules decide whether a share we accept is real and whether a block we assemble is
actually solved. Each one failed silently in production: the pool looked healthy, miners
were paid, and every block we could have submitted would have been rejected. They are
pinned by tests in [`lazarus/protocol/src/pow.rs`](lazarus/protocol/src/pow.rs) and
[`lazarus/protocol/src/verify.rs`](lazarus/protocol/src/verify.rs).

1. **Hashes and targets are big-endian.** `pow_hash` returns the blake2b digest with the
   most significant byte first, which is the order a block id is printed in.
   `bits_to_target` and `target_for_pot` produce targets in that same order, so
   `meets_target` is a plain byte comparison. Mixing the two conventions compares the wrong
   end of the number: difficulty checks then pass or fail at random, and a real solve is
   never recognised. `mainnet_headers_hash_to_their_block_id` re-hashes real headers this
   node accepted and requires our function to reproduce their block ids exactly.

2. **`hash1` must equal the miner's merkle leaf.** A Sia-style miner builds its merkle-root
   field by hashing the coinbase as a merkle *leaf*, prefixed with a `0x00` tag byte:
   `blake2b(0x00 || coinb1 || extranonce1 || extranonce2)`. Our `coinb1` is
   `000000 || h2 || 00000000`, so that preimage is
   `4 zero bytes || h2 || 4 zero bytes || en1 || en2`. Consensus reads bytes 36..52 of the
   same preimage as the header extranonce, so it is the 4-byte pad followed by the miner's
   nonces — never `en1` at offset 0. `pow::header_extranonce` is the single place this
   layout is expressed; every real mainnet block's header extranonce begins with four zero
   bytes for the same reason.

3. **The miner's 8-byte ntime is nonce space.** It lands in the 80-byte ASIC pass as
   `time_offset || nonce3`. We publish it as zero, but a miner may roll it, and those bytes
   are hashed. With `FLAG_USE_TIME_OFFSET` clear they do not affect the block timestamp, so
   they must be reproduced verbatim rather than assumed zero.

4. **Rebuild against the job the miner was given.** Templates republish about once a second,
   so a submission has to be matched to its own job by the id it names. Reaching for the
   current job instead rebuilds a stale share against the wrong template: it fails its own
   target, and a genuine solve is assembled into a block nobody solved.

The header extranonce is independent of the coinbase scriptSig — verified against real
mainnet blocks — which is what lets each stratum session hold its own extranonce1 without
rebuilding the coinbase.

## Share accounting

Difficulty is per session and a power of two. Every miner sitting at the pool floor is not
free: one 1 TH/s rig at difficulty 1 submits over 200 shares a second, which swamps Prime
and buys no extra accuracy. Vardiff aims for roughly one share per miner every few seconds.

A share is judged and paid at the difficulty **its own job** was handed out at, which the
gateway records per session as each job is sent. A session may be retargeted several times
while a miner is still working an earlier job, and holding that share to a target it was
never given rejects work the miner genuinely did.

The tempting shortcut — credit whatever difficulty the hash turns out to reach, capped at the
session's current one — is wrong, and we shipped it briefly. For a miner really working at
difficulty `d` while assigned `A`, share quality above `d` is Pareto: the chance of also
clearing `2d` is a half, `4d` a quarter, and so on. Each level contributes `d/2` to the
expectation and the capped tail another `d`, so the expected credit is

    E[credit] = d * (1 + log2(A/d) / 2)

against a fair value of `d`. Drift of a single doubling overpays by 50%, and a miner that
simply ignores `mining.set_difficulty` collects the difference — so it is not merely
imprecise, it is worth gaming. Pinning credit to the job's difficulty removes the free
parameter: `A` always equals the `d` the miner was working under.

A share whose job is a block or two behind the tip is late, not invalid, and is still paid
(`HEIGHT_LAG`). Blocks here arrive about once a minute and miners run a few jobs behind, so
refusing them discarded around a quarter of all submitted work. Replaying old work earns
nothing: a repeat is the same share and deduplication catches it.

Each session also gets its own extranonce1. Sharing one across the gateway makes identical
rigs walk identical `(extranonce2, nonce)` pairs, so they submit the same shares and
deduplication keeps whichever arrived first — quietly moving credit between miners.

## Do not commit

Cookie files, DATUM admin env, SMTP tokens, wallet addresses, or live `config.json`.

## License

[MIT](LICENSE). The Prime we run (`prime/`) is MIT and is not derived from Ratum.
`lazarus/` (retired) remains [AGPL-3.0](https://github.com/iohzrd/ratum/blob/master/LICENSE)
as a Ratum derivative — see Credits.

## Credits

### DATUM — Bitcoin Ocean / Jason Hughes

The DATUM model this pool is built on — the Prime/gateway split, where the miner's own node
builds the block template and the pool only tracks shares and dictates the coinbase — is
[Bitcoin Ocean](https://github.com/OCEAN-xyz/datum_gateway)'s, by Jason Hughes. The wire
format in [`lazarus/protocol`](lazarus/protocol) is recovered from OCEAN's
`datum_protocol.c` / `.h` (MIT).

### The Prime we run is not Ratum

**[`prime/`](prime/) (`primed`) is the pool server in production.** It is a from-scratch
implementation. It contains no Ratum or iohzrd pool-side code, was not written from Ratum's
source, and is not a fork, rename, or relicensing of [Ratum](https://github.com/iohzrd/ratum).
The wire format, coinbaser v2 encoding, and BLAKE2b header-v2 share layout were recovered
from the MIT-licensed C `datum_gateway` trees listed below (OCEAN, CONVOY, FlyTheElephant1,
iohzrd's *gateway* fork). That last one is a client miners run; it is not the pool.

### Ratum — iohzrd (history, and `lazarus/` only)

[**Ratum**](https://github.com/iohzrd/ratum) by [iohzrd](https://github.com/iohzrd) was the
first Rust DATUM pool on the [Bitcoin Knots BLAKE2b
chain](https://github.com/bitcoinknots/bitcoin/pull/359). This node ran Ratum Prime in
production first (`0.1.3`, `e828545`), then a tree we called `lazarus/` that grew out of
that vendored copy. **`lazarus/` is retired.** It is not what we run. git still records
`lazarus/Cargo.lock` and `lazarus/prime/Cargo.toml` as renames of Ratum files — that history
applies to `lazarus/` only.

What `lazarus/` followed from Ratum (not `prime/`):

| `lazarus/` | Ratum |
|------------|--------|
| `protocol/src/{handshake,channel,nacl,header}.rs` | `core/src/datum/{handshake,framing}.rs` — NaCl-sealed handshake, obfuscated frame headers |
| `protocol/src/mining.rs` | `core/src/datum/messages.rs` — message subtypes and share encoding |
| `protocol/src/coinbaser.rs` | coinbaser v2 encoding |
| `protocol/src/pow.rs` | `core/src/{header,target,nonce}.rs` — version 2 header, two-pass BLAKE2b, targets |
| ledger / TIDES window / coinbaser shape | Ratum Prime's window-and-split shape |
| `#[ignore]`d share-grind tests | Ratum's release-mode `--ignored` PoW tests |

Where `lazarus/` already diverged from Ratum: native TIDES coinbase split, split-only
templates on the gateway, pool-side share verification
(see [Share validation invariants](#share-validation-invariants); those bugs were ours).

**License of `lazarus/`.** Ratum is [GNU AGPL-3.0](https://github.com/iohzrd/ratum/blob/master/LICENSE).
`lazarus/` is a derivative work; treat it as AGPL-3.0 regardless of the `MIT` string in its
`Cargo.toml` files. Do not relicense or redistribute `lazarus/` under MIT. (An earlier
README said Ratum had no license file; that was wrong.)

iohzrd also maintains a [DATUM Gateway fork](https://github.com/iohzrd/datum_gateway) that
some BLAKE2b miners point at us. That is independent MIT client code. `prime/` speaks to it
the same way it speaks to CONVOY and FlyTheElephant1 — as a stock gateway, not as a source
tree.

### DATUM Gateway forks — CONVOY, FlyTheElephant1, iohzrd

[CONVOY's `datum_gateway`](https://github.com/CONVOYMining/datum_gateway) (MIT, Bitcoin Ocean
LLC / Jason Hughes / contributors) is the client `prime/` was written against, together with
the BLAKE2b forks by [FlyTheElephant1](https://github.com/FlyTheElephant1/datum_gateway) and
[iohzrd](https://github.com/iohzrd/datum_gateway). Their `datum_protocol.c`, `datum_coinbaser.c`
and `datum_header_v2.h` are the specification for the wire format, the coinbaser v2 encoding,
and the BLAKE2b header-v2 share layout; `prime/LICENSE` carries the attribution.
