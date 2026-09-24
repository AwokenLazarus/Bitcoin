// Galaxy model for lazarus-xbt.xyz/map: every block since the BLAKE2b fork, sorted into
// systems (pools) and planets (DATUM gateways) from nothing but its coinbase.
//
// A DATUM gateway writes its coinbase scriptSig as: BIP34 height push, then one push holding
// `<primary tag> 0x0F <secondary tag> 0x00` (datum_coinbaser.c). The primary tag is set by the
// pool the gateway mines for, the secondary by the gateway's operator. So:
//   primary tag   -> the system (pool), after folding a pool's tag variants together
//   secondary tag -> the planet (gateway) inside that system
// Pools that never carry anyone else's secondary tag build every template themselves: those
// are the "stratum only" systems. A primary tag seen with one operator and a coinbase paying one
// or two outputs is a miner running their own node and gateway: an independent world.
//
// Everything here is inferred from public chain data, and says so on the page.

export const FORK_HEIGHT = 961640;
// Tags that software writes when the operator does not: they name no one.
// "RATUM" is the default tag of the RATUM gateway/node software: miners running it with default
// settings, not a pool (Mike, 2026-09-22).
const GENERIC_PRIMARY = new Set(["", "solo", "datum gateway", "datum", "gateway", "datum on umbrel", "knots", "8-30 nypost deride and conquer", "unknown", "ratum"]);
const DEFAULT_SECONDARY = new Set(["", "datum user", "8-30 nypost deride and conquer", "unknown", "(no tag)"]);
const isDefaultTag = (t) => DEFAULT_SECONDARY.has(String(t || "").trim().toLowerCase());
// Allegiance (Mike, 2026-09-22): capability, not share. A pool is Rebel when the chain shows it
//   (1) paying miners in the coinbase: a TIDES split (more than two outputs), or the whole block
//       to a miner's own address rather than to the pool's recurring wallet, and
//   (2) taking DATUM: blocks built by gateways carrying an operator's tag (defaults included).
// It is Imperial when it never does one of them. The share of its blocks the pool built itself
// is its Imperial outpost, sized by that share. A wallet is "recurring" when it is paid by at
// least a fifth of the pool's few-output coinbases (and three of them): that is the pool's own.
const MIN_EVIDENCE = 1;
// A pool that passes the test and still builds some blocks itself is mixed: a Rebel system with
// gateway planets and an Imperial outpost for its own stratum. Neither side is a verdict; each is
// sized by its blocks. Pool-built = no gateway tag, or a secondary tag naming the pool itself
// (see planetOf), the same rule the explorer's pie uses for its "Built by the pool" band.
// Where the operator's knowledge overrides the chain, it says so on the map. Keep this short. A
// verdict with a `type` overrides the allegiance test; one with only a `note` is shown beside it.
export const OPERATOR_VERDICT = {
  // Was type "stratum" while DATUM-AP was its only DATUM product. Since 22 Sep 2026 (~20:18 UTC)
  // its blocks carry gateway operators' tags (973724 = AlphaPool/The Mothership), so the chain
  // decides: a mixed pool (Mike, 2026-09-24: "don't flip it to pure DATUM").
  AlphaPool: { note: "Lazarus operator's verdict: known bad actor. Mixed pool since 22 Sep 2026: DATUM gateways plus its own stratum, each sized by its blocks." },
};
const WINDOW_MIN_BLOCKS = 10;

