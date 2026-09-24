// Pages worker for the mempool explorer: the routing nginx-mempool.conf does on the node.
//
//   /api/*, /ws     proxied to ORIGIN_URL (websocket upgrades included); the node's own nginx
//                   keeps deciding which backend answers which path. Visitor cookies never go
//                   up, origin cookies never come down, and a few read-only GETs are held in
//                   the edge cache for seconds so a busy page does not hammer a home server
//   /api/v1/services/*  answered here with []: the node would relay it to mempool.space, which
//                   is a different chain and 800+ ms away
//   /lazarus/pool-tags.json  proxied like the API: pools-sync rewrites it on the node every
//                   3 min, and the pie's gateway bands are only as current as it is. The copy
//                   taken at build time answers when the node cannot
//   everything else try /<lang>/<path>, then /<path>, then /en-US/<path>, then the locale's
//                   app shell, where <lang> comes from the `lang` cookie or Accept-Language
//
// /resources/* and the rest of /lazarus/* never reach this worker (_routes.json, which build.sh
// writes from the theme files it ships).
const LANGS = ["ar", "cs", "da", "de", "es", "fa", "fr", "ko", "it", "he", "ka", "hu", "mk", "nl", "ja", "nb", "pl", "pt", "ro", "ru", "sl", "fi", "sv", "th", "tr", "uk", "vi", "zh", "hi", "ne", "lt", "hr"];
// The /<lang>/ URL prefixes nginx answers with that locale's shell. Locales in this list that
// were never built fall through to the 404 branch, as they do on the node.
const PREFIX_RE = /^\/(ar|bg|bs|cs|da|de|et|el|es|eo|eu|fa|fr|gl|ko|hr|id|it|he|ka|lv|lt|hu|mk|ms|nl|ja|nb|nn|pl|pt|pt-BR|ro|ru|sk|sl|sr|sh|fi|sv|th|tr|uk|vi|zh|hi)\//;

// The same URL answers in a different language per visitor. Cloudflare's edge cache ignores
// Vary on Accept-Language and Cookie, so a shared cache would hand one visitor's language to
// everyone; `private` keeps these in the browser's cache only.
const LOCALE_CACHE = "private, max-age=600";
// Hashed chunks never change under their name, but the same name holds a different language per
// visitor, so they are immutable in the browser and still never shared.
const HASHED_RE = /\.[0-9a-f]{16}\.(js|css)$/;
const HASHED_CACHE = "private, max-age=2592000, immutable";

// Edge cache lifetimes, in seconds, for API GETs whose answer is the same for every visitor.
const API_TTL = [
  [/^\/api\/v1\/(mining\/|historical-price$|statistics\/)/, 60],
  [/^\/lazarus\/pool-tags\.json$/, 60],
  [/^\/api\/v1\/(blocks(\/\d+)?$|prices$|fees\/|difficulty-adjustment$|backend-info$)/, 10],
  [/^\/api\/(v1\/)?block\/[0-9a-f]{64}\/(header|summary|txids)$/, 300],
  // What a block holds never changes under its hash. Dashboards poll a block's first page of
  // transactions (the coinbase) every few seconds; without this each poll is a node lookup.
  [/^\/api\/(v1\/)?block\/[0-9a-f]{64}(\/txs(\/\d+)?|\/raw)?$/, 600],
  // A transaction changes once, when it confirms; the page's websocket says so live.
  [/^\/api\/tx\/[0-9a-f]{64}(\/status|\/hex|\/outspends|\/merkle-proof)?$/, 30],
  // Address answers come from electrs, which serves one request at a time and rebuilds an address
  // by fetching every block it appears in: a miner with a long payout history costs seconds, and
  // everyone else waits behind it. The page's websocket carries new transactions live, so a
  // summary a few seconds old costs nothing. Older pages of history do not change.
  [/^\/api\/address\/[A-Za-z0-9]{20,100}\/txs\/chain\/[0-9a-f]{64}$/, 300],
  [/^\/api\/address\/[A-Za-z0-9]{20,100}(\/utxo|\/txs|\/txs\/chain)?$/, 60],
];

// Search crawlers render pages like a browser, so every address page they index is a full electrs
// rebuild of that address, and Googlebot alone walked ~55 a minute (22 Sep 2026), stalling every
// visitor behind it. Address, transaction and API pages are kept out of indexes (robots.txt), and
// until a crawler rereads that, its address lookups are turned away here.
const CRAWLER_RE = /bot|crawl|spider|slurp|bingpreview|facebookexternalhit|embedly|headless/i;
const ROBOTS = `User-agent: *
Disallow: /address/
Disallow: /tx/
Disallow: /api/
Disallow: /*/address/
Disallow: /*/tx/
Allow: /
`;

