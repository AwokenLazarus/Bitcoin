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
| `umbrel/mempool-theme/` | Lazarus look for the mempool frontend: `nginx-mempool.conf` (`sub_filter` injects the theme into the app shell), `www/theme.css` (palette, type, layout), `www/theme.js` (nav + footer links, fee/goggles/chart recolouring) |
| `pools/pools-sync.py`, `pools/pools-overrides.json` | Mining-pool list merge (Kilombino + mempool.guide + ours) and block re-attribution; runs from `../systemd/pools-sync.timer` |
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
* `theme.js` adds the **Lazarus Pool** nav item and a footer column (re-inserted on route changes via a
  `MutationObserver`), and recolours everything the CSS cannot reach: the fee colour ramp used by the
  mempool blocks, the fee bar and the Goggles WebGL treemap (swapped in place inside webpack's module
  registry before the app boots), the categorical series palette (pool pie), and every colour written to
  the SVG/canvas charts (hue-banded into the palette at `setAttribute` / `fillStyle` time; greys pass
  through). `window.__lazarusTheme` reports which hooks took effect.

If a mempool upgrade changes the bundle, the hooks degrade to stock colours rather than breaking the page.

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
addresses, `Solo <addr>` entries, generic tags -- and patches the API disk cache. New blocks are tagged
live by the backend from the same rows; the sync only fixes ordering differences after the fact.
