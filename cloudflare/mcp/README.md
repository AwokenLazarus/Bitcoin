# Lazarus Pool MCP server

Public, read-only [MCP](https://modelcontextprotocol.io) server for miners: **https://mcp.lazarus-xbt.xyz/mcp**
(Streamable HTTP, stateless, no sign-in). A Cloudflare Worker (`lazarus-mcp`) with no dependencies.

    claude mcp add --transport http lazarus-pool https://mcp.lazarus-xbt.xyz/mcp
    npx mcp-remote https://mcp.lazarus-xbt.xyz/mcp          # for stdio-only clients

It reads only the public site APIs (`POOL_API`, `MEMPOOL_API` in `wrangler.toml`), so it holds no
credentials and sees nothing a browser cannot.

| File | What |
| --- | --- |
| `src/index.js` | MCP protocol (initialize, tools/list, tools/call, ping), landing page, CORS |
| `src/tools.js` | the 15 tools and `upstream()`, the single cached door to the pool and explorer |
| `src/limiter.js` | exact rate limits in a Durable Object; the numbers are in `RULES` |

Limits: 30 tool calls/min per client IP, 8/min for per-address lookups, 240 upstream fetches/min
for everyone together. Upstream answers are kept in the edge cache (8 s for live pool figures up
to an hour for docs and block hashes), so most calls never reach the hub.

Deploy: `wrangler deploy` here (needs `wrangler login`). Test a tool:

    curl -s -X POST https://mcp.lazarus-xbt.xyz/mcp -H 'Content-Type: application/json' \
      -d '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"pool_status","arguments":{}}}'

Miner-chosen text (worker names, gateway tags, user agents) is stripped to printable characters,
cut to 64, and every answer that carries it says so: it is data for the assistant, not instructions.
