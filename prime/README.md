# Lazarus DATUM Prime (`primed`)

The pool side of the DATUM protocol for the BLAKE2b Bitcoin chain, written from scratch.

Any stock `datum_gateway` — OCEAN, [CONVOY](https://github.com/CONVOYMining/datum_gateway),
or the BLAKE2b forks by [FlyTheElephant1](https://github.com/FlyTheElephant1/datum_gateway) and
[iohzrd](https://github.com/iohzrd/datum_gateway) — points at this Prime, unpatched. The
gateway's own node builds every block template. The Prime never sees or chooses transactions;
it does three things:

1. **Dictates the coinbase.** When a gateway asks for a coinbaser, the Prime answers with the
   current TIDES split of the window: one output per miner, proportional to work, after the pool
   fee, then the pool's own output last so the list sums to the requested value. The gateway
   builds its coinbase from that list, so a block found by anyone's miner pays everyone in the
   window.
2. **Verifies every share.** Each share is rebuilt into a full BLAKE2b header v2 from the job
   the gateway described (prevhash, nbits, merkle branches, coinbase halves) and the miner's
   nonce fields, hashed, and checked against the share target — and against the network target,
   so a block is recognised before the gateway says so. The coinbase inside the share is parsed
   and compared with the split the Prime actually issued for that job; a coinbase paying anywhere
   else is rejected.
3. **Keeps the ledger.** Accepted work is credited to the miner's payout address in an
   append-only window sized in multiples of network difficulty. On a block, the split that was
   paid (or, for a stock gateway's pool-only "empty" coinbase, the amount the pool now owes the
   window) is recorded, and the Prime submits the assembled block to its own node too.

## Layout

```
prime/
  Cargo.toml          workspace
  wire/               datum-wire: framing, NaCl session, messages, coinbaser v2, BLAKE2b PoW, share verify
  tides/              tides: share window, split computation, append-only ledger, block log
  primed/             the daemon: sessions, node poller, stats HTTP, CLI
  prime.toml.example
  scripts/regtest-e2e.sh   end-to-end test against a real C datum_gateway on regtest
```

`datum-wire` and `tides` have no async or I/O dependencies beyond `std`; every protocol and
accounting rule is unit-tested in isolation. `primed` is the only crate that touches sockets.

## Build and run

```bash
cd prime
cargo build --release
cp prime.toml.example prime.toml      # edit payout-address, data-dir, rpc, rpc-cookie
target/release/primed -c prime.toml check
target/release/primed -c prime.toml pubkey   # give this to gateway operators
target/release/primed -c prime.toml run
```

Subcommands: `run` (default), `check`, `pubkey`, `window` (dump the window as JSON),
`import-ledger <ledger.json>` (seed an empty window from the previous Prime's export).
Logging is `RUST_LOG` (`info` default; `debug` prints each share decision).

### Taking over from `lazarus-prime`

An existing `lazarus-prime.toml` loads unchanged (`activation-height`, `verify-shares` and
`require-split-gateway` are accepted and reported as no longer applying). A data dir the old
Prime left behind keeps its identity: `lazarus-prime.key` (its 160-byte layout) is read when
there is no `prime.key`, so the pool pubkey every gateway operator pinned stays the same —
`primed pubkey` prints it to confirm. Its `ledger.json` is the whole window; import it before
the first `run`:

```bash
primed -c lazarus-prime.toml import-ledger /path/to/lazarus-prime/ledger.json
primed -c lazarus-prime.toml run
```

This is how the Lazarus pool was cut over on 2026-09-03: same key, same config, 9,021 credit
rows / 570.5 M work / 18 identities imported, `lazarus-gateway` reconnected within a second,
and its shares verified against the same window. Expect a short burst of
`bad-coinbase-outputs` rejects right after any Prime restart: a gateway keeps publishing jobs
built on the coinbaser it got from the previous instance until its next template, and a
coinbase paying a split this instance never issued is refused by design.

### Gateway side

In `datum_gateway_config.json`:

```json
"datum": {
  "pool_host": "stratum.awokenlazarus.xyz",
  "pool_port": 28915,
  "pool_pubkey": "<output of primed pubkey>",
  "pool_pass_workers": true,
  "pool_pass_full_users": true,
  "pooled_mining_only": true
}
```

`mining.pool_address` should be the pool's payout address (a stock gateway sends anything
its template is worth beyond the issued list there). Miners authenticate to the gateway as
`<payout address>.<worker>`; the address is the identity credited in the window. Nothing else
changes for the gateway operator.

The pool's own public stratum, `lazarus-gateway` (in `../lazarus/`), is a DATUM client too and
speaks to this Prime as one; two habits of its are recognised as such. It sends the whole
legacy coinbase as `coinb1` with an empty `coinb2` (a shape no stock gateway produces), and
when its template is worth less than the value a split was issued for it scales every output
down by the same ratio rather than dropping the ones that no longer fit.

## Protocol

Recovered from the C client; the same bytes any gateway already speaks.

* **Frame**: 4-byte header, one little-endian `u32`: `len:22 | reserved:2 | signed | sealed |
  channel | cmd:5`, XOR-obfuscated by a per-direction rolling key (`feedback`, a Murmur3-style
  mix reseeded from the hello). Payloads are plaintext, NaCl sealed boxes, or `crypto_box` on
  the session keys, optionally trailed by an ed25519 signature.
* **Handshake**: client sends a `crypto_box_seal`ed hello containing its long-term and session
  keys, user agent, and a seed; Convoy-lineage clients append `DRS\x01` and an optional resume
  token. The Prime replies sealed to the session key with its MOTD and the seed acknowledgement.
  The generation is detected from the hello, and the matching **configure** layout is sent:
  v1 (32-bit prime id) for OCEAN/fte/iohzrd, v3 (64-bit id, resume token, `ABW_DISABLED`) for
  Convoy.
* **Mining command (0x05)**: coinbaser request → coinbaser v2 reply (`id | (value u64,
  script len, script)…`); pow submit with lazily-attached job (0x01) and coinbase (0x02) sections, the
  BLAKE2b section (0x03: 64-bit ntime and nonce) and the header time (0x04); share receipts
  with the client's reject-code vocabulary; block notify; full-block request and the
  transaction reply.
* **Lineage quirks handled**: 8 vs 256 job slots; `FLAG_BLAKE2B` set (fte/Convoy) or implied by
  the 0x03 section alone (iohzrd); 12-byte extranonce with `b10cf00d` marker; `sia_prevhash`
  form of the previous block.

### BLAKE2b header v2

A share is verified by reproducing exactly what Knots hashes:

```
H1     = tagged_sha256("Bitcoin block header 1",
           version‖prev‖height‖merkle_root‖time‖00‖nbits‖txcount‖flags‖clear_bits‖tagged(xor_key))
H2     = tagged_sha256("Merge-mining hook", H1 ‖ 32 zero bytes ‖ rhs)      the job commitment
coinb1 = 000000 ‖ H2 ‖ 00000000                                            39 bytes, Sia layout
root   = blake2b256(0x00 ‖ coinb1 ‖ extranonce(12))                        what the ASIC calls the merkle root
work   = hidden_prev(32) ‖ nonce(8) ‖ ntime(8) ‖ root(32)                  the 80-byte ASIC pass
hash   = reverse(blake2b256(work) XOR mask(xor_key, clear_bits))
```

`hash <= share_target(pot)` credits `2^pot` work; `hash <= nbits_target` is a block. The
Bitcoin merkle root folds the coinbase txid over the gateway-supplied branches; the coinbase
is `coinb1 ‖ 12 zero bytes ‖ coinb2` with the target byte patched in. A per-connection
`JobSlot` caches the parsed coinbase and the H2 for each `(coinbase id, target byte)`, so a
repeat share on the same job costs one BLAKE2b, not a coinbase parse, a merkle fold, and two
tagged SHA256s.

## TIDES

The window holds credits `(ts, identity, work, height)` until its total work reaches
`window × network difficulty` (converted to difficulty-1 shares), then trims from the oldest
end. A split of value `V` pays `fee = V × fee_bps / 10000` to the pool, then distributes the
rest to identities proportional to their work in the window, dropping outputs below
`min-payout` (their share stays with the pool and is reported as unpaid). The output list is
capped by count and size so the coinbase fits in a gateway's largest coinbase class, with the
pool's output — fee plus whatever could not be placed — appended last. The list therefore
sums to the requested value; a stock gateway pays it verbatim and only adds a pool output of
its own when the template turns out to be worth more, and `lazarus-gateway`, which writes
exactly the list it is given, pays the fee instead of burning it.

Stock gateways build several coinbase sizes and hand small miners those with room for only
the first few outputs, or none at all while a coinbaser reply is in flight. The Prime
classifies every share's coinbase as **Split** (every issued miner output paid), **Partial**
(a subset, each in full), **PoolOnly** (only the pool's script), or **Foreign** (anything
else, rejected). An output is "paid" when it carries at least the issued amount scaled by
`actual value / issued value` when the template is worth less than the split assumed (never
more than issued) — this covers both a gateway that scales the list and one that drops
outputs. The pool's own output is exempt from any minimum; it takes what is left. A block
found on a Partial or PoolOnly coinbase records `owed_sats` — what the pool holds on behalf of
the window — in `blocks.jsonl`.

### On disk (`data-dir`)

| File | What |
|------|------|
| `prime.key` | ed25519 seed ‖ x25519 secret, 0600, generated once (`lazarus-prime.key` is read instead when present) |
| `credits.bin` | append-only 24-byte credit rows; replayed at start, compacted when the window trims |
| `identities.txt` | interned identity table (one address per line, index = row id) |
| `window.json` | window target work and lifetime counters |
| `blocks.jsonl` | one JSON line per block event (candidate, submit outcome, settled/orphaned) |
| `stats.json`, `ledger.json` | mirrors of the HTTP endpoints, rewritten atomically |

## Stats

`GET /stats.json` on `stats-listen` (also mirrored to `data-dir/stats.json`) is the document the
pool UI reads: `pool` (pubkey, fee, window multiple, advertise address, uptime), `node`
(height, tip hash, difficulty, tip age), `window` (target/total work, fill percent, per-miner
`work`, `shares`, `hashrate_ghs`, `share_percent`, `payout_sats` at the current reward),
`clients` (per gateway: generation, user agent, accepted/rejected, last reject reason),
`blocks`, `owed`, `totals`. `/ledger.json` is the previous Prime's credits view for the UI's
hashrate graph; `/healthz` returns `ok`.

## Tests

```bash
cargo test                       # wire (38), tides (7), primed (6)
scripts/regtest-e2e.sh convoy    # or fte | iohzrd: real C gateway + real Knots on regtest
```

The unit tests pin the frame obfuscation, nonce derivation, hello round trip for both
generations, configure v1/v3 byte layouts, coinbaser v2, pow submit parse/encode, tagged
hashes, and share verification including grinding real BLAKE2b shares against an easy target.

The end-to-end script builds the named `datum_gateway`, points it at a `primed` on a local
BLAKE2b regtest node, and checks that the handshake, configure, coinbaser, shares, block
candidates and `submitblock` all happen. All three lineages were run this way while this was
written; a block found through a stock Convoy gateway paid the two-miner TIDES split on-chain
exactly as issued (65.6% / 34.4% after the 0.5% fee), and the block the Prime assembled from
the gateway's transaction reply was byte-identical to the one the gateway submitted. With the
complete list, a Convoy-found block's coinbase carried the miner's 12.4375 and the pool's
0.0625 (0.5%) once each — no second pool output.

## Design notes

* Verification is cheap enough that every share is fully rebuilt and checked, always. The
  ignored `verify_throughput` test measures both paths on a single core: on a Ryzen 9950X3D,
  about 376k shares/s for a share carrying fresh job + coinbase sections against a 2000-tx
  template, and about 536k shares/s for later shares on the same job
  (`cargo test --release -p datum-wire -- --ignored --nocapture verify_throughput`). A whole
  pool's share flow fits on one core with orders of magnitude to spare.
* One Tokio task per connection owns both halves of the socket; reads go through a
  cancel-safe growable buffer so `select!` over reads, tip broadcasts and keepalives can never
  desynchronise the frame stream.
* The ledger is a `Mutex` held only for the microseconds a credit takes; nothing awaits under
  it. Coinbaser replies compute the split from a snapshot.
* Block notify is fanned out with a broadcast channel; a tip change from the node poller or a
  block candidate from any session reaches every other gateway immediately.
* Idle gateways get a zero-length INFO frame every 20 s (the client's global timeout is 60 s);
  a gateway silent for 300 s is dropped. Handshake must complete in 15 s.
* Duplicate shares are caught per job by hash; a job's set is cleared when the gateway moves
  the slot to a new job. Shares one height behind are accepted for `stale-grace-secs` after the
  tip moved, matching template refresh latency.
* The Prime submits every candidate block to its own node as well as trusting the gateway to.
  `duplicate` from `submitblock` is the expected outcome and is recorded as such.

## License

MIT — see [`LICENSE`](LICENSE), which also records provenance. This is not derived from
Ratum (AGPL-3.0) and contains none of its code. The protocol was recovered from the
MIT-licensed DATUM Gateway trees named there.