// Tag variants of one pool, folded to one name. Order matters: first match wins.
const POOL_ALIASES = [
  [/^pyblock/i, "PyBLOCK"],
  [/b2pool/i, "B2Pool"],
  [/^omegapool/i, "Omega Pool"],
  [/^rabid pool/i, "Rabid Pool"],
  [/^xbtpool/i, "xbtpool"],
  [/^bitcoin ?xor|xorpool/i, "Bitcoin Xor"],
  [/alphapool|^datum-ap$/i, "AlphaPool"],
  [/^lazarus/i, "Lazarus"],
  [/^convoy/i, "CONVOY"],
  [/^riptide|^tides$|^tides\.maveth/i, "RIPTIDE"],
  [/iohzrd/i, "RATUM Prime"],
  [/^mining-dutch/i, "Mining-Dutch"],
  [/^pow\.re$/i, "Pow.re"],
  [/^ocean(\.xyz)?$/i, "OCEAN"],
];
// Where a pool publishes itself; shown on the page when known.
export const POOL_LINKS = {
  Lazarus: "https://pool.lazarus-xbt.xyz",
  CONVOY: "https://convoy.xyz",
  RIPTIDE: "https://tides.maveth.ca",
  "Bitcoin Xor": "https://www.xorpool.com",
  B2Pool: "https://b2pool.io",
  PyBLOCK: "https://pyblock.xyz",
  "Pow.re": "https://pow.re",
  "RATUM Prime": "https://pool.iohzrd.tech",
  "Mining-Dutch": "https://www.mining-dutch.nl/",
  AlphaPool: "https://knots.alphapool.tech",
  xbtpool: "https://xbtpool.io",
  OCEAN: "https://ocean.xyz",
};

function pushes(hex) {
  const b = hexToBytes(hex);
  const out = [];
  let i = 0;
  while (i < b.length) {
    const op = b[i++];
    let n = 0;
    if (op >= 1 && op <= 75) n = op;
    else if (op === 76) n = b[i++];
    else if (op === 77) { n = b[i] | (b[i + 1] << 8); i += 2; }
    else { out.push(Uint8Array.of(op)); continue; }
    out.push(b.subarray(i, i + n));
    i += n;
  }
  return out;
}
function hexToBytes(hex) {
  const s = String(hex || "");
  const out = new Uint8Array(s.length >> 1);
  for (let i = 0; i < out.length; i++) out[i] = parseInt(s.substr(i * 2, 2), 16);
  return out;
}
const utf8 = new TextDecoder("utf-8", { fatal: false });
const text = (u8) => utf8.decode(u8);
const printable = (s) => s.length > 0 && !/[\u0000-\u001f\u007f�]/.test(s);
/** Reads like a name, not bytes that happen to be printable: mostly letters, digits and punctuation, with a run of two alphanumerics. */
export function readable(s) {
  const t = String(s || "");
  if (t.length < 2 || !/[A-Za-z0-9]{2,}/.test(t) || /�/.test(t)) return false;
  const good = (t.match(/[\p{L}\p{N}\p{P}\p{Zs}\p{S}]/gu) || []).length;
  const odd = (t.match(/[\u0080-ÿ]/g) || []).length; // Latin-1 debris from binary
  return good / t.length >= 0.85 && odd / t.length < 0.3;
}
// Miners choose these strings. Besides control characters, drop the ones that let a tag lie about
// itself on screen: bidi overrides can print a name backwards as another pool's, and zero-width
// characters pad a name invisibly so it reads as one already on the map.
// Built from a string so the ranges stay visible in the source: written as a regex literal, these
// are the very characters that do not show up in an editor.
const HIDDEN = new RegExp("[\\u0000-\\u001F\\u007F\\u200B-\\u200F\\u202A-\\u202E\\u2066-\\u2069\\uFEFF]", "g");
export const clean = (s) => String(s || "").replace(HIDDEN, "").trim().slice(0, 64);
const norm = (s) => String(s || "").toLowerCase().replace(/[^a-z0-9]/g, "");

/** { layout: "datum" | "other", primary, secondary } from a coinbase scriptSig in hex. */
export function coinbaseTags(raw) {
  const p = pushes(raw);
  const t = p[1];
  if (t && t.length > 1 && t[t.length - 1] === 0) {
    const body = t.subarray(0, t.length - 1);
    const seps = body.reduce((n, c) => n + (c === 0x0f ? 1 : 0), 0);
    if (seps === 1) {
      const k = body.indexOf(0x0f);
      const primary = text(body.subarray(0, k)), secondary = text(body.subarray(k + 1));
      if (printable(secondary) && (primary === "" || printable(primary))) return { layout: "datum", primary: clean(primary), secondary: clean(secondary) };
    }
  }
  // Not DATUM: the tag, if any, is the printable text of the second push.
  let tag = t ? text(t).replace(/\x00+$/, "") : "";
  if (!readable(tag)) tag = "";
  return { layout: "other", primary: clean(tag), secondary: "" };
}

