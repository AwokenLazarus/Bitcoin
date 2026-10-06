// The naughty list is money-facing in public: a pool wallet must not be named as a hasher, a
// DATUM payee must not stay on the naughty list, and a live stratum worker must.
//
//   node scripts/test-naughtylist.mjs
import { addressPays, applyHosted, blocksMissingKey, buildLists, classifyCoinbase, emptyState, gatewayKey, hostedWindows, setGatewayKey, migrate, noteBlock, observeLive, outputsOf, poolVerdict, rememberStratum, validAddress } from "../src/naughtylist.js";
import { buildView, refreshPools } from "../src/pools.js";
import { renderAddress, renderList } from "../src/naughtypage.js";

let failures = 0;
const assert = (cond, msg) => { if (!cond) throw new Error(msg); };
async function test(name, fn) {
  try { await fn(); console.log(`  ok   ${name}`); }
  catch (e) { failures++; console.log(`  FAIL ${name}\n       ${e.message}`); }
}

// Second push is the tag. A DATUM tag is `<primary> 0x0F <secondary> 0x00`.
const hex = (bytes) => [...bytes].map((b) => b.toString(16).padStart(2, "0")).join("");
const push = (bytes) => [bytes.length, ...bytes];
const ascii = (s) => [...s].map((c) => c.charCodeAt(0));
const stratumScript = hex([0x03, 0x01, 0x00, 0x00, ...push(ascii("Lazarus"))]);
const datumScript = hex([0x03, 0x01, 0x00, 0x00, ...push([...ascii("Lazarus"), 0x0f, ...ascii("MinerOne"), 0x00])]);

const outs = (n, prefix) => Array.from({ length: n }, (_, i) => ({ address: `${prefix}${i}`, vout: i, sats: 1000 + i }));

console.log("naughty list");

await test("a pool-built split is stratum and a gateway tag is datum", async () => {
  assert(classifyCoinbase(stratumScript, 40).kind === "stratum", "split was not stratum");
  assert(classifyCoinbase(datumScript, 40).kind === "datum", "gateway tag was not datum");
  const soloScript = hex([0x03, 0x01, 0x00, 0x00, ...push(ascii("Legata"))]);
  assert(classifyCoinbase(soloScript, 1).kind === "skip", "a solo one-output coinbase was treated as a pool");
  assert(classifyCoinbase(stratumScript, 1).kind === "custodial", "a known pool's one-output block was not custodial");
  const self = hex([0x03, 0x01, 0x00, 0x00, ...push([...ascii("Lazarus"), 0x0f, ...ascii("Lazarus"), 0x00])]);
  assert(classifyCoinbase(self, 40).kind === "stratum", "a secondary tag naming the pool was counted as a gateway");
  const b2 = "03b2e30e0004f559c46a04511d7e050c000000000000000000000000142f6d696e6564206f6e204232506f6f6c2e696f2f";
  const named = classifyCoinbase(b2, 139, "B2Pool");
  assert(named.kind === "stratum" && named.pool === "B2Pool", `B2Pool script classified as ${named.kind} ${named.pool}`);
});

await test("a DATUM-majority pool's split does not list every payee as a stratum hasher", async () => {
  const now = 1_800_000_000;
  const state = emptyState();
  noteBlock(state, { h: 100, t: now - 86400, id: "aa", txid: "tx1", script: stratumScript, outputs: outs(6, "bc1q") });
  for (let h = 101; h <= 104; h++) {
    noteBlock(state, { h, t: now - 3600, id: "d" + h, txid: "td" + h, script: datumScript, outputs: outs(6, "gw") });
  }
  const mixed = buildLists(state, [], now);
  assert(!mixed.naughty.some((r) => r.address === "bc1q0"), "a DATUM pool's window payee was called a stratum hasher");
});

await test("a custodial pool block puts its address on the naughty list", async () => {
  const now = 1_800_000_000;
  const state = emptyState();
  noteBlock(state, { h: 50, t: now - 100, id: "c1", txid: "tc", script: stratumScript, explorerPool: "B2Pool", outputs: [{ address: "poolwallet", vout: 0, sats: 3_000_000 }] });
  const { naughty } = buildLists(state, [], now);
  assert(naughty.some((r) => r.address === "poolwallet" && r.why === "custodial-block"), "custodial pool address missing");
});

