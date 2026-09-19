// Advanced-mode Pages worker. _routes.json sends only /api/* and the Search Console proof here;
// every other URL is served straight from the static assets without invoking it.
//
// /api/*: pass the request to the node and hand the answer back, same-origin.
// ORIGIN_URL, ACCESS_CLIENT_ID and ACCESS_CLIENT_SECRET are Pages environment variables. The
// origin is a tunnel hostname behind Cloudflare Access, never the site's own hostname. Each 200 is kept in the colo cache for the s-maxage the node asked for, and kept a
// day longer as a fallback: if the node is unreachable or answers 5xx, the last good copy goes
// out instead, marked X-Lazarus-Stale, so a node restart does not blank the dashboard.
const STALE_S = 86400;
const GOOGLE_PROOF = "/googleda1d0aa98080ef94.html";

export default {
  async fetch(request, env, ctx) {
    const path = new URL(request.url).pathname;
    if (path.startsWith("/api/")) return api(request, env, ctx);
    // Pages 308s every /x.html to /x and Search Console will not follow a redirect when it
    // re-verifies ownership, so this one file is answered here at its exact path.
    if (path === GOOGLE_PROOF) {
      return new Response("google-site-verification: " + GOOGLE_PROOF.slice(1), {
        headers: { "Content-Type": "text/html; charset=utf-8", "Cache-Control": "public, max-age=0, s-maxage=86400" },
      });
    }
    return env.ASSETS.fetch(request);
  },
};

async function api(request, env, ctx) {
  if (request.method === "OPTIONS") {
    return new Response(null, { status: 204, headers: { "Access-Control-Allow-Origin": "*", "Access-Control-Allow-Methods": "GET, HEAD, OPTIONS" } });
  }
  if (request.method !== "GET" && request.method !== "HEAD") {
    return json({ error: "method not allowed" }, 405);
  }
  if (!env.ORIGIN_URL) return json({ error: "origin not configured" }, 500);

  const url = new URL(request.url);
  const target = env.ORIGIN_URL.replace(/\/$/, "") + url.pathname + url.search;
  const cache = caches.default;
  const freshKey = new Request(url.origin + url.pathname + url.search, { method: "GET" });
  const staleKey = new Request(url.origin + "/__stale" + url.pathname + url.search, { method: "GET" });

  const hit = await cache.match(freshKey);
  if (hit) return finish(hit, request, "HIT");

  let res;
  try {
    res = await fetch(target, {
      method: "GET",
      headers: { Accept: "application/json", "User-Agent": "lazarus-pages-proxy", ...accessHeaders(env) },
      signal: AbortSignal.timeout(20000),
    });
  } catch (e) {
    res = null;
  }

  if (!res || res.status >= 500) {
    const stale = await cache.match(staleKey);
    if (stale) {
      const out = new Response(stale.body, stale);
      out.headers.set("X-Lazarus-Stale", "1");
      out.headers.set("Cache-Control", "no-store");
      return finish(out, request, "STALE");
    }
    return res ? finish(new Response(res.body, res), request, "MISS") : json({ error: "origin unreachable" }, 502);
  }

  const out = new Response(res.body, res);
  out.headers.set("Access-Control-Allow-Origin", "*");
  out.headers.delete("Set-Cookie");
  const m = /s-maxage=(\d+)/.exec(out.headers.get("Cache-Control") || "");
  const ttl = m ? parseInt(m[1], 10) : 0;
  if (res.status === 200 && ttl > 0) {
    const fresh = out.clone();
    fresh.headers.set("Cache-Control", `public, max-age=${ttl}`);
    const keep = out.clone();
    keep.headers.set("Cache-Control", `public, max-age=${STALE_S}`);
    ctx.waitUntil(Promise.all([cache.put(freshKey, fresh), cache.put(staleKey, keep)]));
  }
  return finish(out, request, "MISS");
}

// The origin hostname sits behind Cloudflare Access; this service token is the only way in.
function accessHeaders(env) {
  if (!env.ACCESS_CLIENT_ID) return {};
  return { "CF-Access-Client-Id": env.ACCESS_CLIENT_ID, "CF-Access-Client-Secret": env.ACCESS_CLIENT_SECRET };
}

function finish(res, request, state) {
  const out = new Response(request.method === "HEAD" ? null : res.body, res);
  out.headers.set("X-Lazarus-Cache", state);
  if (state === "HIT") out.headers.set("Cache-Control", "public, max-age=0, s-maxage=5");
  return out;
}

function json(obj, status) {
  return new Response(JSON.stringify(obj), {
    status,
    headers: { "Content-Type": "application/json", "Access-Control-Allow-Origin": "*", "Cache-Control": "no-store" },
  });
}
