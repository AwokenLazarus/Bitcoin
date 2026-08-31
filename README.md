# Bitcoin (BLAKE2b)

Code and configs for a BLAKE2b Knots + DATUM mining pool, Electrum indexer, explorer hooks, and a GPU miner.

This is the working tree from a homelab Umbrel node after the SHA-256d → BLAKE2b fork (height 961640 on this chain). Copy the **design**, not host paths or keys.

## Layout

| Path | What |
|------|------|
| `pool/` | Public pool dashboard, Ocean-style coinbaser, browser miner, Electrum cutover, block-notify mail |
| `pool/config.example.json` | Copy to `config.json` and fill in |
| `node/` | Live Umbrel Knots path (rc4 prefix, `blake2b.conf`, app `bitcoin.conf`, pre-start hook) |
| `node/umbrel/` | App `bitcoin.conf`, compose bind snippet, `hooks/pre-start` |
| `scripts/` | Umbrel helpers: DATUM/electrs/pool persist, mempool header widen, status |
| `systemd/` | User units + ensure timer |
| `miner/` | OpenCL BLAKE2b GPU miner |
| `patches/` | DATUM PROP / coinbaser wiring notes |
| `docs/blake2b-mining-pool-playbook.md` | Operator playbook (no personal secrets) |
| `lazarus/` | First-party DATUM Prime, stratum gateway, and protocol (Rust) |


## Knots on Umbrel (live node)

See [`node/README.md`](node/README.md). Short version:

1. `PREFIX=~/blake2b/prefix ./node/install-rc4.sh` after you verify the official rc4 tarball.
2. Install `node/blake2b.conf` + `node/umbrel/bitcoin.conf` in the Knots app datadir.
3. Install `node/umbrel/hooks/pre-start` so rc4 is bind-mounted again after an app update.
4. App stays on RPC **9332** / P2P **9333**. Do not commit `umbrel-bitcoin.conf`.

## Quick start (pool UI)

```bash
cd pool
cp config.example.json config.json
# edit cookie_file, datum_auth_file, stratum_host, explorer_url
python3 server.py
```

DATUM path (OCEAN model): the user runs Knots + a DATUM gateway and points the gateway at Prime on port 28915. Prime only tracks shares and sets the coinbase split; the user's node builds the template. Public stratum on 23334 / 3333 is optional and uses our gateways.

The Prime we run is [`lazarus/prime`](lazarus/prime) (`lazarus-prime`). Public stratum is [`lazarus/gateway`](lazarus/gateway) (ASIC `:23334`, GPU/CPU `:3333`). Remote operators point their own `datum_gateway` at `stratum.awokenlazarus.xyz:28915`. The pool UI scrapes both gateways (`:7152` and `:7153`), Prime stats on localhost `:28916`, and Knots RPC via cookie. There is no pool fee; a found block pays the TIDES window 100% in the coinbase. Coinbase tag is `Lazarus`.

## Do not commit

Cookie files, DATUM admin env, SMTP tokens, wallet addresses, or live `config.json`.
