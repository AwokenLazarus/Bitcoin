// /naughtylist/ as finished HTML. The Worker renders it, so the page reads the same with
// JavaScript off: the lists are tables, the pool filter and the address lookup are links and a
// GET form. /assets/naughty.js only adds a filter box and column sorting on top.
// Every value that came from a coinbase or a pool's API goes through esc().

import { esc, FOOT, HEAD, hr, n, plural, q, safeUrl, when, xbt } from "./shell.js";

const CSS = "/assets/naughty.css?v=2";
const SCRIPT = `<script src="/assets/naughty.js?v=1" defer></script>`;

const STRATUM = {
  yes: ["empire", "Open"],
  only: ["empire", "Stratum only"],
  closed: ["rim", "Closed"],
  hosted: ["empire", "Yes: behind gateways the pool runs"],
  front: ["empire", "Fake pool: a stratum front for another pool's window"],
  no: ["rim", "None: DATUM only"],
  unknown: ["", "Not known"],
};

// The list is rebuilt every five minutes. Past these ages something upstream has stopped.
const STALE_VIEW_SEC = 20 * 60;
const STALE_WATCH_SEC = 2 * 3600;
const STALE_POOL_SEC = 3600;

const ago = (sec) => sec < 90 ? "just now" : sec < 5400 ? `${Math.round(sec / 60)} min ago` : sec < 2 * 86400 ? `${Math.round(sec / 3600)} h ago` : `${Math.round(sec / 86400)} days ago`;
const day = (ts) => ts ? new Date(ts * 1000).toISOString().slice(0, 10) : "—";

/** How a hosted-gateway share was proved, in words: by the job itself, and by the gateway name. */
function proofSplit(p) {
  if (p.direct == null && p.directBlocks == null) return "";
  const direct = p.direct != null ? p.direct : p.directBlocks;
  return ` ${n(direct)} of those are tied to a job its public stratum port handed out; the other ${n(p.hostedBlocks - direct)} carry a gateway name proved that way by another block.`;
}

/** One row's evidence. `long` is for the address page, where there is room for the whole account. */
export function evidence(r, long = false) {
  switch (r.why) {
    case "stratum-endpoint": return "Live session on the pool's stratum endpoint";
    case "pool-stratum-work": return r.stratumPct != null ? `${r.stratumPct}% of its work in the pool's window is stratum` : "Stratum work in the pool's window";
    case "pool-live-stratum": return "Hashing on the pool's stratum product";
    case "pool-stratum-block": return `The pool names it the finder of block ${n(r.evidenceHeight)}, which the pool built`;
    case "custodial-block": return `The pool's own payout address: ${r.blocks ? plural(r.blocks, "block", "blocks") : "a block"} paid to it alone. Its hashers are not named on chain`;
    case "split-payee": return `Paid by ${plural(r.paidBlocks, "split block", "split blocks")}. The pool built ${r.share}% of its blocks itself (${n(r.stratumBlocks)} of ${n(r.stratumBlocks + r.gatewayBlocks)}, by its own block list)`;
    case "behind-pool-gateway": {
      const seen = `Seen there ${r.firstSeen ? `from ${when(r.firstSeen)}` : ""}${r.lastSeen ? ` to ${when(r.lastSeen)}` : ""}`.replace(/\s+/g, " ");
      if (!long) return `Behind ${r.pool}'s own gateway on ${r.host || "Lazarus"}'s DATUM port`;
      return `Hashing on ${r.pool}'s stratum. ${r.pool} ran a gateway of its own into ${r.host || "Lazarus"}'s DATUM window${r.gatewayKey ? ` (gateway ${r.gatewayKey})` : ""}, connected from the server behind its public stratum port, so ${r.pool}'s node built this address's blocks and ${r.host || "Lazarus"}'s coinbase paid it. ${seen}`;
    }
    case "hosted-gateway": {
      const base = `Paid by ${plural(r.paidBlocks, "block", "blocks")} built on gateways the pool runs itself`;
      if (!long) return base;
      return `${base}${r.gatewayNames && r.gatewayNames.length ? ` (${r.gatewayNames.join(", ")})` : ""}: ${r.share}% of its gateway blocks (${n(r.hostedBlocks)} of ${n(r.hostedBlocks + r.otherGatewayBlocks)}).${proofSplit(r)}`;
    }
    case "no-gateway-tag": return "On the pool's miner list with no gateway tag";
    default: return r.list === "nice" ? "Was on stratum. Now on its own node" : "";
  }
}

