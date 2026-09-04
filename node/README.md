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

