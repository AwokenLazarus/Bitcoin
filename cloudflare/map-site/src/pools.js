// What each pool's own public API says about who is on its stratum endpoint.
//
// The chain cannot separate a stratum hasher from a miner on their own node: a split pays both,
// and RATUM gateways write a coinbase with no gateway tag at all. So every row on the naughty
// list that is not a Lazarus session or a custodial block comes from one of these readers.
// Each one fills state.ext[pool].m[address] with what that pool published:
//   at      when we last read it
//   sw, dw  stratum work and gateway work in the pool's payout window
//   hr      live hashrate on the pool's stratum endpoint, GH/s
//   sb, st  newest block the pool says this address found over stratum (height, time)
//   db, dt  newest block it found through a gateway
//   ut      on the pool's miner list with no gateway tag (a hint, not proof)
// Endpoints and fields checked 2026-10-06.

import { ACTIVE_SEC, MEMORY_SEC, buildLists, poolVerdict } from "./naughtylist.js";

const B2 = "https://b2pool.io/api/v1";
const DX = "https://www.dxpool.com/api/address-mining";
const PY = "https://b.pyblock.xyz:8443";

const num = (v) => { const n = Number(v); return Number.isFinite(n) ? n : 0; };
const secs = (iso) => { const t = Date.parse(iso); return Number.isFinite(t) ? Math.floor(t / 1000) : 0; };
const ADDR = /^(bc1[ac-hj-np-z02-9]{11,87}|[13][a-km-zA-HJ-NP-Z1-9]{25,34})$/;
const ok = (a) => typeof a === "string" && ADDR.test(a);
const entry = (x, address) => x.m[address] || (x.m[address] = {});

/**
 * A pool that publishes its whole window: every address with its stratum and gateway work.
 * An address the pool no longer lists has left the window, so its entry goes too, unless it was
 * once on stratum there (the nice list needs to remember that).
 */
function setWindow(x, rows, now) {
  const listed = new Set();
  for (const r of rows) {
    if (!ok(r.address)) continue;
    listed.add(r.address);
    const e = entry(x, r.address);
    e.at = now;
    e.sw = Math.round(num(r.sw));
    e.dw = Math.round(num(r.dw));
    e.hr = Math.round(num(r.hr));
  }
  for (const [address, e] of Object.entries(x.m)) {
    if (listed.has(address)) continue;
    if (e.was && now - (e.at || 0) < MEMORY_SEC) { e.sw = 0; e.dw = 0; e.hr = 0; } else delete x.m[address];
  }
}

function noteFound(e, height, t, stratum) {
  if (stratum) { if (height > (e.sb || 0)) { e.sb = height; e.st = t; } }
  else if (height > (e.db || 0)) { e.db = height; e.dt = t; }
}

/** Oldest-checked first, so a small budget each run still reaches every address in turn. */
function stalest(x, addresses, n) {
  return [...new Set(addresses)].filter(ok)
    .sort((a, b) => ((x.m[a] && x.m[a].chk) || 0) - ((x.m[b] && x.m[b].chk) || 0))
    .slice(0, Math.max(0, n));
}

async function riptide(x, { get, now }) {
  const rows = await get("https://tides.maveth.ca/api/contributors?limit=500&offset=0");
  if (!Array.isArray(rows)) throw new Error("no contributor list");
  setWindow(x, rows.map((r) => ({ address: r.address, sw: r.sv1_work, dw: r.datum_work, hr: num(r.sv1_hashrate_hs) / 1e9 })), now);
}

async function paperclip(x, { get, now }) {
  const d = await get("https://pool.paperclippool.xyz/api/status");
  const rows = d && d.stats && d.stats.window && d.stats.window.miners;
  if (!Array.isArray(rows)) throw new Error("no window");
  setWindow(x, rows.map((r) => {
    const sw = num(r.stratum_work), dw = num(r.datum_work);
    return { address: r.identity, sw, dw, hr: sw + dw ? (num(r.hashrate_hs) / 1e9) * (sw / (sw + dw)) : 0 };
  }), now);
}

async function blockvase(x, { get, now }) {
  const d = await get("https://blockvase.com/api/pool");
  if (!d || !Array.isArray(d.miners)) throw new Error("no miner list");
  // Blockvase closed its stratum port; `public_work` is what is left of it in the window.
  setWindow(x, d.miners.map((r) => ({ address: r.id, sw: r.public_work, dw: r.datum_work, hr: 0 })), now);
}

