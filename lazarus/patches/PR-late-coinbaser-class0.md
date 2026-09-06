### The problem

`datum_stratum_coinbase_index` already returns `DATUM_COINBASE_ID_EMPTY` on a new block, and
`send_mining_notify` pairs that with `subsidy_only_coinbase`, so a miner at a block change gets a
genuinely empty job. That is the right shape, and it is why this repo does *not* have the
unconditional problem I filed against `FlyTheElephant1/datum_gateway`
([PR #5 there](https://github.com/FlyTheElephant1/datum_gateway/pull/5)).

What is left is the not-ready branch, which still returns `0`:

```c
	if (!sdata || ... || !sdata->full_coinbase_ready ||
	    ...) return 0;
```

Coinbase class 0 pays only the pool. It is excluded from
`generate_coinbase_txns_for_stratum_job_subtypebysize` — which is what reads
`available_coinbase_outputs_count` — and is instead built inline as exactly two outputs:

```c
cb2idx[0] += append_bitcoin_varint_hex(2, &s->coinbase[0].coinb2[cb2idx[0]]); // us and witness commit
...
cb2idx[0] += sprintf(&s->coinbase[0].coinb2[cb2idx[0]], "%016llx", __builtin_bswap64(s->coinbase_value));
cb2idx[0] += append_bitcoin_varint_hex(s->pool_addr_script_len, ...);   // the pool's script
```

The pool's script taking the entire `coinbase_value`, plus the witness commitment. The comment
says so: `0 = "empty" --- just pays pool addr`.

Returned from the not-ready branch, that class is paired with `&j->coinbase[cbselect]` and the
**full** template. So the job a miner receives commits to every transaction and fee, and to a
coinbase that cannot pay the pooled split. A block found on it pays the pool alone, and the
miners who earned it get nothing.

### Why it is reachable

Not a narrow race. `stratum_job_coinbaser_ready` enforces a backup timeout that gives up and
publishes in exactly this state:

```c
	if ((sdata->loop_tsms > job->tsms) && (sdata->loop_tsms - job->tsms) > 5000) {
		// enforce a timeout of 5 seconds on waiting on a coinbaser...
		sdata->full_coinbase_ready = false;
		return true;
	}
```

So any coinbaser that has not landed within five seconds guarantees a full template published
with a pool-only coinbase, for as long as the miner holds that job.

Measured on a live BLAKE2b DATUM pool, classifying every accepted share's coinbase by whether it
could pay the split: a gateway built from this tree produced 1 such share in 167. For comparison,
unpatched `FlyTheElephant1` builds on the same pool ran at 100% because their case is
unconditional. Rare here, but the cost when it lands is a whole block.

### The fix

While the DATUM protocol is active, fall back to the subsidy-only job rather than class 0. That
costs one template's fees for the moment the coinbaser is late, against losing the entire block
otherwise — the same trade the `new_block` path already makes.

Solo is untouched, where class 0 legitimately pays the configured address. That is also why the
existing assertion in `datum_blake2b_coinbase_selection_tests` that the not-ready case yields
class 0 still passes: it runs with the protocol inactive.

### Testing

- Applies to `master` and builds clean with `cmake --build`, no new warnings.
- `./build/datum_gateway --test` exits 0, unchanged from stock on the same tree.
- The pool-side counters that found this (per-gateway "accepted shares whose coinbase could not
  have paid the window") are in a from-scratch DATUM server, so the measurement is independent of
  this codebase.
