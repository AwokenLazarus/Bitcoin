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
//
// The same trick is played on us: a stratum pool points a gateway of its own at Lazarus's DATUM
// port, so its hashers arrive here as DATUM miners while that pool's node builds their blocks.
// The watcher finds such a gateway by where it connects from (the IP that serves the other
// pool's public stratum port) and records the addresses behind it in `state.via`. Those are
// confirmed: they are hashing on that pool's stratum, and Lazarus's coinbase is what pays them.

import { coinbaseTags, poolName, POOL_LINKS } from "./galaxy.js";

export const ACTIVE_SEC = 14 * 86400;
export const MEMORY_SEC = 30 * 86400;

// A split's payees are suspects, not confirmed, and only where the pool built at least this share
// of its split blocks itself in the window, over at least this many blocks. Below that a payee
// is more likely on a gateway than on stratum, and the page does not guess.
export const SUSPECT_SHARE = 0.5;
export const SUSPECT_MIN_BLOCKS = 5;
// A payee on this share of a pool's pool-built blocks may be the pool's own wallet, not a hasher.
// A steady miner is on every split of a TIDES window too, so presence singles out a wallet only
// where few addresses have it: with more than POOL_WALLET_MAX of them it says nothing, and
// nobody is hidden. (Measured 2026-10-11: AlphaPool 119 such addresses, B2Pool 69, CONVOY 3.)
export const POOL_WALLET_SHARE = 0.8;
export const POOL_WALLET_MIN_BLOCKS = 8;
export const POOL_WALLET_MAX = 3;

const fold = (s) => String(s || "").toLowerCase().replace(/[^a-z0-9]/g, "");

function sameParty(a, b) {
  const x = fold(a), y = fold(b);
  if (!y) return true;
  if (!x) return false;
  return x.includes(y) || y.includes(x);
}

/** A gateway as `hosted` names it: the folded primary tag, then the secondary tag as written. */
export const gatewayKey = (primary, secondary) => `${fold(primary)}|${secondary || ""}`;

/**
 * How the watcher ties a block to a pool-run gateway. 2: this very block matches a job a public
 * stratum port handed out. 1: the block carries a gateway name proved that way by another block.
 */
export function hostedProof(hosted, height, key) {
  if (!hosted) return 0;
  if (hosted.blocks && hosted.blocks[height]) return 2;
  return key && hosted.tags && hosted.tags[key] ? 1 : 0;
}
export const isHosted = (hosted, height, key) => hostedProof(hosted, height, key) > 0;

/**
 * "datum" — a gateway's own tag is on the coinbase. The finder has a node. The other
 * outputs are just who got paid, not proof that each of them runs a node.
 * "stratum" — the pool wrote a split. On a stratum pool those payees are the hashers.
 * "custodial" — a known pool took the whole coinbase in one output. The block is the pool's
 * stratum, and the one address is the pool's wallet: the hashers are not named.
 * "skip" — a solo miner took one to three outputs. That is their own node, not stratum.
 * A known pool's block with two or three outputs is a small split (a finder and a fee, say): it
 * is counted as a split, and its payees are judged like any other split's, not called custodial.
 * `tags` is what galaxy.js coinbaseTags returns.
 */
export function classifyTags(tags, outputCount, explorerPool = "", hosted = null, height = 0) {
  const t = tags || {};
  // Some pools put the name in a later push ("/mined on B2Pool.io/"), which the tag parser does
  // not read. The explorer's pool name is the same fallback the galaxy map uses.
  const pool = poolName(t.primary || "") || poolName(explorerPool || "") || "Unknown";
  const gateway = t.layout === "datum" && t.secondary && !sameParty(pool, t.secondary) && !sameParty(t.primary, t.secondary);
  if (gateway) {
    const key = gatewayKey(t.primary, t.secondary);
    const proof = hostedProof(hosted, height, key);
    if (proof) return { kind: "stratum", pool, key, hosted: proof };
    return { kind: "datum", pool, key };
  }
  if (outputCount > 3) return { kind: "stratum", pool };
  if (POOL_LINKS[pool] && outputCount === 1) return { kind: "custodial", pool };
  if (POOL_LINKS[pool] && outputCount > 1) return { kind: "stratum", pool };
  return { kind: "skip", pool };
}

export const classifyCoinbase = (scriptHex, outputCount, explorerPool = "", hosted = null, height = 0) =>
  classifyTags(coinbaseTags(scriptHex), outputCount, explorerPool, hosted, height);