await test("an address on its own DATUM node is not naughty", async () => {
  const now = 1_800_000_000;
  const state = emptyState();
  noteBlock(state, { h: 50, t: now - 100, id: "s1", txid: "ts", script: stratumScript, outputs: outs(6, "m") });
  const miners = [{ address: "m0", online: true, fee_path: "datum", via: "prime", hr_ghs: 8, gateway: "abc" }];
  const gateways = [{ identity: "m0", fee_path: "datum", offline: false, own: false }];
  const { naughty } = buildLists(state, miners, now, gateways);
  assert(!naughty.some((r) => r.address === "m0"), "own-node DATUM address stayed naughty");
  assert(!naughty.some((r) => r.address === "m1"), "a split payee was classified from the chain");
});

await test("a Lazarus stratum session that moves to DATUM joins the nice list", async () => {
  const now = 1_800_000_000;
  const state = emptyState();
  observeLive(state, [{ address: "bc1qconverted", online: true, fee_path: "stratum", via: "stratum", hr_ghs: 5 }], now - 86400);
  const before = buildLists(state, [{ address: "bc1qconverted", online: true, fee_path: "stratum", via: "stratum", hr_ghs: 5 }], now);
  assert(before.naughty.some((r) => r.address === "bc1qconverted"), "live stratum missing");
  const after = buildLists(state, [{ address: "bc1qconverted", online: true, fee_path: "datum", via: "prime", hr_ghs: 5 }], now);
  assert(!after.naughty.some((r) => r.address === "bc1qconverted"), "converted session still naughty");
  assert(after.nice.some((r) => r.address === "bc1qconverted"), "converted session missing from the nice list");
});

await test("a live stratum worker stays naughty even after a DATUM coinbase", async () => {
  const now = 1_800_000_000;
  const state = emptyState();
  noteBlock(state, { h: 100, t: now - 86400, id: "aa", txid: "tx1", script: stratumScript, outputs: outs(6, "bc1q") });
  noteBlock(state, { h: 110, t: now - 60, id: "bb", txid: "tx2", script: datumScript, outputs: [{ address: "bc1q0", vout: 1, sats: 1 }] });
  const { naughty } = buildLists(state, [{ address: "bc1q0", online: true, fee_path: "stratum", via: "stratum", hr_ghs: 10 }], now);
  assert(naughty.some((r) => r.address === "bc1q0" && r.live === "stratum"), "live stratum worker was dropped");
});

await test("a live stratum address with no coinbase yet is still listed", async () => {
  const { naughty } = buildLists(emptyState(), [{ address: "bc1qexampleaddress000", online: true, fee_path: "stratum", via: "stratum", hr_ghs: 3 }], 1_800_000_000);
  // bc1qexampleaddress000 may fail the address shape used on the page; the list itself only needs a string.
  assert(naughty.length === 1 && naughty[0].hrGhs === 3, `expected one live hasher, got ${naughty.length}`);
});

await test("the address paid on every pool-built block is the pool wallet", async () => {
  const now = 1_800_000_000;
  const state = emptyState();
  for (let h = 1; h <= 10; h++) {
    const outputs = [{ address: "poolwallet", vout: 0, sats: 100 }, ...outs(5, "m" + h)];
    noteBlock(state, { h, t: now - 1000 + h, id: "id" + h, txid: "t" + h, script: stratumScript, outputs });
  }
  const { naughty } = buildLists(state, [], now);
  assert(naughty.length === 0, "split payees were classified from the chain");
});

await test("the same block id is not counted twice", async () => {
  const state = emptyState();
  const block = { h: 5, t: 10, id: "same", txid: "t", script: stratumScript, outputs: outs(6, "z") };
  noteBlock(state, block);
  noteBlock(state, block);
  assert(state.addrs.z0.sn === 1, `stratum count ${state.addrs.z0.sn}`);
});

