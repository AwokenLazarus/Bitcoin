// The hub's live parts, written into the HTML before it leaves the Worker, so a reader with
// JavaScript off gets the figures, the pool directory and the latest blocks, not placeholders.
// hub.js still refreshes the same elements in a browser that runs it.
//
// Also /map/table/: the galaxy map's data as plain tables, for a browser with no script or WebGL.

import { esc, FOOT, HEAD, n, safeUrl, when } from "./shell.js";

const UNITS = ["H/s", "kH/s", "MH/s", "GH/s", "TH/s", "PH/s", "EH/s", "ZH/s"];
export function hashrate(hs) {
  if (!Number.isFinite(hs) || hs <= 0) return "—";
  let i = 0, v = hs;
  while (v >= 1000 && i < UNITS.length - 1) { v /= 1000; i++; }
  return `${v >= 100 ? Math.round(v) : v.toFixed(v >= 10 ? 1 : 2)} ${UNITS[i]}`;
}
const pct = (v, d = 1) => (Number.isFinite(v) ? `${v.toFixed(d)}%` : "—");
const clock = (ts) => (ts ? `${new Date(ts * 1000).toISOString().slice(11, 16)} UTC` : "");
function duration(s) {
  if (!Number.isFinite(s) || s <= 0) return "—";
  const d = Math.floor(s / 86400), h = Math.round((s % 86400) / 3600);
  if (d >= 1) return `${d}d ${h}h`;
  const m = Math.round((s % 3600) / 60);
  return s >= 3600 ? `${Math.floor(s / 3600)}h ${m}m` : `${Math.max(1, Math.round(s / 60))} min`;
}

const FACTION = { datum: ["rebel", "DATUM"], stratum: ["empire", "Stratum only"], independent: ["rim", "Solo"] };

function poolRow(s) {
  const [cls, label] = FACTION[s.type] || FACTION.independent;
  const name = s.link ? `<a href="${safeUrl(s.link)}" rel="noopener nofollow external">${esc(s.name)}</a>` : esc(s.name);
  return `<tr><td>${name}</td><td><span class="pill ${cls}">${label}</span></td><td class="num">${pct(s.share7d)}</td><td class="num">${n(s.blocks7d)}</td><td class="num">${hashrate(s.estHashrate)}</td><td class="num">${n(s.gateways)}</td></tr>`;
}

const pools7d = (galaxy) => (galaxy.systems || [])
  .filter((s) => s.type !== "independent" && (s.blocks7d || 0) > 0)
  .sort((a, b) => (b.share7d || 0) - (a.share7d || 0));

/** Fill a hub page. Either source may be null: what cannot be filled keeps its static text. */
export function fillHub(res, { stats, galaxy }) {
  const rw = new HTMLRewriter();
  const text = (sel, value, sub) => rw.on(sel, { element(el) { el.setInnerContent(`${esc(value)}${sub ? `<small>${esc(sub)}</small>` : ""}`, { html: true }); } });
  if (stats) {
    const p = stats.pool || {}, net = stats.network || {}, g = stats.galaxy || {};
    text("#s-nethash", hashrate(net.hashrate));
    text("#s-height", n(net.height));
    text("#s-interval", duration(net.blockInterval), "average time between blocks");
    text("#s-pool", pct(p.sharePercent), `${n(p.gateways)} DATUM gateways`);
    text("#s-systems", n(g.systems), `${n(g.blocks7d)} blocks in 7 days`);
    text("#s-blocks", n(p.blocksFound), "blocks found by Lazarus");
    text("#s-miners", n(p.miners), "miners on Lazarus");
    if (Number.isFinite(net.difficulty)) {
      text("#s-difficulty", `${(net.difficulty / 1e9).toFixed(2)} G`, net.retargetChange != null ? `${net.retargetChange >= 0 ? "+" : ""}${net.retargetChange.toFixed(1)}% in ${n(net.retargetBlocks)} blocks` : "");
    }
    if (Number.isFinite(p.stratumFeePercent)) text("[data-fee-stratum]", `${p.stratumFeePercent}%`);
    if (Number.isFinite(p.datumRebatePercent)) text("[data-fee-rebate]", `${p.datumRebatePercent}%`);
    text(".live", `as of ${clock(stats.asOf)}`);
  }
  if (galaxy) {
    const rows = pools7d(galaxy).slice(0, 14);
    if (rows.length) rw.on("#pool-rows", { element(el) { el.setInnerContent(rows.map(poolRow).join(""), { html: true }); } });
    const counts = galaxy.counts || {};
    text("#pool-asof", `${n((counts.datum || 0) + (counts.stratum || 0))} pools seen since the fork · as of ${clock(galaxy.asOf)}`);
    const recent = (galaxy.recent || []).slice(0, 7);
    if (recent.length) {
      rw.on("#latest-blocks", { element(el) {
        el.setInnerContent(recent.map((b) => {
          const who = b.planet && b.planet !== b.system ? `${b.system} · ${b.planet}` : b.system || "unknown";
          return `<li><span class="h">${n(b.height)}</span><span class="who" title="${esc(who)}">${esc(who)}</span><span class="t">${clock(b.ts)}</span></li>`;
        }).join(""), { html: true });
      } });
    }
  }
  return rw.transform(res);
}

