# Heads up: BLAKE2b DATUM gateways can mine blocks that pay the pool only

**Who this affects:** anyone running a BLAKE2b-capable `datum_gateway` pooled to a DATUM Prime.
If your gateway's user agent is plain `v0.4.1-beta` and you mine a BLAKE2b pool, read on.

**Short version:** at every new block, your gateway hands its miners a full job — all the
transactions, all the fees — whose coinbase pays only the pool's address. Your miners' payout
outputs are missing from it. If one of those jobs finds a block, the block is valid and the pool
keeps 100% of it, so the pool ends up owing your miners their share instead of the block paying
them directly.

## What's happening

`datum_gateway` builds several versions of the coinbase transaction. Type 0 is the small
"tiny firmware" one, which contains no miner payouts. Types 1–5 are the real ones that carry the
payout split the pool sent you.

In `send_mining_notify` the choice looks like this:

```c
if (new_block) {
        cbselect = 0;
} else if (stratum_job_is_blake2b(j)) {
        cbselect = full_coinbase ? (unsigned int)j->blake2b_coinbase_index : 0;
}
...
} else if (new_block && !stratum_job_is_blake2b(j)) {
        cb = &j->subsidy_only_coinbase;
}
```

For a SHA256d miner that's fine: on a new block it gets coinbase type 0 *and* the
`subsidy_only_coinbase`, i.e. a genuinely empty job with no transactions. Little is at stake.

For a **BLAKE2b** miner the second condition excludes it, so it gets coinbase type 0 bolted onto
the **full** template. Real merkle branches, hundreds of transactions, every fee — and a
coinbase with no miner outputs. Nothing warns you, because the job is otherwise perfectly good
work and every share it produces is accepted normally.

It is not a race and not a timeout. It happens on every new height, regardless of how fast the
pool answers your coinbaser request. And your miners keep hashing that job until the gateway's
next work update, so the window of exposure is that whole gap, not an instant.

## What it cost us

Lazarus Pool block
[968440](https://mempool.awokenlazarus.xyz/block/000000000000000ed7b3219894f06bbbdeb0d699b3217ad9016be5be902db69d)
was found on one of these jobs — 266 transactions, 0.00144 BTC of fees, and a coinbase with a
single output to the pool. The job was built when the tip moved and was mined for nine minutes
before it hit. The pool recorded 3.0656 BTC owed back to the miners in that window.

Then we measured it. Over ten minutes across 28 connected gateways:

- 41 accepted shares carried a pool-only coinbase, and **all 41 were full jobs** (37–162
  transactions). None was the harmless subsidy-only startup job.
- Every unpatched stock gateway produced some.
- The two gateways running a patched build produced **zero** (541 shares, none unsplit).

One gateway was at 33 of 33 shares unsplit — its miners never moved off the new-block job at
all. That is the one that found 968440.

## Is my gateway affected?

If you operate the pool: count accepted shares whose coinbase pays only your own script, and
split them by whether the job carried transactions. The subsidy-only ones are expected; full
jobs are the bug. Lazarus Prime now reports this as `pool_only_full_jobs` in `stats.json`, per
gateway, and logs a warning naming the gateway.

If you run a gateway: your user agent tells you. Plain `v0.4.1-beta` is affected;
`v0.4.1-beta+lazarus-split` and `lazarus-gateway` are not.

## The fix

The gateway must never hand a pooled BLAKE2b miner a coinbase that lacks the split. The patch
does that in four places:

- Skip the empty-first job while pooled, so there is no unsplit job to hand out.
- Don't send a type-0 `mining.notify` on a pooled BLAKE2b job; wait for the split instead, and
  set `clean_jobs` on the first split notify of a new height so miners drop stale work.
- Don't apply the five-second "give up and mine empty" fallback while pooled.
- Copy the split coinbase into type 0, so firmware that insists on type 0 still pays out.

Patch: `lazarus/patches/datum-gateway-blake2b-split-upstream.patch`. It applies to
`FlyTheElephant1/datum_gateway` master (121edd06) with no fuzz and builds clean.

```bash
git clone https://github.com/FlyTheElephant1/datum_gateway.git
cd datum_gateway
patch -p1 < datum-gateway-blake2b-split-upstream.patch
cmake -S . -B build -DCMAKE_BUILD_TYPE=Release && cmake --build build -j"$(nproc)"
```

## This is not a pool-side problem

Worth stating plainly, because the obvious first guess is that the pool was slow to send the
coinbaser split. It wasn't, and we can show it rather than argue it. For every unsplit share the
pool now records the gateway's own coinbase section index next to the coinbaser that job named
and how many payout outputs that coinbaser carried:

```
published section 0 while holding coinbaser 1, which carried 41 miner outputs
published section 0 while holding coinbaser 5, which carried 41 miner outputs
```

The gateway was holding a fresh 41-output split and handed its miners section 0 regardless.
Replies were landing in 30 ms with none anywhere near the five-second deadline, so nothing timed
out. That also rules out the tempting pool-side "fix" of pushing the split earlier or unprompted:
the split was already there and unused.

A gateway that genuinely never receives a split produces the same unsplit coinbase, so we
hardened that path anyway — Prime never drops a coinbaser request, serves the split from a shared
snapshot so replies never queue behind a lock, and measures its own reply latency. But the cause
here is the gateway's coinbase selection, and only the gateway can fix it.

The size of it, measured live across 30 connected gateways:

| gateways | accepted shares | on a full job with a pool-only coinbase |
|---|---|---|
| patched | 587 | **0** |
| unpatched | 219 | **34** |

Three unpatched gateways had *every* accepted share unsplit — 23 of 23, 5 of 5, 3 of 3. On those,
any block found would have paid the pool alone.

Only forks that implement BLAKE2b jobs are affected. `OCEAN-xyz`, `CONVOYMining` and `iohzrd`
have no BLAKE2b support on master and cannot hit this.

## Nobody's shares were lost

To be clear about the money: shares mined on these jobs are still valid work and are still
credited to the payout window. What is lost is the *direct* payout when such a job finds a
block. The pool records the amount as owed and the window's work carries forward to the next
block that does pay a split, so miners are delayed rather than shorted.