// No Content-Security-Policy on purpose: the Angular app uses inline styles and scripts, the
// theme loads Google Fonts and calls the pool API cross-origin, and a policy tight enough to be
// worth having would break all three. HSTS carries no includeSubDomains and no preload.
function secure(headers) {
  headers.set("Strict-Transport-Security", "max-age=15552000");
  headers.set("X-Frame-Options", "SAMEORIGIN");
  headers.set("Permissions-Policy", "geolocation=(), microphone=(), camera=(), payment=()");
  if (!headers.has("X-Content-Type-Options")) headers.set("X-Content-Type-Options", "nosniff");
  if (!headers.has("Referrer-Policy")) headers.set("Referrer-Policy", "strict-origin-when-cross-origin");
}

function pick(value) {
  const v = (value || "").toLowerCase();
  if (!v) return null;
  if (v.startsWith("en")) return "en-US";
  return LANGS.find((l) => v.startsWith(l)) || null;
}

function language(request) {
  const m = /(?:^|;\s*)lang=([^;]+)/.exec(request.headers.get("Cookie") || "");
  return pick(m && m[1]) || pick(request.headers.get("Accept-Language")) || "en-US";
}

async function asset(env, request, path) {
  const res = await env.ASSETS.fetch(new Request(new URL(path, request.url), { method: request.method, headers: request.headers }));
  // Pages answers a missing file with its SPA fallback (200 + root index.html) or a 3xx to the
  // pretty URL; only a direct hit on the file that was asked for counts.
  if (res.status === 200 && !(res.headers.get("Content-Type") || "").includes("text/html")) return res;
  // A browser revalidating its cached copy (If-None-Match) gets 304 for a file that exists. That
  // is a hit, not a miss: falling through would hand the HTML shell to a <script> tag.
  if (res.status === 304) return res;
  return null;
}

async function shell(env, request, lang, status = 200) {
  const res = await env.ASSETS.fetch(new Request(new URL(`/${lang}/`, request.url), { headers: request.headers }));
  const out = new Response(request.method === "HEAD" ? null : res.body, { status: res.status === 200 ? status : res.status, headers: res.headers });
  out.headers.set("Cache-Control", LOCALE_CACHE);
  out.headers.set("Vary", "Accept-Encoding, Accept-Language, Cookie");
  secure(out.headers);
  return out;
}

// What a visitor sees when the node cannot answer. Never the origin's own error page or the
// Access login redirect: both name the origin hostname.
function unavailable() {
  return new Response('{"error":"explorer backend unavailable"}', {
    status: 503,
    headers: { "Content-Type": "application/json", "Retry-After": "15", "Cache-Control": "no-store" },
  });
}

// Hashrate and difficulty history starts at the BLAKE2b fork (block 961,640, 2026-08-30 06:14:37
// UTC). Before it the chain was SHA-256 Bitcoin at ~1 ZH/s, and one such point is enough for the
// charts to scale their axis to ZH/s or EH/s and flatten this network's ~35 PH/s to zero.
const FORK_TS = 1788070477;
const FORK_HEIGHT = 961640;
const POOL_TAGS = "/lazarus/pool-tags.json";
const HASHRATE_HISTORY = /^\/api\/v1\/mining\/(hashrate|pool\/[^/]+\/hashrate)(\/|$)/;
function postFork(doc) {
  const keep = (x) => !x || typeof x !== "object" ||
    (x.height != null ? Number(x.height) >= FORK_HEIGHT : Number(x.timestamp ?? x.time ?? FORK_TS) >= FORK_TS);
  if (Array.isArray(doc)) return doc.filter(keep);
  if (doc && typeof doc === "object") {
    if (Array.isArray(doc.hashrates)) doc.hashrates = doc.hashrates.filter(keep);
    if (Array.isArray(doc.difficulty)) doc.difficulty = doc.difficulty.filter(keep);
  }
  return doc;
}