/** Is it hashing there now, or when did the source last show it. */
function statusCell(r, now) {
  if (r.live === "stratum") return `<td data-v="${now}"><span class="pill empire live-now">Now</span></td>`;
  const ts = r.seenTs || r.lastSeen || r.lastStratumTs || 0;
  if (!ts) return `<td data-v="0">—</td>`;
  return `<td class="seen" data-v="${ts}" title="${esc(when(ts))}">${esc(ago(Math.max(0, now - ts)))}</td>`;
}

const LOOKUP = (value = "") => `
    <form class="lookup" method="get" action="/naughtylist/">
      <label for="a">Check one address</label>
      <div class="lookup-row">
        <input id="a" name="a" type="text" inputmode="latin" autocomplete="off" spellcheck="false" placeholder="bc1q… or 1…" value="${esc(value)}" required>
        <button class="btn small" type="submit">Look up</button>
      </div>
    </form>`;

const addrCell = (address) => `<td class="addr"><a href="/naughtylist/?a=${q(address)}">${esc(address)}</a></td>`;
const poolCell = (pool) => `<td><a href="/naughtylist/?pool=${q(pool)}">${esc(pool)}</a></td>`;

function table(id, heads, rows, empty, cls = "") {
  if (!rows.length) return `<p class="status" id="${id}">${esc(empty)}</p>`;
  return `<div class="table-wrap"><table class="list${cls ? ` ${cls}` : ""}" id="${id}"><thead><tr>${heads.map((h) => `<th${h.num ? ' class="num"' : ""}${h.sort ? ` data-sort="${h.sort}"` : ""}>${esc(h.t)}</th>`).join("")}</tr></thead><tbody>
${rows.join("\n")}
</tbody></table></div>`;
}

/** What is not up to date, as sentences. An empty list means every source answered in time. */
export function staleness(view, now) {
  const out = [];
  if (view.asOf && now - view.asOf > STALE_VIEW_SEC) out.push(`The list was last rebuilt ${ago(now - view.asOf)}. It is normally rebuilt every five minutes, so the figures below are older than they look.`);
  const c = view.coverage;
  if (c && c.of && c.have < c.of) out.push(`${n(c.have)} of the ${n(c.of)} blocks in the fourteen-day window have been read so far; the rest are being read now.`);
  else if (!c && view.windowDone === false) out.push("Coinbases are still being read back from the tip.");
  const w = view.watch;
  if (w && w.asOf && view.asOf - w.asOf > STALE_WATCH_SEC) out.push(`The listener that ties blocks to pool-run gateways last reported ${ago(now - w.asOf)}. Names already proved stand; blocks found since then have not been checked.`);
  const late = (view.pools || []).filter((p) => p.reader && p.pool !== "Lazarus" && (p.err || (p.ok && view.asOf - p.ok > STALE_POOL_SEC)));
  if (late.length) out.push(`${late.map((p) => p.pool).join(", ")} did not answer on the last read, so ${late.length === 1 ? "its rows show" : "their rows show"} what ${late.length === 1 ? "it" : "they"} last said. A pool silent for six hours drops off the confirmed table.`);
  return out;
}

const SUSPECT_GROUP = {
  "hosted-gateway": (pool, rows) => {
    const r = rows[0];
    return `<strong>${esc(pool)}</strong> builds ${r.share}% of its gateway-tagged blocks on gateways it runs itself (${n(r.hostedBlocks)} of ${n(r.hostedBlocks + r.otherGatewayBlocks)} in fourteen days${r.gatewayNames && r.gatewayNames.length ? `, under the names ${esc(r.gatewayNames.join(", "))}` : ""}).${esc(proofSplit(r))} Each address below was paid by such blocks. The split also pays the pool's real gateway owners, and the pool's own fee address if it takes one, so this is not proof.`;
  },
  "split-payee": (pool, rows) => {
    const r = rows[0];
    return `<strong>${esc(pool)}</strong> built ${r.share}% of its blocks itself (${n(r.stratumBlocks)} of ${n(r.stratumBlocks + r.gatewayBlocks)}, by its own block list). Each address below was paid by its splits and the pool does not say which protocol it uses, so a miner on their own node can be here, and so can the pool's own fee address.`;
  },
  "no-gateway-tag": (pool) => `<strong>${esc(pool)}</strong> lists each address below with no gateway tag. A gateway that writes no tag looks the same.`,
};