await test("coinbase outputs keep the address and the value", async () => {
  const outs2 = outputsOf({ txid: "ab", vout: [{ scriptpubkey_address: "bc1qxx", value: 42, n: 4 }, { scriptpubkey_address: "bc1qyy", value: 0 }] });
  assert(outs2.length === 1 && outs2[0].vout === 4 && outs2[0].sats === 42, JSON.stringify(outs2));
  assert(validAddress("bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4"), "bech32 rejected");
  assert(!validAddress("not an address"), "junk accepted");
});


// ---- pools other than Lazarus: only what a pool's own API says puts an address on the list ----
const A1 = "bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4";
const A2 = "1BvBMSEYstWetqTFn5Au4m4GFg7xJaNVN2";
const A3 = "bc1qar0srrr7xfkvy5l643lydnw9re59gtzzwf5mdq";
const NOW = 1_800_000_000;
const plainScript = (name) => hex([0x03, 0x01, 0x00, 0x00, ...push(ascii(name))]);

await test("an untagged split alone never classifies a payee (RATUM gateways write no tag)", async () => {
  const state = emptyState();
  for (let h = 1; h <= 30; h++) noteBlock(state, { h, t: NOW - 1000 + h, id: "o" + h, txid: "t" + h, script: plainScript("OmegaPool"), outputs: outs(6, "om" + (h % 3)) });
  const { naughty, suspects } = buildLists(state, [], NOW);
  assert(naughty.length === 0 && suspects.length === 0, `classified ${naughty.length} + ${suspects.length} from the chain alone`);
});

await test("stratum work in a pool's own window is confirmed, and a trace of it is not", async () => {
  const state = emptyState();
  state.ext.RIPTIDE = { m: {
    [A1]: { at: NOW - 60, sw: 900, dw: 100, hr: 70 },
    [A2]: { at: NOW - 60, sw: 88, dw: 22_000_000_000, hr: 0 },
    [A3]: { at: NOW - 60, sw: 0, dw: 500, hr: 0 },
  } };
  const { naughty, suspects } = buildLists(state, [], NOW);
  const hit = naughty.find((r) => r.address === A1);
  assert(hit && hit.pool === "RIPTIDE" && hit.why === "pool-stratum-work" && hit.stratumPct === 90 && hit.hrGhs === 70, JSON.stringify(hit));
  assert(!naughty.some((r) => r.address === A2), "88 units of stratum work beside 22 G of gateway work was called a stratum hasher");
  assert(!naughty.some((r) => r.address === A3) && suspects.length === 0, "a gateway-only address was listed");
});

await test("what a pool said goes stale: an old reading confirms nothing", async () => {
  assert(poolVerdict({ at: NOW - 60, sw: 5, dw: 0 }, NOW).verdict === "stratum", "fresh stratum work");
  assert(poolVerdict({ at: NOW - 7 * 3600, sw: 5, dw: 0 }, NOW) === null, "a seven-hour-old reading still confirmed");
  assert(poolVerdict({ sb: 100, st: NOW - 15 * 86400 }, NOW) === null, "a found block older than the window still confirmed");
  assert(poolVerdict({ sb: 100, st: NOW - 86400, db: 120, dt: NOW - 3600 }, NOW).verdict === "own-node", "a newer gateway block did not clear it");
});

await test("an address a pool showed on stratum and now shows on a gateway joins the nice list", async () => {
  const state = emptyState();
  state.ext.PaperclipPool = { m: { [A1]: { at: NOW - 60, sw: 10, dw: 0 } } };
  rememberStratum(state, NOW);
  assert(buildLists(state, [], NOW).naughty.length === 1, "not naughty first");
  state.ext.PaperclipPool.m[A1] = { ...state.ext.PaperclipPool.m[A1], sw: 0, dw: 10, at: NOW + 600 };
  const after = buildLists(state, [], NOW + 660);
  assert(after.naughty.length === 0 && after.nice.length === 1 && after.nice[0].pool === "PaperclipPool", JSON.stringify(after.nice));
});

