# Bitcoin (BLAKE2b)

Code and configs for a BLAKE2b Knots + DATUM mining pool, Electrum indexer, explorer hooks, and a GPU miner.

This is the working tree from a homelab Umbrel node after the SHA-256d → BLAKE2b fork (height 961640 on this chain). Copy the **design**, not host paths or keys.

## Layout

| Path | What |
|------|------|
| `pool/` | Public pool dashboard, Ocean-style coinbaser, browser miner, Electrum cutover, block-notify mail |
| `pool/config.example.json` | Copy to `config.json` and fill in |
| `node/` | Live Umbrel Knots path (rc4 prefix, `blake2b.conf`, app `bitcoin.conf`, pre-start hook) |
| `node/umbrel/` | App `bitcoin.conf`, compose bind snippet, `hooks/pre-start` |
| `scripts/` | Umbrel helpers: DATUM/electrs/pool persist, mempool header widen, status |
| `systemd/` | User units + ensure timer |
| `miner/` | OpenCL BLAKE2b GPU miner |
| `patches/` | DATUM PROP / coinbaser wiring notes |
| `docs/blake2b-mining-pool-playbook.md` | Operator playbook (no personal secrets) |
| `lazarus/` | First-party DATUM Prime, stratum gateway, and protocol (Rust) |


## Knots on Umbrel (live node)

See [`node/README.md`](node/README.md). Short version:

1. `PREFIX=~/blake2b/prefix ./node/install-rc4.sh` after you verify the official rc4 tarball.
2. Install `node/blake2b.conf` + `node/umbrel/bitcoin.conf` in the Knots app datadir.
3. Install `node/umbrel/hooks/pre-start` so rc4 is bind-mounted again after an app update.
4. App stays on RPC **9332** / P2P **9333**. Do not commit `umbrel-bitcoin.conf`.

## Quick start (pool UI)

```bash
cd pool
cp config.example.json config.json
# edit cookie_file, datum_auth_file, stratum_host, explorer_url
python3 server.py
```

DATUM path (OCEAN model): the user runs Knots + a DATUM gateway and points the gateway at Prime on port 28915. Prime only tracks shares and sets the coinbase split; the user's node builds the template. Public stratum on 23334 / 3333 is optional and uses our gateways.

The Prime we run is [`lazarus/prime`](lazarus/prime) (`lazarus-prime`). Public stratum is [`lazarus/gateway`](lazarus/gateway) (ASIC `:23334`, GPU/CPU `:3333`). Remote operators point their own `datum_gateway` at `stratum.awokenlazarus.xyz:28915`. The pool UI scrapes both gateways (`:7152` and `:7153`), Prime stats on localhost `:28916`, and Knots RPC via cookie. There is no pool fee; a found block pays the TIDES window 100% in the coinbase. Coinbase tag is `Lazarus`.

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

## Credits

### DATUM — Bitcoin Ocean / Jason Hughes

The DATUM model this pool is built on — the Prime/gateway split, where the miner's own node
builds the block template and the pool only tracks shares and dictates the coinbase — is
[Bitcoin Ocean](https://github.com/OCEAN-xyz/datum_gateway)'s, by Jason Hughes. The wire
format in [`lazarus/protocol`](lazarus/protocol) is recovered from OCEAN's
`datum_protocol.c` / `.h` (MIT).

### Ratum — iohzrd

[**Ratum**](https://github.com/iohzrd/ratum) by [iohzrd](https://github.com/iohzrd) is the
first Rust DATUM pool for the [Bitcoin Knots BLAKE2b hardfork
chain](https://github.com/bitcoinknots/bitcoin/pull/359), and it is what made this pool
possible on the timescale it happened. Ratum Prime was the Prime this node ran in
production, vendored at `0.1.3` (`e828545`), before `lazarus/` existed. iohzrd also wrote
the [DATUM Gateway fork](https://github.com/iohzrd/datum_gateway) that BLAKE2b miners
actually point at us.

This tree is a reimplementation, not a rebrand — but it grew directly out of that vendored
copy, and git still records `lazarus/Cargo.lock` and `lazarus/prime/Cargo.toml` as renames
of Ratum's files. Concretely, what we took:

| Ours | Follows Ratum's |
|------|-----------------|
| `protocol/src/{handshake,channel,nacl,header}.rs` | `core/src/datum/{handshake,framing}.rs` — NaCl-sealed handshake, obfuscated frame headers |
| `protocol/src/mining.rs` | `core/src/datum/messages.rs` — message subtypes and share encoding |
| `protocol/src/coinbaser.rs` | coinbaser v2 encoding |
| `protocol/src/pow.rs` | `core/src/{header,target,nonce}.rs` — the version 2 header, the two-pass BLAKE2b hash, target handling |
| `prime/` — ledger, TIDES window, coinbaser | `prime/` — the shape of a Prime that keeps a share window and sets the split |
| the `#[ignore]`d tests that grind a real share | Ratum's release-mode `--ignored` proof-of-work tests |

Where we diverged: payouts split the coinbase across the whole TIDES window natively, the
gateway publishes only split templates, and share verification is enforced pool-side
(see [Share validation invariants](#share-validation-invariants)). The bugs documented
there were ours, found in our own code.

Ratum carries no license file. It is credited here as the work it is; if you intend to
reuse it, ask iohzrd first.
