# Knots 29.4.1 (BLAKE2b) on Umbrel

**Live layout (A):** keep the Umbrel Bitcoin Knots **app** and its datadir.
RPC **9332**, P2P **9333**. Lightning/mempool keep talking to this node.

**Alternate (B):** `bitcoin.conf.example` + `scripts/start-node.sh` + `systemd/blake2b-bitcoind.service` — a second `bitcoind` on **18332/18333**. That is not what we run now.

## Files

| Path | Role |
|------|------|
| `install-knots.sh` | Download + sha256 + gpg-verify the official linux-gnu tarball and stage it into `~/blake2b/prefix` (`KNOTS_VER=` to pick a release; previous binaries kept in `bin/backup/`) |
| `blake2b.conf` | App drop-in: block weight, exclusive BLAKE2b `connect=`, DATUM notify (no headline: hardcoded since the final release) |
| `umbrel/bitcoin.conf` | App `bitcoin.conf`: `includeconf=umbrel-bitcoin.conf` + `includeconf=blake2b.conf` |
| `umbrel/hooks/pre-start` | Re-bind the prefix `bitcoind` into the Knots compose after app updates, run `ensure-blake2b-services.sh`; then stock Tor HS wait |
| `umbrel/mempool-hooks/pre-start` | Mempool app hook: widen `blocks.header` for 164-byte headers, `MEMPOOL_BACKEND=electrum` -> host electrs :50011, 800 kWU block weight, local pools JSON |
| `umbrel/mempool-theme/` | Lazarus look for the mempool frontend: `nginx-mempool.conf` (`sub_filter` injects the theme into the app shell; serves our mining-pool logo and favicons), `www/theme.css` (palette, type, layout), `www/theme.js` (nav + footer links, fee/goggles/chart recolouring), `www/chi-rho*.svg` + `www/favicon*` (the Chi Rho mark) |
| `pools/pools-sync.py`, `pools/pools-overrides.json` | Mining-pool list merge (Kilombino + mempool.guide + ours), priority-ordered pool ids, block re-attribution; runs from `../systemd/pools-sync.timer` |
| `umbrel/mempool-patches/` | Backend patch: DATUM template-creator names for any pool (`patch-backend.py` applied by the hook to the pinned image; `datum-template-creator.patch` for upstream) |
| `umbrel/docker-compose.snippet.yml` | The volume line to add (do not commit a live compose — it has RPC/Tor env) |
| `bitcoin.conf.example` | Layout B only |
| `datum_gateway_config.example.json` | DATUM (cookie path redacted) |
| `electrs.toml.example` | header-v2 electrs (see `../patches/electrs-0.11.1-header-v2.patch`, built by `../scripts/build-electrs.sh`; `legacy_protocol_clients` lets mempool's backend use it) |

Do **not** copy `umbrel-bitcoin.conf` into git (Tor control password, `rpcauth`).

## Install on an Umbrel box

1. `PREFIX=~/blake2b/prefix ./install-knots.sh` (fetches [bitcoinknots.org 29.4.1.knots20260508](https://bitcoinknots.org/files/29.x/29.4.1.knots20260508/) + `SHA256SUMS` / `.asc`, verifies both)
2. Copy `blake2b.conf` and `umbrel/bitcoin.conf` into `~/umbrel/app-data/bitcoin-knots/data/bitcoin/`
3. Copy `umbrel/hooks/pre-start` over the app hook (`chmod +x`). Restart **only the Knots app** (not the VM).
4. `bitcoind -version` inside the container must print `v29.4.1.knots20260508` (or the release you staged). Post-fork `getblockheader` hex length **328**. If you stall at the last SHA height, you are still talking to SHA-256d peers — keep exclusive `connect=` in `blake2b.conf`.

DATUM / pool / electrs point at `http://127.0.0.1:9332` and the app cookie file.

## Upgrading Knots

`KNOTS_VER=<ver> ./install-knots.sh`, then `~/.local/bin/umbrel-app-control restart bitcoin-knots`. The app
recreates its container, so the new binary behind the bind-mount is picked up. Expect ~1-2 min without
templates for the pool's own stratum gateway; remote DATUM gateways use their own nodes.

## Policy: fee floor

The app regenerates `umbrel-bitcoin.conf` from `data/app/settings.json` on every start, and the first
`includeconf` wins, so `blake2b.conf` cannot override `minrelaytxfee` / `blockmintxfee` /
`incrementalrelayfee`. Set them in `settings.json` (sat/vB, fractions allowed). We run **0.1 sat/vB**:
the chain's backlog sits at ~0.2-0.3 sat/vB, and the stock 1 sat/vB floor kept it out of our mempool
and out of our block templates.


## Mempool explorer: Lazarus theme

The mempool frontend image is nginx serving a prebuilt Angular bundle, so the theme is applied without
rebuilding it. `umbrel/mempool-hooks/pre-start` stages `umbrel/mempool-theme/` to `~/blake2b/mempool-theme/`
and bind-mounts two paths into the `web` service: `run/conf.d` over `/etc/nginx/conf.d` (our copy of the
stock server block plus a `sub_filter` that appends `theme.css` + `theme.js` to `</head>`, and a
`location /lazarus/` for the assets) and `www/` at `/lazarus-theme`. Editing `www/` on the host is live;
the nginx config needs an app restart.

* `theme.css` re-points mempool's CSS variables and Bootstrap classes at the pool's palette (warm
  near-black, brass, off-white; IBM Plex / Newsreader) and fixes the hard-coded block-side colours.
* `theme.js` adds the **Lazarus Pool** nav item, a footer column (pool links + the Electrum endpoint) and a
  full-width dashboard card, *This chain, end to end*: the public Electrum server
  (`electrum.awokenlazarus.xyz:50002`, SSL, header v2 / protocol 1.8) with a copy button, and live pool
  stats pulled from `pool.awokenlazarus.xyz/api/pool` with links into the pool. All of it is re-inserted on
  route changes via a `MutationObserver`. It also recolours everything the CSS cannot reach: the fee colour ramp used by the
  mempool blocks, the fee bar and the Goggles WebGL treemap (swapped in place inside webpack's module
  registry before the app boots), the categorical series palette (pool pie), and every colour written to
  the SVG/canvas charts (hue-banded into the palette at `setAttribute` / `fillStyle` time; greys pass
  through). `window.__lazarusTheme` reports which hooks took effect.

If a mempool upgrade changes the bundle, the hooks degrade to stock colours rather than breaking the page.

### The Chi Rho mark

`www/chi-rho.svg` is the Lazarus mark, a Chi Rho (labarum, U+2627) drawn as one stroked path in brass.
It is the same geometry as `--chi-rho` in `pool/static/pool.css`, so the two sites carry one mark.
`nginx-mempool.conf` serves it at three kinds of URL:

* `/resources/mining-pools/lazarus.svg` — **the pool icon.** The frontend builds each badge's image URL
  from the pool's slug and swaps in `default.svg` (a pickaxe) when that 404s, which is what Lazarus
  blocks used to show. Serving our slug here puts the Chi Rho on every surface that draws a pool logo:
  the block list, the block page's *Miner* row and block strip, and `/mining/pool/lazarus`. Attribution
  is untouched — this is only the image for the `lazarus` slug.
* `/resources/favicons/{favicon.ico,favicon-16x16.png,favicon-32x32.png,apple-touch-icon.png}` — the tab
  and touch icons, from `chi-rho-icon.svg` (the mark inset on the pool's near-black, one notch lighter in
  the stroke so the Rho's counter survives 16px). Regenerate after editing that file:

  ```sh
  cd node/umbrel/mempool-theme/www
  for px in 16 32 48 180; do
    printf '<!doctype html><style>html,body{margin:0;width:%dpx;height:%dpx;overflow:hidden}img{display:block;width:%dpx;height:%dpx}</style><img src="chi-rho-icon.svg">' $px $px $px $px > _ico.html
    google-chrome --headless --disable-gpu --hide-scrollbars --force-device-scale-factor=1 \
      --window-size=$px,$px --screenshot=/tmp/ico-$px.png "file://$PWD/_ico.html"
  done
  rm _ico.html
  cp /tmp/ico-16.png favicon-16x16.png; cp /tmp/ico-32.png favicon-32x32.png
  cp /tmp/ico-180.png apple-touch-icon.png
  convert /tmp/ico-16.png /tmp/ico-32.png /tmp/ico-48.png favicon.ico
  ```

* `/lazarus/chi-rho.svg` — the theme's own copy, alongside `theme.css`'s `--lz-chi-rho` mask. `theme.css`
  masks the mark onto the stock `app-svg-images.mempool-logo` element (rather than hiding it) so the nav
  brand reads mark + *Lazarus Mempool*, and onto the injected **Lazarus Pool** nav item's `.lz-mark`.
  All four locations are exact-match, so they win over the `location /resources` block; they are in the
  nginx config, so a mempool image upgrade keeps them.

## Mempool explorer: mining-pool names

The stock backend ships a SHA-256 pool list, so almost every BLAKE2b block was "Unknown".
`pools/pools-sync.py` (every 3 min, `systemd/pools-sync.timer`, needs `~/blake2b/lib/python/pymysql`) merges:

1. [Kilombino's `pools-v2.json`](https://github.com/Kilombino/mempool-bip110) -- payout addresses for
   ~400 BLAKE2b miners and pools,
2. [mempool.guide](https://mempool.guide/mining) pool definitions (coinbase-tag regexes), fetched per slug
   from its weekly list and cached for a week (their API is slow; ~25 fetched per run),
3. `pools/pools-overrides.json` -- Lazarus (pool payout address + tag, always first), extra tags, entries to
   drop or fold (PyBLOCK's LOTTO/CAROUSEL/CHIRP -> PyBLOCK), and the generic software tags
   (`DATUM`, `Knots`, `blake2b-mainnet`) that must match last.

It upserts the `pools` table by slug (stable `unique_id`s, so a backend `pools-v2.json` import agrees),
writes the merged list to `~/blake2b/pools/pools-v2.json` (served on :8765 for the backend), then
re-attributes blocks from the fork height in priority order -- Lazarus, other pools, named tags, named
addresses, `Solo <addr>` entries, generic tags -- and patches the API disk cache.

The backend itself matches a new block against `SELECT ... FROM pools`, i.e. in **row-id order**, and a
Lazarus block pays a dozen member addresses that Kilombino also lists individually, so whichever row came
first used to win (Lazarus was id 214; the generic `DATUM` tag was 207). The sync therefore owns the ids:
each priority has a band (Lazarus 1-99, shared pools 100-999, tag pools 1000-9999, address pools
10000-59999, generic 60000+, Unknown 65500), new pools are inserted into their band, and if anything sits
outside its band the table is renumbered in one transaction with `blocks.pool_id` / `hashrates.pool_id`
remapped (two-phase through a scratch range; `hashrate_timestamp` pinned since it is `ON UPDATE
current_timestamp()`). Pools dropped from the list keep their rows (FK from `blocks`) but lose their
matchers. Result: the backend tags blocks correctly on arrival, including the in-memory recent-blocks
window that a DB fix-up cannot reach; a steady-state run logs `0 renumbered ... 0 retagged`.
`pools-sync.py --dry-run` shows what a run would change.

## Mempool explorer: DATUM gateway names on blocks

A block found through a DATUM gateway carries two coinbase tags, `Lazarus` (set by the pool) and the
gateway operator's own, as `<primary> 0x0F <secondary> 0x00`. mempool already parses this and shows the
secondary tag on the block (`extras.pool.minerNames`), but only for the pool literally named `OCEAN`.
`umbrel/mempool-patches/` generalises that check so any pooled DATUM coinbase gets its names -- see its
README for the detection rule and the upstream patch. `pre-start` runs `patch-backend.py`, which copies the
three compiled files out of the pinned `api` image, applies the change and bind-mounts them read-only over
`/backend/package/...`; if an image upgrade moves the anchors, it exits non-zero and the mounts are dropped,
so the backend falls back to stock behaviour. `theme.js` then prefixes the pool on the block badge, so the
block bar reads `Lazarus - <gateway tag>`, and `pools-sync.py` keeps `minerNames` when it patches the cache.
The recent-blocks window is seeded from `data/cache.json`, so after deploying the patch clear its `blocks`
across a stop/start to see names on blocks indexed before it.