await test("B2Pool: SV1 and hosted-gateway blocks are pool-built, a foreign gateway tag is the miner's own", async () => {
  const state = emptyState();
  // 101 the pool's own tag on a DATUM-layout coinbase, 102 another gateway's tag.
  const own = hex([0x03, 0x01, 0x00, 0x00, ...push([...ascii("B2Pool"), 0x0f, ...ascii("b2pool.io"), 0x00])]);
  const foreign = hex([0x03, 0x01, 0x00, 0x00, ...push([...ascii("B2Pool"), 0x0f, ...ascii("MinerOne"), 0x00])]);
  noteBlock(state, { h: 100, t: NOW - 500, id: "a", txid: "ta", script: plainScript("B2Pool"), outputs: [...outs(5, "x"), { address: A3, vout: 9, sats: 5 }] });
  noteBlock(state, { h: 101, t: NOW - 400, id: "b", txid: "tb", script: own, outputs: outs(6, "x") });
  noteBlock(state, { h: 102, t: NOW - 300, id: "c", txid: "tc", script: foreign, outputs: outs(6, "x") });
  const iso = (t) => new Date(t * 1000).toISOString();
  const blocks = [
    ...[100, 90, 91, 92, 93].map((height) => ({ height, proto: "SV1", found_at: iso(NOW - 500) })),
    { height: 101, proto: "DATUM", found_at: iso(NOW - 400) },
    { height: 102, proto: "DATUM", found_at: iso(NOW - 300) },
  ];
  const miner = { [A1]: [{ height: 101, proto: "DATUM", found_at: iso(NOW - 400) }], [A2]: [{ height: 100, proto: "SV1", found_at: iso(NOW - 500) }, { height: 102, proto: "DATUM", found_at: iso(NOW - 300) }] };
  const get = async (url) => {
    if (url.includes("/miners?")) return { miners: [{ address: A1, hashrate: 5e12 }, { address: A2, hashrate: 1e12 }] };
    if (url.includes("/blocks?")) return blocks;
    const a = url.split("/miner/")[1];
    if (url.includes("b2pool.io") && a) return { found: true, blocks_found_list: miner[a] || [] };
    throw new Error("not this pool");
  };
  await refreshPools(state, { get, getText: async () => { throw new Error("no"); }, now: NOW, budget: 40 });
  assert(state.ext.B2Pool.w.s === 6 && state.ext.B2Pool.w.d === 1, JSON.stringify(state.ext.B2Pool.w));
  const { naughty, suspects } = buildLists(state, [], NOW);
  const hosted = naughty.find((r) => r.address === A1);
  assert(hosted && hosted.why === "pool-stratum-block" && hosted.evidenceHeight === 101 && hosted.hrGhs === 5000, `hosted-gateway finder: ${JSON.stringify(hosted)}`);
  assert(!naughty.some((r) => r.address === A2), "a miner whose newest block came through their own gateway stayed naughty");
  const payee = suspects.find((r) => r.address === A3);
  assert(payee && payee.why === "split-payee" && payee.pool === "B2Pool" && payee.share === 85.7, `split payee: ${JSON.stringify(payee)}`);
  assert(!suspects.some((r) => r.address === A2), "an address the pool cleared was still a suspect");
});

await test("one pool failing leaves the others read, and says so", async () => {
  const state = emptyState();
  const get = async (url) => {
    if (url.includes("tides.maveth.ca")) return [{ address: A1, sv1_work: 10, datum_work: 0, sv1_hashrate_hs: 3e9 }];
    throw new Error("HTTP 503");
  };
  const report = await refreshPools(state, { get, getText: get, now: NOW, budget: 16 });
  assert(report.RIPTIDE === 1 && /failed/.test(report.B2Pool), JSON.stringify(report));
  assert(state.ext.B2Pool.err && !state.ext.RIPTIDE.err && state.ext.RIPTIDE.ok === NOW, "ok/err not recorded");
  assert(buildLists(state, [], NOW).naughty.length === 1, "the pool that answered was not used");
});

await test("a v1 document gains its block index from the payouts it remembers", async () => {
  const state = emptyState();
  noteBlock(state, { h: 7, t: NOW - 100, id: "s", txid: "t", script: stratumScript, outputs: outs(6, "m") });
  delete state.blk; delete state.ext; state.v = 1;
  migrate(state);
  assert(state.v === 2 && state.blk[7] && state.blk[7][1] === "Lazarus" && state.blk[7][2] === "s", JSON.stringify(state.blk));
});

