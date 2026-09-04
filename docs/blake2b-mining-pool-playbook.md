# BLAKE2b Bitcoin on Umbrel: Knots rc4, DATUM pool, explorer, Electrum, remote ASICs

Agent playbook for everything required to run a **public mining stack** on a Bitcoin chain that hard-forked from SHA-256d to **BLAKE2b** (header v2, 164-byte headers), including **getting Knots 29.4.1rc4 (`bitcoind`) working beside or instead of stock Umbrel Knots**.

Drop this file into another operator’s AI agent. Copy the **design**. Do not copy one lab’s hosts, keys, or payouts.

**Never put secrets in this file or in chat.** Source env files in the shell. Never print cookie files, RPC passwords, DATUM admin passwords, miner-web passwords, SMTP tokens, wallet addresses, emails, or LAN/WAN IPs that identify a person.

---

## What you end up with

| Piece | Role |
|-------|------|
| **Knots `v29.4.1.knots20260508rc4`** (or newer BLAKE2b Knots) | Full node that understands the fork |
| Umbrel Bitcoin Knots **app datadir** (optional but typical) | Existing chainstate, cookie, Lightning/mempool still pointed here |
| DATUM Gateway (API enabled, local coinbaser patched) | Stratum + templates |
| Small HTTP pool API | Dashboard, Ocean-style coinbaser, browser-miner WS bridge |
| mempool.space-family explorer | Blocks UI (MariaDB header column widened) |
| electrs header-v2 / BLAKE2b | Electrum protocol |
| nginx `stream` TLS on `:50002` | Public Electrum SSL |
| DNS-only `<STRATUM_HOST>` + WAN TCP forward | Remote ASICs (Cloudflare cannot carry stratum) |