/** Suspects, one collapsible group per pool and reason, with the reason said once. */
function suspectGroups(suspects, open) {
  const groups = new Map();
  for (const r of suspects) {
    const k = `${r.pool}|${r.why}`;
    if (!groups.has(k)) groups.set(k, []);
    groups.get(k).push(r);
  }
  return [...groups.values()].sort((a, b) => b.length - a.length).map((rows) => {
    const { pool, why } = rows[0];
    const body = rows.map((r) => `<tr>${addrCell(r.address)}<td class="num" data-v="${r.paidBlocks || 0}">${r.paidBlocks ? n(r.paidBlocks) : "—"}</td><td class="num" data-v="${r.lastStratumHeight || 0}"${r.lastStratumTs ? ` title="${esc(when(r.lastStratumTs))}"` : ""}>${r.lastStratumHeight ? n(r.lastStratumHeight) : "—"}</td></tr>`);
    const say = (SUSPECT_GROUP[why] || ((p) => `<strong>${esc(p)}</strong>`))(pool, rows);
    return `<details class="group"${open || rows.length <= 25 ? " open" : ""}>
<summary><span class="group-name">${esc(pool)}</span> <span class="count">${plural(rows.length, "address", "addresses")}</span></summary>
<p class="note">${say}</p>
${table(`suspect-${q(pool)}-${why}`, [{ t: "Address" }, { t: "Blocks that paid it", num: 1, sort: "num" }, { t: "Newest such block", num: 1, sort: "num" }], body, "")}
</details>`;
  }).join("\n");
}

