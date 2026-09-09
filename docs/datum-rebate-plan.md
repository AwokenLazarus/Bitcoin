# DATUM rebate: 3% stratum / 3% solo, one point credited to DATUM miners

Status: **code and UI ready, not deployed**. Nothing here is live; the live pool is still
`stratum-fee-bps = 200`, `fee-bps = 0`, `solo_fee_bps = 200`, and the dashboard shows 0% / 2%
because it reads those numbers from primed.

## Goal

Raise the public-stratum fee and the dedicated-solo fee from 2% to 3%, keep the pool's cut
at 2%, and hand the new point to the miners who run their own DATUM gateway. DATUM work stays
at 0% fee and is credited a share of every block's stratum fee on top of its own share.

## How it works

### The point is credited, not paid in the coinbase

The coinbase is the plain split at the new rate: stratum work is charged 3%, DATUM work 0%,
and the pool output receives the whole fee. **When the block is found**, one point of stratum
work's value is credited to the DATUM miners in the window, pro rata by DATUM work, as
**carry** — the same per-identity balance TIDES already uses for under-floor earnings. It is
then paid out of the pool's remainder with each miner's next output that clears `min-payout`.

```
stratum_value  = value × Σstratum_work / total_work
fee charged    = stratum_value × 3%              (in the pool output of this coinbase)
credited       = stratum_value × 1%              (→ DATUM miners' carry, ∝ datum_work)
pool nets      = stratum_value × 2%              (after the carry is paid out next block)
```

Why credit rather than pay: a DATUM miner too small to make a given coinbase (under the floor,
or squeezed out by the output budget) is credited exactly like one that did, on every block
its work is in the window. Its balance accumulates and is paid once it clears the floor. The
coinbase itself keeps today's shape and output count — only the pool output grows by the point.

Only DATUM work with a payable script is eligible. If a window has **no** payable DATUM work
the point is *deferred* into a `rebate_owed` balance and handed out with the next block that
has one, so it never quietly stays with the pool.

### Solo blocks

The dedicated solo ports (`lazarus-gateway --mode solo`, `prime_port 0`) never talk to Prime.
Their 3% fee output goes to the pool address. Prime's node poller now scans each new block one
behind the tip (`primed/src/solo.rs`); a coinbase tagged `Lazarus/solo` that pays the pool
script credits `coinbase_value × solo-rebate-bps / 10 000` to the DATUM miners in the window
at that moment, as carry, the same way. Each is logged to `solo-rebates.jsonl` with the
per-identity credits; the cursor is `solo-scan.json`; nothing is backfilled.

### Accounting

- Credits are the positive rebate entries inside `BlockRecord.carry_delta`, so an orphan
  reverses them and a return re-applies them with no new code path. `rebate_credited` and
  `rebate_delta` (the `rebate_owed` move) are recorded per block.
- `rebate_owed` lives in `window.json` next to carry and survives restarts.
- Split, partial and pool-only blocks all credit alike: the pool holds the fee in every case.
  Solo/unknown coinbases credit nothing.

### Invariants (tested)

- Outputs never sum past `value`; credits equal the pot less rounding dust
  (`rebate_conservation_fuzz`, 2 000 random windows).
- A rebate cannot exceed the stratum fee it comes from (config error, and clamped in code).
- The fuzz also exposed a latent hole in **carry**: room for carry was measured against payees
  placed *so far*, so a large carry on an early payee could push a later payee's earned share
  past `value`. Never triggered live (carry is ~137k sats vs a ~6M remainder), but the rebate
  turns carry into a 3M-sat-per-block flow, so carry is now budgeted against every payable
  miner's earned share up front (`carry_on_an_early_payee_cannot_eat_a_later_payees_share`).
  With the 3% fee in the pool output the remainder (~9.7M) comfortably clears each block's
  credits (~3M) the very next block.

## Simulation on the live window (2026-09-09, 152 identities, 95.8% stratum / 4.2% DATUM)

3.125 BTC subsidy, `min-payout 10000`. Twelve consecutive blocks with the same window:

