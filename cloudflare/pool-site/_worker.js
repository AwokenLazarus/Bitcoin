// Advanced-mode Pages worker. _routes.json sends only /api/* and the Search Console proofs here;
// every other URL is served straight from the static assets without invoking it.
//
// /api/*: pass the request to the node and hand the answer back, same-origin.
// ORIGIN_URL, ACCESS_CLIENT_ID and ACCESS_CLIENT_SECRET are Pages environment variables. The
// origin is a tunnel hostname behind Cloudflare Access, never the site's own hostname. Each 200 is kept in the colo cache for the s-maxage the node asked for, and kept a
// day longer as a fallback: if the node is unreachable or answers 5xx, the last good copy goes
// out instead, marked X-Lazarus-Stale, so a node restart does not blank the dashboard.
//
// FALLBACK_ORIGIN_URL is optional: the standby node's tunnel hostname. It is asked only when
// ORIGIN_URL is unreachable or answers 5xx, and its answer is used only when the standby is
// really serving (see usable()), marked X-Lazarus-Origin: fallback. The standby runs no site
// until a failover starts it, so before that this changes nothing: the stale copy still goes out.
const STALE_S = 86400;
// One file per Search Console owner (Mike, Brett). Each is that Google account's own token.
const GOOGLE_PROOFS = new Set(["/googleda1d0aa98080ef94.html", "/google4631882008b8273c.html"]);

export default {
  async fetch(request, env, ctx) {
    const path = new URL(request.url).pathname;
    if (path.startsWith("/api/")) return api(request, env, ctx);
    // Pages 308s every /x.html to /x and Search Console will not follow a redirect when it
    // re-verifies ownership, so these files are answered here at their exact paths.
    if (GOOGLE_PROOFS.has(path)) {
      return new Response("google-site-verification: " + path.slice(1), {
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
  const cache = caches.default;
  const freshKey = new Request(url.origin + url.pathname + url.search, { method: "GET" });
  const staleKey = new Request(url.origin + "/__stale" + url.pathname + url.search, { method: "GET" });

  const hit = await cache.match(freshKey);
  if (hit) return finish(hit, request, "HIT");

  let res = await ask(env.ORIGIN_URL, url, accessHeaders(env.ACCESS_CLIENT_ID, env.ACCESS_CLIENT_SECRET), "follow");
  let fallback = false;
  if ((!res || res.status >= 500) && env.FALLBACK_ORIGIN_URL) {
    const alt = await ask(
      env.FALLBACK_ORIGIN_URL,
      url,
      accessHeaders(env.FALLBACK_ACCESS_CLIENT_ID || env.ACCESS_CLIENT_ID, env.FALLBACK_ACCESS_CLIENT_SECRET || env.ACCESS_CLIENT_SECRET),
      // Not followed: the only redirect an origin sends is Access turning the token away to
      // its login page, and usable() has to see it.
      "manual",
    );
    if (usable(alt)) {
      res = alt;
      fallback = true;
    }
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
  if (fallback) out.headers.set("X-Lazarus-Origin", "fallback");
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

// One GET to a node; null when it cannot be reached.
async function ask(origin, url, headers, redirect) {
  try {
    return await fetch(origin.replace(/\/$/, "") + url.pathname + url.search, {
      method: "GET",
      headers: { Accept: "application/json", "User-Agent": "lazarus-pages-proxy", ...headers },
      redirect,
      signal: AbortSignal.timeout(20000),
    });
  } catch (e) {
    return null;
  }
}

// Whether the standby's answer is the site's own: not an error, and not Access refusing the
// token (3xx to its login page, 401 or 403), which would otherwise go out as the API's answer.
function usable(res) {
  return !!res && res.status < 500 && !(res.status >= 300 && res.status < 400) && res.status !== 401 && res.status !== 403;
}

// The origin hostname sits behind Cloudflare Access; this service token is the only way in.
function accessHeaders(id, secret) {
  if (!id) return {};
  return { "CF-Access-Client-Id": id, "CF-Access-Client-Secret": secret };
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
