// Naughty list for lazarus-xbt.xyz/naughtylist.
//
// A stratum hasher lets the pool write the block. A DATUM miner writes it on their own node and
// the pool only splits the coinbase. The chain shows which one happened: a DATUM gateway puts
// `<primary> 0x0F <secondary> 0x00` in the coinbase (see galaxy.js). A secondary tag that names
// the pool itself is the pool's own stratum, the same rule the galaxy map uses.
//
// An address is on the naughty list while it is still being paid by those pool-built splits, or
// while Lazarus's public API shows it hashing through stratum. It moves to the nice list when a
// later DATUM coinbase pays it (or Lazarus shows it only on a gateway) and no stratum worker is
// still online. Someone who only ever mined DATUM is on neither list: the nice list is converts.
//
// A pool can also run the gateways itself and point its stratum hashers at them. The coinbase
// then carries a gateway tag and reads as DATUM, but the pool's node built the block. The chain
// cannot show that. `hosted` can: scripts/hosted-watch.py listens on the pools' public stratum
// ports and ties a found block to the job a port handed out, and so proves which gateway names
// are the pool's own. A block under such a name is the pool's stratum, whatever its tag says.

import { coinbaseTags, poolName, POOL_LINKS } from "./galaxy.js";

export const ACTIVE_SEC = 14 * 86400;
export const MEMORY_SEC = 30 * 86400;
export const PAYS_CAP = 40;

// A split's payees are suspects, not confirmed, and only where the pool built at least this share
// of its split blocks itself in the window, over at least this many blocks. Below that a payee
// is more likely on a gateway than on stratum, and the page does not guess.
export const SUSPECT_SHARE = 0.5;
export const SUSPECT_MIN_BLOCKS = 5;
// A payee on this share of a pool's pool-built blocks is the pool's own wallet, not a hasher.
export const POOL_WALLET_SHARE = 0.8;
export const POOL_WALLET_MIN_BLOCKS = 8;

const fold = (s) => String(s || "").toLowerCase().replace(/[^a-z0-9]/g, "");

function sameParty(a, b) {
  const x = fold(a), y = fold(b);
  if (!y) return true;
  if (!x) return false;
  return x.includes(y) || y.includes(x);
}

/**
 * "datum" — a gateway's own tag is on the coinbase. The finder has a node. The other
 * outputs are just who got paid, not proof that each of them runs a node.
 * "stratum" — the pool wrote a split. On a stratum pool those payees are the hashers.
 * "custodial" — a known pool took the whole coinbase (one or two outputs). The block is
 * entirely naughty: every output is the pool's stratum, and the hashers are not named.
 * "skip" — a solo miner took one or two outputs. That is their own node, not stratum.
 */
/** A gateway as `hosted` names it: the folded primary tag, then the secondary tag as written. */
export const gatewayKey = (primary, secondary) => `${fold(primary)}|${secondary || ""}`;

/** Whether the watcher proved this block, or this gateway name, to be the pool's own. */
export function isHosted(hosted, height, key) {
  if (!hosted) return false;
  return !!((hosted.blocks && hosted.blocks[height]) || (key && hosted.tags && hosted.tags[key]));
}

export function classifyCoinbase(scriptHex, outputCount, explorerPool = "", hosted = null, height = 0) {
  const t = coinbaseTags(scriptHex);
  // Some pools put the name in a later push ("/mined on B2Pool.io/"), which this parser does not
  // read. The explorer's pool name is the same fallback the galaxy map uses.
  const pool = poolName(t.primary || "") || poolName(explorerPool || "") || "Unknown";
  const gateway = t.layout === "datum" && t.secondary && !sameParty(pool, t.secondary) && !sameParty(t.primary, t.secondary);
  if (gateway) {
    const key = gatewayKey(t.primary, t.secondary);
    if (isHosted(hosted, height, key)) return { kind: "stratum", pool, key, hosted: true };
    return { kind: "datum", pool, key };
  }
  if (outputCount > 3) return { kind: "stratum", pool };
  if (POOL_LINKS[pool] && outputCount >= 1) return { kind: "custodial", pool };
  return { kind: "skip", pool };
}

