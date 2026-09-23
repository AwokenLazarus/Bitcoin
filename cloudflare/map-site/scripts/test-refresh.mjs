// Tests for the cron's guards (worker.js `refresh`). The cron runs unattended every five minutes
// and writes the two KV keys the whole site reads, so the cases that matter are the ones where an
// upstream answers badly: a wrong or empty KV read, an explorer that has fallen behind, a hole
// left by an outage. Each must leave the stored record alone rather than publish a gutted map.
//
//   node scripts/test-refresh.mjs
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";
import { refresh } from "../src/worker.js";
import { FORK_HEIGHT } from "../src/galaxy.js";

const here = dirname(fileURLToPath(import.meta.url));
const ALL = JSON.parse(readFileSync(join(here, "..", "data", "blocks.json"), "utf8")).filter((r) => r.h >= FORK_HEIGHT);
const TIP = ALL[ALL.length - 1].h;
let failures = 0;

// The explorer, as far as `refresh` is concerned: a tip height and pages of 15 blocks walking down.
function fakeUpstream({ tip = TIP, tipStatus = 200, have = ALL } = {}) {
  const byHeight = new Map(have.map((r) => [r.h, r]));
  const calls = { pages: 0 };
  globalThis.fetch = async (url) => {
    const u = String(url);
    if (u.endsWith("/api/blocks/tip/height")) return new Response(String(tip), { status: tipStatus });
    const m = u.match(/\/api\/v1\/blocks\/(\d+)$/);
    if (m) {
      calls.pages++;
      const from = Number(m[1]);
      const page = [];
      for (let h = from; h > from - 15 && h >= FORK_HEIGHT; h--) {
        const r = byHeight.get(h);
        // the shape blockRecord() reads, rebuilt from the stored record
        if (r) page.push({ height: r.h, id: r.id, timestamp: r.t, tx_count: r.n, weight: r.w, extras: { coinbaseRaw: "", totalFees: r.f, reward: r.r, medianFee: r.mf, matchRate: r.mr, feeRange: r.lf == null ? [] : [r.lf], coinbaseAddresses: r.ad } });
      }
      return new Response(JSON.stringify(page), { status: 200 });
    }
    return new Response("null", { status: 200 }); // gateways / hashrate / pool: optional
  };
  return calls;
}

function fakeKV(blocks) {
  const store = { blocks: blocks === null ? null : JSON.stringify(blocks), galaxy: null };
  const writes = [];
  return {
    writes,
    GALAXY: {
      async get(key, type) { const v = store[key]; return v == null ? null : (type === "json" ? JSON.parse(v) : v); },
      async put(key, value) { store[key] = value; writes.push(key); },
    },
    store,
  };
}

async function test(name, fn) {
  try {
    await fn();
    console.log(`  ok   ${name}`);
  } catch (e) {
    failures++;
    console.log(`  FAIL ${name}\n       ${e.message}`);
  }
}
const assert = (cond, msg) => { if (!cond) throw new Error(msg); };
async function throws(fn, re, msg) {
  try { await fn(); } catch (e) { assert(re.test(e.message), `${msg}: wrong error "${e.message}"`); return; }
  throw new Error(`${msg}: did not throw`);
}

console.log(`refresh() guards — ${ALL.length} blocks, tip ${TIP}`);

await test("a normal run stores both keys", async () => {
  const env = fakeKV(ALL.slice(0, -3)); // three blocks behind
  fakeUpstream();
  const r = await refresh(env);
  assert(r.blocks === ALL.length, `expected ${ALL.length} blocks, got ${r.blocks}`);
  assert(env.writes.includes("blocks") && env.writes.includes("galaxy"), `expected both keys written, got ${env.writes}`);
});

await test("nothing new: the block record is not rewritten", async () => {
  const env = fakeKV(ALL);
  const writes = env.writes;
  fakeUpstream();
  const r = await refresh(env);
  assert(r.stored === false, "expected the blocks key to be left alone");
  assert(!writes.includes("blocks"), "rewrote the blocks key with identical content");
  assert(writes.includes("galaxy"), "the built model should still be refreshed");
});

await test("an empty KV read cannot gut the record", async () => {
  const env = fakeKV(null);
  fakeUpstream();
  await throws(() => refresh(env), /cold start/, "empty KV");
  assert(env.store.galaxy === null, "published a galaxy built from a handful of blocks");
});

await test("a gap-riddled record is stored but not published", async () => {
  // A damaged read: every fortieth block, spanning the whole chain. One walk cannot repair it, and
  // a galaxy built from it would show wrong shares for every pool.
  const sparse = ALL.filter((_, i) => i % 40 === 0);
  const env = fakeKV(sparse);
  fakeUpstream();
  await throws(() => refresh(env), /keeping the published model/, "sparse record");
  assert(env.store.galaxy === null, "published a galaxy built from a sparse record");
  assert(env.writes.includes("blocks"), "should still store what it fetched, so the record heals");
});

await test("an explorer behind our tip is refused", async () => {
  const env = fakeKV(ALL);
  fakeUpstream({ tip: TIP - 200 });
  await throws(() => refresh(env), /behind stored/, "lagging explorer");
  assert(env.store.galaxy === null, "published a map missing the recent chain");
});

await test("an unreadable tip is refused", async () => {
  const env = fakeKV(ALL);
  fakeUpstream({ tipStatus: 502 });
  await throws(() => refresh(env), /HTTP 502/, "explorer 502");
});

await test("a hole left by an outage is filled from the top down", async () => {
  // 400 blocks missing below the tip: more than one run's page budget, which is why the old walk
  // (which only looked above what it had) left the hole there for good.
  const hole = new Set();
  for (let h = TIP - 500; h < TIP - 100; h++) hole.add(h);
  let stored = ALL.filter((r) => !hole.has(r.h));
  const before = stored.length;
  fakeUpstream();
  for (let run = 0; run < 3; run++) {
    const env = fakeKV(stored);
    await refresh(env);
    stored = JSON.parse(env.store.blocks);
  }
  assert(stored.length > before, `no blocks recovered (${before} -> ${stored.length})`);
  assert(stored.length === ALL.length, `hole not closed after three runs: ${ALL.length - stored.length} still missing`);
});

console.log(failures ? `\n${failures} failing` : "\nall passing");
process.exit(failures ? 1 : 0);
