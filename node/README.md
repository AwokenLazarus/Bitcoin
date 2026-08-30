# Knots 29.4.1rc4 (BLAKE2b) on Umbrel

**Live layout (A):** keep the Umbrel Bitcoin Knots **app** and its datadir.
RPC **9332**, P2P **9333**. Lightning/mempool keep talking to this node.

**Alternate (B):** `bitcoin.conf.example` + `scripts/start-node.sh` + `systemd/blake2b-bitcoind.service` — a second `bitcoind` on **18332/18333**. That is not what we run now.

## Files

| Path | Role |
|------|------|
| `install-rc4.sh` | Extract the official linux-gnu tarball into `~/blake2b/prefix` (verify SUMS first) |
| `blake2b.conf` | App drop-in: headline, block weight, exclusive BLAKE2b `connect=`, DATUM notify |
| `umbrel/bitcoin.conf` | App `bitcoin.conf`: `includeconf=umbrel-bitcoin.conf` + `includeconf=blake2b.conf` |
| `umbrel/hooks/pre-start` | Re-bind rc4 `bitcoind` into the Knots compose after app updates; then stock Tor HS wait |
| `umbrel/docker-compose.snippet.yml` | The volume line to add (do not commit a live compose — it has RPC/Tor env) |
| `bitcoin.conf.example` | Layout B only |
| `datum_gateway_config.example.json` | DATUM (cookie path redacted) |
| `electrs.toml.example` | electrs |

Do **not** copy `umbrel-bitcoin.conf` into git (Tor control password, `rpcauth`).

## Install on an Umbrel box

1. Official tarball from [bitcoinknots.org 29.4.1rc4](https://bitcoinknots.org/files/29.x/29.4.1.knots20260508rc4/) + `SHA256SUMS` / `.asc`. `PREFIX=~/blake2b/prefix ./install-rc4.sh`
2. Copy `blake2b.conf` and `umbrel/bitcoin.conf` into `~/umbrel/app-data/bitcoin-knots/data/bitcoin/`
3. Copy `umbrel/hooks/pre-start` over the app hook (`chmod +x`). Restart **only the Knots app** (not the VM).
4. `bitcoind -version` inside the container must print `v29.4.1.knots20260508rc4`. Post-fork `getblockheader` hex length **328**. If you stall at the last SHA height, you are still talking to SHA-256d peers — keep exclusive `connect=` in `blake2b.conf`.

DATUM / pool / electrs point at `http://127.0.0.1:9332` and the app cookie file.
