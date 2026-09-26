# Lazarus Pool MCP server

Public, read-only [MCP](https://modelcontextprotocol.io) server for miners: **https://mcp.lazarus-xbt.xyz/mcp**
(Streamable HTTP, stateless, no sign-in). A Cloudflare Worker (`lazarus-mcp`) with no dependencies.

    claude mcp add --transport http lazarus-pool https://mcp.lazarus-xbt.xyz/mcp
    npx mcp-remote https://mcp.lazarus-xbt.xyz/mcp          # for stdio-only clients

It reads only the public site APIs (`POOL_API`, `MEMPOOL_API` in `wrangler.toml`), so it holds no
credentials and sees nothing a browser cannot.

| File | What |
| --- | --- |
| `src/index.js` | MCP protocol (initialize, tools/list, tools/call, ping), request limits, rate gating, landing page, CORS |
| `src/tools.js` | the 15 original tools (overview, workers, payouts, gateways, pool, chain lookups, docs) |
| `src/audit.js` | payout audit: `miner_audit`, `miner_immature`, `miner_makegoods`, `verify_payout`, `lazarus_faq` |
| `src/faq.js` | the versioned, public-safe knowledge base behind `lazarus_faq` (no operator internals) |
| `src/upstream.js` | `upstream()`, the single cached door to the pool and explorer; `PublicError`; shared helpers |
| `src/address.js` | strict mainnet address validation (bech32 / bech32m / base58check checksums) |
| `src/limiter.js` | exact rate limits in a Durable Object; the numbers are in `RULES` |
| `test/run.mjs` | offline tests against recorded public-API fixtures: `node test/run.mjs` |

## Security

- **Read-only.** Every tool only GETs the public pool and explorer APIs through `upstream()`; no
  secrets, no bindings but the `LIMITER` Durable Object, `workers_dev` and `preview_urls` off.
- **Validate first.** Arguments are checked before any limiter unit is spent or anything is fetched.
  Addresses must decode with a valid checksum as mainnet P2PKH/P2SH (base58check), P2WPKH/P2WSH
  (bech32) or P2TR (bech32m); heights are integers from 961,640 (the first BLAKE2b block).
- **Request limits.** POST with `Content-Type: application/json` only (415 otherwise); body ≤ 64 KB,
  counted while reading so a chunked body is capped too; batches of 1–10 messages with at most 3
  tool calls, run one after another; ids must be strings or numbers.
- **Rate limits** (`RULES`, exact, per minute): 30 tool calls per client IP (IPv6 per /64); 8
  per-address units per client IP (`miner_audit` and `verify_payout` cost 2); 20 units per payout
  address across all clients; 240 upstream fetches for everyone. A tool's whole fan-out must fit the
  remaining upstream budget before it starts, and `upstream()` refuses any read beyond the tool's
  declared `fanout`. A limiter fault fails open (the server stays up).
- **Output.** Answers over 60,000 characters are refused; lists are capped by `limit` arguments.
- **Errors** carry only messages written for clients (`PublicError`); anything else is reported
  generically, so no upstream host, body or stack trace leaks.

Upstream answers are kept in the edge cache (8 s for live pool figures up to an hour for docs and
block hashes), so most calls never reach the hub.

Deploy: `wrangler deploy` here (needs `wrangler login`). Test a tool:

    curl -s -X POST https://mcp.lazarus-xbt.xyz/mcp -H 'Content-Type: application/json' \
      -d '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"pool_status","arguments":{}}}'

Miner-chosen text (worker names, gateway tags, user agents) is stripped to printable characters,
cut to 64, and every answer that carries it says so: it is data for the assistant, not instructions.
