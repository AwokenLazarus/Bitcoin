# lazarus-xbt.xyz/map — XBT Galaxy

An interactive galaxy of every mining pool and DATUM gateway on the Bitcoin BLAKE2b (XBT) network.
Pools are solar systems, DATUM gateways are planets, every flash is a block.

- **Rebel Alliance** (gold / blue): DATUM pools. Their blocks carry other operators' gateway tags.
- **Galactic Empire** (red, Death Stars): stratum-only pools that build every template themselves.
- **Outer Rim** (green): solo miners on their own node and gateway.

Everything is inferred from public chain data. A DATUM gateway writes its coinbase scriptSig as
height push, then `<primary tag> 0x0F <secondary tag> 0x00` (`datum_coinbaser.c`): the primary
tag names the pool, the secondary names the gateway. `src/galaxy.js` holds the rules (tag
aliases, software default tags, pool types) and says why each exists.

## Pieces

| Path | What |
|---|---|
| `src/galaxy.js` | coinbase tag parser and galaxy builder, shared by the Worker and the scripts |
| `src/worker.js` | serves `/map/` (static assets) and `/map/data.json` (KV); a cron every 5 minutes adds new blocks from the explorer API and rebuilds the galaxy |
| `public/map/` | the page: three.js scene, LCARS interface, opening crawl |
| `scripts/backfill.mjs` | one-off: fetch every block since the fork into `data/blocks.json` |

KV namespace `lazarus-map-galaxy` holds two keys: `blocks` (compact record of every block since
block 961,640) and `galaxy` (the built model the page draws).

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
