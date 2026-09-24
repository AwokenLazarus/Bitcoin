// lazarus-xbt.xyz: the ecosystem hub (static pages under public/), the galaxy map at /map, the
// figures the pages show at /api/stats, the map's data at /map/data.json, and a cron that keeps
// that data current from the public explorer and pool APIs. Stateless apart from two KV keys:
//   blocks  compact record of every block since the fork (see galaxy.js blockRecord)
//   galaxy  the built model the page draws
import { blockRecord, buildGalaxy, FORK_HEIGHT } from "./galaxy.js";

const EXPLORER = "https://mempool.lazarus-xbt.xyz";
const POOL = "https://pool.lazarus-xbt.xyz";
const UA = "lazarus-map/1.0 (+https://lazarus-xbt.xyz/map)";
const CURRENT_APK = "lazarus-key-27.apk"; // the build /key/app serves
const MAX_PAGES = 20; // 15 blocks a page: a cron that missed 5 hours still catches up

const SECURITY = {
  "X-Content-Type-Options": "nosniff",
  "Referrer-Policy": "strict-origin-when-cross-origin",
  "Strict-Transport-Security": "max-age=31536000; includeSubDomains",
};

// The edge holds data.json for a minute and may serve it stale for five more while it refreshes,
// so a burst after expiry meets the cache rather than KV.
const DATA_CACHE = "public, max-age=60, s-maxage=60, stale-while-revalidate=300";
const etagOf = (s) => {
  let h = 2166136261 >>> 0;
  for (let i = 0; i < s.length; i++) { h ^= s.charCodeAt(i); h = Math.imul(h, 16777619) >>> 0; }
  return `W/"${s.length.toString(36)}-${h.toString(36)}"`;
};

async function getJSON(url) {
  const r = await fetch(url, { headers: { "User-Agent": UA, Accept: "application/json" }, signal: AbortSignal.timeout(20000) });
  if (!r.ok) throw new Error(`${url} -> HTTP ${r.status}`);
  return r.json();
}

const REORG_DEPTH = 12; // deeper than any reorg this chain has seen; the guards below trust it

/** The highest height at or below `from` that we have no record of, or null if there is no hole. */
function highestMissing(byHeight, from) {
  for (let h = from; h >= FORK_HEIGHT; h--) if (!byHeight.has(h)) return h;
  return null;
}

