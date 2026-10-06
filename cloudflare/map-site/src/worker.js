// lazarus-xbt.xyz: the ecosystem hub (static pages under public/), the galaxy map at /map, the
// figures the pages show at /api/stats, the map's data at /map/data.json, and a cron that keeps
// that data current from the public explorer and pool APIs. Stateless apart from two KV keys:
//   blocks  compact record of every block since the fork (see galaxy.js blockRecord)
//   galaxy  the built model the page draws
import { blockRecord, buildGalaxy, FORK_HEIGHT } from "./galaxy.js";
import { addressPays, applyHosted, blocksMissingKey, emptyState, migrate, noteBlock, observeLive, outputsOf, prune, rememberStratum, setGatewayKey, validAddress } from "./naughtylist.js";
import { askPools, buildView, refreshPools, storedEvidence } from "./pools.js";
import { renderAddress, renderList } from "./naughtypage.js";
import { fillHub, renderMapTable } from "./hubfill.js";

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

async function getText(url) {
  const r = await fetch(url, { headers: { "User-Agent": UA, Accept: "text/html" }, signal: AbortSignal.timeout(20000) });
  if (!r.ok) throw new Error(`${url} -> HTTP ${r.status}`);
  return r.text();
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
  let naughty = null;
  try {
    naughty = await ingestNaughty(env, tip);
  } catch (e) {
    console.error("naughty list ingest failed", e.message);
  }
  return { tip, blocks: recs.length, added: recs.length - stored.length, pages, stored: changed, coverage: Math.round(coverage * 1000) / 10, naughty };
}

// How many new blocks one cron reads coinbases for. Fourteen days is a few hours of crons.
const INGEST_BLOCKS = 48;
const INGEST_WINDOW_SEC = 14 * 86400;

async function noteExplorerBlock(state, b, hosted) {
  const id = b.id;
  if (state.ingested && state.ingested[b.height] === id) return false;
  let outputs = [];
  let txid = "";
  try {
    const txs = await getJSON(`${EXPLORER}/api/block/${id}/txs/0`);
    const coinbase = Array.isArray(txs) ? txs[0] : null;
    if (coinbase) {
      txid = coinbase.txid || "";
      outputs = outputsOf(coinbase);
    }
  } catch (e) {
    console.error("coinbase tx", b.height, e.message);
  }
  if (!outputs.length) {
    const extras = b.extras || {};
    const addrs = Array.isArray(extras.coinbaseAddresses) ? extras.coinbaseAddresses : [];
    outputs = addrs.filter(Boolean).map((address, vout) => ({ address, vout, sats: null }));
  }
  noteBlock(state, {
    h: b.height, t: b.timestamp, id, txid,
    script: (b.extras && b.extras.coinbaseRaw) || "",
    explorerPool: (b.extras && b.extras.pool && b.extras.pool.name) || "",
    outputs,
  }, hosted);
  return true;
}

// Coinbases one cron reads again to learn an older block's gateway name.
const REKEY_BLOCKS = 20;

// Requests one cron may spend on the pools' own APIs, shared by the readers that ask per address.
const POOL_BUDGET = 16;

/**
 * Walk back through coinbases a slice per cron, read what each pool publishes about its miners,
 * and store both the working document (`naughtylist`) and the small one the page reads
 * (`naughtyview`), so a page view never parses the working document.
 */
