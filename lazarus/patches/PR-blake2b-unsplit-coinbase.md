# PR: BLAKE2b pooled jobs must never carry an unsplit coinbase

Target: `FlyTheElephant1/datum_gateway`, base `master` (121edd06)
Branch: `fix/blake2b-unsplit-coinbase`
Patch: `lazarus/patches/datum-gateway-blake2b-split-upstream.patch`

---

## Title

Fix: BLAKE2b pooled jobs hand miners a full template with a pool-only coinbase

## Body

### The problem

In `send_mining_notify`, coinbase selection runs like this:

```c
if (new_block) {
        cbselect = 0;
} else if (stratum_job_is_blake2b(j)) {
        cbselect = full_coinbase ? (unsigned int)j->blake2b_coinbase_index : 0;
}
...
} else if (new_block && !stratum_job_is_blake2b(j)) {
        snprintf(s, sizeof(s), "\"N%s%2.2x\",\"%s\",\"", j->job_id, (unsigned int)255, j->prevhash);
        cb = &j->subsidy_only_coinbase;
}
```

For a SHA256d miner on a new block this is coherent: `cbselect = 0` is paired with
`cb = &j->subsidy_only_coinbase`, so the miner gets a genuinely empty, subsidy-only job. Little
value is at risk and the comment right below says as much ("new block work always is just a
blank coinbase").

A **BLAKE2b** job is excluded from that pairing by `!stratum_job_is_blake2b(j)`, but not from the
`cbselect = 0` above it. So a BLAKE2b miner receives coinbase **type 0** — which holds no
coinbaser outputs — attached to the **full** template: real merkle branches, every transaction,
every fee. A block found on that job is valid and pays the pool's script alone. The DATUM payout
split is silently lost.

It is not a race and not the coinbaser timeout. It happens on every new height regardless of how
quickly the pool answers the coinbaser request, and the miner keeps that job until the gateway's
next work update, so the exposure is that entire interval rather than an instant.

### Evidence

Observed on a live BLAKE2b pool (Lazarus). Block
`000000000000000ed7b3219894f06bbbdeb0d699b3217ad9016be5be902db69d` (height 968440) was found on
one of these jobs: 266 transactions, 144,067 sats of fees, and a coinbase with a single output to
the pool script. The job was built when the tip moved and was mined for nine minutes before it
hit. The pool had to record 3.0656 BTC as owed back to the miners in that payout window.

Measured across 28 connected gateways over ten minutes, classifying every accepted share's
coinbase:

- 41 shares carried a pool-only coinbase, and **all 41 were full jobs** (37–162 transactions).
  None was the harmless subsidy-only startup job.
- Every unpatched gateway produced some; one produced 33 out of 33.
- Gateways running this patch produced **zero** across 541 shares.

The coinbaser timeout was ruled out directly, not by elimination. The pool logged, for each
pool-only share, the gateway's own coinbase section index alongside the coinbaser the job named
and how many outputs that coinbaser carried. Every case reads the same way:

```
faf00bff61f1c194 ... published section 0 while holding coinbaser 1, which carried 41 miner outputs
83beb91a828c3e07 ... published section 0 while holding coinbaser 5, which carried 41 miner outputs
```

The gateway had a fresh 41-output split in hand and published section 0 anyway. Coinbaser replies
over the same period were measured at 30 ms worst-case with zero replies approaching the
five-second deadline, so nothing had timed out. The selection itself is the bug, which is why no
amount of pool-side speed or an unsolicited push of the split can prevent it.

### The fix

The gateway should never hand a pooled miner work whose coinbase cannot pay the pool's split.
Four changes, all gated on `datum_protocol_is_active()` so **solo behaviour is unchanged**:

1. `datum_stratum.c` — on a pooled BLAKE2b job, do not send a type-0 `mining.notify`; wait until
   the coinbaser has filled `blake2b_coinbase_index`. Set `clean_jobs` on the first split notify
   of a new height so miners drop stale-prevhash work, since the empty-first job is skipped.
2. `datum_stratum.c` — `stratum_job_coinbaser_ready` no longer applies the five-second
   "give up and publish empty" fallback while pooled. Solo keeps it.
3. `datum_coinbaser.c` — `generate_coinbase_txns_for_stratum_job` returns instead of building an
   empty coinbase when a pooled job has no coinbaser outputs, and the coinbaser thread only
   publishes once at least one output is in hand.
4. `datum_coinbaser.c` — copy the BLAKE2b payout coinbase (type 4) into type 0, so firmware that
   insists on type 0 still pays the split rather than silently paying the pool alone.

### Trade-off

Skipping the empty-first job means miners briefly hold previous-height work at a block change
instead of hashing a subsidy-only job. That is why change 1 also forces `clean_jobs` on the first
split notify. In exchange, no miner is ever handed a full template it cannot be paid from. For a
DATUM pool that is the right side of the trade: the empty-job window costs a few seconds of
stale hashing, while an unsplit full job costs the entire block.

### Testing

- Applies to `master` (121edd06) with no fuzz.
- `cmake -S . -B build -DCMAKE_BUILD_TYPE=Release && cmake --build build` completes with no new
  warnings in the three modified files.
- Running in production on Lazarus Pool: gateways on this patch show zero unsplit shares, versus
  a continuous stream from unpatched gateways on the same pool.

### Scope

Only forks implementing BLAKE2b jobs are affected. `OCEAN-xyz/datum_gateway`,
`CONVOYMining/datum_gateway` and `iohzrd/datum_gateway` have no `stratum_job_is_blake2b` on
master and cannot hit this.
