// Fallback-origin behaviour of both Pages workers, with fetch and the edge cache stubbed.
//   node --test cloudflare/worker-fallback.test.mjs
import assert from "node:assert/strict";
import { beforeEach, test } from "node:test";
import pool from "./pool-site/_worker.js";
import mempool from "./mempool-site/_worker.js";

const HUB = "https://hub.example";
const HOME = "https://home.example";
const ENV = { ORIGIN_URL: HUB, ACCESS_CLIENT_ID: "id", ACCESS_CLIENT_SECRET: "secret", FALLBACK_ORIGIN_URL: HOME };

let calls;
let stored;
// answers: origin -> Response factory, or null for "cannot be reached"
function stub(answers) {
  calls = [];
  stored = new Map();
  globalThis.caches = {
    default: {
      match: async (req) => stored.get(req.url)?.clone(),
      put: async (req, res) => void stored.set(req.url, res),
    },
  };
  globalThis.fetch = async (input, init = {}) => {
    const req = input instanceof Request ? input : new Request(input, init);
    const origin = new URL(req.url).origin;
    calls.push({ origin, token: req.headers.get("CF-Access-Client-Id"), redirect: init.redirect ?? req.redirect });
    const make = answers[origin];
    if (!make) throw new TypeError("unreachable");
    return make();
  };
}
const ok = (body, extra = {}) => () => new Response(body, { status: 200, headers: { "Content-Type": "application/json", ...extra } });
const status = (code) => () => new Response(code === 204 || code === 304 ? null : "x", { status: code, headers: code === 302 ? { Location: "https://login.example" } : {} });
const ctx = () => {
  const pending = [];
  return { waitUntil: (p) => pending.push(p), done: () => Promise.all(pending) };
};
const origins = () => calls.map((c) => c.origin);

beforeEach(() => stub({}));

// ---- pool site -------------------------------------------------------------
const poolGet = (env, c = ctx()) => pool.fetch(new Request("https://pool.example/api/pool"), env, c);

test("pool: a healthy hub is the only node asked", async () => {
  stub({ [HUB]: ok('{"from":"hub"}'), [HOME]: ok('{"from":"home"}') });
  const res = await poolGet(ENV);
  assert.equal(await res.text(), '{"from":"hub"}');
  assert.equal(res.headers.get("X-Lazarus-Origin"), null);
  assert.deepEqual(origins(), [HUB]);
});

test("pool: an unreachable hub is answered by the standby, and marked", async () => {
  stub({ [HOME]: ok('{"from":"home"}', { "Cache-Control": "public, s-maxage=5" }) });
  const c = ctx();
  const res = await poolGet(ENV, c);
  assert.equal(res.status, 200);
  assert.equal(await res.text(), '{"from":"home"}');
  assert.equal(res.headers.get("X-Lazarus-Origin"), "fallback");
  assert.deepEqual(origins(), [HUB, HOME]);
  assert.equal(calls[1].token, "id");
  assert.equal(calls[1].redirect, "manual");
  await c.done();
  assert.equal(stored.size, 2, "the standby's 200 is cached like the hub's");
});

test("pool: a hub 5xx is answered by the standby", async () => {
  stub({ [HUB]: status(502), [HOME]: ok('{"from":"home"}') });
  const res = await poolGet(ENV);
  assert.equal(res.headers.get("X-Lazarus-Origin"), "fallback");
  assert.equal(await res.text(), '{"from":"home"}');
});

test("pool: the standby has its own service token when one is set", async () => {
  stub({ [HOME]: ok("{}") });
  await poolGet({ ...ENV, FALLBACK_ACCESS_CLIENT_ID: "home-id", FALLBACK_ACCESS_CLIENT_SECRET: "home-secret" });
  assert.deepEqual(calls.map((c) => c.token), ["id", "home-id"]);
});