// state.blk[height] = [time, pool, kind, outputs, gateway key, proof, block id, coinbase txid]
//   kind   "d" a gateway tag, "s" a split the pool built, "c" paid to the pool alone
//   proof  0, or how a gateway block was tied to the pool (see hostedProof); then kind is "s"
// state.skip[height] = block id of a block that is nobody's pool (so it is not read again)
// state.pays[address] = [height, vout, sats, height, vout, sats, ...] for every block in state.blk
// state.seen[address] = { s, d, at } what Lazarus's live sessions showed (the nice list needs it)
// Everything a row says about an address is worked out from these each time (see derive), so a
// reorg, a re-judged gateway or a block leaving the window cannot leave a stale count behind.
export function emptyState() {
  return { v: 3, asOf: 0, tip: 0, blk: {}, skip: {}, pays: {}, seen: {}, ext: {} };
}

/**
 * Bring a stored document up to v3. Earlier versions kept at most 40 payouts an address and
 * running counts; v3 keeps every payout in the window, so the chain part is read again. What the
 * pools said, what the watcher saw and who was once on Lazarus's stratum carry over.
 */
export function migrate(state) {
  if (state && state.v === 3) {
    for (const k of ["blk", "skip", "pays", "seen", "ext"]) if (!state[k]) state[k] = {};
    return state;
  }
  const out = emptyState();
  if (!state) return out;
  out.ext = state.ext || {};
  if (state.via) out.via = state.via;
  for (const [address, a] of Object.entries(state.addrs || {})) {
    if (a.wasS || a.wasD) out.seen[address] = { s: a.wasS ? 1 : 0, d: a.wasD ? 1 : 0, at: state.asOf || 0 };
  }
  return out;
}

/** Forget one height: the block entry and every payout from it. For a reorg. */
export function dropBlock(state, height) {
  const h = Number(height);
  delete state.skip[h];
  if (!state.blk[h]) return;
  delete state.blk[h];
  for (const [address, p] of Object.entries(state.pays)) {
    let j = 0;
    for (let i = 0; i < p.length; i += 3) {
      if (p[i] === h) continue;
      if (j !== i) { p[j] = p[i]; p[j + 1] = p[i + 1]; p[j + 2] = p[i + 2]; }
      j += 3;
    }
    if (j !== p.length) p.length = j;
    if (!p.length) delete state.pays[address];
  }
}

/**
 * Record one block's coinbase outputs. `block.tags` is the parsed coinbase tag (or pass
 * `block.script`). A height already recorded with the same id is skipped; with another id the
 * old block was orphaned, so it is dropped first.
 */
export function noteBlock(state, block, hosted = null) {
  const id = String(block.id || "");
  const h = Number(block.h);
  const prev = state.blk[h] ? state.blk[h][6] : state.skip[h];
  if (prev != null) {
    if (prev === id) return { kind: "dup" };
    dropBlock(state, h);
  }
  const outputs = (block.outputs || []).filter((o) => o && o.address);
  const tags = block.tags || coinbaseTags(block.script || "");
  const cls = classifyTags(tags, outputs.length, block.explorerPool || "", hosted, h);
  if (h > (state.tip || 0)) state.tip = h;
  if (cls.kind === "skip") { state.skip[h] = id; return cls; }
  const kind = cls.kind === "datum" ? "d" : cls.kind === "custodial" ? "c" : "s";
  state.blk[h] = [block.t || 0, cls.pool, kind, outputs.length, cls.key || "", cls.hosted || 0, id, block.txid || ""];
  const seen = new Set();
  for (const o of outputs) {
    if (seen.has(o.address)) continue;
    seen.add(o.address);
    (state.pays[o.address] || (state.pays[o.address] = [])).push(h, o.vout, o.sats);
  }
  return cls;
}

/**
 * Judge every gateway block again against what the watcher has proved: a name is proved by one
 * block and then covers every block under it. Returns how many blocks changed. A missing
 * `hosted` document changes nothing: a proof is a block on chain and is not taken back because
 * the watcher's document could not be read.
 */
export function applyHosted(state, hosted) {
  if (!hosted) return 0;
  let moved = 0;
  for (const [h, e] of Object.entries(state.blk)) {
    if (!e[4]) continue;
    const proof = hostedProof(hosted, h, e[4]);
    if (proof === e[5]) continue;
    if (!proof && e[5]) continue;
    e[5] = proof;
    e[2] = "s";
    moved++;
  }
  return moved;
}