export async function refresh(env) {
  const stored = (await env.GALAXY.get("blocks", "json")) || [];
  const byHeight = new Map(stored.map((r) => [r.h, r]));
  const have = stored.length ? stored[stored.length - 1].h : FORK_HEIGHT - 1;
  // A tip that cannot be read, or that sits behind what we already have, means the explorer is
  // resyncing or serving a stale cache. Trusting it would delete good records (see the prune below).
  const tipRes = await fetch(`${EXPLORER}/api/blocks/tip/height`, { headers: { "User-Agent": UA }, signal: AbortSignal.timeout(20000) });
  if (!tipRes.ok) throw new Error(`explorer tip HTTP ${tipRes.status}`);
  const tip = Number(await tipRes.text());
  if (!Number.isFinite(tip) || tip < FORK_HEIGHT) throw new Error("explorer tip unreadable");
  if (stored.length && tip < have - REORG_DEPTH) throw new Error(`explorer tip ${tip} is behind stored ${have}`);
  // Always re-read the newest page (a reorg replaces blocks), then walk down to what we have.
  let h = tip, pages = 0;
  while (pages < MAX_PAGES && h >= FORK_HEIGHT && h > have - 6) {
    const page = await getJSON(`${EXPLORER}/api/v1/blocks/${h}`);
    if (!page.length) break;
    for (const b of page) if (b.height >= FORK_HEIGHT) byHeight.set(b.height, blockRecord(b));
    h = page[page.length - 1].height - 1;
    pages++;
  }
  // An outage longer than the page budget used to leave a hole nothing ever went back for, because
  // the next run only looked above `have`. Spend whatever budget is left walking down from the
  // highest hole instead; successive runs chip away until the record is contiguous.
  let gap = highestMissing(byHeight, tip);
  while (pages < MAX_PAGES && gap != null && gap >= FORK_HEIGHT) {
    const page = await getJSON(`${EXPLORER}/api/v1/blocks/${gap}`);
    if (!page.length) break;
    for (const b of page) if (b.height >= FORK_HEIGHT) byHeight.set(b.height, blockRecord(b));
    pages++;
    gap = highestMissing(byHeight, Math.min(gap, page[page.length - 1].height) - 1);
  }
  for (const k of [...byHeight.keys()]) if (k > tip) byHeight.delete(k); // orphaned above the new tip
  const recs = [...byHeight.values()].sort((a, b) => a.h - b.h);
  // Never publish a shorter history than we already had: an empty or wrong KV read must not be
  // able to overwrite the record with the handful of blocks one walk can reach.
  if (recs.length < stored.length - REORG_DEPTH) throw new Error(`refusing to shrink blocks ${stored.length} -> ${recs.length}`);
  if (!stored.length && recs.length < 500) throw new Error(`cold start: only ${recs.length} blocks, backfill first`);
  // Blocks arrive slower than this cron runs, so most runs have nothing new to store; rewriting
  // megabytes of identical JSON every five minutes is pure cost.
  const changed = recs.length !== stored.length || (recs.length > 0 && stored.length > 0 && recs[recs.length - 1].id !== stored[stored.length - 1].id);
  if (changed) await env.GALAXY.put("blocks", JSON.stringify(recs));
  // Storing what we fetched is how a damaged record heals, run by run. Publishing it is another
  // matter: a record full of holes would put wrong shares and a half-empty galaxy on the site, so
  // the last good model stays up until the holes are filled.
  const span = recs.length ? tip - recs[0].h + 1 : 0;
  const coverage = span ? recs.length / span : 0;
  if (stored.length && coverage < 0.9) throw new Error(`record covers ${(coverage * 100).toFixed(1)}% of ${recs[0].h}..${tip}; keeping the published model`);
  const [gw, hr, pool] = await Promise.all([
    getJSON(`${POOL}/api/gateways`).catch(() => null),
    getJSON(`${EXPLORER}/api/v1/mining/hashrate/3d`).catch(() => null),
    getJSON(`${POOL}/api/pool?h=0`).catch(() => null),
  ]);
  const galaxy = buildGalaxy(recs, { lazarusGateways: (gw && gw.gateways) || [], lazarusPool: pool, networkHashrate: hr && hr.currentHashrate ? Math.round(hr.currentHashrate) : null });
  await env.GALAXY.put("galaxy", JSON.stringify(galaxy));
  return { tip, blocks: recs.length, added: recs.length - stored.length, pages, stored: changed, coverage: Math.round(coverage * 1000) / 10 };
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
    // The hub used to carry a mining guide; the pool site owns that, so those URLs go there.
    if (url.pathname === "/start" || url.pathname.startsWith("/start/")) {
      return Response.redirect("https://pool.lazarus-xbt.xyz/", 301);
    }
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
      const inm = request.headers.get("If-None-Match");
      const notModified = (tag) => withHeaders(new Response(null, { status: 304, headers: { ETag: tag, "Cache-Control": DATA_CACHE } }));
      // An open map polls every minute but blocks arrive every seven, so most polls are asking a
      // question the ETag can answer in a few bytes.
      const hit = await cache.match(key);
      if (hit) {
        const tag = hit.headers.get("ETag");
        if (tag && inm === tag) return notModified(tag);
        return withHeaders(hit, { "X-Edge-Cache": "HIT" });
      }
      const body = await env.GALAXY.get("galaxy");
      if (!body) return withHeaders(new Response('{"error":"galaxy not built yet"}', { status: 503, headers: { "Content-Type": "application/json", "Retry-After": "60" } }));
      const tag = etagOf(body);
      const res = new Response(body, { headers: { "Content-Type": "application/json; charset=utf-8", "Cache-Control": DATA_CACHE, ETag: tag, "Access-Control-Allow-Origin": "*" } });
      ctx.waitUntil(cache.put(key, res.clone()));
      if (inm === tag) return notModified(tag);
      return withHeaders(res, { "X-Edge-Cache": "MISS" });
    }
    // Lazarus Key builds live in R2: an APK is far past the Workers asset limit. /key/app is the
    // stable link that always serves the current build, so pages and printed kits need no edit.
    if (url.pathname === "/key/app" || url.pathname.startsWith("/key/downloads/")) {
      if (request.method !== "GET" && request.method !== "HEAD") return new Response("method not allowed", { status: 405 });
      const name = url.pathname === "/key/app" ? CURRENT_APK : url.pathname.slice("/key/downloads/".length);
      if (!/^lazarus-key-[0-9]+\.apk$/.test(name)) return withHeaders(new Response("not found", { status: 404 }));
      const object = await env.DOWNLOADS.get(name, { range: request.headers, onlyIf: request.headers });
      if (!object) return withHeaders(new Response("not found", { status: 404 }));
      const headers = new Headers();
      object.writeHttpMetadata(headers);
      headers.set("etag", object.httpEtag);
      headers.set("Content-Type", "application/vnd.android.package-archive");
      headers.set("Content-Disposition", `attachment; filename="${name}"`);
      headers.set("Cache-Control", "public, max-age=3600");
      const partial = request.headers.get("range") !== null && object.range;
      if (partial) headers.set("Content-Range", `bytes ${object.range.offset}-${object.range.offset + object.range.length - 1}/${object.size}`);
      const status = object.body ? (partial ? 206 : 200) : 304;
      return withHeaders(new Response(request.method === "HEAD" ? null : object.body, { status, headers }));
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
