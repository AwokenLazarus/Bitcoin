# mempool backend patch: DATUM template-creator names for any pool

## What it changes

A DATUM gateway writes two tags into the coinbase (`datum_gateway/src/datum_coinbaser.c`):

```
<BIP34 height push> <push: primary 0x0F secondary 0x00> <push: unique id> <extranonces>
```

The primary tag is set by the pool (`Lazarus`), the secondary by the gateway operator -- the person whose
node built the block template. The unique-id push is 3 bytes when the gateway mines solo and longer
(7 or 11 bytes, the pool's prime id appended) when it mines through a pool. All four gateway lineages
(OCEAN, Convoy, FlyTheElephant1, iohzrd) share this layout.

mempool's backend already parses it (`parseDATUMTemplateCreator`) and the frontend shows
`minerNames[1]` on the block bar, the block page and transaction pages -- but the backend only runs the
parser when `pool.name === 'OCEAN'`. The patch replaces that name check with a structural one:

* `isDATUMCoinbase(coinbaseRaw)`: exactly one `0x0F` separator, no control bytes either side (UTF-8 names such as `💰Pirate Lounge 🏴‍☠️` count, as they do upstream for OCEAN), a
  terminating NUL, and a pooled unique-id push (`> 3` bytes). Solo DATUM blocks are deliberately left
  alone: their secondary tag is the miner's own free text (slogans, mottos), not a template creator.
* `parseTemplateCreator(poolName, coinbaseRaw)`: OCEAN and DMND behave exactly as before; any other
  pool gets DATUM names when `isDATUMCoinbase` holds and the secondary differs from the primary.

Checked against 300 recent blocks on each chain: mainnet output is identical to upstream (7 OCEAN
blocks, nothing else); on BLAKE2b 15 pooled blocks gain names (Pow.re, TIDES, pool.iohzrd.tech) and 136
solo DATUM blocks are unchanged.

## Files

| File | Purpose |
|---|---|
| `datum-template-creator.patch` | `git format-patch` against `mempool/mempool` master (`backend/src/utils/bitcoin-script.ts`, the two call sites, unit tests). Builds with `tsc`, lints, `jest` passes. |
| `patch-backend.py` | Applies the same change to the compiled JS of the image pinned in the Umbrel compose file. Run by `../mempool-hooks/pre-start`; writes `run/backend/{api/blocks.js,repositories/BlocksRepository.js,utils/bitcoin-script.js}` + `run/IMAGE` (image digest + script hash, so it only rebuilds when either changes). Any missing anchor -> non-zero exit and the hook drops the bind mounts. |

Deploy: `rsync -az --exclude run node/umbrel/mempool-patches/ umbrel@<host>:blake2b/mempool-patches/`,
install the hook, `umbrel-app-control restart mempool`. Blocks already in the in-memory window keep their
old `minerNames: null` until they age out; clear `blocks`/`blockSummaries` in
`app-data/mempool/data/cache.json` across a stop/start to refresh them.

## Upstream PR

This only reaches other explorers (mempool.guide runs upstream master) once it is merged upstream. The
deploy key on the host is scoped to this repo, so the PR has to be opened from an account with a fork:

```
git clone https://github.com/mempool/mempool && cd mempool
git am /path/to/datum-template-creator.patch
git push <your fork> HEAD:datum-template-creator
```

Suggested title: **Expose DATUM template creator names for any pool**

Suggested body:

> `parseDATUMTemplateCreator` is only run for the pool named `OCEAN`, so blocks mined through a DATUM
> gateway at any other pool never get `minerNames` even though the frontend already renders them.
>
> This detects the DATUM coinbase layout structurally instead (`<primary> 0x0F <secondary> 0x00` tag
> push followed by the pooled unique-id push, see `datum_gateway/src/datum_coinbaser.c`
> `generate_coinbase_input`) and exposes the template creator for any pool that produces it. Solo
> DATUM coinbases (3-byte unique id) are skipped since their secondary tag is free text, and a
> secondary that repeats the primary is dropped as it carries no information. OCEAN and DMND keep their
> existing code paths.
>
> On mainnet this is a no-op today: over the last 300 blocks the output is identical (only OCEAN blocks
> have names). It matters for other DATUM pools -- e.g. the BLAKE2b chain that mempool.guide indexes,
> where several pools run DATUM primes and their gateways' tags are currently invisible.
>
> Adds unit tests for the detector and the dispatcher.

# mempool backend patch: Knots 29.4.2 `difficulty_blake2b`

## What it changes

Knots 29.4.2 renamed the RPC field `difficulty` to `difficulty_blake2b` (= difficulty * 2^32) in
`getblock`, `getblockheader`, `getblockchaininfo` and `getmininginfo`, and removed `getdifficulty`.
A stock mempool 3.3.1 backend then reads `undefined`: every block insert fails with
`Column 'difficulty' cannot be null`, so nothing after the switch is indexed, and
`/api/v1/mining/hashrate/*` reports `currentDifficulty: 0`.

Two lines change. `convertBlock` falls back to `difficulty_blake2b / 2^32`, and the hashrate route
falls back to `getblockchaininfo` when `getdifficulty` throws. Both still read `difficulty` first,
so the patched backend also runs against a node that sends the old field.

## Files

| File | Purpose |
|---|---|
| `difficulty-blake2b.patch` | `git diff` against `mempool/mempool` v3.3.1 (`9332d9d`): the two lines plus the optional field on `IBitcoinApi.Block`. Type-checks with the backend's own `tsc`; the emitted `bitcoin-api.js` is byte-identical to the file the hub has run since 2026-09-21. Apply it before any rebuild from source. |
| `difficulty_blake2b.py` | The same change to compiled JS in place: `difficulty_blake2b.py <backend package dir> [--check]`. Backs each file up, replaces it by rename, and does nothing on a second run. For a backend run from a directory (the hub's `/opt/mempool-backend/package`, home's `~/mempool-hotfix/backend/package`). |
| `patch-backend.py` | Imports `difficulty_blake2b.py` and adds its two files to the ones it builds from the Umbrel image, so the pre-start hook mounts them with the DATUM files. `patch-backend.py --files` prints the list the hook mounts. |

Deploy the directory as above; both scripts must sit side by side. The home explorer answers REST
from the Umbrel `api` container and websockets from `~/mempool-hotfix`, so home needs both the hook
(container) and `difficulty_blake2b.py ~/mempool-hotfix/backend/package` (then restart
`mempool-hotfix.service`).