export function emptyState() {
  return { v: 2, asOf: 0, tip: 0, scannedTo: null, pools: {}, ingested: {}, addrs: {}, blk: {}, ext: {} };
}

/**
 * Bring a stored v1 document up to v2. `blk` is one entry per classified block; a v1 document
 * never kept it, so it is rebuilt from the payouts each address remembers.
 */
export function migrate(state) {
  if (!state.ext) state.ext = {};
  if (!state.blk) {
    state.blk = {};
    for (const a of Object.values(state.addrs || {})) {
      for (const p of a.pays || []) if (!state.blk[p.h]) state.blk[p.h] = [p.t || 0, p.pool, p.k];
    }
  }
  state.v = 2;
  return state;
}

function addrOf(state, address) {
  return state.addrs[address] || (state.addrs[address] = {
    sh: 0, st: 0, sp: "", sn: 0, dh: 0, dt: 0, dp: "", dn: 0, ps: {}, pays: [],
  });
}

/** Record one block's coinbase outputs. A height already recorded with the same id is skipped. */
export function noteBlock(state, block, hosted = null) {
  const id = String(block.id || "");
  const prev = state.ingested[block.h];
  if (prev && prev === id) return { kind: "dup" };
  const outputs = (block.outputs || []).filter((o) => o && o.address);
  const cls = classifyCoinbase(block.script || "", outputs.length, block.explorerPool || "", hosted, block.h);
  state.ingested[block.h] = id;
  if (block.h > (state.tip || 0)) state.tip = block.h;
  if (cls.kind === "skip") return cls;
  const pools = state.pools[cls.pool] || (state.pools[cls.pool] = { s: 0, d: 0, c: 0 });
  const tally = cls.kind === "datum" ? "d" : cls.kind === "custodial" ? "c" : "s";
  pools[tally]++;
  // [time, pool, tally, outputs, gateway key ("" when there is no gateway tag), pool-run gateway]
  (state.blk || (state.blk = {}))[block.h] = [block.t || 0, cls.pool, tally, outputs.length, cls.key || "", cls.hosted ? 1 : 0];
  const seen = new Set();
  for (const o of outputs) {
    if (seen.has(o.address)) continue;
    seen.add(o.address);
    const a = addrOf(state, o.address);
    const key = cls.kind === "datum" ? "d" : cls.kind === "custodial" ? "c" : "s";
    if (cls.kind === "datum") {
      if (block.h >= a.dh) { a.dh = block.h; a.dt = block.t || 0; a.dp = cls.pool; }
      a.dn++;
    } else {
      if (block.h >= a.sh) { a.sh = block.h; a.st = block.t || 0; a.sp = cls.pool; }
      a.sn++;
      if (key === "s") a.ps[cls.pool] = (a.ps[cls.pool] || 0) + 1;
    }
    const pay = { h: block.h, t: block.t || 0, tx: block.txid || "", v: o.vout, sats: o.sats, pool: cls.pool, k: key };
    if (cls.hosted) pay.g = 1;
    a.pays.push(pay);
    if (a.pays.length > PAYS_CAP) a.pays.splice(0, a.pays.length - PAYS_CAP);
  }
  return cls;
}

/**
 * Gateway blocks noted before block entries kept the gateway key: [height, block id]. The caller
 * reads each coinbase again and hands the key to `setGatewayKey`. Newest first.
 */
export function blocksMissingKey(state, limit = 20) {
  const out = [];
  for (const h of Object.keys(state.blk || {}).sort((a, b) => b - a)) {
    const e = state.blk[h];
    if (e[2] !== "d" || e.length > 4 || !state.ingested[h]) continue;
    out.push([Number(h), state.ingested[h]]);
    if (out.length >= limit) break;
  }
  return out;
}