export function poolName(tag) {
  for (const [re, name] of POOL_ALIASES) if (re.test(tag)) return name;
  return tag;
}

/** The compact record kept for every block. */
export function blockRecord(b) {
  const e = b.extras || {};
  const t = coinbaseTags(e.coinbaseRaw);
  const explorerPool = clean(e.pool && e.pool.name);
  const fr = Array.isArray(e.feeRange) ? e.feeRange : [];
  return {
    h: b.height, id: b.id, t: b.timestamp, n: b.tx_count, w: b.weight,
    l: t.layout === "datum" ? 1 : 0, p: t.primary, s: t.secondary, x: explorerPool,
    o: Array.isArray(e.coinbaseAddresses) ? e.coinbaseAddresses.length : (e.coinbaseAddress ? 1 : 0),
    ad: Array.isArray(e.coinbaseAddresses) && e.coinbaseAddresses.length <= 3 ? e.coinbaseAddresses.slice(0, 3) : (e.coinbaseAddress && !e.coinbaseAddresses ? [e.coinbaseAddress] : undefined),
    f: e.totalFees || 0, r: e.reward || 0, mf: e.medianFee ?? null, lf: fr.length ? fr[0] : null,
    mr: e.matchRate ?? null,
  };
}

function selfTag(pool, tag) {
  const a = norm(pool), b = norm(tag);
  if (!b) return true;
  return a.includes(b) || b.includes(a) || (POOL_LINKS[pool] && norm(POOL_LINKS[pool]).includes(b));
}

function policy(rows) {
  const n = rows.length;
  if (!n) return null;
  const avg = (f) => rows.reduce((s, r) => s + (Number(f(r)) || 0), 0) / n;
  const withMr = rows.filter((r) => r.mr != null);
  const withLf = rows.filter((r) => r.lf != null);
  // A block that did not report a median fee must not be averaged in as a zero: that dragged whole
  // pools toward "0 sat/vB", which is not what their blocks paid.
  const withMf = rows.filter((r) => r.mf != null);
  return {
    txs: Math.round(avg((r) => r.n)),
    weightKwu: Math.round(avg((r) => r.w) / 1000),
    feesSats: Math.round(avg((r) => r.f)),
    medianFee: withMf.length ? Math.round((withMf.reduce((s, r) => s + r.mf, 0) / withMf.length) * 10) / 10 : null,
    minFee: withLf.length ? Math.round(Math.min(...withLf.map((r) => r.lf)) * 10) / 10 : null,
    matchRate: withMr.length ? Math.round(withMr.reduce((s, r) => s + r.mr, 0) / withMr.length) : null,
    emptyBlocks: rows.filter((r) => r.n <= 1).length,
    outputs: Math.round(avg((r) => r.o)),
  };
}

/**
 * Build the galaxy from block records (any order) plus optional live Lazarus gateway sessions.
 * `now` is seconds; blocks newer than now - 7 days count toward the weekly share.
 */
