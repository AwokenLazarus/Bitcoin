// lazarus-xbt.xyz: the galaxy map at /map, its data at /map/data.json, and a cron that keeps
// the data current from the public explorer and pool APIs. Stateless apart from two KV keys:
//   blocks  compact record of every block since the fork (see galaxy.js blockRecord)
//   galaxy  the built model the page draws
import { blockRecord, buildGalaxy, FORK_HEIGHT } from "./galaxy.js";

const EXPLORER = "https://mempool.lazarus-xbt.xyz";
const POOL = "https://pool.lazarus-xbt.xyz";
const UA = "lazarus-map/1.0 (+https://lazarus-xbt.xyz/map)";
const MAX_PAGES = 20; // 15 blocks a page: a cron that missed 5 hours still catches up

const SECURITY = {
  "X-Content-Type-Options": "nosniff",
  "Referrer-Policy": "strict-origin-when-cross-origin",
  "Strict-Transport-Security": "max-age=31536000; includeSubDomains",
};

async function getJSON(url) {
  const r = await fetch(url, { headers: { "User-Agent": UA, Accept: "application/json" }, signal: AbortSignal.timeout(20000) });
  if (!r.ok) throw new Error(`${url} -> HTTP ${r.status}`);
  return r.json();
}

export async function refresh(env) {
  const stored = (await env.GALAXY.get("blocks", "json")) || [];
  const byHeight = new Map(stored.map((r) => [r.h, r]));
  const have = stored.length ? stored[stored.length - 1].h : FORK_HEIGHT - 1;
  const tip = Number(await (await fetch(`${EXPLORER}/api/blocks/tip/height`, { headers: { "User-Agent": UA } })).text());
  if (!(tip >= FORK_HEIGHT)) throw new Error("explorer tip unreadable");
  // Always re-read the newest page (a reorg replaces blocks), then walk down to what we have.
  let h = tip, pages = 0;
  while (pages < MAX_PAGES && h > have - 6) {
    const page = await getJSON(`${EXPLORER}/api/v1/blocks/${h}`);
    if (!page.length) break;
    for (const b of page) if (b.height >= FORK_HEIGHT) byHeight.set(b.height, blockRecord(b));
    h = page[page.length - 1].height - 1;
    pages++;
  }
  for (const k of [...byHeight.keys()]) if (k > tip) byHeight.delete(k); // orphaned above the new tip
  const recs = [...byHeight.values()].sort((a, b) => a.h - b.h);
  const [gw, hr, pool] = await Promise.all([
    getJSON(`${POOL}/api/gateways`).catch(() => null),
    getJSON(`${EXPLORER}/api/v1/mining/hashrate/3d`).catch(() => null),
    getJSON(`${POOL}/api/pool?h=0`).catch(() => null),
  ]);
  const galaxy = buildGalaxy(recs, { lazarusGateways: (gw && gw.gateways) || [], lazarusPool: pool, networkHashrate: hr && hr.currentHashrate ? Math.round(hr.currentHashrate) : null });
  await env.GALAXY.put("blocks", JSON.stringify(recs));
  await env.GALAXY.put("galaxy", JSON.stringify(galaxy));
  return { tip, blocks: recs.length, added: recs.length - stored.length, pages };
}

function withHeaders(res, extra = {}) {
  const out = new Response(res.body, res);
  for (const [k, v] of Object.entries({ ...SECURITY, ...extra })) out.headers.set(k, v);
  return out;
}

export default {
  async fetch(request, env, ctx) {
    const url = new URL(request.url);
    if (url.pathname === "/" || url.pathname === "") return Response.redirect(`${url.origin}/map/`, 302);
    if (url.pathname === "/map") return Response.redirect(`${url.origin}/map/`, 301);
    if (url.pathname === "/map/data.json") {
      if (request.method !== "GET" && request.method !== "HEAD") return new Response("method not allowed", { status: 405 });
      const cache = caches.default, key = new Request(`${url.origin}/map/data.json`);
      const hit = await cache.match(key);
      if (hit) return withHeaders(hit, { "X-Edge-Cache": "HIT" });
      const body = await env.GALAXY.get("galaxy");
      if (!body) return withHeaders(new Response('{"error":"galaxy not built yet"}', { status: 503, headers: { "Content-Type": "application/json", "Retry-After": "60" } }));
      const res = new Response(body, { headers: { "Content-Type": "application/json; charset=utf-8", "Cache-Control": "public, max-age=60, s-maxage=60", "Access-Control-Allow-Origin": "*" } });
      ctx.waitUntil(cache.put(key, res.clone()));
      return withHeaders(res, { "X-Edge-Cache": "MISS" });
    }
    if (!url.pathname.startsWith("/map/")) return withHeaders(new Response("not found\n", { status: 404 }));
    const res = await env.ASSETS.fetch(request);
    return withHeaders(res);
  },
  async scheduled(_event, env, ctx) {
    ctx.waitUntil(refresh(env).then((r) => console.log("galaxy refresh", JSON.stringify(r))).catch((e) => console.error("galaxy refresh failed", e.message)));
  },
};
