# lazarus-xbt.xyz — the Lazarus hub (and the XBT Galaxy map)

The apex serves the ecosystem hub: `/` (home), `/learn/`, `/ecosystem/`, `/faq/`, `/blake2b/`, `/key/`, `/stream/`, plus
`/api/stats` (live figures for those pages, from the pool API, the explorer and the galaxy model).
Pages are plain HTML in `public/` with one stylesheet and one script in `public/assets/`.

## The map

An interactive galaxy of every mining pool and DATUM gateway on the Bitcoin BLAKE2b (XBT) network.
Pools are solar systems, DATUM gateways are planets, every flash is a block.

- **Rebel Alliance** (gold / blue): DATUM pools. Their blocks carry other operators' gateway tags.
- **Galactic Empire** (red, Death Stars): stratum-only pools that build every template themselves.
- **Outer Rim** (green): solo miners on their own node and gateway.

A pool that takes DATUM and still builds some blocks itself is **mixed** (Lazarus, AlphaPool since
22 Sep 2026): a Rebel system whose gateway planets and Imperial outpost (its own stratum) are each
sized by their blocks. Pool-built means no gateway tag, or a secondary tag naming the pool itself
(`AlphaPool/AlphaPool`, `DATUM-AP/DATUM-AP`); the explorer's "Built by the pool" band uses the same rule.

Everything is inferred from public chain data. A DATUM gateway writes its coinbase scriptSig as
height push, then `<primary tag> 0x0F <secondary tag> 0x00` (`datum_coinbaser.c`): the primary
tag names the pool, the secondary names the gateway. `src/galaxy.js` holds the rules (tag
aliases, software default tags, pool types) and says why each exists.

## Pieces

| Path | What |
|---|---|
| `src/galaxy.js` | coinbase tag parser and galaxy builder, shared by the Worker and the scripts |
| `src/worker.js` | serves `/map/` (static assets) and `/map/data.json` (KV); a cron every 5 minutes adds new blocks from the explorer API and rebuilds the galaxy |
| `public/map/` | the map page: three.js scene, LCARS interface, opening crawl |
| `public/index.html`, `public/{learn,ecosystem,faq,blake2b,key,stream}/` | the hub's pages (`/start/` is a 301 to the pool since 22 Sep) |
| `public/assets/hub.css`, `public/assets/hub.js` | the hub's styles and its live figures |
| `scripts/backfill.mjs` | one-off: fetch every block since the fork into `data/blocks.json` |

KV namespace `lazarus-map-galaxy` holds the map's two keys: `blocks` (compact record of every block since
block 961,640) and `galaxy` (the built model the page draws).

## Naughty list (`/naughtylist/`)

Addresses hashing on a pool's stratum endpoint, for every pool, rendered by the Worker as plain
HTML (`src/naughtypage.js`): no script is needed to read it, filter by pool (`?pool=`) or look up
an address (`?a=`). JSON is at `/api/naughtylist`.

- `src/naughtylist.js`: the coinbase walk (14 days) and the rules. The chain alone classifies
  nothing but a block paid to a pool alone: a split pays stratum hashers and gateway operators
  alike, and RATUM gateways write no tag.
- `src/pools.js`: one reader per pool API (Lazarus, B2Pool, dxpool, RIPTIDE, PaperclipPool,
  Blockvase, PyBLOCK, CONVOY, Bitcoin Xor) and the table of what each pool publishes. To add a
  pool, add a reader that fills `state.ext[pool].m[address]` and a `POOLS` row. A pool with no row
  still appears on the page from its blocks, marked not checked.
- The store (`naughtylist`, v3) is `blk` (one entry per block of the last 14 days) and `pays`
  (every coinbase output of those blocks, per address). Counts are worked out from them on each
  run (`derive`), never kept as running totals. What the cron reads is the difference between
  the `blocks` record and the store: a block missed in an outage is read later, an orphaned block
  is dropped, a coinbase the explorer failed on is retried. A view built from under 97% of the
  window is not published. `node scripts/naughty-backfill.mjs blocks.json naughtylist.json
  hostedgateways.json` fills a store in one go.
- A block paid to a pool in one output is custody. Two or three outputs from a known pool is a
  small split (a finder and a fee), judged like any split. A gateway block the watcher tied to a
  pool counts as the pool's: `direct` when that block's job was heard on a public stratum port,
  `named` when it only carries a gateway name proved by another block. The page gives both.
- The page says what is out of date (`staleness`): a list not rebuilt for 20 minutes, a window
  not fully read, a watcher silent for 2 hours, a pool that did not answer. `assets/naughty.js`
  adds a filter box and column sorting; the page is complete without it.
- KV: `naughtylist` is the working document, `naughtyview` the small one the page reads. The cron
  writes both. B2Pool and dxpool answer per address, so they are read a few addresses per run
  (`POOL_BUDGET`); B2Pool drops a client that sends a few hundred requests in a burst.
- `node scripts/naughty-pools.mjs state.json` runs every reader over a saved document and prints
  the lists. `npm test` covers the rules and that the page carries no inline script.

## Without JavaScript

Every hub page reads with scripts off. `assets/js.js` sets `.js` on `<html>`; styles that hide
content until a script reveals it, and the mobile menu drawer, apply only under `.js`. The Worker
fills the live figures, pool directory and latest blocks into `/` and `/ecosystem/`
(`src/hubfill.js`, `run_worker_first` in `wrangler.jsonc`). The map itself needs WebGL;
`/map/table/` is the same data as tables.

## Deploy

```sh
wrangler deploy                       # wrangler's own login; the REST token has no Workers scope
```

Re-seed from scratch (only if the KV data is lost or the record format changes):

```sh
ssh -f -N -L 127.0.0.1:13006:127.0.0.1:3006 lazarus-hub   # the hub explorer, about 5 minutes
node scripts/backfill.mjs
wrangler kv key put blocks --path data/blocks.json --namespace-id <id> --remote
```

The cron rebuilds `galaxy` on its next run.

## Safety

Pool and gateway tags are chosen by miners. The page only ever sets them with `textContent`,
and the tooltip text tells the reader what a tag is. Pool links shown come from the fixed
table in `galaxy.js`, never from the chain.