async function proxy(request, env, ctx, url) {
  if (!env.ORIGIN_URL) return new Response("origin not configured", { status: 500 });
  const target = env.ORIGIN_URL.replace(/\/$/, "") + url.pathname + url.search;
  const headers = new Headers(request.headers);
  // The API is public and stateless. A visitor's cookies are none of the origin's business, and
  // without them every visitor's GET is the same request, which is what makes it cacheable.
  headers.delete("Cookie");
  headers.set("X-Forwarded-Host", url.host);

  const rule = request.method === "GET" && !request.headers.get("Upgrade") && API_TTL.find(([re]) => re.test(url.pathname));
  const key = rule && new Request(url.toString(), { method: "GET" });
  if (rule) {
    const hit = await caches.default.match(key);
    if (hit) {
      const out = new Response(hit.body, hit);
      out.headers.set("X-Edge-Cache", "HIT");
      return out;
    }
    // Ask for the whole answer so there is a 200 to store, not a 304 for one browser's copy.
    headers.delete("If-None-Match");
    headers.delete("If-Modified-Since");
  }
  // The origin hostname sits behind Cloudflare Access; this service token is the only way in.
  if (env.ACCESS_CLIENT_ID) {
    headers.set("CF-Access-Client-Id", env.ACCESS_CLIENT_ID);
    headers.set("CF-Access-Client-Secret", env.ACCESS_CLIENT_SECRET);
  }
  let res;
  try {
    res = await fetch(new Request(target, { method: request.method, headers, body: request.body, redirect: "manual" }));
  } catch {
    return unavailable();
  }
  // A websocket handshake has to go back exactly as it came.
  if (res.status === 101 || res.webSocket) return res;
  // A 3xx here is Access sending the service token to its login page (304 is a real answer).
  if ((res.status >= 300 && res.status < 400 && res.status !== 304) || res.status === 502 || res.status === 503 || res.status >= 520) return unavailable();

  let out = new Response(res.body, res);
  if (res.status === 200 && request.method === "GET" && HASHRATE_HISTORY.test(url.pathname)) {
    try {
      const body = JSON.stringify(postFork(await res.clone().json()));
      const h = new Headers(res.headers);
      h.delete("Content-Length");
      h.delete("Content-Encoding");
      h.delete("ETag");
      out = new Response(body, { status: 200, headers: h });
    } catch {
      /* not JSON after all: pass it through unchanged */
    }
  }
  // Access sets its CF_Authorization session cookie on every answer; relayed, it would hand each
  // visitor a session for the origin.
  out.headers.delete("Set-Cookie");
  out.headers.delete("X-Powered-By");
  if (rule && res.status === 200 && !out.headers.has("Set-Cookie")) {
    out.headers.set("Cache-Control", `public, max-age=5, s-maxage=${rule[1]}`);
    ctx.waitUntil(caches.default.put(key, out.clone()));
    out.headers.set("X-Edge-Cache", "MISS");
  }
  return out;
}

export default {
  async fetch(request, env, ctx) {
    const url = new URL(request.url);
    const path = url.pathname;

    if (path === "/robots.txt") {
      return new Response(ROBOTS, { headers: { "Content-Type": "text/plain; charset=utf-8", "Cache-Control": "public, max-age=3600" } });
    }
    if (path.startsWith("/api/address/") && CRAWLER_RE.test(request.headers.get("User-Agent") || "")) {
      return new Response('{"error":"address lookups are not for crawlers; see /robots.txt"}', {
        status: 429,
        headers: { "Content-Type": "application/json", "Retry-After": "86400", "Cache-Control": "no-store" },
      });
    }
    // nginx: `location = /api` and `= /api/` are the static API docs, in English.
    if (path === "/api" || path === "/api/") return shell(env, request, "en-US");
    if (path.startsWith("/api/v1/services/")) {
      return new Response("[]", { headers: { "Content-Type": "application/json", "Cache-Control": "public, max-age=3600" } });
    }
    if (path.startsWith("/api/") || path === "/ws" || path.startsWith("/ws/")) return proxy(request, env, ctx, url);
    if (path === POOL_TAGS) {
      const live = request.method === "GET" ? await proxy(request, env, ctx, url) : null;
      if (live && (live.status === 200 || live.status === 304)) return live;
      const snap = await asset(env, request, path);
      return snap || live || new Response(null, { status: 405 });
    }

    if (request.method !== "GET" && request.method !== "HEAD") return new Response(null, { status: 405 });

    const prefixed = PREFIX_RE.exec(path);
    if (prefixed) {
      const hit = await asset(env, request, path);
      if (hit) return hit;
      const has = await env.ASSETS.fetch(new Request(new URL(`/${prefixed[1]}/`, request.url)));
      return has.status === 200 && LANGS.includes(prefixed[1]) ? shell(env, request, prefixed[1]) : new Response("Not Found", { status: 404 });
    }

    const lang = language(request);
    if (path !== "/" && /\.[A-Za-z0-9]+$/.test(path)) {
      for (const p of [`/${lang}${path}`, path, `/en-US${path}`]) {
        const hit = await asset(env, request, p);
        if (hit) {
          const out = new Response(hit.body, hit);
          out.headers.set("Cache-Control", HASHED_RE.test(path) ? HASHED_CACHE : LOCALE_CACHE);
          out.headers.set("Vary", "Accept-Encoding, Accept-Language, Cookie");
          secure(out.headers);
          return out;
        }
      }
    }
    return shell(env, request, lang);
  },
};