async function pyblock(x, { get, now }) {
  // The live window masks addresses. The snapshot committed with each found block does not.
  const stats = await get(`${PY}/wavicles_api.php?mode=stats`);
  const last = stats && Array.isArray(stats.blocks) && stats.blocks.find((b) => b && b.snapshot);
  if (!last) throw new Error("no block snapshot");
  if (x.snap === last.snapshot) { for (const e of Object.values(x.m)) if (e.sw || e.dw) e.at = now; return; }
  const snap = await get(`${PY}/wavicles_api.php?mode=snapshot&hash=${encodeURIComponent(last.snapshot)}`);
  const rows = snap && snap.window && snap.window.identities;
  if (!Array.isArray(rows)) throw new Error("snapshot has no identities");
  setWindow(x, rows.map((r) => ({ address: r.identity, sw: r.stratum_work, dw: Math.max(0, num(r.work) - num(r.stratum_work)), hr: 0 })), now);
  x.snap = last.snapshot;
  x.snapHeight = last.height || null;
}

async function convoy(x, { get, now }) {
  const rows = await get("https://convoy.xyz/data/json/blocksfound?range=1w");
  if (!Array.isArray(rows)) throw new Error("no block list");
  // isDatumBlock is true for the pool's own gateway too, so only a false is taken as evidence.
  for (const b of rows) {
    if (!ok(b.solverAddress) || b.isDatumBlock !== false) continue;
    const e = entry(x, b.solverAddress);
    e.at = now;
    noteFound(e, num(b.height), secs(b.time), true);
  }
}

async function xor(x, { getText, now }) {
  const html = await getText("https://www.xorpool.com/miners");
  const rows = html.split("<tr>").slice(1);
  if (!rows.length) throw new Error("no miner table");
  const listed = new Set();
  for (const tr of rows) {
    const m = /href="\/datum\/miner\/([A-Za-z0-9]+)"/.exec(tr);
    if (!m || !ok(m[1]) || !/>pooled</.test(tr)) continue;
    listed.add(m[1]);
    const e = entry(x, m[1]);
    e.at = now;
    e.ut = /title="Gateway tag"/.test(tr) ? 0 : 1;
  }
  for (const address of Object.keys(x.m)) if (!listed.has(address)) delete x.m[address];
}

/**
 * B2Pool labels a block SV1, DATUM or RATUM. Its DATUM label covers two things: a miner's own
 * gateway on 28915, and the pool's hosted gateway on 23333, where the pool still writes the
 * block and the coinbase carries the pool's own tag. The chain tells those apart, so a DATUM
 * block counts as the miner's own only when its coinbase carries someone else's gateway tag.
 * true: the pool built it. false: a miner's node built it. null: cannot tell.
 */
function b2PoolBuilt(proto, height, chainKind) {
  if (proto === "SV1") return true;
  if (proto === "RATUM") return false;
  if (proto !== "DATUM") return null;
  const k = chainKind(height);
  return k === "d" ? false : k ? true : null;
}

async function b2pool(x, { get, now, budget, candidates, chainKind }) {
  const [list, blocks] = await Promise.all([get(`${B2}/miners?limit=100`), get(`${B2}/blocks?limit=500`)]);
  // The pool's own count of how its blocks were found. Split payees are weighed against it.
  if (Array.isArray(blocks)) {
    const w = { s: 0, d: 0 };
    for (const b of blocks) {
      if (now - secs(b.found_at) > ACTIVE_SEC) continue;
      const built = b2PoolBuilt(b.proto, num(b.height), chainKind);
      if (built === true) w.s++; else if (built === false) w.d++;
    }
    x.w = w;
  }
  const live = [];
  for (const m of (list && list.miners) || []) {
    if (!ok(m.address)) continue;
    live.push(m.address);
    const e = entry(x, m.address);
    e.seen = now;
    e.phr = Math.round(num(m.hashrate) / 1e9);
  }
  // blocks_found_list is the only place the pool ties an address to a protocol.
  for (const address of stalest(x, live.concat(candidates), budget - 2)) {
    const e = entry(x, address);
    try {
      const d = await get(`${B2}/miner/${address}`);
      delete e.sb; delete e.st; delete e.db; delete e.dt;
      for (const b of (d && d.blocks_found_list) || []) {
        const built = b2PoolBuilt(b.proto, num(b.height), chainKind);
        if (built != null) noteFound(e, num(b.height), secs(b.found_at), built);
      }
      e.at = now;
    } catch (err) {
      e.miss = (e.miss || 0) + 1;
    }
    e.chk = now;
  }
  for (const [address, e] of Object.entries(x.m)) {
    if (!e.sb && !e.db && now - (e.seen || e.chk || 0) > MEMORY_SEC) delete x.m[address];
  }
}

