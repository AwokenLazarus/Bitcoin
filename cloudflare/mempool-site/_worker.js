// Pages worker for the mempool explorer: the routing nginx-mempool.conf does on the node.
//
//   /api/*, /ws     proxied to ORIGIN_URL (websocket upgrades included); the node's own nginx
//                   keeps deciding which backend answers which path
//   everything else try /<lang>/<path>, then /<path>, then /en-US/<path>, then the locale's
//                   app shell, where <lang> comes from the `lang` cookie or Accept-Language
//
// /resources/* and /lazarus/* never reach this worker (_routes.json).
const LANGS = ["ar", "cs", "da", "de", "es", "fa", "fr", "ko", "it", "he", "ka", "hu", "mk", "nl", "ja", "nb", "pl", "pt", "ro", "ru", "sl", "fi", "sv", "th", "tr", "uk", "vi", "zh", "hi", "ne", "lt", "hr"];
// The /<lang>/ URL prefixes nginx answers with that locale's shell. Locales in this list that
// were never built fall through to the 404 branch, as they do on the node.
const PREFIX_RE = /^\/(ar|bg|bs|cs|da|de|et|el|es|eo|eu|fa|fr|gl|ko|hr|id|it|he|ka|lv|lt|hu|mk|ms|nl|ja|nb|nn|pl|pt|pt-BR|ro|ru|sk|sl|sr|sh|fi|sv|th|tr|uk|vi|zh|hi)\//;

// The same URL answers in a different language per visitor. Cloudflare's edge cache ignores
// Vary on Accept-Language and Cookie, so a shared cache would hand one visitor's language to
// everyone; `private` keeps these in the browser's cache only.
const LOCALE_CACHE = "private, max-age=600, no-transform";

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
  return out;
}

function proxy(request, env, url) {
  if (!env.ORIGIN_URL) return new Response("origin not configured", { status: 500 });
  const target = env.ORIGIN_URL.replace(/\/$/, "") + url.pathname + url.search;
  const headers = new Headers(request.headers);
  headers.set("X-Forwarded-Host", url.host);
  // The origin hostname sits behind Cloudflare Access; this service token is the only way in.
  if (env.ACCESS_CLIENT_ID) {
    headers.set("CF-Access-Client-Id", env.ACCESS_CLIENT_ID);
    headers.set("CF-Access-Client-Secret", env.ACCESS_CLIENT_SECRET);
  }
  return fetch(new Request(target, { method: request.method, headers, body: request.body, redirect: "manual" }));
}

export default {
  async fetch(request, env) {
    const url = new URL(request.url);
    const path = url.pathname;

    // nginx: `location = /api` and `= /api/` are the static API docs, in English.
    if (path === "/api" || path === "/api/") return shell(env, request, "en-US");
    if (path.startsWith("/api/") || path === "/ws" || path.startsWith("/ws/")) return proxy(request, env, url);

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
          out.headers.set("Cache-Control", LOCALE_CACHE);
          out.headers.set("Vary", "Accept-Encoding, Accept-Language, Cookie");
          return out;
        }
      }
    }
    return shell(env, request, lang);
  },
};
