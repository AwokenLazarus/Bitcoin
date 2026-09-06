# Lazarus DATUM Prime (`primed`)

The pool side of the DATUM protocol for the BLAKE2b Bitcoin chain, written from scratch.

Any stock `datum_gateway` — OCEAN, [CONVOY](https://github.com/CONVOYMining/datum_gateway),
the BLAKE2b forks by [FlyTheElephant1](https://github.com/FlyTheElephant1/datum_gateway) and
[iohzrd](https://github.com/iohzrd/datum_gateway), or the packaged
[StartOS](https://github.com/Retropex/datum-gateway-startos/releases) build — points at this
Prime, unpatched (see [Supported gateways](#supported-gateways)). The
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
  scripts/regtest-e2e.sh          end-to-end test against a real C datum_gateway on regtest
  scripts/regtest-divergence.sh   two-node test: gateway template survives a pool node with a different mempool/tip
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
`<payout address>.<worker>`; the address is the identity credited in the window. The
identity ends at the first `.` or `~`, so a `~modifier` suffix that a gateway without
`stratum_username_mod` forwards verbatim still credits the address. Nothing else changes for
the gateway operator.

The pool's own public stratum, `lazarus-gateway` (in `../lazarus/`), is a DATUM client too and
speaks to this Prime as one; two habits of its are recognised as such. It sends the whole
legacy coinbase as `coinb1` with an empty `coinb2` (a shape no stock gateway produces), and
when its template is worth less than the value a split was issued for it scales every output
down by the same ratio rather than dropping the ones that no longer fit.

## Supported gateways

Four upstreams, two protocol generations. The refs are what the e2e script builds, and are
the versions this Prime is checked against.

| Name | Upstream | Ref tracked | Generation |
|---|---|---|---|
| `convoy` | `CONVOYMining/datum_gateway` | `master` | Convoy, configure v3 |
| `fte` | `FlyTheElephant1/datum_gateway` | `test/console-collapse-pr14-pr17` (now also `master`) | OCEAN, configure v1 |
| `iohzrd` | `iohzrd/datum_gateway` | `master` (has the `blake2b` branch and two commits more) | OCEAN, configure v1 |
| `startos` | packaged by `Retropex/datum-gateway-startos` | the `datum_gateway` submodule of the newest `pow_*` release | OCEAN, configure v1 |

The StartOS package ships the gateway as a submodule, so a release pins one gateway commit
rather than naming a branch. `scripts/regtest-e2e.sh startos` resolves that commit from the
release and builds it, which keeps the test honest as new packages ship. Its unreleased
`pow-convoy` branch is Convoy-lineage and reachable as `startos-convoy`.

Two upstream rules are load-bearing and are enforced here rather than discovered in
production:

* **RDTS output scripts.** Knots activates RDTS (BIP 110) as a flag day at the BLAKE2b fork
  height, which limits every output of the generation transaction to a 34-byte scriptPubKey
  (83 if it starts with `OP_RETURN`). The newest gateways enforce it too: an oversized miner
  payout is left out of the coinbase, and an oversized *pool* payout stops them serving work
  for the block at all. `address::to_script` therefore treats an address whose output script
  cannot fit — a witness program over 32 bytes is valid but does not — as unpayable, so its
  share stays in the pool remainder instead of becoming an output a gateway would drop. An
  unpayable `payout-address` is refused at startup.
* **The ABW flag.** Convoy's configure v3 carries a flags byte, and this pool sets
  `ABW_DISABLED` because it runs no anti-withholding. Convoy builds from 2026-09-02 onward
  require it: without it the gateway waits for an assignment that never comes and serves no
  work. Convoy-lineage builds from *before* that date reject a non-zero flags byte outright,
  so they need a gateway update rather than a change here. Only the unreleased `pow-convoy`
  branch is still pinned that far back; every published StartOS package is OCEAN-lineage and
  never sees this byte.

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

### Why stock gateways mine pool-only coinbases, and what the pool can do about it

The cause is in the gateway, not here. In stock `send_mining_notify`:

```c
if (new_block) {
        cbselect = 0;
} else if (stratum_job_is_blake2b(j)) {
        cbselect = full_coinbase ? (unsigned int)j->blake2b_coinbase_index : 0;
}
```

On the **first notify of every new height** stock hands every miner coinbase type 0, and stock
builds type 0 as the "tiny firmware" variant with no miner outputs even when types 1–5 hold the
split. So that job is a full template — hundreds of transactions, all the fees — whose coinbase
pays only the pool script, and the miners keep hashing it until the gateway's next work update.
It is unconditional: no coinbaser reply, however fast, changes it.

Block 968440 was found on exactly that job, nine minutes after the tip moved. Measured over ten
minutes on 28 live gateways, every pool-only share was a *full* job (37–162 transactions), none
was the subsidy-only startup job, and the two gateways running `lazarus-gateway` and the
split-only patch had none at all.

That the reply is not the problem is now recorded per share rather than inferred. Each pool-only
warning names the gateway's own coinbase section index next to the coinbaser that job cited and
the number of outputs we issued under it, and every case reads:

```
published section 0 while holding coinbaser 1, which carried 41 miner outputs
```

The gateway held a fresh 41-output split and published section 0 regardless, with replies at
30 ms and `coinbasers_slow` at zero. Nothing timed out and nothing was missing.

Only the gateway can fix it, and `lazarus/patches/datum-gateway-split-only.patch` does: it never
sends type 0 on a pooled BLAKE2b job, and copies the type-4 split into type 0 so firmware that
insists on type 0 still pays TIDES. Point an operator at that patch or at `lazarus-gateway`.
`lazarus/patches/datum-gateway-blake2b-split-upstream.patch` is the same fix without the Lazarus
user-agent bump, filed upstream as
[FlyTheElephant1#5](https://github.com/FlyTheElephant1/datum_gateway/pull/5);
`docs/blake2b-unsplit-coinbase-advisory.md` is the plain-language write-up to hand to an operator.

How bad it is depends on the build, and there are two severities:

| build | when it publishes a pool-only full job | fix |
|---|---|---|
| `FlyTheElephant1` `master` | every BLAKE2b miner, first notify of every height — 100% of shares live | [#5](https://github.com/FlyTheElephant1/datum_gateway/pull/5) |
| `CONVOYMining` `b9ea7dc` | only when the coinbaser is late — 1 share in 167 live | [#13](https://github.com/CONVOYMining/datum_gateway/pull/13) |
| `iohzrd` `40cf813` | same late-coinbaser case | [#1](https://github.com/iohzrd/datum_gateway/pull/1) |
| `OCEAN-xyz` `dbc3b14` | same late-coinbaser case, SHA256d (no BLAKE2b code at all) | not filed |

Convoy and iohzrd already return `DATUM_COINBASE_ID_EMPTY` on a new block and pair it with
`subsidy_only_coinbase`, so they avoid the unconditional form. What all of them keep is
`cbselect = 0` when `!full_coinbase_ready`, with class 0 pool-only and the full template attached —
and the five-second give-up in `stratum_job_coinbaser_ready` makes that reachable by design. So the
narrow case is upstream DATUM behaviour rather than a BLAKE2b fork bug, and it is why Prime measures
its own reply latency even though it is not the cause of the unconditional one.

What the pool owes in return is to not be a *second* cause, because a reply that never arrives
leaves `available_coinbase_outputs_count` at zero and produces the same unsplit coinbase.
`datum_protocol_coinbaser_fetch` blocks on a condvar for five seconds and then gives up, so
silence is the one reply Prime cannot give. The per-session token bucket now chooses only
whether a reply is recomputed or repeated verbatim from the last one for that same value; it
never withholds one, and being over it no longer counts toward the reject-flood budget. The
split is computed off a snapshot of the window shared by all sessions and rebuilt at most once a
second (`Shared::coinbaser_base`), so a reply never queues behind the ledger mutex when every
gateway asks at once at a tip change. Live, that holds replies to tens of milliseconds with
`coinbasers_slow` at zero — which is how we know the gateway is the remaining cause.

For the same reason Prime **never sends a coinbaser the gateway did not ask for**. The reply is
only used when its value equals the requested one, and it lands in a global two-slot buffer
whose index flips on every reply — so an unsolicited one can make a legitimate in-flight fetch
read the wrong value and fall back to the pool-only coinbase this is all guarding against.

### Restarts used to throw away a burst of good work

A reconnecting gateway is still serving jobs whose coinbases carry the split from coinbaser ids
the *previous* process issued. The new session has no record of those ids, so those outputs are
ones Prime cannot vouch for and the shares are refused as `bad-coinbase-outputs` — valid miner
work discarded purely because we restarted. Measured at ~100 shares per restart.

A session now sends a block-notify immediately after `configure`, which stock handles as
`datum_blocktemplates_notifynew` and answers by rebuilding its templates and asking for a fresh
coinbaser. The gateway rotates off the stale work in seconds instead of minutes: the same restart
went from 101 refused shares to 1.

Note this is the *only* thing worth pushing at handshake. Pushing an unsolicited **coinbaser**
does not work and is actively harmful — see above.

`totals` in `stats.json` carries the early warning: `pool_only_shares` counts accepted shares
whose coinbase could not have paid the window, per gateway in `clients` as well. A gateway
publishing unsplit work says so over thousands of shares before it gets lucky, and each one
logs a warning naming the gateway (at most once every five minutes).

Two kinds land there and they mean different things. Stock emits one **subsidy-only** job per
height (`JOB_STATE_EMPTY_PLUS`: no transactions, coinbase id `0xff`), which is cheap to lose. A
**full** job with a pool-only coinbase is the expensive one — a whole template's fees and
subsidy with no miner outputs — and it is counted separately as `pool_only_full_jobs`, per
gateway too. That is the number to watch, and on an unpatched stock gateway it climbs on every
new height. 968440 was one of these. The warning also states which of the two gateway faults it
is, read off the share: if the coinbaser it cited carried outputs, the gateway had the split and
published a section without it; if it carried none, the gateway never got a split to place.
`coinbasers_slow`,
`coinbaser_max_ms`, `coinbasers_repeated` and `coinbasers_over_rate` show whether the pool is
the reason: a reply over a second logs a warning while there is still margin against the
gateway's five-second deadline.

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
`clients` (per gateway: generation, user agent, accepted/rejected, last reject reason,
`pool_only_shares`), `blocks`, `owed`, `totals`. `/ledger.json` is the previous Prime's credits view for the UI's
hashrate graph; `/healthz` returns `ok`.

## Tests

```bash
cargo test                                  # wire (44), tides (11), primed (12)
cargo test --release -p primed --test replay_e2e -- --ignored --nocapture   # hostile gateway vs a real primed
scripts/regtest-e2e.sh convoy               # or fte | iohzrd | startos: real C gateway + real Knots on regtest
MINER_CMD='...' scripts/regtest-divergence.sh fte   # two nodes with different mempools and tips
```

`replay_e2e` starts a `primed`, speaks DATUM to it as a gateway would, grinds one genuine
diff-1 share (~2^32 BLAKE2b hashes, 10–60 s across all cores) and replays it fourteen ways —
job section flipped in an unread field, bare, on another slot, after a reconnect, from another
key — asserting it is credited exactly once. `PRIMED_BIN=/path/to/primed` points the same
attack at another build; against the pre-fix binary the first replay is accepted.

The unit tests pin the frame obfuscation, nonce derivation, hello round trip for both
generations, configure v1/v3 byte layouts, coinbaser v2, pow submit parse/encode, tagged
hashes, and share verification including grinding real BLAKE2b shares against an easy target.

The end-to-end script builds the named `datum_gateway`, points it at a `primed` on a local
BLAKE2b regtest node, and checks that the handshake, configure, coinbaser, shares, block
candidates and `submitblock` all happen. `convoy`, `fte`, `iohzrd` and `startos` all pass it at
the heads in the table above, each reaching handshake, configure and coinbaser against a real
Knots regtest node. `startos-convoy` fails on purpose: its pin predates Convoy's ABW flag, so
it rejects the configure and the script says so. Earlier,
a block found through a stock Convoy gateway paid the two-miner TIDES split on-chain
exactly as issued (65.6% / 34.4% after the 0.5% fee), and the block the Prime assembled from
the gateway's transaction reply was byte-identical to the one the gateway submitted. With the
complete list, a Convoy-found block's coinbase carried the miner's 12.4375 and the pool's
0.0625 (0.5%) once each — no second pool output.

### Template divergence

The e2e test runs the gateway and the Prime against the same node, so it cannot tell whether
the block the Prime submits is the gateway's template or something the pool's node would have
built. `regtest-divergence.sh` separates them: node A is the pool's node (the Prime submits
there), node B is the gateway's node, a fresh datadir synced from A. It cuts the two apart and
gives each transactions the other never sees (raw transactions signed by A's wallet but
broadcast only to B; wallet sends on A), so `getblocktemplate` differs on the two nodes at the
same height. It then mines through a stock `fte` gateway and asserts, from `getblock` on A:

* the block A accepted contains every B-only transaction and none of A's own — the pool node
  accepted a block it could not have built, so the Prime reassembled the gateway's template
  byte-for-byte rather than substituting its node's view;
* the coinbase carries the gateway's secondary tag (`Lazarus␏divergence-gw`) and the issued
  split (first block after start is `pool-only`, as the stock gateway publishes an empty
  coinbaser job until the first reply lands; the owed amount is recorded);
* the gateway's own node B has the same block, so both sides agree without ever talking.

Then it drifts the tips. With B one block behind A (A mines a block B never hears about), the
gateway's next solve is a competing block at A's tip height: the Prime accepts the share inside
`stale-grace-secs`, A answers `inconclusive` (valid, not best), the 30 s confirm pass labels the
record `orphan:split`, the next solve on B's branch reorgs A, and the label clears
(`back in the main chain`, record `split`, settled). Finally A and B are reconnected and must
agree. The whole run takes about a minute on regtest with a GPU miner.

Doing this by hand first surfaced three bugs, all fixed:

* a record labelled `orphan:` was never re-checked, so a competing block that later won the
  reorg — exactly the lagging-gateway-node case above — stayed an orphan forever, and the pool
  UI read orphan state from the wrong field (`submit` rather than `kind`);
* two block candidates solved on the *same job* (regtest does it every share; on mainnet it
  needs two solves of one job seconds apart) overwrote each other in the pending map, so the
  gateway's single transaction reply assembled and submitted only the later one. Every
  candidate for a job is now kept and submitted from the one transaction set;
* when the gateway's node is two or more blocks *ahead* of the pool's node, every share is
  rejected as stale (correct: the Prime cannot verify work on a chain its node has not seen, and
  a stock gateway then reconnects every 30 s for lack of accepts). That is a lagging pool node,
  not a stale gateway, and it is now logged as such once a minute per session.

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
* Duplicate shares are caught by hash in one set shared by every session and keyed by block
  height. The hash commits to the job (prev, merkle, nBits, txcount, version) and the miner's
  nonces, so it is unique per height and needs no per-job scoping; nothing a gateway sends —
  a re-sent job section, a reconnect, a new key — can empty it. Housekeeping prunes heights
  below what the stale check still accepts, and at a hard cap new work is refused rather than
  old work forgotten. (Before 2026-09-06 the set was per session and per job and was cleared
  whenever the job section changed, which let one share be credited without limit.) Shares one
  height behind are accepted for `stale-grace-secs` after the tip moved, matching template
  refresh latency.
* The coinbase check bounds miner outputs from both sides. An issued output may not be paid
  *less* than its share (scaled down when the template is worth less than the split assumed)
  and may not be paid *more* than Prime issued against its script (scaled up by the same ratio
  when the template is worth more, which is how `lazarus-gateway` rescales). The upper bound
  is what stops a gateway paying every miner exactly and sending the pool's remainder — fee,
  rounding, unplaced dust — to an address of its own choosing; it is held per script, not per
  identity, because two window identities may resolve to one scriptPubKey.
* Identities are folded before interning: a bech32 address is lowercased (BIP 173 forbids mixed
  case, so this is safe and idempotent), base58 and non-addresses are kept byte-exact. One
  payout address is one TIDES row, however each rig's config cases it.
* A gateway's txcount convention (does `txn_count` include the coinbase?) is learned from the
  first share that verifies on a job and pinned for the slot, so `VerifiedShare::commitment`
  is always the header the miner actually ground.
* What a connection can make the Prime hold is bounded: `max-connections` (256) and
  `max-connections-per-ip` (8) at accept; a coinbase section over 20 000 bytes or a ninth
  coinbase id in a slot is refused; only the 16 most recently started job slots keep their
  sections; and `session-coinbase-budget` (4 MiB) caps the total. Coinbaser requests are
  token-bucketed (32, refilled one per second) because each one is a full split over the
  window under the ledger lock, and a session with 2 000 rejects or malformed messages in
  10 s is dropped. Release builds keep `overflow-checks` on.
* The Prime submits every candidate block to its own node as well as trusting the gateway to.
  `duplicate` from `submitblock` is the expected outcome and is recorded as such;
  `inconclusive` means a valid block that is not (yet) the best tip. Records settle when the
  node reports positive confirmations; a record called `orphan:` is re-checked for 100 blocks
  and un-labelled if a reorg brings it back.

## License

MIT — see [`LICENSE`](LICENSE), which also records provenance. This is not derived from
Ratum (AGPL-3.0) and contains none of its code. The protocol was recovered from the
MIT-licensed DATUM Gateway trees named there.