/** The galaxy map as tables: every pool, then each pool's gateways, then the miners on their own. */
export function renderMapTable(galaxy) {
  const systems = (galaxy.systems || []).slice().sort((a, b) => (b.blocks7d || 0) - (a.blocks7d || 0) || (b.blocks || 0) - (a.blocks || 0));
  const pools = systems.filter((s) => s.type !== "independent");
  const solo = systems.filter((s) => s.type === "independent" && (s.blocks7d || 0) > 0);
  const head = `<thead><tr><th>Pool</th><th>Type</th><th class="num">Share (7d)</th><th class="num">Blocks (7d)</th><th class="num">Est. hashrate</th><th class="num">Gateways</th></tr></thead>`;
  const gateways = pools.filter((s) => (s.planets || []).length).map((s) => {
    const rows = s.planets.slice().sort((a, b) => (b.blocks7d || 0) - (a.blocks7d || 0) || (b.blocks || 0) - (a.blocks || 0))
      .map((p) => `<tr><td>${esc(p.tag)}${p.house ? ' <span class="pill empire">built by the pool</span>' : ""}</td><td class="num">${n(p.blocks7d)}</td><td class="num">${n(p.blocks)}</td><td class="num">${hashrate(p.estHashrate)}</td><td class="num">${p.last ? n(p.last.height) : "—"}</td></tr>`).join("\n");
    return `<h3 id="g-${esc(encodeURIComponent(s.name))}">${esc(s.name)} <span class="count">(${n(s.planets.length)})</span></h3>
    <div class="table-wrap"><table class="list"><thead><tr><th>Gateway tag</th><th class="num">Blocks (7d)</th><th class="num">Blocks since the fork</th><th class="num">Est. hashrate</th><th class="num">Last block</th></tr></thead><tbody>
${rows}
</tbody></table></div>`;
  }).join("\n");
  const soloRows = solo.map((s) => `<tr><td>${esc(s.name)}</td><td class="num">${pct(s.share7d, 2)}</td><td class="num">${n(s.blocks7d)}</td><td class="num">${n(s.blocks)}</td><td class="num">${hashrate(s.estHashrate)}</td></tr>`).join("\n");
  return `${HEAD("Galaxy map as a table: every XBT pool and gateway | Lazarus", "Every pool, DATUM gateway and solo miner on Bitcoin BLAKE2b (XBT), read from coinbase tags. The galaxy map's data as plain tables, with no JavaScript.", "https://lazarus-xbt.xyz/map/table/", "/map/", "/assets/naughty.css?v=1")}
    <p class="kicker">Galaxy map · as a table</p>
    <h1>Every pool and gateway, in rows.</h1>
    <p class="lede">The <a href="/map/">galaxy map</a> draws this in 3D and needs JavaScript and WebGL. This page is the same data with neither: who found blocks on Bitcoin BLAKE2b, read from the tags in each coinbase.</p>
    <p class="status">As of ${when(galaxy.asOf)}, chain tip ${n(galaxy.tip)}. ${n(galaxy.blocks7d)} blocks in the last seven days. Network hashrate ${hashrate(galaxy.networkHashrate)}.</p>
    <h2 id="pools">Pools</h2>
    <div class="table-wrap"><table class="list">${head}<tbody>
${pools.map(poolRow).join("\n")}
</tbody></table></div>
    <h2 id="gateways">Gateways, by pool</h2>
    <p class="note">A gateway is named by the tag its operator set. A row marked built by the pool is the pool's own stratum, not a miner's node.</p>
    ${gateways}
    <h2 id="solo">Miners on their own node <span class="count">(${n(solo.length)} with a block in 7 days)</span></h2>
    <div class="table-wrap"><table class="list"><thead><tr><th>Tag</th><th class="num">Share (7d)</th><th class="num">Blocks (7d)</th><th class="num">Blocks since the fork</th><th class="num">Est. hashrate</th></tr></thead><tbody>
${soloRows}
</tbody></table></div>
${FOOT}`;
}