export async function ingestNaughty(env, tip) {
  const now = Math.floor(Date.now() / 1000);
  const state = migrate((await env.GALAXY.get("naughtylist", "json")) || emptyState());
  // Gateway names proved to be a pool's own (scripts/hosted-watch.py). Absent: nothing is proved.
  const hosted = await env.GALAXY.get("hostedgateways", "json").catch(() => null);
  let noted = 0;
  let pages = 0;
  // The chain grew: read the tip page so a block found since the last cron is not skipped.
  if (state.scannedTo == null || tip > (state.tip || 0)) {
    const page = await getJSON(`${EXPLORER}/api/v1/blocks/${tip}`);
    pages++;
    if (Array.isArray(page) && page.length) {
      for (const b of page) {
        if (!b || b.height < FORK_HEIGHT || noted >= INGEST_BLOCKS) break;
        if (b.timestamp && now - b.timestamp > INGEST_WINDOW_SEC) { state.windowDone = true; break; }
        if (await noteExplorerBlock(state, b, hosted)) noted++;
      }
      if (state.scannedTo == null) state.scannedTo = page[page.length - 1].height;
    }
  }
  while (!state.windowDone && noted < INGEST_BLOCKS && pages < 6 && state.scannedTo - 1 >= FORK_HEIGHT) {
    const page = await getJSON(`${EXPLORER}/api/v1/blocks/${state.scannedTo - 1}`);
    pages++;
    if (!Array.isArray(page) || !page.length) break;
    let aged = false;
    for (const b of page) {
      if (!b || b.height < FORK_HEIGHT || noted >= INGEST_BLOCKS) break;
      if (b.timestamp && now - b.timestamp > INGEST_WINDOW_SEC) { aged = true; break; }
      if (await noteExplorerBlock(state, b, hosted)) noted++;
    }
    state.scannedTo = page[page.length - 1].height;
    if (aged) state.windowDone = true;
  }
  state.tip = tip;
  // A run that is still catching up on coinbases leaves the pools for the next one.
  const pools = noted > 8 ? {} : await refreshPools(state, { get: getJSON, getText, now, budget: POOL_BUDGET });
  const [miners, gateways] = await Promise.all([
    getJSON(`${POOL}/api/miners`).catch(() => null),
    getJSON(`${POOL}/api/gateways`).catch(() => null),
  ]);
  // Gateway blocks noted before entries kept their gateway name: read a few coinbases again each
  // run, then re-judge every noted block against what has been proved since.
  let rekeyed = 0;
  if (hosted && noted <= 8) {
    for (const [h, id] of blocksMissingKey(state, REKEY_BLOCKS)) {
      const b = await getJSON(`${EXPLORER}/api/v1/block/${id}`).catch(() => null);
      if (!b || !b.extras) break;
      setGatewayKey(state, h, b.extras.coinbaseRaw || "");
      rekeyed++;
    }
  }
  const rejudged = applyHosted(state, hosted);
  const online = (miners && miners.online) || [];
  observeLive(state, online, now);
  rememberStratum(state, now);
  prune(state, now);
  const view = buildView(state, online, (gateways && gateways.gateways) || [], now);
  // The Lazarus rows come from live sessions. If the pool did not answer, the last view still has
  // them and a new one would silently drop every one, so the old view stays up.
  const keepOld = !miners && (await env.GALAXY.get("naughtyview")) != null;
  await Promise.all([
    env.GALAXY.put("naughtylist", JSON.stringify(state)),
    keepOld ? null : env.GALAXY.put("naughtyview", JSON.stringify(view)),
  ]);
  return { noted, rekeyed, rejudged, scannedTo: state.scannedTo, windowDone: !!state.windowDone, naughty: view.naughty.length, suspects: view.suspects.length, nice: view.nice.length, pools, viewKept: keepOld };
}

async function naughtyView(env) {
  const view = await env.GALAXY.get("naughtyview", "json");
  if (view) return view;
  // Before the first cron after a deploy there is no view yet: build one from the working document.
  const state = migrate((await env.GALAXY.get("naughtylist", "json")) || emptyState());
  return buildView(state, [], [], Math.floor(Date.now() / 1000));
}

