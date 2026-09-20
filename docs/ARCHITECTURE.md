# Lazarus Pool: how it is put together

Lazarus Pool mines the BLAKE2b fork of Bitcoin (Knots, `consensusrules=rdts`, header v2). It pays with TIDES through split coinbases and speaks DATUM so miners can run their own gateways. This is the general shape; hosts, addresses and firewall rules are deliberately not here.

## One server, two nodes

The pool runs on a single bare-metal hub with two independent Knots nodes:

- **mining node** — serves block templates to the pool's own gateways and to Prime. Nothing else talks to it. Curated peer list of BLAKE2b nodes, `blocknotify` pokes the local gateway so a new tip becomes a new job as fast as possible.
- **services node** — serves electrs (public Electrum, TLS), the explorer backend (a mempool.space fork), and the hot wallet. Running these on their own node keeps indexing and wallet RPC load away from template serving.

Both nodes and all pool state live on encrypted volumes; the pool-state volume is unlocked at boot by a Tang server on another machine, so a reboot needs no hands.

## The pool processes (`prime/`, `lazarus/`)

- **`primed`** (Prime) — the pool side of the DATUM protocol. Keeps the TIDES window (a rolling window of `window x difficulty` work), issues the coinbase split for every job, verifies every share against the node's chain, books found blocks (deferred earnings, DATUM rebate, stale balances) and answers operator requests written as files into its `payouts/` directory (`hold`, `paid`, `release`, `credit`). Stats and the public ledger are JSON on a loopback port.
- **`lazarus-gateway`** — a DATUM gateway that also serves stratum. The hub runs four: pooled ASIC (with the overflow relay that hands hashrate above the pool's self-cap to partner pools), pooled GPU, solo ASIC, solo GPU. Remote miners run either this gateway or the stock DATUM gateway fork against the same Prime.
- **Identity** — one keypair per Prime; gateways pin its public key. Gateways connecting from the hub itself count as house stratum and are charged the stratum fee; remote DATUM gateways pay no fee and receive the DATUM rebate.

## Money

- The coinbase pays miners directly (TIDES split). What a coinbase cannot place (below the payout floor, over the size budget) becomes *carry* and rides on later coinbases, out of the pool's remainder.
- The pool's own output lands in a hot wallet on the services node. A once-a-minute signer sweeps matured fees to the operators' addresses and pays make-goods from a queue.
- Miners who leave with a small balance are paid by the coinbase after a stale period, or by hand through Prime's `hold` / `paid` requests.

## Websites (`cloudflare/`, `pool/`)

- **pool site** — `pool/server.py`, a stdlib Python backend: one writer that scrapes Prime and the gateways into SQLite, read-only replicas that serve the API. The public site is a Cloudflare Pages project: a static snapshot of the HTML plus a small worker that proxies `/api/*` to the hub through a Cloudflare Tunnel guarded by Access (service token only). Edge caching on the API responses.
- **explorer** — mempool.space frontend on Pages with the same worker/tunnel pattern in front of the hub's backend; `/ws` passes through for live updates.
- `cloudflare/deploy.sh` builds a site from a running backend and deploys it; a verify script checks the deployed site against the origin before the script returns.

## Operations (kept outside this repo)

Units, firewall, node configs, monitoring and backups are managed on the hub and the operator's machine and are not published. In outline: every service is a hardened systemd unit under its own user; the firewall is default-drop with per-IP meters on the mining ports and admin access only over a private overlay network; an external monitor checks ~30 things every two minutes and mails on change; the ledger is snapshotted, encrypted, every five minutes with an off-site copy.
