# PR: pooled jobs never carry a pool-only coinbase on a full template

Target: `FlyTheElephant1/datum_gateway`, base `master` (a5f28aa); also applies to `feature/cutting-edge` (8002c56)
Patch: `lazarus/patches/datum-gateway-fte-late-coinbaser.patch` (2 files, +32 −3)

---

## Title

Pooled: serve subsidy-only work (or nothing) until the coinbaser lands, never class 0 on a full template

## Body

### The problem

`f827701` made every miner get `COINBASE_TYPE_YUGE` once the coinbaser is in. That is the right
policy, and on a live BLAKE2b pool it means a found block pays the whole window. What is left is the
window *before* the coinbaser is in. There `datum_stratum_coinbase_index` still returns `0`:

```c
	if (new_block) return DATUM_COINBASE_ID_EMPTY;
	if (!sdata || !sdata->cur_stratum_job ||
	    sdata->cur_stratum_job->job_state < JOB_STATE_FULL_PRIORITY_WAIT_COINBASER ||
	    !sdata->full_coinbase_ready) return 0;
```

Class 0 is built inline as two outputs, the pool's script taking the whole `coinbase_value` and the
witness commitment, and `send_mining_notify` pairs it with `&j->coinbase[0]` and the **full**
template. A block found on that job pays the pool alone.

It is reachable four ways. The first two happen on every new block:

1. **The type-2 "full work" blast.** After the empty blast of a new height, the `JOB_STATE_EMPTY_PLUS`
   job's second pass (`Blasting full work for type 2 job`) calls `send_mining_notify(..., new_block=false)`
   with the job at state 2, below `JOB_STATE_FULL_PRIORITY_WAIT_COINBASER`, so every miner gets class 0
   on the full template until the priority job with the coinbaser replaces it.
2. **A miner arriving on a quiet thread, or a vardiff resend,** while this thread's
   `full_coinbase_ready` is false (the thread has not looked at the job's coinbaser yet).
3. **The five-second give-up** in `stratum_job_coinbaser_ready`, which sets `full_coinbase_ready = false`
   and publishes. The job then stays class 0 for as long as it is current.
4. **The coinbaser thread's own timeout.** `datum_protocol_coinbaser_fetch` returns 0 when the pool's
   reply is more than 5 s late, and `datum_coinbaser_thread` still generates the job's coinbases (every
   class without the split) and clears `need_coinbaser`. Every class, YUGE included, then pays only the pool.

### Measured

Regtest, Bitcoin Knots 29.4.2, a DATUM Prime (Lazarus `primed`), `a5f28aa` built with no changes;
three stratum clients recorded every `mining.notify` (the class is the job id's last byte), two CPU
miners submitted real shares, and a new block arrived about every 12 s.

| | pool-only class-0 notifies | time a miner held one | pool-only shares | time on subsidy-only work |
|---|---|---|---|---|
| `a5f28aa`, normal pool | 90 over 93 new blocks | 0.106% (10–37 ms per block) | 0 of 3 | 0.12% |
| `a5f28aa`, pool reply 7 s late | 14 | 62.6 s over 3 clients | 1 (a real share on a full job) | |
| `a5f28aa` + this patch, normal | 0 over 126 new blocks | 0 | 0 of 6 | 0.22% |
| `a5f28aa` + this patch, reply 7 s late | 0 | 0 | 0 | |

The same test on the other C builds: cutting-edge `8002c56` 99/111 blocks, 62.6 s late; iohzrd `c031568`
47/51, 61.2 s; iohzrd `7491a50` 132/141, 55 s and 1 share; CONVOY `ac9b70c` 222/243, 62.4 s. So the patch
turns the ~20 ms after each block from pool-only into subsidy-only work, and removes the late case.

Live on Lazarus over the 24 h to 2026-09-27: 6 gateways on FlyTheElephant builds (`1b5567be`,
`a5f28aa`, `8002c56`), 4 of them flagged by the pool at least once for a pool-only share on a full
job. iohzrd and CONVOY builds show the same pattern; it is not specific to this fork.

### The fix

All four changes apply only while `datum_protocol_is_active()`; solo is unchanged.

1. `datum_stratum_coinbase_index`: not ready → `DATUM_COINBASE_ID_EMPTY` instead of `0`.
2. `send_mining_notify`: first ask `stratum_job_coinbaser_ready` (covers path 2). If the job still
   has no coinbaser and is not a new height's job, send nothing: only a new-height job carries
   `subsidy_only_coinbase`, so the priority and normal jobs have no subsidy-only coinbase to fall back
   to, and the miner keeps its current work until the split is in.
3. `stratum_job_coinbaser_ready`: no five-second give-up while pooled.
4. `datum_coinbaser_thread`: a 0-output result while pooled is retried, never published. A pool's
   reply always carries at least one output (its own), so 0 means no reply.

Trade-off: while the pool is late, miners keep hashing their previous job or the subsidy-only job of
the new height. That costs one template's fees for that moment. Class 0 on a full template costs the
whole block.

### Testing

- `cmake . && make` clean, no new warnings; `./datum_gateway --test` passes, unchanged. The existing
  assertions that the not-ready case yields class 0 still hold because the test runs with the protocol
  inactive.
- The regtest numbers above. The harness is at `~/xbt-rnd/xbt-084/fte-compat.sh` in the Lazarus lab
  and can be shared on request.