async function naughtyAddress(env, address) {
  if (!validAddress(address)) return { error: "that is not an address", address };
  const now = Math.floor(Date.now() / 1000);
  const [stored, view, utxos] = await Promise.all([
    env.GALAXY.get("naughtylist", "json").catch(() => null),
    naughtyView(env).catch(() => null),
    getJSON(`${EXPLORER}/api/address/${encodeURIComponent(address)}/utxo`).catch(() => []),
  ]);
  const state = migrate(stored || emptyState());
  const chainKind = (h) => (state.blk[h] ? state.blk[h][2] : null);
  const asked = await askPools(address, { get: getJSON, now, chainKind }).catch(() => []);
  const pays = addressPays(state, address, Array.isArray(utxos) ? utxos : []);
  const unspent = pays.filter((p) => p.unspent);
  const listed = view ? [...view.naughty, ...view.suspects, ...view.nice].find((r) => r.address === address) || null : null;
  return {
    address,
    listed,
    stored: storedEvidence(state, address, now),
    asked,
    pays,
    unspentSats: unspent.reduce((s, p) => s + (Number(p.sats) || 0), 0),
    unspentCount: unspent.length,
    explorer: `https://mempool.lazarus-xbt.xyz/address/${address}`,
  };
}

const PAGE_CSP = "default-src 'self'; script-src 'self' https://static.cloudflareinsights.com; style-src 'self' https://fonts.googleapis.com; font-src https://fonts.gstatic.com; img-src 'self' data:; connect-src 'self' https://cloudflareinsights.com; frame-ancestors 'none'; base-uri 'none'; form-action 'self'";