| 12-block totals (sats)        | today 2% | 3% + 1 pt credited | delta       |
|-------------------------------|---------:|-------------------:|------------:|
| paid to stratum-path miners   | 3 516 224 218 | 3 480 344 441 | −35 879 777 |
| paid to DATUM-path miners     |   153 680 666 |   186 620 610 | +32 939 944 |
| pool keeps                    |    80 095 116 |    83 034 949 | +2 939 833 (= one block's credit still in flight) |

Per block after the first: stratum miners pay ~2.99M more, DATUM miners receive ~2.99M more
via carry, the pool nets the same ~6.7M. The pool output is ~9.7M in each coinbase and
~3.0M of it flows back out as carry the following block.

- Every DATUM miner: **≈ +23%** on its earnings while DATUM is 4% of the window (1% of 96%
  spread over 4% of work). Falls as hashrate moves to DATUM — which is the point.
- Largest stratum miner (21.8% of the window): −1.02%.
- 34 DATUM identities credited per block, including the ones under the floor: e.g. the
  identity earning 840 sats/block is also credited 196 sats/block and reaches the 10 000-sat
  floor in 10 blocks instead of 12, never having missed a rebate.
- A 3.125 BTC solo block credits 3 125 000 sats across the same 34 identities (17 sats dust).
- All-stratum window: 3 125 000 deferred to `rebate_owed`, credited with the next DATUM block.

Solo today: ~84 TH/s on port 23335 (≈1.9% of pool hashrate), 0 solo blocks found so far,
so the solo credit is a small, occasional flow.

## What changed (uncommitted)

| Area | Change |
|---|---|
| `prime/tides/src/split.rs` | `SplitParams.datum_rebate_bps`; `compute(…, rebate_owed, …)`; `Split.{rebate_credits, rebate_sats, rebate_owed_credited, rebate_deferred}`; `rebate_credits()` helper; `carry_delta` includes credits; `rebate_delta`; carry-room fix; tests |
| `prime/tides/src/lib.rs` | `Window.rebate_owed` (+ `Meta.rebate_owed` in `window.json`), `Ledger.settle_rebate/set_rebate_owed`, `BlockRecord.{rebate_credited, rebate_delta}`; restart test |
| `prime/primed/src/config.rs` | `datum-rebate-bps`, `solo-rebate-bps`, `solo-coinbase-tag` (+ validation, test) |
| `prime/primed/src/solo.rs` | new: solo-block observer crediting carry, `solo-rebates.jsonl`, tests |
| `prime/primed/src/node.rs` | calls `solo::scan` on tip change; orphan/return reverses/re-applies `rebate_delta` (credits ride in `carry_delta`) |
| `prime/primed/src/session.rs` | coinbaser keeps its credits; block-found folds them into carry and settles `rebate_owed`; log lines |
| `prime/primed/src/state.rs` | `CoinbaserBase.rebate_owed` snapshot |
| `prime/primed/src/stats.rs` | `pool.datum_rebate_bps/solo_rebate_bps`, `window.sample_rebate_*`, `window.rebate_owed_sats`, `window.solo_rebates`, per-miner `rebate_sats` (next block's credit) |
| `prime/prime.toml.example` | 300 / 100 / 100 documented |
| `lazarus/gateway/src/main.rs` | solo fee default 200 → 300 (config still wins) |
| `lazarus/solo-asic.json`, `lazarus/solo-gpu.json`, `docs/solo-mining-plan.md` | 300 bps |
| `pool/server.py` | `fees.{datum_rebate_percent, solo_rebate_percent, datum_uplift_percent, datum_work_percent, stratum_work_percent, datum_miners, rebate_owed_btc, sample_rebate_btc}`, `ths_btc_day_datum_bonus`, coinbaser `rebate_sats/rebate_percent/rebate_owed_sats`, per-miner `rebate_btc`/`est_datum_btc_day`/`est_bonus_btc_day`/`datum_uplift_percent`; `stratum_fee_percent` fallback 3.0 |
| `pool/static/{index.html,pool.js,shared.js,pool.css,miner.js,miner.html}` | the advertising (below) |
| `node/umbrel/mempool-theme/www/theme.js` | explorer's pool paragraph names the bonus and the live uplift |
| `README.md` | fee line |

Everything is default-off: a primed built from this tree with today's `lazarus-prime.toml`
behaves exactly as now. An older primed reading a `window.json` with `rebate_owed` ignores it.

## The advertising

One number carries it: **`datum_uplift_percent`** = `datum_rebate_percent × Σstratum_work ÷
Σdatum_work`, computed in `pool/server.py` from the live window — what DATUM work earns *above*
its proportional share of every block right now. At today's 95.5/4.5 split that is **+21.2%**.
It is deliberately volatile: the fewer DATUM miners there are, the larger each one's slice, so
early movers see the biggest number and the page says so.

Where it appears (all of it keyed off `datum_rebate_percent > 0`, so one config knob turns the
whole campaign on or off, and the page reads exactly as it did before when off):

| Placement | Copy |
|---|---|
| `#datum-promo` | brass banner above the hero: "DATUM miners now earn more than their work", the mechanism in one sentence, live `+21.2% right now`, and a "Run a gateway →" button that opens the DATUM setup tab |
| hero lede + first action | "Build your own blocks with DATUM: pay 0% and collect a share of the public stratum's fee on every block"; primary button is now **Set up DATUM** |
| `#live-ths` chip and the first ticker cell | headline 1 TH/s/day is the DATUM figure **including** the bonus (`ths_btc_day_datum_bonus`), against the stratum figure beside it — 0.0667 vs 0.0534 XBT/day today |
| `#live-bonus-chip` | its own live chip: `DATUM bonus +21.2%` |
| "Two ways in · one gets paid extra" | DATUM card carries a `+21.2% DATUM bonus` line; the stratum card says one point of the 3% goes to DATUM miners |
| pillars, fee cards, connect tabs | the bonus pill on the DATUM rate; the stratum pane's Fee row links "run your own gateway" straight to the DATUM tab |
| `#datum-bonus-explain` | the full mechanism in the DATUM pane, including why crediting beats paying (a gateway too small for a coinbase still earns every satoshi) and the live work split |
| `#payout-rebate-note` under *Next payout* | "This block also credits 0.0298 XBT to the DATUM miners in the window" — and that it is not one of the outputs above |
| *How it works* step 04 | the credit-not-output mechanism, shown only when the rebate is on |
| miner page, DATUM path | a `DATUM bonus` cell (sats credited by the next block, `+21.2%` above proportional share) and Est./day that includes the bonus |
| miner page, stratum path | a callout quantifying the switch: "about 69.05 XBT/day at this hashrate instead of 55.26 XBT" for the largest stratum miner, of which 12.08 XBT/day is the bonus |
| explorer (`theme.js`) | the pool paragraph on the mempool dashboard names the credit and the live uplift |

Asset query strings are bumped to `?v=datum1` in `index.html` and `miner.html` so Cloudflare
and browsers pick the new bundle up.

### UI verification

Exercised end to end against a `stats.json` shaped like the one the new build serves
(`primed`'s real live window, rebate math applied exactly as `tides::split` does), with
`pool/server.py` running against it and the page rendered in headless Chrome:

- rebate **on**: every element above renders with the right numbers; `/api/pool` reports
  `datum_uplift_percent 21.205`, `datum_miners 37`, `sample_rebate_btc 0.02984248`;
  `/api/coinbaser` `rebate_sats 2984248`; miner pages for the largest DATUM miner, the
  smallest DATUM miner (27 sats credited) and the largest stratum miner all correct.
- rebate **off** (today's live `stats.json`, `datum_rebate_bps` absent): all fourteen elements
  hidden, fees read 0% / 2%, no bonus wording anywhere.

## Rollout (when approved)

1. **Announce** to miners ahead of time (public stratum 2% → 3%, solo 2% → 3%, DATUM miners
   are credited the point on every block, worth ≈ +21% on their share at today's split). Every
   fee number and every line of bonus copy on the site follows primed, so the announcement and
   the dashboard cannot disagree.
2. **Build** `primed` and `lazarus-gateway` from this tree; run the suites
   (`cargo test --workspace` in `prime/`, `cargo test` in `lazarus/gateway/`) — green now.
3. **Prime** (`/home/umbrel/blake2b/etc/lazarus-prime.toml`):
   ```toml
   stratum-fee-bps = 300
   datum-rebate-bps = 100
   solo-rebate-bps = 100
   ```
   Restart `primed` (`kill -TERM` + `start-lazarus-prime.sh`; window, carry and the new
   balance persist). This is the one step that needs a primed bounce — schedule it.
4. **Solo gateways** (`lazarus-solo-asic.json`, `lazarus-solo-gpu.json`): `solo_fee_bps: 300`,
   restart both solo instances. Pooled gateways are untouched.
5. **Pool UI**: deploy `server.py` + `static/` (the `?v=datum1` bump is already in the HTML);
   `config.json` `stratum_fee_percent: 3.0` (fallback only; live values come from primed).
   Deploy `node/umbrel/mempool-theme/www/theme.js` to the explorer for the same copy there.
6. **Verify**: coinbaser log shows `rebate_credit=… rebate_owed_out=…`; `stats.json`
   `pool.stratum_fee_bps == 300`, `window.sample_rebate_sats ≈ 1% × stratum share × value`,
   `window.sample_pool_sats` ≈ today's + that amount, per-miner `rebate_sats > 0` only for
   DATUM identities. First found block: log line "credited N sats of DATUM rebate",
   `blocks[].rebate_credited`, `carry_total_sats` up by ~3M, then back down on the next block
   as `sample_carry_paid_sats` pays it.
7. **Watch** the first solo block: `solo-rebates.jsonl` entry with per-identity credits.

## Rollback

Set `datum-rebate-bps = 0`, `solo-rebate-bps = 0` (and `stratum-fee-bps = 200` if the fee
itself is rolled back) and restart primed. Credits already in carry are still paid out
normally (they are ordinary carry). A non-zero `rebate_owed` stays on file and is not handed
out until re-enabled; `Ledger::set_rebate_owed` zeroes it if wanted.

## Known edges

- **One-block lag.** A block's credit is paid in the *next* coinbase. On a stretch of
  pool-only blocks it accumulates as carry and is paid when a split block lands (same as
  under-floor carry today).
- **Coinbaser snapshot.** `CoinbaserBase` is rebuilt at most once a second; two blocks found
  within that second could both hand out the same `rebate_owed` (saturates at zero, bounded
  by the balance — normally 0).
- **Reorg after a solo credit.** A solo block credited one-behind-tip and then orphaned is not
  un-credited; the log has the per-identity amounts for a manual `settle_carry` reversal.
- **Mixed identities.** An address with work on both paths pays 3% on its stratum part and is
  credited on its DATUM part; `fee_path` in stats is still the majority path.
- **Concentration.** While DATUM is ~4% of the window the uplift is ≈ +23%. If that is not the
  intent, `datum-rebate-bps` can be set lower than the extra point.