for (const code of [302, 401, 403, 502]) {
  test(`pool: a standby answering ${code} is not used; the stale copy goes out`, async () => {
    stub({ [HUB]: ok('{"from":"hub"}', { "Cache-Control": "public, s-maxage=5" }) });
    const c = ctx();
    await poolGet(ENV, c);
    await c.done();
    for (const k of [...stored.keys()]) if (!k.includes("/__stale")) stored.delete(k);
    const kept = stored;
    stub({ [HOME]: status(code) });
    stored = kept;
    globalThis.caches.default.match = async (req) => kept.get(req.url)?.clone();
    const res = await poolGet(ENV);
    assert.equal(res.headers.get("X-Lazarus-Stale"), "1");
    assert.equal(res.headers.get("X-Lazarus-Origin"), null);
    assert.equal(await res.text(), '{"from":"hub"}');
  });
}

test("pool: both nodes down and nothing cached is a 502", async () => {
  const res = await poolGet(ENV);
  assert.equal(res.status, 502);
  assert.deepEqual(origins(), [HUB, HOME]);
});

test("pool: without FALLBACK_ORIGIN_URL nothing but the hub is asked", async () => {
  const res = await poolGet({ ...ENV, FALLBACK_ORIGIN_URL: undefined });
  assert.equal(res.status, 502);
  assert.deepEqual(origins(), [HUB]);
});

// ---- explorer --------------------------------------------------------------
const TIP = "https://mempool.example/api/v1/blocks/tip/height";
const mempoolFetch = (env, init = {}, url = TIP) => mempool.fetch(new Request(url, init), env, ctx());

test("explorer: a healthy hub is the only node asked", async () => {
  stub({ [HUB]: ok("975754"), [HOME]: ok("1") });
  const res = await mempoolFetch(ENV);
  assert.equal(await res.text(), "975754");
  assert.equal(res.headers.get("X-Lazarus-Origin"), null);
  assert.deepEqual(origins(), [HUB]);
});

for (const [name, hub] of [["unreachable", null], ["502", status(502)], ["530", status(530)], ["an Access redirect", status(302)]]) {
  test(`explorer: hub ${name} is answered by the standby, and marked`, async () => {
    stub({ ...(hub ? { [HUB]: hub } : {}), [HOME]: ok("975754") });
    const res = await mempoolFetch(ENV);
    assert.equal(res.status, 200);
    assert.equal(await res.text(), "975754");
    assert.equal(res.headers.get("X-Lazarus-Origin"), "fallback");
    assert.deepEqual(origins(), [HUB, HOME]);
  });
}

test("explorer: a cached hashrate answer from the standby keeps its mark", async () => {
  stub({ [HOME]: ok('{"hashrates":[],"difficulty":[]}') });
  const res = await mempoolFetch(ENV, {}, "https://mempool.example/api/v1/mining/hashrate/3d");
  assert.equal(res.headers.get("X-Lazarus-Origin"), "fallback");
});

for (const code of [302, 401, 403, 503]) {
  test(`explorer: a standby answering ${code} is a 503, never its own page`, async () => {
    stub({ [HOME]: status(code) });
    const res = await mempoolFetch(ENV);
    assert.equal(res.status, 503);
    assert.equal(res.headers.get("Location"), null);
  });
}

test("explorer: a request with a body is not asked twice", async () => {
  stub({ [HOME]: ok("txid") });
  const res = await mempoolFetch(ENV, { method: "POST", body: "00", duplex: "half" }, "https://mempool.example/api/tx");
  assert.equal(res.status, 503);
  // Node refuses to build the upstream Request without `duplex` (Workers do not need it), so
  // the hub is not reached here either; what matters is that the standby is never asked.
  assert.ok(!origins().includes(HOME));
});

test("explorer: a hub 404 is the answer; the standby is not asked", async () => {
  stub({ [HUB]: status(404), [HOME]: ok("1") });
  const res = await mempoolFetch(ENV);
  assert.equal(res.status, 404);
  assert.deepEqual(origins(), [HUB]);
});

test("explorer: without FALLBACK_ORIGIN_URL nothing but the hub is asked", async () => {
  const res = await mempoolFetch({ ...ENV, FALLBACK_ORIGIN_URL: undefined });
  assert.equal(res.status, 503);
  assert.deepEqual(origins(), [HUB]);
});