export function setGatewayKey(state, height, scriptHex) {
  const e = state.blk && state.blk[height];
  if (!e || e.length > 4) return;
  const t = coinbaseTags(scriptHex);
  e[4] = t.layout === "datum" ? gatewayKey(t.primary, t.secondary) : "";
  e[5] = 0;
}

/**
 * Re-judge the gateway blocks already noted against what the watcher has proved since: a name is
 * proved by one block and then covers every block under it. Returns how many blocks moved from
 * DATUM to the pool's own. Nothing moves back: a proof is a block on chain.
 */
export function applyHosted(state, hosted) {
  if (!hosted) return 0;
  const moved = new Map();
  for (const [h, e] of Object.entries(state.blk || {})) {
    if (e[2] !== "d" || e.length < 5 || !isHosted(hosted, h, e[4])) continue;
    e[2] = "s";
    e[5] = 1;
    const n = state.pools[e[1]] || (state.pools[e[1]] = { s: 0, d: 0, c: 0 });
    n.d = Math.max(0, n.d - 1);
    n.s++;
    moved.set(Number(h), e[1]);
  }
  if (!moved.size) return 0;
  for (const a of Object.values(state.addrs || {})) {
    let touched = false;
    for (const p of a.pays || []) {
      if (p.k !== "d" || !moved.has(p.h)) continue;
      p.k = "s";
      p.g = 1;
      a.dn = Math.max(0, (a.dn || 0) - 1);
      a.sn = (a.sn || 0) + 1;
      a.ps[p.pool] = (a.ps[p.pool] || 0) + 1;
      if (p.h >= (a.sh || 0)) { a.sh = p.h; a.st = p.t || 0; a.sp = p.pool; }
      touched = true;
    }
    // Its newest DATUM payout may have been one of the blocks that moved.
    if (touched && moved.has(a.dh)) {
      const d = (a.pays || []).filter((p) => p.k === "d").sort((x, y) => y.h - x.h)[0];
      a.dh = d ? d.h : 0;
      a.dt = d ? d.t || 0 : 0;
      a.dp = d ? d.pool : "";
    }
  }
  return moved.size;
}

/**
 * Per pool, over the active window: blocks built on gateways the pool runs itself, blocks under
 * other gateway names, and the names proved. Only pools with at least one proved block.
 */
export function hostedWindows(state, now) {
  const out = {};
  for (const e of Object.values(state.blk || {})) {
    const [t, pool, k] = e;
    if ((t && now - t > ACTIVE_SEC) || (k !== "d" && !e[5])) continue;
    const p = out[pool] || (out[pool] = { hostedBlocks: 0, otherGatewayBlocks: 0, names: {} });
    if (e[5]) {
      p.hostedBlocks++;
      const name = String(e[4] || "").split("|").slice(1).join("|");
      if (name) p.names[name] = (p.names[name] || 0) + 1;
    } else p.otherGatewayBlocks++;
  }
  for (const [pool, p] of Object.entries(out)) {
    if (!p.hostedBlocks) { delete out[pool]; continue; }
    p.names = Object.entries(p.names).sort((x, y) => y[1] - x[1]).map((x) => x[0]);
    p.share = Math.round((p.hostedBlocks / (p.hostedBlocks + p.otherGatewayBlocks)) * 1000) / 10;
  }
  return out;
}

export function outputsOf(tx) {
  const vout = Array.isArray(tx && tx.vout) ? tx.vout : [];
  const out = [];
  for (let i = 0; i < vout.length; i++) {
    const o = vout[i] || {};
    const address = o.scriptpubkey_address || (o.scriptpubkey && o.scriptpubkey.address) || null;
    const sats = Number(o.value);
    if (!address || !Number.isFinite(sats) || sats <= 0) continue;
    out.push({ address, vout: o.n != null ? o.n : i, sats });
  }
  return out;
}