/** The list page. `view` is the document the cron stores; `pool` filters to one pool's rows. */
export function renderList(view, { pool = "", now = Math.floor(Date.now() / 1000) } = {}) {
  const pools = view.pools || [];
  const known = pools.find((p) => p.pool === pool);
  const only = known ? known.pool : "";
  const pick = (rows) => (only ? rows.filter((r) => r.pool === only) : rows);
  const naughty = pick(view.naughty || []), suspects = pick(view.suspects || []), nice = pick(view.nice || []);
  const liveNow = naughty.filter((r) => r.live === "stratum");
  const finders = only && only !== "dxpool" ? [] : (view.finders || []);
  const title = only ? `Naughty list: stratum hashers on ${only} | Lazarus` : "Naughty list: who is still hashing on stratum, on every pool | Lazarus";
  const description = "Addresses hashing on a pool's stratum endpoint on Bitcoin BLAKE2b (XBT), from each pool's own public data. Confirmed and suspected are kept apart. Works without JavaScript.";
  const withData = pools.filter((p) => p.reader).length;
  const stale = staleness(view, now);
  const liveHr = liveNow.reduce((s, r) => s + (r.hrGhs || 0), 0);

  const chips = pools.filter((p) => p.confirmed || p.suspects).map((p) => `<a class="chip"${p.pool === only ? ' aria-current="true"' : ""} href="/naughtylist/?pool=${q(p.pool)}">${esc(p.pool)} <span>${n(p.confirmed + p.suspects)}</span></a>`).join("");

  const poolRows = pools.map((p) => {
    const [cls, label] = STRATUM[p.stratum] || STRATUM.unknown;
    const own = !p.own ? "—" : p.own.d == null ? n(p.own.s) : `${n(p.own.s)} of ${n(p.own.s + p.own.d)}`;
    const late = p.err || (p.ok && view.asOf - p.ok > STALE_POOL_SEC);
    const read = !p.reader ? (p.stratum === "no" ? "Nothing to read" : "No usable list") : !p.ok ? (p.err ? "Did not answer" : "Not read yet") : `${late ? "Did not answer. Last read " : ""}${ago(Math.max(0, now - p.ok))}`;
    return `<tr${p.pool === only ? ' aria-current="true"' : ""}>
<td><a href="/naughtylist/?pool=${q(p.pool)}">${esc(p.pool)}</a></td>
<td>${cls || label !== "Not known" ? `<span class="pill ${cls}">${esc(label)}</span>` : "Not checked"}</td>
<td class="num" data-v="${p.blocks || 0}">${p.blocks ? n(p.blocks) : "—"}</td>
<td class="num" data-v="${p.d || 0}">${p.blocks ? n(p.d) : "—"}</td>
<td class="num" data-v="${p.hosted ? p.hosted.hostedBlocks : 0}">${p.hosted ? n(p.hosted.hostedBlocks) : "—"}</td>
<td class="num">${own}</td>
<td class="num" data-v="${p.confirmed || 0}">${p.confirmed ? n(p.confirmed) : "—"}</td>
<td class="num" data-v="${p.suspects || 0}">${p.suspects ? n(p.suspects) : "—"}</td>
<td class="num${late ? " late" : ""}">${esc(read)}</td>
</tr>`;
  });

  const naughtyRows = naughty.map((r) => `<tr>${addrCell(r.address)}${poolCell(r.pool)}${statusCell(r, now)}<td class="why">${esc(evidence(r))}</td><td class="num" data-v="${r.hrGhs || 0}">${hr(r.hrGhs)}</td></tr>`);
  const niceRows = nice.map((r) => `<tr>${addrCell(r.address)}${poolCell(r.pool)}<td class="why">${esc(evidence(r))}</td><td class="num" data-v="${r.hrGhs || 0}">${hr(r.hrGhs)}</td></tr>`);
  const finderRows = finders.map((f) => `<tr><td class="num">${n(f.h)}</td><td class="num">${when(f.t)}</td><td class="addr">${esc(f.name)}</td></tr>`);
  const how = pools.filter((p) => p.reads).map((p) => {
    const hosted = p.hosted ? ` Proved so far, last 14 days: ${n(p.hosted.hostedBlocks)} of its ${n(p.hosted.hostedBlocks + p.hosted.otherGatewayBlocks)} gateway blocks (${p.hosted.share}%) were built on gateways it runs itself, under the names ${esc(p.hosted.names.join(", "))}.${esc(proofSplit(p.hosted))}` : "";
    const front = p.front && p.front.last ? ` Its gateway was last seen on Lazarus's DATUM port ${esc(when(p.front.last))}${view.asOf - p.front.last > 3600 ? `, ${esc(ago(now - p.front.last))}: it is not connected now` : ""}.` : "";
    return `<dt id="how-${q(p.pool)}">${p.site ? `<a href="${safeUrl(p.site)}" rel="noopener nofollow external">${esc(p.pool)}</a>` : esc(p.pool)}</dt><dd>${esc(p.reads)}${hosted}${front}</dd>`;
  }).join("\n");

  return `${HEAD(title, description, "https://lazarus-xbt.xyz/naughtylist/", "/naughtylist/", CSS)}
    <p class="kicker">Bitcoin BLAKE2b · every pool</p>
    <h1>Still hashing on stratum.</h1>
    <p class="lede">A stratum hasher lets the pool write the block. A DATUM miner writes it on their own node. An address is listed here when a pool's own public data shows it on that pool's stratum endpoint. ${n(withData)} pools publish enough to tell, and each row says what it rests on.</p>
    <dl class="statstrip tally">
      <div><dt>On stratum now</dt><dd>${n(liveNow.length)}<small>${liveHr ? `${hr(liveHr)} seen live` : "live sessions"}</small></dd></div>
      <div><dt>Confirmed, 14 days</dt><dd>${n(naughty.length)}<small>a pool's data names the address</small></dd></div>
      <div><dt>Suspected</dt><dd>${n(suspects.length)}<small>the odds only, not proof</small></dd></div>
      <div><dt>Moved to own node</dt><dd>${n(nice.length)}<small>the nice list</small></dd></div>
    </dl>
    <p class="status">Rebuilt ${esc(ago(Math.max(0, now - (view.asOf || now))))} (${when(view.asOf)}), chain tip ${n(view.tip)}.${view.coverage ? ` ${n(view.coverage.have)} of ${n(view.coverage.of)} blocks of the last fourteen days read.` : ""}</p>
${stale.length ? `    <div class="callout warn" role="status"><p><strong>Not fully up to date.</strong></p><ul>${stale.map((s) => `<li>${esc(s)}</li>`).join("")}</ul></div>` : ""}
${LOOKUP()}
    <nav class="chips" aria-label="Show one pool"><a class="chip"${only ? "" : ' aria-current="true"'} href="/naughtylist/">Every pool</a>${chips}</nav>
    <p class="jump"><a href="#confirmed">Confirmed (${n(naughty.length)})</a> · <a href="#suspected">Suspected (${n(suspects.length)})</a> · <a href="#nice">Nice list (${n(nice.length)})</a> · <a href="#pools">Pools</a> · <a href="#how">How each pool is read</a> · <a href="/api/naughtylist">JSON</a></p>
    <p class="filter" hidden><label for="filter">Filter the tables</label> <input id="filter" type="search" autocomplete="off" spellcheck="false" placeholder="part of an address"> <span id="filter-count" class="count" aria-live="polite"></span></p>

    <h2 id="confirmed">Confirmed on stratum <span class="count">(${n(naughty.length)})</span></h2>
    <p class="note">A pool's own data names each of these. <strong>Now</strong> means the source shows it hashing on stratum at the last read. Otherwise the column says when the source last showed it; a row leaves after fourteen days, or sooner when the same source shows the address on its own node.</p>
    ${table("naughty-table", [{ t: "Address" }, { t: "Pool", sort: "text" }, { t: "Last shown", sort: "num" }, { t: "What shows it" }, { t: "Stratum hashrate", num: 1, sort: "num" }], naughtyRows, only ? `No address on ${only} is confirmed on stratum in this window.` : "No address is confirmed on stratum in this window.", "filterable")}

    <h2 id="suspected">Suspected <span class="count">(${n(suspects.length)})</span></h2>
    <p class="note">Not proof. The chain alone cannot settle it: a split pays stratum hashers and gateway operators alike, and some gateways write no tag in the coinbase. Each group says what the suspicion rests on.</p>
    ${suspects.length ? suspectGroups(suspects, !!only) : `<p class="status">${esc(only ? `No suspect on ${only}.` : "No suspects in this window.")}</p>`}
${finderRows.length ? `
    <h2 id="finders">dxpool stratum finders, named in part <span class="count">(${n(finderRows.length)})</span></h2>
    <p class="note">dxpool's stratum pool publishes the finder of each block as the first and last characters of <code>address.worker</code> or of an account name. These are its stratum blocks of the last fourteen days.</p>
    <details class="group"><summary><span class="group-name">Blocks</span> <span class="count">${n(finderRows.length)}</span></summary>
    ${table("finder-table", [{ t: "Block", num: 1 }, { t: "Found", num: 1 }, { t: "Finder, as published" }], finderRows, "")}</details>` : ""}

    <h2 id="nice">Nice list <span class="count">(${n(nice.length)})</span></h2>
    <p class="note">Addresses we saw on stratum that the same source now shows on their own node. A miner who only ever ran a node is on neither list.</p>
    ${table("nice-table", [{ t: "Address" }, { t: "Pool", sort: "text" }, { t: "What shows it" }, { t: "Hashrate", num: 1, sort: "num" }], niceRows, "No converted address in this window yet.", "filterable")}

    <h2 id="pools">Pools</h2>
    <p class="note">Every pool with blocks in the last fourteen days, and every pool we have checked. A block with a gateway tag was built on a miner's node unless the gateway is proved to be the pool's own. One without a tag may be the pool's stratum or a gateway that writes none, so nothing is classified from that alone.</p>
    ${table("pool-table", [{ t: "Pool", sort: "text" }, { t: "Stratum endpoint" }, { t: "Blocks, 14 days", num: 1, sort: "num" }, { t: "Gateway tag, miner's own", num: 1, sort: "num" }, { t: "Gateway tag, pool-run", num: 1, sort: "num" }, { t: "Pool-built, by its own list", num: 1 }, { t: "Confirmed", num: 1, sort: "num" }, { t: "Suspected", num: 1, sort: "num" }, { t: "Pool data read", num: 1 }], poolRows, "No pool has been read yet.")}

    <h2 id="how">How each pool is read</h2>
    <dl class="how">
${how}
    </dl>
    <p class="note">A pool not named here is listed in the table above as soon as it finds a block, marked not checked, and none of its payees are classified until its data has been read. Corrections: <a href="https://github.com/AwokenLazarus/Lazarus-Pool/issues" rel="noopener">open an issue</a>.</p>
${SCRIPT}
${FOOT}`;
}