/** "1NtgNkt...R.11x33" is the first seven characters of `address.worker`; the address starts with them. */
const dxPrefix = (name) => { const p = String(name || "").split("...")[0]; return /^(bc1|[13])[A-Za-z0-9]{4,}$/.test(p) ? p : ""; };

async function dxpool(x, { get, now, budget, known }) {
  // dxpool's `xbt` product is its stratum pool: account names, pool-held balances, one output.
  const d = await get(`${DX}/xbt/blocks?page_size=50`);
  const items = (d && d.items) || [];
  x.finders = items
    .map((b) => ({ h: num(b.height), t: Math.floor(num(b.timestamp) / 1000), name: String(b.miner || "").slice(0, 40) }))
    .filter((b) => b.h && now - b.t <= ACTIVE_SEC);
  x.stratumBlocks = x.finders.length;
  const prefixes = [...new Set(x.finders.map((f) => dxPrefix(f.name)).filter(Boolean))];
  const matching = known.filter((a) => prefixes.some((p) => a.startsWith(p)));
  const positive = Object.keys(x.m).filter((a) => x.m[a].hr > 0 || x.m[a].paid);
  const first = stalest(x, matching.concat(positive), budget - 1);
  const rest = stalest(x, known, Math.min(4, budget - 1 - first.length));
  for (const address of first.concat(rest)) {
    const e = entry(x, address);
    try {
      const info = await get(`${DX}/xbt/miner/${address}/info`);
      e.hr = Math.round(num(info && info.hashrate && info.hashrate.data) / 1e9);
      e.paid = num(info && info.paid) > 0 ? 1 : 0;
      e.at = now;
    } catch (err) {
      e.miss = (e.miss || 0) + 1;
    }
    e.chk = now;
  }
  // Remember which checked addresses are not there, so the rotation moves on, but keep it small.
  for (const [address, e] of Object.entries(x.m)) {
    if (!e.hr && !e.paid && now - (e.chk || 0) > 2 * 86400) delete x.m[address];
  }
}