/** The naughty list as HTML, held at the edge for a minute per URL. */
async function naughtyPage(request, env, ctx, url) {
  const a = (url.searchParams.get("a") || "").trim();
  const pool = (url.searchParams.get("pool") || "").slice(0, 64);
  const key = new Request(`${url.origin}/naughtylist/?${a ? `a=${encodeURIComponent(a.slice(0, 100))}` : `pool=${encodeURIComponent(pool)}`}`);
  const cache = caches.default;
  const hit = await cache.match(key);
  if (hit) return withHeaders(hit, { "X-Edge-Cache": "HIT" });
  let html, status = 200;
  if (a) {
    const d = await naughtyAddress(env, a);
    if (d.error) status = 400;
    html = renderAddress(d);
  } else {
    html = renderList(await naughtyView(env), { pool });
  }
  const res = new Response(html, {
    status,
    headers: {
      "Content-Type": "text/html; charset=utf-8",
      "Cache-Control": a ? "public, max-age=60" : "public, max-age=60, s-maxage=60, stale-while-revalidate=300",
      "Content-Security-Policy": PAGE_CSP,
      "X-Frame-Options": "DENY",
      "Permissions-Policy": "camera=(), microphone=(), geolocation=()",
      ...(a ? { "X-Robots-Tag": "noindex" } : {}),
    },
  });
  if (status === 200) ctx.waitUntil(cache.put(key, res.clone()));
  return withHeaders(res, { "X-Edge-Cache": "MISS" });
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

const HUB_FILLED = new Set(["/", "/ecosystem/"]);
const STATS_HEADERS = { "Content-Type": "application/json; charset=utf-8", "Cache-Control": "public, max-age=30, s-maxage=60", "Access-Control-Allow-Origin": "*" };

/** /api/stats as text, from the edge cache when it is there. Pages and the API share the one copy. */
async function cachedStats(env, ctx, url) {
  const cache = caches.default, key = new Request(`${url.origin}/api/stats`);
  const hit = await cache.match(key);
  if (hit) return { body: await hit.text(), hit: true };
  const body = JSON.stringify(await stats(env));
  ctx.waitUntil(cache.put(key, new Response(body, { headers: STATS_HEADERS })));
  return { body, hit: false };
}

/** The galaxy model, from the edge copy of /map/data.json when there is one, else from KV. */
async function cachedGalaxy(env, url) {
  const hit = await caches.default.match(new Request(`${url.origin}/map/data.json`));
  if (hit) return hit.json();
  return env.GALAXY.get("galaxy", "json");
}

export default {
  async fetch(request, env, ctx) {
    const url = new URL(request.url);
    if (url.pathname === "/map") return Response.redirect(`${url.origin}/map/`, 301);
    if (url.pathname === "/naughtylist") return Response.redirect(`${url.origin}/naughtylist/`, 301);
    if (url.pathname === "/naughtylist/" || url.pathname === "/naughtylist/index.html") {
      if (request.method !== "GET" && request.method !== "HEAD") return new Response("method not allowed", { status: 405 });
      try {
        return await naughtyPage(request, env, ctx, url);
      } catch (e) {
        console.error("naughty page", e.message);
        return withHeaders(new Response("The naughty list could not be built just now. Try again in a minute.\n", { status: 503, headers: { "Content-Type": "text/plain; charset=utf-8", "Retry-After": "60", "Cache-Control": "no-store" } }));
      }
    }
    if (url.pathname === "/api/naughtylist" || url.pathname === "/api/naughtylist/address") {
      if (request.method !== "GET" && request.method !== "HEAD") return new Response("method not allowed", { status: 405 });
      try {
        const data = url.pathname.endsWith("/address")
          ? await naughtyAddress(env, url.searchParams.get("a") || "")
          : await naughtyView(env);
        return withHeaders(new Response(JSON.stringify(data), {
          status: data.error ? 400 : 200,
          headers: { "Content-Type": "application/json; charset=utf-8", "Cache-Control": "public, max-age=30, s-maxage=60", "Access-Control-Allow-Origin": "*" },
        }));
      } catch (e) {
        return withHeaders(new Response(JSON.stringify({ error: e.message }), { status: 503, headers: { "Content-Type": "application/json", "Retry-After": "30", "Cache-Control": "no-store" } }));
      }
    }
    // The hub used to carry a mining guide; the pool site owns that, so those URLs go there.
    if (url.pathname === "/start" || url.pathname.startsWith("/start/")) {
      return Response.redirect("https://pool.lazarus-xbt.xyz/", 301);
    }
    if (url.pathname === "/api/stats") {
      if (request.method !== "GET" && request.method !== "HEAD") return new Response("method not allowed", { status: 405 });
      try {
        const { body, hit } = await cachedStats(env, ctx, url);
        return withHeaders(new Response(body, { headers: STATS_HEADERS }), { "X-Edge-Cache": hit ? "HIT" : "MISS" });
      } catch (e) {
        return withHeaders(new Response(JSON.stringify({ error: e.message }), { status: 503, headers: { "Content-Type": "application/json", "Retry-After": "30", "Cache-Control": "no-store" } }));
      }
    }
    // Pages whose live parts are written in here, so they read whole with JavaScript off.
    if (HUB_FILLED.has(url.pathname) && request.method === "GET") {
      const page = await env.ASSETS.fetch(request);
      if (!page.ok || !(page.headers.get("Content-Type") || "").includes("text/html")) return withHeaders(page);
      const [stats, galaxy] = await Promise.all([
        cachedStats(env, ctx, url).then((r) => JSON.parse(r.body)).catch(() => null),
        cachedGalaxy(env, url).catch(() => null),
      ]);
      const filled = fillHub(page, { stats, galaxy });
      return withHeaders(filled, { "Cache-Control": "public, max-age=60" });
    }
    if (url.pathname === "/map/table") return Response.redirect(`${url.origin}/map/table/`, 301);
    if (url.pathname === "/map/table/") {
      if (request.method !== "GET" && request.method !== "HEAD") return new Response("method not allowed", { status: 405 });
      const galaxy = await cachedGalaxy(env, url).catch(() => null);
      if (!galaxy) return withHeaders(new Response("The map data is not built yet. Try again in a minute.\n", { status: 503, headers: { "Content-Type": "text/plain; charset=utf-8", "Retry-After": "60" } }));
      return withHeaders(new Response(renderMapTable(galaxy), {
        headers: { "Content-Type": "text/html; charset=utf-8", "Cache-Control": "public, max-age=60, s-maxage=60, stale-while-revalidate=300", "Content-Security-Policy": PAGE_CSP, "X-Frame-Options": "DENY" },
      }));
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