const PAY_LIMIT = 100;
const KIND = {
  "datum-block": ["kind-d", "Gateway-tagged block"],
  "pool-gateway": ["kind-s", "Built on a gateway the pool runs itself"],
  custodial: ["kind-s", "Block paid to the pool alone"],
  stratum: ["kind-s", "Split with no gateway tag"],
};

/** One address: what each source says, and the coinbase outputs that paid it. */
export function renderAddress(d, { all = false, now = Math.floor(Date.now() / 1000) } = {}) {
  const address = d.address;
  const title = `${address} on the naughty list | Lazarus`;
  const head = HEAD(title, "What each Bitcoin BLAKE2b pool's public data says about one address, and the coinbase outputs that paid it.", "https://lazarus-xbt.xyz/naughtylist/", "/naughtylist/", CSS);
  if (d.error) {
    return `${head}
    <p class="kicker">Naughty list · one address</p>
    <h1>That is not an address.</h1>
    <p class="lede">Enter a full Bitcoin address: <code>bc1…</code>, <code>1…</code> or <code>3…</code>.</p>
${LOOKUP(String(address || "").slice(0, 100))}
    <p><a href="/naughtylist/">Back to the list</a></p>
${FOOT}`;
  }
  const listed = d.listed;
  const live = listed && listed.live === "stratum";
  const [cls, verdict] = !listed ? ["", "Not on either list"]
    : listed.list === "naughty" ? ["empire", live ? `On stratum now at ${listed.pool}` : `Confirmed on stratum at ${listed.pool}`]
    : listed.list === "suspect" ? ["rebel", `Suspected, not confirmed, at ${listed.pool}`]
    : ["rim", `On the nice list at ${listed.pool}`];
  const lastShown = listed && listed.list === "naughty" && !live && (listed.seenTs || listed.lastSeen) ? ` Last shown ${ago(Math.max(0, now - (listed.seenTs || listed.lastSeen)))}.` : "";
  const says = [];
  if (listed) says.push(`<li><strong>${esc(listed.pool)}.</strong> ${esc(evidence(listed, true))}.</li>`);
  for (const s of d.stored || []) {
    if (listed && s.pool === listed.pool) continue;
    says.push(`<li><strong>${esc(s.pool)}.</strong> ${esc(s.text)}</li>`);
  }
  for (const a of d.asked || []) {
    if (a.failed) { says.push(`<li><strong>${esc(a.pool)}, asked just now.</strong> Did not answer in time.</li>`); continue; }
    const bits = [];
    if (a.pool === "B2Pool") {
      bits.push(a.hrGhs ? `${hr(a.hrGhs)} over the last hour` : "No hashrate in the last hour");
      bits.push(`${plural(a.blocksFound, "block", "blocks")} found`);
      if (a.stratumBlock) bits.push(`newest stratum block ${n(a.stratumBlock)}`);
      if (a.gatewayBlock) bits.push(`newest gateway block ${n(a.gatewayBlock)}`);
    } else if (a.pool === "dxpool") {
      bits.push(a.hrGhs ? `${hr(a.hrGhs)} on its stratum pool right now` : "No hashrate on its stratum pool right now");
      if (a.paidXbt) bits.push(`${a.paidXbt} XBT paid from its stratum pool so far`);
    }
    says.push(`<li><strong>${esc(a.pool)}, asked just now.</strong> ${esc(bits.join(". "))}.</li>`);
  }
  const pays = d.pays || [];
  // What paid it, by pool and by who built the block.
  const tally = new Map();
  for (const p of pays) {
    const k = `${p.pool}|${p.via ? "via" : p.kind}`;
    const t = tally.get(k) || { pool: p.pool, kind: p.kind, via: p.via, blocks: 0, sats: 0 };
    t.blocks++;
    t.sats += Number(p.sats) || 0;
    tally.set(k, t);
  }
  const label = (p) => p.via ? `Earned on ${esc(p.via)}'s stratum, behind its gateway on ${esc(p.pool)}` : (KIND[p.kind] || KIND.stratum)[1];
  const kindCls = (p) => p.via ? "kind-s" : (KIND[p.kind] || KIND.stratum)[0];
  const tallyRows = [...tally.values()].sort((x, y) => y.sats - x.sats).map((t) => `<tr><td>${esc(t.pool || "—")}</td><td class="${kindCls(t)}">${label(t)}</td><td class="num">${n(t.blocks)}</td><td class="num">${xbt(t.sats)}</td></tr>`);
  const shown = all ? pays : pays.slice(0, PAY_LIMIT);
  const payRows = shown.map((p) => `<tr><td class="num">${n(p.height)}</td><td class="num">${when(p.ts)}</td><td class="num">${xbt(p.sats)}</td><td>${esc(p.pool || "—")}</td><td class="${kindCls(p)}">${label(p)}</td><td>${d.utxosRead === false ? "—" : p.unspent ? "unspent" : "spent"}</td></tr>`);
  const total = pays.reduce((s, p) => s + (Number(p.sats) || 0), 0);
  const unspentSay = d.utxosRead === false ? "The explorer did not answer, so spent and unspent are not marked." : `${plural(d.unspentCount || 0, "output is", "outputs are")} still unspent (${xbt(d.unspentSats || 0)}).`;
  return `${head}
    <p class="kicker">Naughty list · one address</p>
    <h1 class="addr-title">${esc(address)}</h1>
    <p class="lede"><span class="pill ${cls} verdict">${esc(verdict)}</span>${esc(lastShown)}</p>
    <h2>What the pools say</h2>
    ${says.length ? `<ul class="says">${says.join("\n")}</ul>` : `<p class="status">No pool we read shows this address in its current data.</p>`}
    <p class="note">B2Pool and dxpool answer for one address at a time, so they are asked when this page is opened. A pool that hides its miner list cannot be asked.${d.asOf ? ` The list was rebuilt ${esc(ago(Math.max(0, now - d.asOf)))}.` : ""}</p>
    <h2>Coinbase outputs, last fourteen days</h2>
    <p class="note">${plural(pays.length, "coinbase output", "coinbase outputs")} paid this address ${xbt(total)} in the fourteen days of blocks indexed. ${esc(unspentSay)} A block's kind describes who built the block, not this address: a split pays everyone in the pool's window. <a href="${safeUrl(d.explorer)}" rel="noopener">Open the address on the explorer</a>.</p>
    ${tallyRows.length ? table("pay-tally", [{ t: "Pool" }, { t: "Who built the block" }, { t: "Blocks", num: 1 }, { t: "Paid", num: 1 }], tallyRows, "") : ""}
    ${pays.length ? `<h3>Each output${shown.length < pays.length ? `, newest ${n(shown.length)} of ${n(pays.length)}` : ""}</h3>` : ""}
    ${table("pay-table", [{ t: "Block", num: 1 }, { t: "When", num: 1 }, { t: "Amount", num: 1 }, { t: "Pool" }, { t: "Who built the block" }, { t: "Output" }], payRows, "No coinbase output for this address is in the index. The chain read covers fourteen days back from the tip.")}
    ${shown.length < pays.length ? `<p><a href="/naughtylist/?a=${q(address)}&amp;all=1">Show all ${n(pays.length)} outputs</a></p>` : ""}
${LOOKUP()}
    <p><a href="/naughtylist/">Back to the list</a></p>
${FOOT}`;
}
