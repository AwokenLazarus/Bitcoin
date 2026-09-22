// lazarus-xbt.xyz: the ecosystem hub (static pages under public/), the galaxy map at /map, the
// figures the pages show at /api/stats, the map's data at /map/data.json, and a cron that keeps
// that data current from the public explorer and pool APIs. Stateless apart from two KV keys:
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

/** The figures the hub pages show, from the pool's own API, the explorer and the galaxy model. */
async function stats(env) {
  const [pool, retarget, galaxy] = await Promise.all([
    getJSON(`${POOL}/api/pool?h=0`).catch(() => null),
    getJSON(`${EXPLORER}/api/v1/difficulty-adjustment`).catch(() => null),
    env.GALAXY.get("galaxy", "json").catch(() => null),
  ]);
  if (!pool) throw new Error("pool api unreadable");
  const fees = pool.fees || {};
  const counts = (galaxy && galaxy.counts) || {};
  return {
    asOf: Math.floor(Date.now() / 1000),
    network: {
      height: pool.height ?? null,
      hashrate: pool.network_hr_hs ?? (galaxy && galaxy.networkHashrate) ?? null,
      difficulty: pool.difficulty ?? null,
      blockInterval: pool.block_interval_seconds ?? null,
      retargetChange: retarget ? retarget.difficultyChange : null,
      retargetBlocks: retarget ? retarget.remainingBlocks : null,
    },
    pool: {
      hashrate: pool.pool_hr_ghs != null ? pool.pool_hr_ghs * 1e9 : null,
      sharePercent: pool.pool_share != null ? pool.pool_share * 100 : null,
      gateways: pool.datum_gateway_count ?? null,
      miners: pool.miners_online ?? null,
      blocksFound: pool.blocks_found ?? null,
      stratumFeePercent: fees.stratum_percent ?? null,
      datumFeePercent: fees.datum_percent ?? 0,
      datumRebatePercent: fees.datum_rebate_percent ?? null,
      windowFillPercent: pool.window_fill_percent ?? null,
    },
    galaxy: {
      systems: (counts.datum || 0) + (counts.stratum || 0),
      gateways: galaxy ? (galaxy.systems || []).reduce((n, s) => n + (s.gateways || 0), 0) : null,
      blocks7d: galaxy ? galaxy.blocks7d : null,
    },
  };
}

export default {
  async fetch(request, env, ctx) {
    const url = new URL(request.url);
    if (url.pathname === "/map") return Response.redirect(`${url.origin}/map/`, 301);
    if (url.pathname === "/api/stats") {
      if (request.method !== "GET" && request.method !== "HEAD") return new Response("method not allowed", { status: 405 });
      const cache = caches.default, key = new Request(`${url.origin}/api/stats`);
      const hit = await cache.match(key);
      if (hit) return withHeaders(hit, { "X-Edge-Cache": "HIT" });
      let body;
      try {
        body = JSON.stringify(await stats(env));
      } catch (e) {
        return withHeaders(new Response(JSON.stringify({ error: e.message }), { status: 503, headers: { "Content-Type": "application/json", "Retry-After": "30", "Cache-Control": "no-store" } }));
      }
      const res = new Response(body, { headers: { "Content-Type": "application/json; charset=utf-8", "Cache-Control": "public, max-age=30, s-maxage=60", "Access-Control-Allow-Origin": "*" } });
      ctx.waitUntil(cache.put(key, res.clone()));
      return withHeaders(res, { "X-Edge-Cache": "MISS" });
    }
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
    // Everything else is a static page or asset; a path with no file behind it gets the 404 page.
    const res = await env.ASSETS.fetch(request);
    if (res.status === 404) {
      const page = await env.ASSETS.fetch(new Request(`${url.origin}/404.html`, { headers: request.headers }));
      if (page.ok) return withHeaders(new Response(page.body, { status: 404, headers: page.headers }));
    }
    return withHeaders(res);
  },
  async scheduled(_event, env, ctx) {
    ctx.waitUntil(refresh(env).then((r) => console.log("galaxy refresh", JSON.stringify(r))).catch((e) => console.error("galaxy refresh failed", e.message)));
  },
};