await test("the page is whole without a script, and escapes what pools and coinbases say", async () => {
  const state = emptyState();
  const evil = '<img src=x onerror=alert(1)>';
  for (let h = 1; h <= 4; h++) noteBlock(state, { h, t: NOW - 100, id: "e" + h, txid: "t" + h, script: plainScript(evil), outputs: outs(6, "p") });
  state.ext.RIPTIDE = { ok: NOW, m: { [A1]: { at: NOW - 60, sw: 9, dw: 1, hr: 2500 } } };
  const view = buildView(state, [{ address: A2, online: true, fee_path: "stratum", via: "stratum", hr_ghs: 12 }], [], NOW);
  assert(view.pools.some((p) => p.pool === evil && p.stratum === "unknown"), "a pool nobody has checked was left out of the table");
  const html = renderList(view);
  assert(!html.includes("<img src=x"), "a coinbase tag reached the page unescaped");
  assert(html.includes(`/naughtylist/?a=${A1}`) && html.includes("2.50 TH/s") && html.includes("90% of its work"), "the RIPTIDE row is missing from the HTML");
  assert(html.includes(`/naughtylist/?a=${A2}`) && html.includes("Live session"), "the Lazarus row is missing from the HTML");
  assert(/<form[^>]+method="get"[^>]+action="\/naughtylist\/"/.test(html), "no lookup form");
  const scripts = html.match(/<script\b[^>]*>/g) || [];
  assert(scripts.every((t) => /\bsrc="\/assets\//.test(t)), `inline script on the page: ${scripts.join(" ")}`);
  assert(!/Loading|needs JavaScript/i.test(html), "the page still waits for a script");
  const one = renderList(view, { pool: "RIPTIDE" });
  assert(one.includes(A1) && !one.includes(`?a=${A2}`), "the pool filter did not filter");
  assert(renderList(view, { pool: '"><script>' }).includes(A2), "an unknown pool name was not ignored");
  const bad = renderAddress({ error: "that is not an address", address: '"><script>alert(1)</script>' });
  assert(!bad.includes("<script>alert"), "the address field is not escaped");
  const page = renderAddress({ address: A1, listed: view.naughty.find((r) => r.address === A1), stored: [], asked: [{ pool: "dxpool", hrGhs: 0, paidXbt: 1.5 }], pays: [{ height: 5, ts: NOW, sats: 1e8, pool: evil, kind: "stratum", unspent: true }], unspentSats: 1e8, unspentCount: 1, explorer: "https://mempool.lazarus-xbt.xyz/address/" + A1 });
  assert(page.includes("Confirmed on stratum at RIPTIDE") && page.includes("1 XBT") && !page.includes("<img src=x"), "address page");
});

// A pool that runs the gateways itself: the tag says DATUM, the watcher's proof says otherwise.
const gw = (pool, name) => hex([0x03, 0x01, 0x00, 0x00, ...push([...ascii(pool), 0x0f, ...ascii(name), 0x00])]);
const HOSTED = { v: 1, tags: { [gatewayKey("AlphaPool", "Soggy Waffle")]: { n: 3 } }, blocks: { 950: gatewayKey("AlphaPool", "DATUM User") } };

await test("a block under a gateway name proved to be the pool's own is the pool's stratum", async () => {
  const soggy = gw("AlphaPool", "Soggy Waffle"), own = gw("AlphaPool", "nine009"), generic = gw("AlphaPool", "DATUM User");
  assert(classifyCoinbase(soggy, 160).kind === "datum", "with nothing proved a gateway tag is DATUM");
  const c = classifyCoinbase(soggy, 160, "", HOSTED, 900);
  assert(c.kind === "stratum" && c.hosted && c.pool === "AlphaPool", "a proved name was still DATUM");
  assert(classifyCoinbase(own, 160, "", HOSTED, 901).kind === "datum", "another gateway on the same pool was swept up");
  assert(classifyCoinbase(gw("Lazarus", "Soggy Waffle"), 100, "", HOSTED, 902).kind === "datum", "the same name on another pool was swept up");
  // a stock name proves the block it was seen on and nothing about the name
  assert(classifyCoinbase(generic, 160, "", HOSTED, 950).hosted === true, "a proved block was not listed");
  assert(classifyCoinbase(generic, 160, "", HOSTED, 951).kind === "datum", "a stock name was treated as proved");
});

await test("payees of pool-run gateway blocks are suspects once those are most of the pool's gateway blocks", async () => {
  const now = 2_000_000_000, state = emptyState();
  const soggy = gw("AlphaPool", "Soggy Waffle"), own = gw("AlphaPool", "nine009");
  for (let h = 1; h <= 6; h++) noteBlock(state, { h, t: now - 1000 + h, id: "s" + h, txid: "t" + h, script: soggy, outputs: [...outs(5, "hasher"), { address: "poolwallet", vout: 9, sats: 5 }] }, HOSTED);
  for (let h = 7; h <= 8; h++) noteBlock(state, { h, t: now - 500 + h, id: "o" + h, txid: "t" + h, script: own, outputs: outs(2, "hasher") }, HOSTED);
  const w = hostedWindows(state, now).AlphaPool;
  assert(w.hostedBlocks === 6 && w.otherGatewayBlocks === 2 && w.share === 75 && w.names[0] === "Soggy Waffle", JSON.stringify(w));
  const lists = buildLists(state, [], now, []);
  const row = lists.suspects.find((r) => r.address === "hasher3");
  assert(row && row.why === "hosted-gateway" && row.pool === "AlphaPool" && row.paidBlocks === 6, JSON.stringify(row));
  assert(!lists.naughty.some((r) => r.pool === "AlphaPool"), "a split payee is not proof of anything");
  assert(!lists.nice.some((r) => r.address === "hasher3"), "a pool-run gateway block made someone nice");
  const html = renderList(buildView(state, [], [], now));
  assert(html.includes("built on gateways the pool runs itself (Soggy Waffle)") && html.includes("6 of 8"), "the page does not say what the row rests on");
  assert(html.includes("6 of its 8 gateway blocks (75%) were built on gateways it runs itself, under the names Soggy Waffle."), "the pool's own row does not say what was proved");
});

await test("a name proved later re-judges the blocks already noted, once", async () => {
  const now = 2_000_000_000, state = emptyState();
  const soggy = gw("AlphaPool", "Soggy Waffle"), own = gw("AlphaPool", "nine009");
  for (let h = 1; h <= 6; h++) noteBlock(state, { h, t: now - 1000 + h, id: "s" + h, txid: "t" + h, script: soggy, outputs: outs(5, "hasher") });
  noteBlock(state, { h: 7, t: now - 100, id: "o7", txid: "t7", script: own, outputs: [{ address: "hasher0", vout: 0, sats: 1 }, { address: "owner", vout: 1, sats: 1 }] });
  assert(state.pools.AlphaPool.d === 7 && state.addrs.hasher1.dn === 6 && state.addrs.hasher1.sn === 0, "setup");
  assert(!buildLists(state, [], now, []).suspects.length, "nothing is proved yet");
  assert(applyHosted(state, HOSTED) === 6, "six blocks should move");
  assert(applyHosted(state, HOSTED) === 0, "and only once");
  assert(state.pools.AlphaPool.d === 1 && state.pools.AlphaPool.s === 6, JSON.stringify(state.pools));
  const a = state.addrs.hasher1;
  assert(a.dn === 0 && a.sn === 6 && a.dh === 0 && a.sh === 6 && a.ps.AlphaPool === 6 && a.pays.every((p) => p.k === "s" && p.g === 1), JSON.stringify(a));
  // paid by a real gateway's block too: that payout is still its newest DATUM one
  assert(state.addrs.hasher0.dh === 7 && state.addrs.hasher0.dn === 1, JSON.stringify(state.addrs.hasher0));
  assert(state.addrs.owner.dn === 1 && !state.addrs.owner.sn, "the owner of a real gateway was touched");
  assert(buildLists(state, [], now, []).suspects.some((r) => r.address === "hasher1" && r.why === "hosted-gateway"), "the re-judged payees are not listed");
});

await test("blocks noted before the gateway name was kept are read again", async () => {
  const now = 2_000_000_000, state = emptyState();
  const soggy = gw("AlphaPool", "Soggy Waffle");
  for (let h = 1; h <= 3; h++) noteBlock(state, { h, t: now - 100, id: "id" + h, txid: "t" + h, script: soggy, outputs: outs(5, "hasher") });
  for (const e of Object.values(state.blk)) e.length = 4; // as an older Worker stored them
  assert(applyHosted(state, HOSTED) === 0, "a block with no name on file cannot be judged");
  const todo = blocksMissingKey(state, 2);
  assert(todo.length === 2 && todo[0][0] === 3 && todo[0][1] === "id3", JSON.stringify(todo));
  for (const [h] of blocksMissingKey(state, 9)) setGatewayKey(state, h, soggy);
  assert(!blocksMissingKey(state, 9).length && applyHosted(state, HOSTED) === 3, "the names were not kept");
});


await test("an address behind another pool's gateway on Lazarus is confirmed, and its Lazarus payouts are marked", async () => {
  const now = 2_000_000_000, state = emptyState();
  const A = "bc1qbehindctrl", B = "bc1qhonest";
  const first = now - 5000;
  noteBlock(state, { h: 10, t: first - 100, id: "b10", txid: "t10", script: datumScript, outputs: [{ address: A, vout: 0, sats: 700 }, { address: B, vout: 1, sats: 900 }] });
  noteBlock(state, { h: 11, t: now - 600, id: "b11", txid: "t11", script: datumScript, outputs: [{ address: A, vout: 0, sats: 800 }, { address: B, vout: 1, sats: 900 }] });
  state.via = { gateways: { "097b7017ccfd7669": { pool: "CTRL" } }, addrs: { [A]: { pool: "CTRL", gateway: "097b7017ccfd7669", first, last: now - 60 }, bc1qneverpaid: { pool: "CTRL", gateway: "097b7017ccfd7669", first, last: now - 60 }, bc1qlonggone: { pool: "CTRL", gateway: "x", first: 1, last: now - 20 * 86400 } } };
  // our own pool shows both as DATUM miners on a gateway, which is exactly the disguise
  const miners = [A, B].map((address) => ({ address, online: true, fee_path: "datum", via: "prime", hr_ghs: 500 }));
  const gateways = [{ identity: A, gateway: "097b7017ccfd7669", fee_path: "datum" }, { identity: B, gateway: "aa", fee_path: "datum" }];
  const lists = buildLists(state, miners, now, gateways);
  const row = lists.naughty.find((r) => r.address === A);
  assert(row && row.pool === "CTRL" && row.why === "behind-pool-gateway" && row.live === "stratum" && row.hrGhs === 500, JSON.stringify(row));
  assert(lists.naughty.some((r) => r.address === "bc1qneverpaid" && r.pool === "CTRL"), "an address with no payout yet was left off");
  assert(!lists.naughty.some((r) => r.address === "bc1qlonggone"), "an address last seen weeks ago is still listed");
  assert(![...lists.naughty, ...lists.suspects, ...lists.nice].some((r) => r.address === B), "an honest gateway owner was listed");
  const pays = addressPays(state, A, [{ txid: "t11", vout: 0 }]);
  assert(pays[0].height === 11 && pays[0].via === "CTRL" && pays[0].unspent && pays[1].via === null, JSON.stringify(pays));
  // the gateway was connected before this address was first noticed behind it
  state.via.gateways["097b7017ccfd7669"].first = first - 1000;
  assert(addressPays(state, A, []).every((p) => p.via === "CTRL"), "an output paid after the gateway connected was not marked");
  state.via.gateways["097b7017ccfd7669"].first = first;
  assert(addressPays(state, B, []).every((p) => p.via === null), "an honest address's payouts were marked");
  const view = buildView(state, miners, gateways, now);
  const html = renderList(view);
  assert(/Hashing on CTRL(&#39;|&#x27;|')s stratum/.test(html) && html.includes("gateway 097b7017ccfd7669"), "the list does not say what the row rests on");
  const page = renderAddress({ address: A, listed: view.naughty.find((r) => r.address === A), stored: [], asked: [], pays, unspentCount: 1, unspentSats: 800 });
  assert(page.includes("Confirmed on stratum at CTRL") && page.includes("Earned on CTRL's stratum, behind its gateway on Lazarus"), "the address page does not mark the outputs");
});

console.log(failures ? `\n${failures} failing` : "\nall passing");
process.exit(failures ? 1 : 0);
