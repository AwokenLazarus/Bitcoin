# lazarus-hub: what runs the pool on the Latitude server

Copies of the files that run Lazarus Pool on `lazarus-hub` (Latitude `m4-metal-medium`, LAX, 103.219.170.129, Tailscale 100.117.160.26) as of 2026-09-20, the day the pool moved there from the home server. Nothing here is a secret: the rpcauth hash is masked, the tunnel token, wallet descriptors, LUKS passphrase and prime key live only on the hub's encrypted volume and in Mike's Proton Pass.

| Directory | What |
| --- | --- |
| `systemd/` | every service unit: two Knots nodes (`bitcoind-mining` :9332 serves templates, `bitcoind-services` :9342 serves electrs, the explorer and the hot wallet), `electrs`, `mempool-backend`, `cloudflared`, `lazarus-prime`, four gateways (`asic` :23334, `gpu` :3333, `solo` :23335, `solo-gpu` :3334), the pool site (`writer` :8888, `reader@1,2` :8889), `fee-wallet` timer, `hub-autounlock` (Tang/Clevis LUKS unlock) |
| `etc/lazarus/` | production `prime.toml` (prime-id 1, identical to the home config but for paths and the house gateway id) and the signer's environment; the four gateway configs are the home `lazarus-{asic,gpu,solo-asic,solo-gpu}.json` with hub paths, the asic one keeping its `overflow` block and an `identity_key_file` |
| `etc/bitcoin-*/` | node configs (`rdts` consensus rules, curated BLAKE2b peers, `blocknotify` -> gateway) |
| `etc/nftables.conf` | default-drop firewall with per-IP meters on the mining ports |
| `etc/nginx/` | explorer origin (:3006 -> mempool backend, tiered cache) and Electrum TLS (:50002 -> electrs) |
| `etc/fstab` | the LUKS volume `/srv/pool` waits for `hub-autounlock` |
| `sbin/` | `build-lazarus` (builds primed + gateway from this repo, `REF=` picks the branch), `hub-unlock`/`hub-autounlock`, the cutover swap, the mempool backend config generator, node seeding |
| `agentlaz/` | what AgentLaz runs against the hub: `hub-monitor.py` (33 checks, cron */2, Proton mail alerts), `hub-backup.sh` (encrypted 5-min ledger snapshots, hourly NAS copy), `hub-cutover-copy.sh` |

Runbooks, build log and audits are in the knowledgebase vault (`06 Execution/2026-09-19 Hub Build Plan.md`, `2026-09-20 Cutover Runbook (Phase 8).md`, `05 Reports/2026-09-20 Cutover Audit.md`, `2026-09-20 Hub Full Audit.md`).

Deploying prime or gateway code to the hub: push the branch, then on the hub `sudo -u electrum env REF=<branch> HOME=/var/lib/electrum /usr/local/sbin/build-lazarus`, check `/opt/lazarus/stage/SOURCE_COMMIT`, install from `/opt/lazarus/stage/` and restart the unit. Deploying the sites: `cloudflare/deploy.sh {pool|mempool}` from AgentLaz with the hub's reader forwarded (`ssh -L 8889:127.0.0.1:8889 lazarus-hub`, `LAZARUS_NODE=127.0.0.1`).
