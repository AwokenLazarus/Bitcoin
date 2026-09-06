# Solo mining on Lazarus

Solo is a second, parallel service: a miner points an ASIC or a GPU at a solo port, and if
one of its shares is a block, that block's whole reward goes to the address in its username
less a 2.5 % fee. Between blocks it earns nothing. This is the opposite trade from the
pooled service, where every miner is paid a share of every block the pool finds.

| | Who it is for | Where | Fee | The rest of the block |
|---|---|---|---|---|
| **Solo ASIC** | BLAKE2b ASICs, no node | `stratum.awokenlazarus.xyz:23335` | 2.5 % | to the miner's own address |
| **Solo GPU** | GPUs and CPUs, no node | `stratum.awokenlazarus.xyz:3334` | 2.5 % | to the miner's own address |

## What this is not

There is no solo DATUM option. An operator who already runs Knots and a `datum_gateway` has
everything needed to solo mine on their own — their node builds the template and their
gateway pays their address — so routing that through our Prime would add a fee and a
dependency in exchange for nothing. Solo here exists for miners who do *not* want to run a
node. An earlier draft of this plan had a 0.5 % solo DATUM listener on Prime; it was
designed, built and then removed for this reason.

Prime is therefore not involved in solo at all. It has no solo listener, no solo fee, no
solo book, and no solo code path. That is deliberate: the pooled hot path and the TIDES
window cannot be affected by anything a solo miner does, because they never meet.

## Topology

```
pooled:
  ASICs ──:23334──► lazarus-gateway (asic) ──DATUM──►┐
  GPUs  ──:3333 ──► lazarus-gateway (gpu)  ──DATUM──►│ primed :28915 ── TIDES window, split, verify
  remote datum_gateway ──────────────────────DATUM──►┘

solo (standalone, prime_port = 0):
  ASICs ──:23335──► lazarus-gateway --mode solo (asic) ──► local Knots getblocktemplate
  GPUs  ──:3334 ──► lazarus-gateway --mode solo (gpu)  ──► local Knots getblocktemplate
```

A solo gateway talks to the node and to its miners, and to nothing else. It builds its own
templates, builds a coinbase per miner, verifies shares, and submits blocks itself.

## Coinbase

One template is shared by every miner on a solo port; the coinbase is not. Each authorized
identity gets its own coinbase with two value outputs:

```
fee_sats   = floor(coinbasevalue × 250 / 10_000)
outputs    = [ miner: coinbasevalue - fee_sats, pool: fee_sats ]
scriptSig  = BIP34 height push + "Lazarus/solo" + extranonce slot
```

Because the coinbase differs per identity, the merkle root and therefore the whole job
differs per identity: each solo miner gets its own `mining.notify`. `Job` was split into a
shared `Arc<Template>` (block header fields, transaction list, merkle branches) and a light
per-identity `Job` that references it, so N miners cost N small structs rather than N copies
of the template.

The tag is `Lazarus/solo`, not `Lazarus`. The pool's block scanner keys off it, and so does
block-explorer attribution.

## Difficulty

The two ports exist only because ASICs and GPUs want difficulties three orders of magnitude
apart, and a single vardiff range cannot serve both without one of them either flooding the
gateway or going minutes between shares.

| | `vardiff_min` | `vardiff_start` | `vardiff_max` |
|---|---|---|---|
| Solo ASIC `:23335` | 1024 | 4096 | 131072 |
| Solo GPU `:3334` | 1 | 1 | 131072 |

These match the pooled ports of the same class, so a miner moving between pooled and solo
sees the same share rate. Difficulty affects only how often a miner reports in; the block
target is the network's and is unchanged by it.

## Who is paid

The username's identity part (before the first `.`) is the payout address, normalized the
same way as on the pooled ports so that `BC1Q…` and `bc1q…` are one miner, not two. An
identity that does not decode to a payable script is refused at `mining.authorize` rather
than silently mining for nobody.

## Keeping solo out of the pooled books

This is the property everything else is arranged around, since getting it wrong means
paying a block out twice.

* Solo gateways run with `prime_port = 0`. No connection to Prime exists, so no solo share
  can be credited to the TIDES window.
* Solo blocks carry `Lazarus/solo`. `pool/server.py`'s scanner checks that tag *before*
  `Lazarus` (which it contains) and records the block in `solo_blocks`, never in
  `found_blocks` — so `close_round_for_block` is never reached and no round is settled.
* Block-explorer attribution has a `Lazarus Solo` entry ranked ahead of `Lazarus`, matched
  by tag only. It deliberately does not list the pool's payout address: that address is in a
  solo coinbase too, as the fee output, and matching on it would take pooled blocks as well.
* The solo tables in the UI are their own section. Solo work never appears under Miners,
  Next payout, or the window.

## Files

| | |
|---|---|
| `lazarus/gateway/src/main.rs` | `mode: "solo"`, `Template`/`Job` split, per-identity coinbase and notify, solo book, `/solo.json` |
| `lazarus/solo-asic.json`, `lazarus/solo-gpu.json` | the two solo instances |
| `scripts/ensure-blake2b-services.sh` | supervises both |
| `pool/server.py` | `solo_blocks` table, solo branch in `scan_found_blocks`, `/api/solo`, `solo` on `/api/miner/<addr>` |
| `pool/static/` | Solo section, solo fee card and tab |
| `node/pools/pools-overrides.json`, `pools-sync.py` | `Lazarus Solo` attribution ranked ahead of `Lazarus` |

## Verification

Run on regtest (`lazarus-regtest/up.sh up`), against a real Knots node with BLAKE2b active:

* `verify-block.py <height>` — reads the block back off the chain and checks the coinbase
  has exactly two value outputs, that the pool's is exactly 2.5 %, that the finder gets the
  remainder with nothing unaccounted, and that the tag is `Lazarus/solo`.
* `scan-test.py` — runs `pool/server.py`'s real scanner over the chain and asserts the solo
  blocks land in `solo_blocks`, that `found_blocks` and `rounds` stay empty, and that
  pooled window work is untouched.
* `cpu-miner.py` — a CPU stand-in for the OpenCL miner, for driving the ports when the GPU
  is busy.

Both checks passed against blocks mined and accepted by the node, with the pooled gateway
connected to Prime throughout and its window still at zero work.