/** Drop payees and height marks older than the memory window. Counts are left: they only ever grow. */
export function prune(state, now) {
  const cut = now - MEMORY_SEC;
  const floorH = state.tip ? state.tip - 4000 : 0;
  for (const h of Object.keys(state.ingested)) if (Number(h) < floorH) delete state.ingested[h];
  for (const [h, b] of Object.entries(state.blk || {})) if (b[0] && b[0] < now - ACTIVE_SEC) delete state.blk[h];
  for (const [address, a] of Object.entries(state.addrs)) {
    a.pays = (a.pays || []).filter((p) => !p.t || p.t >= now - ACTIVE_SEC);
    const last = Math.max(a.st || 0, a.dt || 0);
    if (last && last < cut && !a.pays.length) delete state.addrs[address];
  }
  state.asOf = now;
}

export function poolWallets(state) {
  const out = new Set();
  for (const [pool, n] of Object.entries(state.pools || {})) {
    if (!n.s || n.s < POOL_WALLET_MIN_BLOCKS) continue;
    const need = Math.ceil(n.s * POOL_WALLET_SHARE);
    for (const [address, a] of Object.entries(state.addrs)) {
      if ((a.ps && a.ps[pool]) >= need) out.add(address);
    }
  }
  return out;
}

function liveByAddress(miners) {
  const by = new Map();
  for (const m of miners || []) {
    if (!m || !m.address || m.online === false) continue;
    let row = by.get(m.address);
    if (!row) by.set(m.address, (row = { stratum: false, datum: false, hr: 0 }));
    const stratum = m.fee_path === "stratum" || (m.via === "stratum" && m.fee_path !== "datum");
    if (stratum) row.stratum = true;
    else row.datum = true;
    row.hr += Number(m.hr_ghs) || 0;
  }
  return by;
}

function row(address, a, live, list) {
  return {
    address,
    list,
    pool: (live && live.stratum ? a.sp : a.dp) || a.sp || a.dp || "Lazarus",
    hrGhs: live ? Math.round(live.hr) : null,
    lastStratumHeight: a.sh || null,
    lastStratumTs: a.st || null,
    lastDatumHeight: a.dh || null,
    lastDatumTs: a.dt || null,
    live: live ? (live.stratum ? "stratum" : "datum") : null,
  };
}

/** Addresses mining to a DATUM endpoint on their own node, with no stratum worker still online. */
export function ownNodeDatum(miners, gateways) {
  const stratum = new Set();
  const datum = new Set();
  for (const m of miners || []) {
    if (!m || !m.address || m.online === false) continue;
    const onStratum = m.fee_path === "stratum" || (m.via === "stratum" && m.fee_path !== "datum");
    if (onStratum) stratum.add(m.address);
    else if (m.fee_path === "datum" || m.via === "prime" || m.gateway) datum.add(m.address);
  }
  for (const g of gateways || []) {
    const id = g && g.identity;
    if (!id || g.offline || g.own || g.fee_path === "stratum") continue;
    if (!stratum.has(id)) datum.add(id);
  }
  for (const a of stratum) datum.delete(a);
  return datum;
}

/**
 * Remember a Lazarus session. A TIDES coinbase pays every address in the window, so a DATUM
 * block does not mean each payee swapped. The swap we can confirm is the pool's own fee path:
 * stratum, then later datum, with no stratum worker left online.
 */
export function observeLive(state, miners, now) {
  let changed = false;
  for (const [address, L] of liveByAddress(miners)) {
    const a = addrOf(state, address);
    if (L.stratum) {
      if (!a.wasS) { a.wasS = 1; a.st = a.st || now; a.sp = a.sp || "Lazarus"; changed = true; }
    } else if (L.datum && !a.wasD) {
      a.wasD = 1;
      changed = true;
    }
  }
  return changed;
}

/**
 * Blocks per pool in the active window, as the chain shows them: d carry a gateway operator's
 * tag, s are splits without one, c pay the pool alone. An untagged split is not proof of
 * stratum (RATUM gateways write no tag), so this is context for the page and nothing is
 * classified from it.
 */