/**
 * Per pool, over the active window: gateway blocks tied to the pool by the job itself (`direct`),
 * by a proved gateway name (`named`), and the gateway blocks under other names. Only pools with
 * at least one proved block.
 */
export function hostedWindows(state, now) {
  const out = {};
  for (const e of Object.values(state.blk || {})) {
    const [t, pool, , , key, proof] = e;
    if (!key || (t && now - t > ACTIVE_SEC)) continue;
    const p = out[pool] || (out[pool] = { hostedBlocks: 0, direct: 0, named: 0, otherGatewayBlocks: 0, names: {} });
    if (proof) {
      p.hostedBlocks++;
      if (proof === 2) p.direct++; else p.named++;
      const name = String(key).split("|").slice(1).join("|");
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

/** Drop blocks older than the active window with their payouts, and sessions past the memory window. */
export function prune(state, now) {
  const cut = now - ACTIVE_SEC;
  const floorH = state.tip ? state.tip - 6000 : 0;
  for (const h of Object.keys(state.skip)) if (Number(h) < floorH) delete state.skip[h];
  let aged = false;
  for (const [h, b] of Object.entries(state.blk)) if (b[0] && b[0] < cut) { delete state.blk[h]; aged = true; }
  if (aged) {
    for (const [address, p] of Object.entries(state.pays)) {
      let j = 0;
      for (let i = 0; i < p.length; i += 3) {
        if (!state.blk[p[i]]) continue;
        if (j !== i) { p[j] = p[i]; p[j + 1] = p[i + 1]; p[j + 2] = p[i + 2]; }
        j += 3;
      }
      if (j !== p.length) p.length = j;
      if (!p.length) delete state.pays[address];
    }
  }
  for (const [address, s] of Object.entries(state.seen)) if ((s.at || 0) < now - MEMORY_SEC) delete state.seen[address];
  state.asOf = now;
}

const blankAgg = () => ({ sh: 0, st: 0, sp: "", sn: 0, dh: 0, dt: 0, dp: "", dn: 0, ps: {}, sl: {}, gn: {}, gl: {}, c: null });

/**
 * What the stored blocks say about each address, worked out fresh:
 *   sh/st/sp, dh/dt/dp  newest pool-built and newest gateway-tagged block that paid it
 *   ps[pool], sl[pool]  splits from that pool that paid it, and the newest [height, time]
 *   gn[pool], gl[pool]  the same for blocks built on gateways the pool runs itself
 *   c                   newest block that paid it alone as the pool: { pool, h, t, n }
 * and per pool the blocks of each kind (`pools`).
 */
export function derive(state, now) {
  const addrs = {};
  const pools = {};
  const live = {};
  for (const [h, e] of Object.entries(state.blk || {})) {
    if (e[0] && now - e[0] > ACTIVE_SEC) continue;
    live[h] = e;
    const n = pools[e[1]] || (pools[e[1]] = { s: 0, d: 0, c: 0 });
    n[e[2]]++;
  }
  for (const [address, p] of Object.entries(state.pays || {})) {
    let a = null;
    for (let i = 0; i < p.length; i += 3) {
      const h = p[i], e = live[h];
      if (!e) continue;
      if (!a) a = addrs[address] = blankAgg();
      const [t, pool, kind, , , proof] = e;
      if (kind === "d") {
        if (h >= a.dh) { a.dh = h; a.dt = t; a.dp = pool; }
        a.dn++;
        continue;
      }
      if (h >= a.sh) { a.sh = h; a.st = t; a.sp = pool; }
      a.sn++;
      if (kind === "c") {
        if (!a.c || a.c.pool !== pool) a.c = { pool, h, t, n: 1 };
        else { a.c.n++; if (h > a.c.h) { a.c.h = h; a.c.t = t; } }
        continue;
      }
      a.ps[pool] = (a.ps[pool] || 0) + 1;
      if (!a.sl[pool] || h > a.sl[pool][0]) a.sl[pool] = [h, t];
      if (proof) {
        a.gn[pool] = (a.gn[pool] || 0) + 1;
        if (!a.gl[pool] || h > a.gl[pool][0]) a.gl[pool] = [h, t];
      }
    }
  }
  return { addrs, pools };
}

/** Payees on most of a pool's splits in the window, where that is rare enough to mean a wallet. */
export function poolWallets(derived) {
  const out = new Set();
  for (const [pool, n] of Object.entries(derived.pools || {})) {
    if (!n.s || n.s < POOL_WALLET_MIN_BLOCKS) continue;
    const need = Math.ceil(n.s * POOL_WALLET_SHARE);
    const often = Object.keys(derived.addrs).filter((address) => (derived.addrs[address].ps[pool] || 0) >= need);
    if (often.length <= POOL_WALLET_MAX) for (const address of often) out.add(address);
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
  const seen = state.seen || (state.seen = {});
  for (const [address, L] of liveByAddress(miners)) {
    const m = seen[address] || (seen[address] = { s: 0, d: 0, at: now });
    if (L.stratum && !m.s) { m.s = 1; changed = true; }
    if (!L.stratum && L.datum && !m.d) { m.d = 1; changed = true; }
    m.at = now;
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
  for (const [t, pool, k, outs, key] of Object.values(state.blk || {})) {
    if (t && now - t > ACTIVE_SEC) continue;
    const p = out[pool] || (out[pool] = { pool, s: 0, d: 0, c: 0, pooled: false });
    p[k]++;
    if (!key || outs > 3) p.pooled = true;
  }
  for (const p of Object.values(out)) p.blocks = p.s + p.d + p.c;
  return out;
}

// What a pool said is trusted for this long after we last read it.
export const FRESH_SEC = 6 * 3600;
// An address behind another pool's gateway counts as hashing there now for this long after the
// watcher last saw it (it republishes every half hour).
export const VIA_LIVE_SEC = 3600;
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
 * Naughty (confirmed): a live Lazarus stratum session; an address behind another pool's gateway
 *   on Lazarus (`state.via`); an address a pool's own API shows on stratum; or any output of a
 *   custodial pool block.
 * Suspects: an address a pool lists with no gateway tag; a payee of a split from a pool whose
 *   own block list says most of its blocks were found over stratum; or a payee of blocks built
 *   on gateways the pool runs itself, where those are at least SUSPECT_SHARE of the pool's
 *   gateway blocks. None is proof, and each row says what it rests on.
 * Nice: an address we saw on stratum that the same source now shows on its own node.
 */
export function buildLists(state, miners, now, gateways, derived = derive(state, now)) {
  const wallets = poolWallets(derived);
  const memory = state.seen || {};
  const live = liveByAddress(miners);
  const ownNode = ownNodeDatum(miners, gateways);
  const windows = poolWindows(state, now);
  const ext = state.ext || {};
  const naughty = [];
  const suspects = [];
  const nice = [];
  const seen = new Set();
  const blank = blankAgg;
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
  // Behind another pool's gateway on our own DATUM port, seen within the active window.
  const via = (state.via && state.via.addrs) || {};
  const behind = (address) => {
    const v = via[address];
    return v && v.pool && now - (v.last || 0) <= ACTIVE_SEC ? v : null;
  };
  const behindRow = (address, a, L, v) => {
    const item = row(address, a, L, "naughty");
    item.pool = v.pool;
    item.why = "behind-pool-gateway";
    item.host = "Lazarus";
    item.gatewayKey = v.gateway || "";
    item.firstSeen = v.first || null;
    item.seenTs = v.last || null;
    item.lastSeen = v.last || null;
    // on our pool it shows as DATUM; what it is doing is hashing on the other pool's stratum
    item.live = now - (v.last || 0) <= VIA_LIVE_SEC ? "stratum" : null;
    if (!item.live) item.hrGhs = null;
    return item;
  };
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
    // When the pool's data last showed it: the block it found, or our last good read of the pool.
    item.seenTs = c.v.why === "pool-stratum-block" ? c.e.st || null : c.e.at || null;
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

  const addresses = new Set([...Object.keys(derived.addrs), ...Object.keys(memory)]);
  for (const address of addresses) {
    const a = derived.addrs[address] || blank();
    seen.add(address);
    const L = live.get(address);
    const v = behind(address);
    if (v) { naughty.push(behindRow(address, a, L, v)); continue; }
    const hashingStratum = !!(L && L.stratum);
    const onOwnNode = ownNode.has(address);
    const custodial = a.c;
    const { confirmed, cleared, untagged, moved } = fromPools(address);
    if (hashingStratum) {
      const item = row(address, a, L, "naughty");
      item.pool = "Lazarus";
      item.why = "stratum-endpoint";
      item.seenTs = now;
      naughty.push(item);
      continue;
    }
    if (confirmed) { naughty.push(confirmedRow(address, a, L, confirmed)); continue; }
    if (wallets.has(address) && !custodial) continue;
    if (onOwnNode) {
      if (memory[address] && memory[address].s) nice.push({ ...row(address, a, L, "nice"), pool: "Lazarus" });
      continue;
    }
    if (custodial) {
      const item = row(address, a, L, "naughty");
      item.pool = custodial.pool;
      item.why = "custodial-block";
      item.live = null;
      item.hrGhs = null;
      item.blocks = custodial.n;
      item.lastStratumHeight = custodial.h;
      item.seenTs = custodial.t || null;
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
    const newest = (counts, last, ok) => Object.keys(counts)
      .filter((pool) => ok[pool] && !cleared.has(pool) && !(a.c && a.c.pool === pool))
      .map((pool) => ({ pool, blocks: counts[pool], lastHeight: last[pool][0], lastTs: last[pool][1] || 0 }))
      .sort((x, y) => y.blocks - x.blocks || y.lastHeight - x.lastHeight)[0];
    const best = newest(a.ps, a.sl, odds);
    if (best) {
      suspects.push({
        address, list: "suspect", pool: best.pool, why: "split-payee",
        paidBlocks: best.blocks, lastStratumHeight: best.lastHeight, lastStratumTs: best.lastTs,
        ...odds[best.pool],
      });
      continue;
    }
    // Payees of blocks built on the pool's own gateways.
    const top = newest(a.gn, a.gl, hostedOdds);
    if (top) {
      const o = hostedOdds[top.pool];
      suspects.push({
        address, list: "suspect", pool: top.pool, why: "hosted-gateway",
        paidBlocks: top.blocks, lastStratumHeight: top.lastHeight, lastStratumTs: top.lastTs,
        share: o.share, hostedBlocks: o.hostedBlocks, directBlocks: o.direct, otherGatewayBlocks: o.otherGatewayBlocks, gatewayNames: o.names.slice(0, 6),
      });
    }
  }
  for (const address of Object.keys(via)) {
    const v = behind(address);
    if (seen.has(address) || !v) continue;
    seen.add(address);
    naughty.push(behindRow(address, blank(), live.get(address) || null, v));
  }
  for (const [address, L] of live) {
    if (seen.has(address) || !L.stratum) continue;
    seen.add(address);
    const item = row(address, blank(), L, "naughty");
    item.pool = "Lazarus";
    item.why = "stratum-endpoint";
    item.seenTs = now;
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
  const byHr = (x, y) => (y.live ? 1 : 0) - (x.live ? 1 : 0) || (y.hrGhs || 0) - (x.hrGhs || 0) || (y.seenTs || 0) - (x.seenTs || 0) || (y.lastStratumHeight || 0) - (x.lastStratumHeight || 0);
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

/** Every coinbase output in the window for one address, marked unspent when the explorer's UTXO set says so. */
export function addressPays(state, address, utxos) {
  const p = (state.pays && state.pays[address]) || [];
  const unspent = new Set((utxos || []).map((u) => `${u.txid}:${u.vout}`));
  // Lazarus outputs earned while the address sat behind another pool's gateway on Lazarus.
  const v = state.via && state.via.addrs && state.via.addrs[address];
  // From when that pool's gateway first connected to us, not from when this address was first
  // noticed behind it: the watcher sees one address per pass, the gateway carried them all along.
  const gateways = Object.values((state.via && state.via.gateways) || {}).filter((g) => g && v && g.pool === v.pool);
  const since = v ? Math.min(v.first || Infinity, ...gateways.map((g) => g.first || Infinity)) : 0;
  const until = v ? Math.max(v.last || 0, ...gateways.map((g) => g.last || 0)) + VIA_LIVE_SEC : 0;
  const pays = [];
  for (let i = 0; i < p.length; i += 3) {
    const e = state.blk[p[i]];
    if (!e) continue;
    const [t, pool, kind, , , proof, , txid] = e;
    pays.push({
      height: p[i], ts: t, txid, vout: p[i + 1], sats: p[i + 2], pool,
      kind: kind === "d" ? "datum-block" : kind === "c" ? "custodial" : proof ? "pool-gateway" : "stratum",
      via: v && pool === "Lazarus" && t >= since && t <= until ? v.pool : null,
      unspent: !!(txid && unspent.has(`${txid}:${p[i + 1]}`)),
    });
  }
  pays.sort((x, y) => y.height - x.height);
  return pays;
}