// `stratum`: does the pool run an endpoint where it writes the block. `reads`: what its API
// gives us, as the page words it.
export const POOLS = [
  { pool: "Lazarus", site: "https://pool.lazarus-xbt.xyz", stratum: "yes",
    reads: "Live sessions from /api/miners and /api/gateways: each address is on stratum or on its own gateway." },
  { pool: "B2Pool", site: "https://b2pool.io", stratum: "yes", every: 0, run: b2pool,
    reads: "Its block list says how each block was found (SV1, DATUM or RATUM), and each miner page lists the blocks that address found. Its DATUM label includes the pool's hosted gateway, where the pool still builds the block and the coinbase carries the pool's own tag, so a DATUM block counts as the miner's own only when the coinbase carries another gateway's tag. An address whose newest found block was built by the pool is confirmed. The rest of the split's payees are suspects, weighed by the pool's share of pool-built blocks." },
  { pool: "dxpool", site: "https://www.dxpool.com", stratum: "yes", every: 0, run: dxpool,
    reads: "Its stratum pool is a separate product with account names and one coinbase output, so its hashers are not in the coinbase. The block list names each finder only in part. Any address can be checked against the stratum product one at a time; a hashrate there is confirmed." },
  { pool: "RIPTIDE", site: "https://tides.maveth.ca", stratum: "yes", every: 900, run: riptide,
    reads: "/api/contributors gives every address in the window with its SV1 work and its DATUM work." },
  { pool: "PaperclipPool", site: "https://pool.paperclippool.xyz", stratum: "yes", every: 900, run: paperclip,
    reads: "/api/status gives every address in the window with its stratum work and its DATUM work." },
  { pool: "Blockvase", site: "https://blockvase.com", stratum: "closed", every: 900, run: blockvase,
    reads: "/api/pool gives every address in the window with its public-stratum work and its DATUM work. The stratum port is closed, so stratum work there is what is left in the window." },
  { pool: "PyBLOCK", site: "https://b.pyblock.xyz:8443", stratum: "yes", every: 900, run: pyblock,
    reads: "The live window masks addresses. The snapshot committed with each found block lists every address with its stratum work, so the list is as of the pool's last block." },
  { pool: "CONVOY", site: "https://convoy.xyz", stratum: "yes", every: 900, run: convoy,
    reads: "The found-block list names the solver and whether the block came through DATUM. Only the finder of a non-DATUM block is confirmed; nothing separates the other payees." },
  { pool: "Bitcoin Xor", site: "https://www.xorpool.com", stratum: "yes", every: 900, run: xor,
    reads: "The miner page shows a gateway tag beside an address that comes through a tagged gateway. An address with no tag is a suspect: an untagged gateway looks the same." },
  { pool: "AlphaPool", site: "https://xbt.alphapool.tech", stratum: "no",
    reads: "Its stratum pool stopped at block 973964. What is left is DATUM only. Its API does not tie an address to a gateway, so many addresses behind one hosted gateway cannot be told apart." },
  { pool: "Omega Pool", site: "https://omegapool.tech", stratum: "no",
    reads: "DATUM only: its connect page says a miner pointed at the pool's port does not work. Most of its blocks carry no gateway tag because RATUM gateways do not write one." },
  { pool: "Rabid Pool", site: "https://pool.rabidmining.com", stratum: "no",
    reads: "DATUM only for this chain: its summary reports no stratum endpoint." },
  { pool: "Mining-Dutch", site: "https://www.mining-dutch.nl/", stratum: "only",
    reads: "Stratum only, with accounts and wallet payouts. Its API has pool totals and no addresses, so only the pool's own coinbase address can be listed." },
  { pool: "CTRL", site: "https://ctrlpool.com", stratum: "yes",
    reads: "ctrlpool, formerly xbtpool. It mines through another pool's DATUM window, hides its miner list, and answers for one address at a time with no protocol field." },
  { pool: "Pow.re", site: "https://pow.re", stratum: "unknown",
    reads: "A hashrate desk with no pool pages or API. Its blocks pay one or two outputs." },
];

export const poolInfo = (name) => POOLS.find((p) => p.pool === name) || null;

/**
 * Run the readers that are due. Each gets its own slice of the request budget and fails alone:
 * a pool that does not answer keeps what it said last time, and the page says when that was.
 */
export async function refreshPools(state, { get, getText, now, budget = 16 }) {
  const ext = state.ext || (state.ext = {});
  const known = Object.keys(state.addrs || {});
  const due = POOLS.filter((p) => p.run && now - ((ext[p.pool] && ext[p.pool].tried) || 0) >= (p.every || 0));
  const rotating = due.filter((p) => !p.every).length || 1;
  const report = {};
  const chainKind = (h) => (state.blk && state.blk[h] ? state.blk[h][2] : null);
  await Promise.all(due.map(async (p) => {
    const x = ext[p.pool] || (ext[p.pool] = { m: {} });
    if (!x.m) x.m = {};
    const candidates = known.filter((a) => state.addrs[a].ps && state.addrs[a].ps[p.pool]);
    try {
      await p.run(x, { get, getText, now, budget: Math.floor(budget / rotating), candidates, known, chainKind });
      x.ok = now;
      x.err = "";
      report[p.pool] = Object.keys(x.m).length;
    } catch (e) {
      x.err = String((e && e.message) || e).slice(0, 140);
      report[p.pool] = `failed: ${x.err}`;
    }
    x.tried = now;
  }));
  return report;
}