export function poolWindows(state, now) {
  const out = {};
  for (const [t, pool, k, outs] of Object.values(state.blk || {})) {
    if (t && now - t > ACTIVE_SEC) continue;
    const p = out[pool] || (out[pool] = { pool, s: 0, d: 0, c: 0, pooled: false });
    p[k]++;
    if (k !== "d" || outs > 3) p.pooled = true;
  }
  for (const p of Object.values(out)) p.blocks = p.s + p.d + p.c;
  return out;
}

// What a pool said is trusted for this long after we last read it.
export const FRESH_SEC = 6 * 3600;
// Under this share of an address's work in a pool's window, stratum work is a test, not a habit.
export const STRATUM_WORK_SHARE = 0.05;

/**
 * What one pool's API says about one address (see pools.js for the fields).
 * { verdict: "stratum", why } when the pool shows stratum work, a live stratum hashrate, or
 * names it the finder of a stratum block newer than any gateway block it found.
 * { verdict: "own-node" } when the pool shows only gateway work.
 * { verdict: "untagged" } when the pool lists it with no gateway tag: a hint.
 */
export function poolVerdict(e, now) {
  if (!e) return null;
  const fresh = !!e.at && now - e.at <= FRESH_SEC;
  const sw = e.sw || 0, dw = e.dw || 0;
  if (fresh && sw + dw > 0) {
    const share = sw / (sw + dw);
    if (share >= STRATUM_WORK_SHARE) return { verdict: "stratum", why: "pool-stratum-work", share };
    return { verdict: "own-node" };
  }
  if (fresh && e.hr > 0) return { verdict: "stratum", why: "pool-live-stratum" };
  const sb = e.sb || 0, db = e.db || 0;
  if (db > sb) return { verdict: "own-node" };
  if (sb && e.st && now - e.st <= ACTIVE_SEC) return { verdict: "stratum", why: "pool-stratum-block" };
  if (fresh && e.ut) return { verdict: "untagged" };
  return null;
}

/**
 * Three lists.
 * Naughty (confirmed): a live Lazarus stratum session; an address a pool's own API shows on
 *   stratum; or any output of a custodial pool block.
 * Suspects: an address a pool lists with no gateway tag; a payee of a split from a pool whose
 *   own block list says most of its blocks were found over stratum; or a payee of blocks built
 *   on gateways the pool runs itself, where those are at least SUSPECT_SHARE of the pool's
 *   gateway blocks. None is proof, and each row says what it rests on.
 * Nice: an address we saw on stratum that the same source now shows on its own node.
 */