Public references: mempool.guide; [paulscode/electrs-pruned header-v2](https://github.com/paulscode/electrs-pruned/blob/main/docs/electrum-header-v2.md); official Knots rc4 linux-gnu tarball (`bitcoin-29.4.1.knots20260508rc4-x86_64-linux-gnu.tar.gz` + `SHA256SUMS` / `.asc`).

Typical public-fork facts (confirm on *your* chain):

- Last SHA-256d height / first BLAKE2b height (one public fork: **961639** / **961640**).
- Work is **Sia-style 80-byte** BLAKE2b, not SHA-256d midstate.
- Headers are **164 bytes** (328 hex chars).
- Coinbase may require a **tag** and/or a **`blake2b_headline`**. Copy whatever the chain documents. One public fork uses a dated news-headline string in `bitcoin.conf` as `blake2b_headline=…`.

---

## Placeholders

| Placeholder | Meaning |
|-------------|--------|
| `<UMBREL_HOME>` | Umbrel user home (example layout: `~/umbrel` + a side directory `~/blake2b`) |
| `<PREFIX>` | Extracted Knots rc4 + DATUM binaries (example `~/blake2b/prefix`) |
| `<APP_DATADIR>` | Umbrel Knots app data (example `~/umbrel/app-data/bitcoin-knots/data/bitcoin`) |
| `<NODE_HOST>` | Host that runs Knots + DATUM |
| `<RPC_PORT>` | **Umbrel Knots app default `9332`** (not Core `8332`) |
| `<P2P_PORT>` | **Umbrel Knots app default `9333`** |
| `<RPC_PORT_B>` | Alternate RPC if you run a *second* bitcoind (example `18332`) |
| `<P2P_PORT_B>` | Alternate P2P for a second node (example `18333`) |
| `<STRATUM_PORT>` | DATUM gateway stratum for ASICs — **`23334`** here |
| `<STRATUM_GPU_PORT>` | Second DATUM gateway for GPUs / low starting difficulty — **`3333`** here |
| `<DATUM_HTTP_GPU>` | GPU gateway HTTP — **`7153`** here |
| `<DATUM_PRIME_PORT>` | DATUM Prime (remote gateways) — **`28915`** here |
| `<DATUM_HTTP>` | DATUM HTTP — **`7152`** here |
| `<POOL_UI_PORT>` | Dashboard — **`8888`** here |
| `<ELECTRS_PORT>` | electrs Electrum RPC — **`50011`** if Fulcrum still owns `50001` |
| `<ELECTRUM_TLS_PORT>` | Public SSL — **`50002`** |
| `<POOL_FEE_PERCENT>` | Example `0.5` |
| `<POOL_ADDRESS>` | Fee remainder — never publish |
| `<POOL_UI_HOST>` | HTTPS website (may be Cloudflare-proxied) |
| `<STRATUM_HOST>` | **Separate DNS-only** name for miners |
| `<ELECTRUM_HOST>` | DNS-only Electrum TLS name |
| `<EXPLORER_HOST>` | Explorer URL |
| `<WAN_IP>` | Firewall public IPv4 |
| `<ALIAS_NODE>` | Firewall alias → `<NODE_HOST>` |
| `<FORK_HEIGHT>` | First BLAKE2b height (example `961640`) |

---

## Architecture

```
Umbrel Knots app  :9332 RPC / :9333 P2P   (datadir = <APP_DATADIR>)
        │ includeconf=blake2b.conf
        │ binary MUST be rc4+ BLAKE2b Knots
        ▼
DATUM Gateway  :23334 stratum   :7152 HTTP   (ASICs / browser)
DATUM Gateway  :3333  stratum   :7153 HTTP   (GPUs / low starting difficulty)
DATUM Prime    :28915                         (remote datum_gateway clients)
        ▲
        │ GET /api/coinbaser  (local-stratum path)
Pool UI :8888  (+ WS /mine → local ASIC gateway)
        │
WAN :23334 ──► ASIC DATUM Gateway   (DNS-only <STRATUM_HOST>)
WAN :3333  ──► GPU DATUM Gateway    (same DNS-only name, low vardiff start)
WAN :<DATUM_PRIME_PORT> ──► DATUM Prime  (same DNS-only name, different port)
WAN :50002 ──► nginx ──► electrs :50011   (after index = tip)
WAN :8333/:9333 ──► Knots P2P (optional; already common on Umbrel)

Cloudflare orange-cloud ──HTTPS──► <POOL_UI_HOST> only
```

**Stratum cannot share a Cloudflare-proxied hostname with the website.** Orange-cloud is HTTP 80/443 only. `stratum+tcp://<POOL_UI_HOST>:<STRATUM_PORT>` fails for remote ASICs even when the site works. Use a second, grey-cloud name.

LAN split-DNS that points `<POOL_UI_HOST>` at an HTTP reverse proxy will **refuse** `:23334`. Rewrite `<STRATUM_HOST>` → `<NODE_HOST>`.

House miners that policy-route out WAN (NoVPN) **cannot** use `stratum+tcp://<WAN_IP>:<STRATUM_PORT>` (no ISP hairpin). They must use `<STRATUM_HOST>` or `<NODE_HOST>`. Remote ASICs are not on the LAN; they use public DNS → WAN NAT. That is expected, not a broken forward.

---

## 1. Knots 29.4.1rc4 on Umbrel

This is the foundation. Stock Umbrel Knots (older than BLAKE2b support) will **stall at the last SHA-256d block** and will not validate 164-byte headers.

### 1.1 Get the official binary

1. Download from the Knots release that includes BLAKE2b / this fork (the working build was **`v29.4.1.knots20260508rc4`**, tarball `bitcoin-29.4.1.knots20260508rc4-x86_64-linux-gnu.tar.gz`).
2. Also fetch `SHA256SUMS` and `SHA256SUMS.asc`. Verify checksums and the signature before extract.
3. Extract to `<PREFIX>` so you have `<PREFIX>/bin/bitcoind` and `bitcoin-cli`.
4. Confirm:

```bash
<PREFIX>/bin/bitcoind -version
# Bitcoin Knots daemon version v29.4.1.knots20260508rc4
```

Keep the tarball + SUMS next to the prefix so you can prove what is running.

### 1.2 Do not blindly replace the Umbrel app

Umbrel’s Bitcoin Knots app typically:

- Listens **RPC `9332`**, **P2P `9333`** (not 8332/8333).
- Stores data under `<APP_DATADIR>` (cookie file lives here).
- Runs `bitcoind` **inside Docker** (`-datadir=/data/bitcoin`).
- May already have Lightning, mempool, Fulcrum pointed at that RPC/cookie.

Two viable layouts:

**Layout A — keep the Umbrel app (what ended up live here)**

- Leave the app’s ports and datadir alone.
- Make sure the **process inside the container is rc4+**. If the app image is still old Knots, mount/replace `bitcoind` with `<PREFIX>/bin/bitcoind` or upgrade the app image. `bitcoind -version` inside the container must print rc4.
- Add a drop-in conf (do not paste secrets):

`<APP_DATADIR>/bitcoin.conf`:

```conf
includeconf=umbrel-bitcoin.conf
includeconf=blake2b.conf
```

`<APP_DATADIR>/blake2b.conf`:

```conf
# Chain-mandated headline — copy from the fork’s docs, not this file’s example wording.
blake2b_headline=<CHAIN_HEADLINE>
# This fork’s block weight/size (confirm on the chain; one public fork used 800000 / 300000)
blockmaxweight=800000
blockmaxsize=300000

# Exclusive outbound to BLAKE2b-speaking peers.
# Default DNS seeds are still SHA-256d. Majority SHA peers advertise heights
# past the fork using the old hash and *starve header sync at the last SHA block*.
# Use connect= (not addnode) until you are past <FORK_HEIGHT>, then you can relax.
connect=<BLAKE2B_PEER_1>
connect=<BLAKE2B_PEER_2>
# …enough peers from the chain’s mining / explorer peer list…

# DATUM template refresh
blocknotify=curl -fsS -o /dev/null http://127.0.0.1:<DATUM_HTTP>/NOTIFY
```

Restart **only the Knots app** (not the whole VM) after the drop-in. Confirm `getblockchaininfo` crosses `<FORK_HEIGHT>` and a post-fork `getblockheader` hex length is **328**.

**Layout B — second node on alternate ports**

Use this if you must not touch the Lightning/Umbrel Knots container:

- Separate datadir (copy or IBD; copying `<APP_DATADIR>` is faster than a new IBD if the app is already synced to the fork).
- `bitcoin.conf`:

```conf
datadir=<SECOND_DATADIR>
server=1
listen=1
txindex=1
dbcache=4096
rpcport=<RPC_PORT_B>
port=<P2P_PORT_B>
rpcallowip=127.0.0.1
blake2b_headline=<CHAIN_HEADLINE>
blockmaxweight=800000
blockmaxsize=300000
addnode=127.0.0.1:<P2P_PORT>
connect=<BLAKE2B_PEER_…>
blocknotify=curl -fsS -o /dev/null http://127.0.0.1:<DATUM_HTTP>/NOTIFY
```

- Wrapper:

```bash
exec <PREFIX>/bin/bitcoind -datadir=<SECOND_DATADIR> -conf=<CONF>
```

- `systemd --user` unit, `Restart=on-failure`, `TimeoutStopSec=180` (flush can take minutes).
- Point DATUM/electrs/pool at **this** RPC/cookie, not the old app, if the app is still SHA-256d-only.

**Do not run two `bitcoind`s on the same `rpcport`/`port`.** `start` scripts should `pgrep` the prefix binary and exit if already running.

### 1.3 Cookie and RPC for everything else

DATUM, pool UI, electrs, and coinbaser scripts should use:

- Cookie: `<APP_DATADIR>/.cookie` for Layout A (Docker maps this to `/data/bitcoin/.cookie`).
- RPC URL: `http://127.0.0.1:<RPC_PORT>` (Umbrel: **9332**).

Agents: `--user "$(tr -d '\n' < <cookie>)"` in a script. **Never print the cookie.**

`bitcoin-cli` wrapper:

```bash
exec <PREFIX>/bin/bitcoin-cli -datadir=<DATADIR> -conf=<CONF> "$@"
```

For the Docker app, `docker exec` + the app’s `bitcoin-cli` is also fine if the binary inside is rc4.

### 1.4 Verify the node (before DATUM)

```text
bitcoind -version                          # rc4+
getblockchaininfo → blocks >= <FORK_HEIGHT>
getblockheader <post-fork-hash>            # hex length 328
getpeerinfo                                # peers that know BLAKE2b, not only SHA height spam
```

If you are stuck at `<FORK_HEIGHT>-1` with lots of peers: you are talking to SHA-256d majority. Switch to exclusive `connect=` BLAKE2b peers.

### 1.5 Persist

`systemd --user`:

- `blake2b-bitcoind.service` → start wrapper (Layout B) **or** rely on the Umbrel app compose (Layout A).
- `loginctl enable-linger` for the umbrel user so user units start at boot.
- An ensure timer can restart DATUM / pool / electrs if they die; do **not** SIGKILL bitcoind from a 3-minute cron.

Do not reboot the hypervisor guest that holds this VM unless asked. Do not `pkill bitcoind` during IBD or a long flush.

---

## 2. DATUM Gateway

### Build / install

- Preferred for this pool: `lazarus-gateway` in `lazarus/` — it never publishes an unsplit job.
- Remote `datum_gateway`: clone [FlyTheElephant1 `test/console-collapse-pr14-pr17`](https://github.com/FlyTheElephant1/datum_gateway/tree/test/console-collapse-pr14-pr17) (BLAKE2b type-4 payout coinbase) and apply `lazarus/patches/datum-gateway-split-only.patch`. Stock OCEAN / unpatched FlyTheElephant1 is refused at Prime handshake (`v0.4.1-beta`).
- CMake with **`-DENABLE_API`**. A first build without API means `:7152` and `/clients` are missing — rebuild.
- Install to `<PREFIX>/bin/datum_gateway`. Keep `LD_LIBRARY_PATH=<PREFIX>/lib` (or `~/blake2b/lib`).

Launch:

```bash
export LD_LIBRARY_PATH=<LIB>
exec <PREFIX>/bin/datum_gateway -c <DATUM_JSON>
```

### Config (shape only — no secrets)

```json
{
  "bitcoind": {
    "rpcurl": "http://127.0.0.1:<RPC_PORT>",
    "rpccookiefile": "<APP_DATADIR>/.cookie",
    "notify_fallback": true
  },
  "stratum": {
    "listen_addr": "0.0.0.0",
    "listen_port": 23334,
    "vardiff_min": 1,
    "vardiff_target_shares_min": 8
  },
  "api": {
    "listen_addr": "0.0.0.0",
    "listen_port": 7152,
    "modify_conf": false
  },
  "mining": {
    "pool_address": "<POOL_ADDRESS>",
    "coinbase_tag_primary": "<POOL_TAG> / <CHAIN_HEADLINE>",
    "pow_algorithm": "auto",
    "pool_fee_percent": 0,
    "allow_hasher_time_rolling": false
  },
  "datum": {
    "pooled_mining_only": false
  }
}
```

Admin password lives in an env file, not git. Digest-auth `/clients` for the pool scraper.

`listen_addr` **0.0.0.0** so WAN NAT and LAN ASICs work. Host firewall on the node should not drop `:23334`.

### Hashrate units

Stock DATUM often prints TH/s as if SHA-256d. A few GH/s become **0.0**. Patch the UI to GH/s (or auto-scale). One working internal→GH/s scale factor is **`4.294967296`**. Verify against a known GPU/ASIC.

### Stock solo vs fee

Unpatched solo pays **100%** to `pool_address` and **ignores** `pool_fee_percent`. For Ocean-style outputs:

1. Serve the v2 coinbaser blob (section 3).
2. Patch DATUM to `GET http://127.0.0.1:<POOL_UI_PORT>/api/coinbaser?value=…`
3. Solo path: `empty_only = (available_coinbase_outputs_count == 0)` — **not** `empty_only = true`.

HTTP fail → 100% pool address (safe).

Miner URLs to publish (same user/pass, same DNS-only host):

```text
ASIC / high start:  stratum+tcp://<STRATUM_HOST>:<STRATUM_PORT>
GPU  / low start:   stratum+tcp://<STRATUM_HOST>:<STRATUM_GPU_PORT>
User: <payout-address>.<worker>
Pass: x
Algo: BLAKE2b (Sia-style 80-byte work)
```

Run a **second** `datum_gateway` on `<STRATUM_GPU_PORT>` (example `3333`) pointed at the same Prime.
`vardiff_min` 1 and a lower `vardiff_target_shares_min` so a weak GPU is assigned difficulty 1 on subscribe.
Prime `min-diff` must also be 1 (or the gateway cannot step below the pool floor — a 1024 floor flatlines small GPUs).
Do not change the ASIC port for this; GPUs that were stuck at high vdiff reconnect to `:3333`.
Both gateways share `bitcoind` / `mining.pool_address` / Prime. Separate API port (`7153`) and log file.
Match ensure-scripts by **config path**, not a bare `datum_gateway` pgrep, or the GPU process will hide a dead ASIC gateway.

---

## 2b. DATUM Prime (remote gateways)

This pool must **accept** incoming DATUM protocol connections so other operators can run
their own gateway + Knots, choose their own templates, and still get paid in the
coinbase by contributed work (OCEAN model). That is the **pool-side** listener (DATUM Prime),
not a client of someone else's pool.

Prime **refuses stock OCEAN DATUM** (`v0.4.1-beta/...`). That client publishes empty/tiny
jobs (`JOB_STATE_EMPTY_PLUS`, coinbase type 0) while waiting for the coinbaser; a find on
that work pays only the pool script. Remotes must run `lazarus-gateway` (example
`lazarus/gateway.remote.example.json`) **or** clone
[FlyTheElephant1 `test/console-collapse-pr14-pr17`](https://github.com/FlyTheElephant1/datum_gateway/tree/test/console-collapse-pr14-pr17)
(BLAKE2b type-4 payout coinbase) and apply `lazarus/patches/datum-gateway-split-only.patch`
so empty-first is closed and the hello UA contains `lazarus-split`. Unpatched FlyTheElephant1
still uses UA `v0.4.1-beta` and is refused. `require-split-gateway = true` in Prime toml.

Do **not** set `datum.pool_host` to another public pool. Point remote gateways at **this**
pool's Prime:

```json
{
  "datum": {
    "pool_host": "<STRATUM_HOST>",
    "pool_port": 28915,
    "pool_pubkey": "<from this pool's stats / dashboard>",
    "pool_pass_workers": true,
    "pool_pass_full_users": true,
    "pooled_mining_only": true
  }
}
```

Username up to the first `.` must be an address the node accepts (`BadUsername` / reason 14
otherwise). `pool_pass_full_users: true` pays each miner separately.

Prime listens `0.0.0.0:<DATUM_PRIME_PORT>`. Stats stay on localhost. Advertise
`<STRATUM_HOST>:<DATUM_PRIME_PORT>` (DNS-only; Cloudflare cannot carry this TCP).
WAN dest NAT that port the same way as stratum. Local ASICs can keep using `:23334`.

Fee is `--fee-bps` (default 0 — no pool fee). Window is `--window` × network difficulty (OCEAN TIDES, often 8).

---

## 3. Ocean-style coinbase (PROP)

Paid **in the found block**, by accepted work this round, minus fee.

Scrape DATUM `/clients` (digest). Credit **delta** of `diff_acc` per address into `round_work`. Close the round on a found block (scan coinbase for your pool tag).

Skip zero work, outputs **< 546 sats**, more than ~512 outputs.

`GET /api/coinbaser?value=<sats>` blob:

1. `uint8 datum_id = 1`
2. Each miner output: `uint64le sats` + `uint8 script_len` + `script`

```
fee_bp = percent * 100
rest   = value * (10000 - fee_bp) / 10000
amt    = rest * work / total_work
```

Scripts from `validateaddress`. Fee + dust **omitted** so DATUM pays `<POOL_ADDRESS>`.

Proof of wiring (not of a found block): coinbaser preview matches the fee split; log `Local coinbaser: N miner output(s)`.

---

## 4. Public pool UI

Python `ThreadingHTTPServer` on `<POOL_UI_PORT>`:

- Scrape DATUM + Knots RPC (cookie, **9332** on Umbrel).
- SQLite: samples, miners, rounds, payouts.
- `GET /api/pool|miners|miner/<addr>|blocks|payouts|coinbaser`
- `POST /api/browser-stat` `{user, hs}`
- WebSocket `/mine` → `127.0.0.1:<STRATUM_PORT>`
- Static dashboard; explorer links = `<EXPLORER_HOST>`
- `config.stratum_host` = **`<STRATUM_HOST>`**, not the website name

Keep HTML IDs: `bm-addr`, `bm-btn`, `bm-hr`, `bm-sh`, `bm-st`. Cache-bust JS. `systemd --user` + ensure timer.

---

## 5. Browser miner

Workers + BLAKE2b + WS bridge.

- Subscribe → extranonce1; notify `coinb1` was **39 bytes** — assert and fail clearly if DATUM changes it.
- Queue notify until extranonce1 exists.
- DATUM hashrate from shares → **0 H/s** for CPUs at diff 1 (hours to a share). Count hashes in-tab; POST `/api/browser-stat`; unique `web` + hex worker; format KH/s; overlay + list sessions before the next scrape. Cap `hs`. Display only.

---

## 6. Hardware miners

**GPU:** OpenCL/CUDA, header-v2 80-byte work, user `<address>.<worker>`. Point at `<STRATUM_HOST>:<STRATUM_GPU_PORT>` (low starting difficulty), not the ASIC port. Persist as a user systemd unit on the GPU box. If a GPU was assigned a difficulty it cannot meet on `:23334`, it will sit at 0 accepted shares until it reconnects to `:3333`.

**Innosilicon S11 / DragonMint B52 (SiaMaster):** Sia stratum, same work family.

API (do not store the web password in git):

- `POST /api/auth` → `jwt`
- `GET /api/pools`, `GET /api/overview`, `GET /api/summary`
- `POST /api/updatePools` form `Pool1` / `UserName1` / `Password1`
- **Do not GET `/api/reboot` unless you mean it**

Local S11: `Pool1=stratum+tcp://<STRATUM_HOST>:<STRATUM_PORT>` (AdGuard rewrite → node). Do not use `<WAN_IP>` from a NoVPN house host.

After connect: `shares_acc` climbing and `last_share_s` small matter more than DATUM `0.00 GH/s` in the first minute.

**Power:** S11 is ~**1380 W ±8%** at the wall (PSU **1600 W+**, 10× 6-pin). Three boards at ~1.42 TH/s each is full tilt. A dedicated **20A / 120V** circuit is 16A continuous (~1920 W); startup inrush trips many 20A breakers even with nothing else on the circuit. “Efficiency / reduced” in the UI may show `tuning: false` after reboot and **not** cut watts. Pulling **one board** (power + data, miner unplugged) drops ~⅓ hash power and usually stops trips. Prefer a dedicated / 240V feed long-term.

---

## 7. Publish stratum for remote ASICs

1. Cloudflare **A** `<STRATUM_HOST>` → `<WAN_IP>`, **proxied=false**.
2. Keep `<POOL_UI_HOST>` proxied for HTTPS if you want.
3. DDNS: one hostname per ddclient account; check-ip = **WAN interface**, not a web checker (VPN exit poison).
4. AdGuard rewrite `<STRATUM_HOST>` → `<NODE_HOST>`.
5. OPNsense dest NAT WAN TCP `<STRATUM_PORT>` → `<ALIAS_NODE>`:`<STRATUM_PORT>` + associated pass. Clone that pair for `<STRATUM_GPU_PORT>` (`3333`). Backup `config.xml`. `configctl filter reload`.
6. Confirm `rdr` + `pass` in `pfctl` for **both** ports. External TCP check from **outside** the LAN (multi-region). Hairpin and LAN split-DNS are not evidence.
7. `ss` on the node: `ESTAB` from a **public** IP to `:23334` and `:3333`.
8. `/api/miners`: remote `host` is public, shares climbing. GPU clients on `:3333` should show `vdiff` near 1 until they earn shares.

Tell miners **only** `<STRATUM_HOST>`, never the website name.

---

## 8. Block-found mail (optional)

Watch `/api/payouts` or `found_blocks`. Seed state (no replay). Hosted SMTP rejects a vanity `From:` that is not an identity on that account (Proton vs Cloudflare Email Routing). Keep the working SMTP user until a real send identity exists. Never print tokens.

---

## 9. Explorer (mempool on Umbrel)

Post-fork blocks fail if MariaDB `blocks.header` is `varchar(160)`.

```sql
ALTER TABLE blocks MODIFY header VARCHAR(512) NULL;
```

Hook this on every container recreate (Umbrel mempool MariaDB is on the app docker network; use **that app’s** DB user — do not hardcode passwords in git). Then hit the blocks API to backfill.

Until header-v2 electrs is at tip: `MEMPOOL_BACKEND=none` (RPC only). Do not use stock Fulcrum for post-fork headers.

Optional: a small “pools JSON” HTTP helper if you retag unknown miners on the explorer.

---

## 10. electrs + public Electrum

Build header-v2 electrs (paulscode electrs-pruned and/or patched 0.11.x). Umbrel often lacks clang — a local LLVM prefix + `LIBCLANG_PATH` + rustup works. `cargo build --release`; install the binary next to a toml:

```toml
network = "bitcoin"
daemon_dir = "<APP_DATADIR>"
daemon_rpc_addr = "127.0.0.1:<RPC_PORT>"
daemon_p2p_addr = "127.0.0.1:<P2P_PORT>"
electrum_rpc_addr = "0.0.0.0:<ELECTRS_PORT>"
monitoring_addr = "127.0.0.1:4225"
db_dir = "<ELECTRS_DB>"
jsonrpc_timeout = 30
index_batch_size = 10
```

First index is **many hours** (need pre-fork UTXOs). Watch `^electrs_index_height` (not the HELP line). RPC may time out while CPU is 100% indexing.

**Do not cut wallets over** until `index >= <FORK_HEIGHT>` and `index >= tip - 2`.

Public TLS (separate small VM, `network_mode: host`):

```nginx
stream {
  upstream electrs { server <NODE_HOST>:<ELECTRS_PORT>; }
  server {
    listen 50002 ssl;
    listen [::]:50002 ssl;
    proxy_pass electrs;
    proxy_timeout 24h;
    ssl_certificate     /certs/fullchain.pem;
    ssl_certificate_key /certs/privkey.pem;
    ssl_protocols TLSv1.2 TLSv1.3;
  }
}
```

If the proxy still says `server <NODE_HOST>:50001` (old Fulcrum), TLS connects then **EOF**. Flip to `:<ELECTRS_PORT>` only after tip. Wallet: `<ELECTRUM_HOST>:50002` SSL. Subscribe header hex **≥ 320** after the fork.

A 3-minute timer: read heights; when ready, rewrite nginx upstream and `docker restart`; then `server.version` + subscribe + scripthash.

---

## 11. Suggested agent order

1. Official rc4 tarball, verify SUMS, `bitcoind -version`.
2. Umbrel app: `includeconf=blake2b.conf`, exclusive `connect=` BLAKE2b peers, headline, block weight. Confirm tip ≥ fork and header hex 328.
3. DATUM with API + cookie on **9332**. Stratum subscribe locally.
4. Coinbaser + DATUM fetch patch; preview 99.5/0.5 (or your fee).
5. Pool UI; browsers POST hashrate; GPU/S11 on `<STRATUM_HOST>`.
6. Explorer header `VARCHAR(512)`; mempool RPC-only until electrs is ready.
7. electrs index; do not publish wallets early.
8. DNS-only `<STRATUM_HOST>` + WAN `:23334` (ASIC) and `:3333` (GPU); prove with public `ESTAB` + accepted shares.
9. Never dump cookies/passwords; never add a WAN hostname unasked; never stop GPU-passthrough guests unasked; never GET an ASIC `/api/reboot` by accident.

---

## 12. Verification checklist

- [ ] `bitcoind -version` is `v29.4.1.knots20260508rc4` (or newer BLAKE2b Knots)
- [ ] Umbrel RPC still **9332** / P2P **9333** if you kept the app (Lightning/mempool unbroken)
- [ ] `getblockchaininfo` blocks ≥ `<FORK_HEIGHT>`; post-fork header hex **328**
- [ ] Not stuck at last SHA height with only SHA peers
- [ ] DATUM subscribe on `<NODE_HOST>:<STRATUM_PORT>` (ASIC) and `:<STRATUM_GPU_PORT>` (GPU starts at difficulty 1)
- [ ] Coinbaser preview matches the fee split
- [ ] `/api/miners` shows ASIC/GPU shares; browsers show KH/s from POST
- [ ] Explorer loads a post-fork block
- [ ] electrs height ≈ tip before flipping nginx from `:50001`
- [ ] `<ELECTRUM_HOST>:50002` SSL: version + long header + scripthash
- [ ] `<STRATUM_HOST>` grey-cloud; WAN rdr+pass for `:23334` and `:3333`; external TCP open on both
- [ ] Public IP `ESTAB` to DATUM; remote shares accepted
- [ ] House ASIC uses `<STRATUM_HOST>`, not `<WAN_IP>`
- [ ] Pool UI stratum string uses `<STRATUM_HOST>`

---

## 13. What this playbook is not

Not the chain’s official mining doc. Not a license to attack other pools. Not one operator’s inventory, IPs, or keys.

---

## Upstream pointers

- Knots rc4 linux-gnu release (`v29.4.1.knots20260508rc4`) — official SUMS + sig
- [DATUM Gateway](https://github.com/OCEAN-xyz/datum_gateway) (stock; empty-first — refused by this Prime)
- [FlyTheElephant1 `datum_gateway` BLAKE2b branch](https://github.com/FlyTheElephant1/datum_gateway/tree/test/console-collapse-pr14-pr17) + `lazarus/patches/datum-gateway-split-only.patch`
- [paulscode/electrs-pruned — electrum-header-v2](https://github.com/paulscode/electrs-pruned/blob/main/docs/electrum-header-v2.md)
- [mempool.space](https://github.com/mempool/mempool)
- mempool.guide — live BLAKE2b explorer / Electrum example
- Umbrel Bitcoin Knots app (ports **9332** / **9333**, Docker datadir `/data/bitcoin`)