/** One address, asked of the pools that answer per address. For the lookup page. */
export async function askPools(address, { get, now, chainKind = () => null }) {
  const out = [];
  await Promise.all([
    (async () => {
      const d = await get(`${B2}/miner/${address}`);
      if (!d || d.found === false) return;
      const e = {};
      for (const b of d.blocks_found_list || []) {
        const built = b2PoolBuilt(b.proto, num(b.height), chainKind);
        if (built != null) noteFound(e, num(b.height), secs(b.found_at), built);
      }
      const hr = Math.round(num(d.hashrate && d.hashrate.h1) / 1e9);
      out.push({ pool: "B2Pool", hrGhs: hr, lastShare: secs(d.last_share_at), stratumBlock: e.sb || null, gatewayBlock: e.db || null, blocksFound: num(d.blocks_found) });
    })().catch(() => {}),
    (async () => {
      const d = await get(`${DX}/xbt/miner/${address}/info`);
      const hr = Math.round(num(d && d.hashrate && d.hashrate.data) / 1e9);
      const paid = num(d && d.paid);
      if (hr > 0 || paid > 0) out.push({ pool: "dxpool", hrGhs: hr, paidXbt: paid, stratumProduct: true, at: now });
    })().catch(() => {}),
  ]);
  return out;
}

/**
 * The document the page and /api/naughtylist read: the three lists, and one row per pool. A pool
 * is in the table when we have checked it or when it has three blocks in the window, so a pool
 * nobody has looked at yet still shows up, marked not checked.
 */
export function buildView(state, miners, gateways, now) {
  const lists = buildLists(state, miners, now, gateways);
  const ext = state.ext || {};
  const count = (rows) => { const t = {}; for (const r of rows) t[r.pool] = (t[r.pool] || 0) + 1; return t; };
  const confirmed = count(lists.naughty), suspected = count(lists.suspects);
  const names = new Set(POOLS.map((p) => p.pool));
  // A lone miner's own gateway also tags its coinbase; only something that pays a split, or
  // takes blocks whole, is a pool.
  for (const w of Object.values(lists.windows)) if (w.blocks >= 3 && w.pooled) names.add(w.pool);
  const pools = [...names].map((name) => {
    const info = poolInfo(name) || { pool: name, stratum: "unknown" };
    const w = lists.windows[name] || { s: 0, d: 0, c: 0, blocks: 0 };
    const x = ext[name];
    return {
      pool: name, site: info.site || null, stratum: info.stratum, reads: info.reads || null,
      reader: !!info.run || name === "Lazarus",
      blocks: w.blocks, s: w.s, d: w.d, c: w.c,
      own: x && x.w ? x.w : x && x.stratumBlocks != null ? { s: x.stratumBlocks, d: null } : null,
      confirmed: confirmed[name] || 0, suspects: suspected[name] || 0,
      ok: name === "Lazarus" ? (miners && miners.length ? now : null) : (x && x.ok) || null,
      err: (x && x.err) || "",
    };
  }).sort((a, b) => b.confirmed + b.suspects - (a.confirmed + a.suspects) || b.blocks - a.blocks || a.pool.localeCompare(b.pool));
  return {
    asOf: now,
    tip: state.tip || null,
    scannedTo: state.scannedTo || null,
    windowDone: !!state.windowDone,
    poolWalletsHidden: lists.poolWallets,
    naughty: lists.naughty,
    suspects: lists.suspects,
    nice: lists.nice,
    pools,
    finders: ((ext.dxpool && ext.dxpool.finders) || []).slice(0, 60),
  };
}

/** What each pool's stored data says about one address, in words, for the lookup page. */
export function storedEvidence(state, address, now) {
  const out = [];
  for (const [pool, x] of Object.entries(state.ext || {})) {
    const e = x && x.m && x.m[address];
    if (!e) continue;
    const v = poolVerdict(e, now);
    const sw = e.sw || 0, dw = e.dw || 0;
    let text = "";
    if (sw + dw > 0) text = `${Math.round((sw / (sw + dw)) * 1000) / 10}% of its work in the pool's window is stratum, the rest through a gateway.`;
    else if (e.sb || e.db) text = [e.sb ? `Newest block found over stratum: ${e.sb}.` : "", e.db ? `Newest block found through a gateway: ${e.db}.` : ""].filter(Boolean).join(" ");
    else if (e.ut) text = "On the pool's miner list with no gateway tag.";
    else if (e.ut === 0) text = "On the pool's miner list with a gateway tag.";
    else if (e.hr > 0) text = "Hashing on the pool's stratum product.";
    else continue;
    out.push({ pool, verdict: v ? v.verdict : null, text });
  }
  return out;
}