export function buildLists(state, miners, now, gateways) {
  const wallets = poolWallets(state);
  const live = liveByAddress(miners);
  const ownNode = ownNodeDatum(miners, gateways);
  const windows = poolWindows(state, now);
  const ext = state.ext || {};
  const naughty = [];
  const suspects = [];
  const nice = [];
  const seen = new Set();
  const recent = (p) => !p.t || now - p.t <= ACTIVE_SEC;
  const blank = () => ({ sh: 0, st: 0, sp: "", dh: 0, dt: 0, dp: "", ps: {}, pays: [] });
  // A pool whose own block list says at least SUSPECT_SHARE of its blocks came over stratum.
  const odds = {};
  for (const [pool, x] of Object.entries(ext)) {
    const w = x && x.w;
    if (!w || !(w.s + w.d) || w.s < SUSPECT_MIN_BLOCKS) continue;
    const share = w.s / (w.s + w.d);
    if (share >= SUSPECT_SHARE) odds[pool] = { stratumBlocks: w.s, gatewayBlocks: w.d, share: Math.round(share * 1000) / 10 };
  }

  // A pool that builds most of its gateway blocks on gateways it runs itself. The split pays
  // everyone in its window, the pool's real gateway owners too, so a payee is a suspect.
  const hostedOdds = {};
  for (const [pool, p] of Object.entries(hostedWindows(state, now))) {
    if (p.hostedBlocks >= SUSPECT_MIN_BLOCKS && p.share >= SUSPECT_SHARE * 100) hostedOdds[pool] = p;
  }
  const fromPools = (address) => {
    let confirmed = null, untagged = null, moved = null;
    const cleared = new Set();
    for (const [pool, x] of Object.entries(ext)) {
      const e = x && x.m && x.m[address];
      const v = poolVerdict(e, now);
      if (!v) continue;
      if (v.verdict === "own-node") { cleared.add(pool); if (e.was) moved = { pool, e }; }
      else if (v.verdict === "untagged") untagged = { pool, e };
      else if (!confirmed || (e.hr || 0) > (confirmed.e.hr || 0)) confirmed = { pool, e, v };
    }
    return { confirmed, cleared, untagged, moved };
  };
  const confirmedRow = (address, a, L, c) => {
    const item = row(address, a, L, "naughty");
    item.pool = c.pool;
    item.why = c.v.why;
    item.live = null;
    item.hrGhs = null;
    if (c.v.why === "pool-stratum-block") {
      item.evidenceHeight = c.e.sb;
      item.lastStratumHeight = c.e.sb;
      if (c.e.phr && now - (c.e.seen || 0) <= FRESH_SEC) item.hrGhs = c.e.phr;
    } else {
      if (c.v.share != null) item.stratumPct = Math.round(c.v.share * 1000) / 10;
      if (c.e.hr > 0) { item.hrGhs = c.e.hr; item.live = "stratum"; }
    }
    return item;
  };

  for (const [address, a] of Object.entries(state.addrs || {})) {
    seen.add(address);
    const L = live.get(address);
    const hashingStratum = !!(L && L.stratum);
    const onOwnNode = ownNode.has(address);
    const custodial = (a.pays || []).find((p) => p.k === "c" && recent(p));
    const { confirmed, cleared, untagged, moved } = fromPools(address);
    if (hashingStratum) {
      const item = row(address, a, L, "naughty");
      item.pool = "Lazarus";
      item.why = "stratum-endpoint";
      naughty.push(item);
      continue;
    }
    if (confirmed) { naughty.push(confirmedRow(address, a, L, confirmed)); continue; }
    if (wallets.has(address) && !custodial) continue;
    if (onOwnNode) {
      if (a.wasS) nice.push({ ...row(address, a, L, "nice"), pool: "Lazarus" });
      continue;
    }
    if (custodial) {
      const item = row(address, a, L, "naughty");
      item.pool = custodial.pool;
      item.why = "custodial-block";
      item.live = null;
      item.hrGhs = null;
      naughty.push(item);
      continue;
    }
    if (moved) {
      const item = row(address, a, null, "nice");
      item.pool = moved.pool;
      nice.push(item);
      continue;
    }
    if (untagged) {
      suspects.push({ address, list: "suspect", pool: untagged.pool, why: "no-gateway-tag", lastStratumHeight: null });
      continue;
    }
    // A split's payees, where the pool's own block list makes stratum the likelier answer.
    const paid = {};
    for (const p of a.pays || []) {
      if (p.k !== "s" || !recent(p) || !odds[p.pool] || cleared.has(p.pool)) continue;
      const n = paid[p.pool] || (paid[p.pool] = { pool: p.pool, blocks: 0, lastHeight: 0, lastTs: 0 });
      n.blocks++;
      if (p.h > n.lastHeight) { n.lastHeight = p.h; n.lastTs = p.t || 0; }
    }
    const best = Object.values(paid).sort((x, y) => y.blocks - x.blocks || y.lastHeight - x.lastHeight)[0];
    if (best) {
      suspects.push({
        address, list: "suspect", pool: best.pool, why: "split-payee",
        paidBlocks: best.blocks, lastStratumHeight: best.lastHeight, lastStratumTs: best.lastTs,
        ...odds[best.pool],
      });
      continue;
    }
    // Payees of blocks built on the pool's own gateways.
    const run = {};
    for (const p of a.pays || []) {
      if (!p.g || !recent(p) || !hostedOdds[p.pool] || cleared.has(p.pool)) continue;
      const n = run[p.pool] || (run[p.pool] = { pool: p.pool, blocks: 0, lastHeight: 0, lastTs: 0 });
      n.blocks++;
      if (p.h > n.lastHeight) { n.lastHeight = p.h; n.lastTs = p.t || 0; }
    }
    const top = Object.values(run).sort((x, y) => y.blocks - x.blocks || y.lastHeight - x.lastHeight)[0];
    if (top) {
      const o = hostedOdds[top.pool];
      suspects.push({
        address, list: "suspect", pool: top.pool, why: "hosted-gateway",
        paidBlocks: top.blocks, lastStratumHeight: top.lastHeight, lastStratumTs: top.lastTs,
        share: o.share, hostedBlocks: o.hostedBlocks, otherGatewayBlocks: o.otherGatewayBlocks, gatewayNames: o.names.slice(0, 6),
      });
    }
  }
  for (const [address, L] of live) {
    if (seen.has(address) || !L.stratum) continue;
    seen.add(address);
    const item = row(address, blank(), L, "naughty");
    item.pool = "Lazarus";
    item.why = "stratum-endpoint";
    naughty.push(item);
  }
  // A pool can show an address the chain walk has no payout for.
  for (const x of Object.values(ext)) {
    for (const address of Object.keys((x && x.m) || {})) {
      if (seen.has(address)) continue;
      seen.add(address);
      if (ownNode.has(address)) continue;
      const { confirmed, untagged, moved } = fromPools(address);
      if (confirmed) naughty.push(confirmedRow(address, blank(), null, confirmed));
      else if (moved) nice.push({ ...row(address, blank(), null, "nice"), pool: moved.pool });
      else if (untagged) suspects.push({ address, list: "suspect", pool: untagged.pool, why: "no-gateway-tag", lastStratumHeight: null });
    }
  }
  const byHr = (x, y) => (y.hrGhs || 0) - (x.hrGhs || 0) || (y.lastStratumHeight || 0) - (x.lastStratumHeight || 0);
  naughty.sort(byHr);
  suspects.sort((x, y) => (y.share || 0) - (x.share || 0) || (y.paidBlocks || 0) - (x.paidBlocks || 0) || (y.lastStratumHeight || 0) - (x.lastStratumHeight || 0));
  nice.sort((x, y) => (y.hrGhs || 0) - (x.hrGhs || 0));
  return { naughty, suspects, nice, poolWallets: wallets.size, windows, hosted: hostedWindows(state, now) };
}