export function buildGalaxy(records, { now = Math.floor(Date.now() / 1000), lazarusGateways = [], lazarusPool = null, networkHashrate = null } = {}) {
  const recs = records.filter((r) => r && r.h >= FORK_HEIGHT).sort((a, b) => a.h - b.h);
  const weekAgo = now - 7 * 86400;
  // The denominator is the span of heights in the last week, not the number of records we hold: a
  // gap in the record would otherwise shrink the denominator and inflate every pool's share.
  const weekRecs = recs.filter((r) => r.t >= weekAgo);
  const week = weekRecs.length ? Math.max(weekRecs.length, recs[recs.length - 1].h - weekRecs[0].h + 1) : 1;
  const coverage = Math.round((1000 * weekRecs.length) / week) / 10; // 100 = no blocks missing

  // Pass 1: group by pool candidate (the primary tag, or the explorer's name for non-DATUM blocks).
  const byPool = new Map();
  // An operator under a software default tag is named by their gateway tag if they set one, else
  // by the address the block paid: many miners share "RATUM" or "Knots", not their payout address.
  const short = (ad) => (ad.length > 16 ? ad.slice(0, 8) + "…" + ad.slice(-6) : ad);
  const soloKey = (r) => {
    if (r.l && !isDefaultTag(r.s)) return r.s;
    const base = r.p || (readable(r.x) ? r.x : "") || "Solo";
    const payee = r.ad && r.ad.length ? r.ad[r.ad.length > 1 ? 1 : 0] : null;
    return GENERIC_PRIMARY.has(base.toLowerCase()) && payee ? `${base} · ${short(payee)}` : base;
  };
  for (const r of recs) {
    const generic = r.l ? GENERIC_PRIMARY.has(r.p.toLowerCase()) : GENERIC_PRIMARY.has(String(r.p || (readable(r.x) ? r.x : "") || "").toLowerCase());
    const name = generic ? null : poolName(r.l ? r.p : r.p || (readable(r.x) ? r.x : "Unknown"));
    const key = generic ? `indie:${soloKey(r)}` : `pool:${name}`;
    let g = byPool.get(key);
    if (!g) byPool.set(key, (g = { key, name: generic ? soloKey(r) : name, generic, rows: [] }));
    g.rows.push(r);
  }

  // Pass 2: decide what each group is. Tags other than the pool's own are its gateways.
  const systems = [];
  // A planet is someone else's gateway on this pool's coinbase. A secondary that names the pool
  // itself is not one, whether it matches the system's name or the block's own primary tag
  // (DATUM-AP/DATUM-AP, DATUM-AlphaPool/AlphaPool): those blocks are the pool's own work.
  const planetOf = (sysName, r) => (r.l ? (r.s && !selfTag(sysName, r.s) && !selfTag(r.p, r.s) ? r.s : null) : null);
  for (const g of byPool.values()) {
    const outs = g.rows.map((r) => r.o).sort((a, b) => a - b);
    const medOut = outs[outs.length >> 1] || 0;
    const named = new Set(), third = [];
    for (const r of g.rows) {
      const t = planetOf(g.name, r);
      if (t == null) continue;
      third.push(r);
      if (!isDefaultTag(t)) named.add(t);
    }
    const few = g.rows.filter((r) => r.o <= 3 && r.ad && r.ad.length);
    const seenBy = new Map();
    for (const r of few) for (const ad of new Set(r.ad)) seenBy.set(ad, (seenBy.get(ad) || 0) + 1);
    const recurring = new Set([...seenBy].filter(([, c]) => c >= 3 && c >= few.length / 5).map(([a]) => a));
    const paysMiners = (r) => r.o > 3 || (r.ad && r.ad.some((a) => !recurring.has(a)));
    const isDatum = (r) => planetOf(g.name, r) != null;
    const paidAll = g.rows.filter(paysMiners).length, datumAll = g.rows.filter(isDatum).length;
    const recent = g.rows.filter((r) => r.t >= weekAgo);
    const win = recent.length >= WINDOW_MIN_BLOCKS ? recent : g.rows;
    const pct = (f) => Math.round((1000 * win.filter(f).length) / win.length) / 10;
    const faction = { window: win === recent ? "7d" : "all", blocks: win.length,
      datumBlocks: win.filter(isDatum).length, poolBuiltBlocks: win.filter((r) => !isDatum(r)).length,
      coinbasePaidPct: pct(paysMiners), datumPct: pct(isDatum), poolBuiltPct: pct((r) => !isDatum(r)),
      paysMinersEver: paidAll >= MIN_EVIDENCE, datumEver: datumAll >= MIN_EVIDENCE };
    let type;
    if (g.generic) type = "independent";
    else if (!POOL_LINKS[g.name] && medOut <= 3) type = named.size >= 3 ? "cluster" : "independent"; // not a known pool and no split: an operator's own node (or a software default shared by many)
    else type = faction.paysMinersEver && faction.datumEver ? "datum" : "stratum";      // a pool: the capability test
    const verdict = !g.generic && OPERATOR_VERDICT[g.name];
    if (verdict) { if (verdict.type) type = verdict.type; faction.operatorNote = verdict.note; }
    faction.mixed = type === "datum" && faction.datumBlocks > 0 && faction.poolBuiltBlocks > 0;
    g.faction = faction;
    // One miner, one world. A solo miner whose primary tag is their own name on some blocks and a
    // software default on others used to land in two systems of the same name, splitting their
    // blocks and their share, so independents are folded by name and not only by key.
    const sameName = (name) => systems.find((x) => x.type === "independent" && norm(x.name) === norm(name));
    if (type === "cluster") {
      // Each operator under a software default primary is their own world.
      for (const r of g.rows) {
        const k = `indie:${soloKey(r)}`;
        const sys = systems.find((x) => x.key === k) || sameName(soloKey(r));
        if (sys) sys.rows.push(r);
        else systems.push({ key: k, name: soloKey(r), type: "independent", rows: [r] });
      }
      continue;
    }
    const existing = systems.find((x) => x.key === g.key) || (type === "independent" && sameName(g.name));
    if (existing) for (const r of g.rows) existing.rows.push(r);
    else systems.push({ key: g.key, name: g.name, type, rows: g.rows, faction: g.faction });
  }

  const out = [];
  for (const s of systems) {
    s.rows.sort((a, b) => a.h - b.h);
    const planets = new Map();
    for (const r of s.rows) {
      let tag = s.type === "independent" ? null : planetOf(s.name, r);
      const k = tag == null ? "__own__" : tag;
      let p = planets.get(k);
      if (!p) planets.set(k, (p = { tag, rows: [] }));
      p.rows.push(r);
    }
    const blocks7 = s.rows.filter((r) => r.t >= weekAgo).length;
    const last = s.rows[s.rows.length - 1];
    const own = s.type === "datum" ? `${s.name} public stratum` : s.type === "stratum" ? `${s.name} (pool-built templates)` : s.name;
    const plist = [...planets.values()].map((p) => {
      const b7 = p.rows.filter((r) => r.t >= weekAgo).length;
      const lp = p.rows[p.rows.length - 1];
      return {
        tag: p.tag == null ? own : p.tag, house: p.tag == null, unnamed: p.tag != null && isDefaultTag(p.tag),
        blocks: p.rows.length, blocks7d: b7, firstHeight: p.rows[0].h, last: { height: lp.h, ts: lp.t, id: lp.id },
        estHashrate: networkHashrate ? Math.round((networkHashrate * b7) / week) : null, policy: policy(p.rows),
      };
    }).sort((a, b) => b.blocks - a.blocks);
    out.push({
      id: s.key, name: s.name, type: s.type, link: POOL_LINKS[s.name] || null,
      blocks: s.rows.length, blocks7d: blocks7, share7d: Math.round((10000 * blocks7) / week) / 100,
      estHashrate: networkHashrate ? Math.round((networkHashrate * blocks7) / week) : null,
      firstHeight: s.rows[0].h, last: { height: last.h, ts: last.t, id: last.id },
      gateways: plist.filter((p) => !p.house).length, policy: policy(s.rows),
      // An independent is one miner on one node: its single "planet" is the system itself, and the
      // page falls back to the system's own figures. Shipping it repeated everything about three
      // hundred solo miners twice, for two thirds of the file.
      planets: s.type === "independent" ? [] : plist,
      faction: s.type === "independent" ? null : s.faction || null,
    });
  }
  const sysOf = new Map();
  for (const s of out) for (const r of systems.find((x) => x.key === s.id).rows) sysOf.set(r.h, s);

  // Which other systems a gateway tag also mines in.
  const tagSystems = new Map();
  for (const s of out) for (const p of s.planets) if (!p.house) {
    const k = norm(p.tag);
    if (!tagSystems.has(k)) tagSystems.set(k, new Set());
    tagSystems.get(k).add(s.name);
  }
  for (const s of out) for (const p of s.planets) if (!p.house) p.alsoIn = [...tagSystems.get(norm(p.tag))].filter((n) => n !== s.name);

  // Live sessions for Lazarus gateways, matched by their secondary tag.
  const laz = out.find((s) => s.name === "Lazarus");
  if (laz && lazarusGateways.length) {
    const live = new Map();
    for (const g of lazarusGateways) {
      if (g.own) continue;
      const k = norm(g.secondary_tag || g.name);
      if (!k) continue;
      const v = live.get(k) || { sessions: 0, online: 0, software: new Set(), work: 0, accepted: 0, rejected: 0, connectedMax: 0, lastShare: null };
      v.sessions++;
      if (!g.offline) v.online++;
      if (g.user_agent) v.software.add(clean(String(g.user_agent).split("/")[0]));
      v.work += Number(g.work) || 0; v.accepted += Number(g.accepted) || 0; v.rejected += Number(g.rejected) || 0;
      v.connectedMax = Math.max(v.connectedMax, Number(g.connected_s) || 0);
      if (g.last_share_s != null) v.lastShare = v.lastShare == null ? g.last_share_s : Math.min(v.lastShare, g.last_share_s);
      live.set(k, v);
    }
    const seen = new Set();
    for (const p of laz.planets) {
      const v = live.get(norm(p.tag));
      if (!v) continue;
      seen.add(norm(p.tag));
      p.live = { sessions: v.sessions, online: v.online, software: [...v.software].slice(0, 3), connectedS: v.connectedMax, lastShareS: v.lastShare,
                 rejectPct: v.accepted ? Math.round((10000 * v.rejected) / (v.accepted + v.rejected)) / 100 : 0 };
    }
    // Gateways online now that have not found a block yet: moons still forming.
    for (const [k, v] of live) {
      if (seen.has(k) || !v.online) continue;
      const g = lazarusGateways.find((x) => norm(x.secondary_tag || x.name) === k);
      laz.planets.push({ tag: clean(g.secondary_tag || g.name), house: false, blocks: 0, blocks7d: 0, firstHeight: null, last: null, estHashrate: null, policy: null, alsoIn: [],
        live: { sessions: v.sessions, online: v.online, software: [...v.software].slice(0, 3), connectedS: v.connectedMax, lastShareS: v.lastShare,
                rejectPct: v.accepted ? Math.round((10000 * v.rejected) / (v.accepted + v.rejected)) / 100 : 0 } });
    }
    laz.gateways = laz.planets.filter((p) => !p.house).length;
  }
  // Lazarus reports its public stratum live: hashrate and miners on the Imperial outpost.
  if (laz && lazarusPool) {
    const house = laz.planets.find((p) => p.house);
    if (house) house.liveStratum = { hashrate: Math.round(Number(lazarusPool.stratum_hr_ghs || 0) * 1e9), miners: Number(lazarusPool.stratum_hr_miners) || 0 };
  }

  out.sort((a, b) => b.blocks - a.blocks);
  const recent = recs.slice(-40).reverse().map((r) => {
    const sy = sysOf.get(r.h); let t = sy && sy.type !== "independent" ? planetOf(sy.name, r) : null;
    // A block a pool built itself came through that pool's own stratum, whether or not the pool
    // also takes DATUM: that endpoint is the outpost, and the tag has to match the one the map
    // gives the outpost world so the flash and the chain land on it.
    const viaStratum = !!(sy && sy.type !== "independent" && t == null);
    if (viaStratum) t = sy.type === "stratum" ? `${sy.name} (pool-built templates)` : `${sy.name} public stratum`;
    return { height: r.h, ts: r.t, id: r.id, system: sy ? sy.name : "?", systemType: sy ? sy.type : null, planet: t, viaStratum };
  });
  const tip = recs.length ? recs[recs.length - 1] : null;
  return {
    asOf: now, tip: tip && { height: tip.h, ts: tip.t, id: tip.id }, forkHeight: FORK_HEIGHT,
    blocks: recs.length, blocks7d: week, coverage, networkHashrate,
    counts: { datum: out.filter((s) => s.type === "datum").length, stratum: out.filter((s) => s.type === "stratum").length, independent: out.filter((s) => s.type === "independent").length },
    systems: out, recent,
  };
}