/** Mark every address a pool currently shows on stratum, so a later move to its own node is seen. */
export function rememberStratum(state, now) {
  for (const x of Object.values(state.ext || {})) {
    for (const e of Object.values((x && x.m) || {})) {
      const v = poolVerdict(e, now);
      if (v && v.verdict === "stratum" && v.why !== "pool-stratum-block") e.was = 1;
    }
  }
}

const ADDR = /^(bc1[ac-hj-np-z02-9]{11,87}|[13][a-km-zA-HJ-NP-Z1-9]{25,34})$/;

export function validAddress(s) {
  return ADDR.test(String(s || ""));
}

/** Coinbase outputs we indexed for one address, marked unspent when the explorer's UTXO set says so. */
export function addressPays(state, address, utxos) {
  const a = state.addrs && state.addrs[address];
  const unspent = new Set((utxos || []).map((u) => `${u.txid}:${u.vout}`));
  const pays = (a && a.pays ? a.pays : []).map((p) => ({
    height: p.h, ts: p.t, txid: p.tx, vout: p.v, sats: p.sats, pool: p.pool,
    kind: p.k === "d" ? "datum-block" : p.k === "c" ? "custodial" : "stratum",
    unspent: !!(p.tx && unspent.has(`${p.tx}:${p.v}`)),
  }));
  pays.sort((x, y) => y.height - x.height);
  return pays;
}
