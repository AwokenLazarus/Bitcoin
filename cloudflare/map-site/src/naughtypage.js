// /naughtylist/ as finished HTML. The Worker renders it, so the page reads the same with
// JavaScript off: the lists are tables, the pool filter and the address lookup are links and a
// GET form. Every value that came from a coinbase or a pool's API goes through esc().

import { esc, FOOT, HEAD, hr, n, plural, q, safeUrl, when, xbt } from "./shell.js";

const STRATUM = {
  yes: ["empire", "Open"],
  only: ["empire", "Stratum only"],
  closed: ["rim", "Closed"],
  hosted: ["empire", "Yes: behind gateways the pool runs"],
  no: ["rim", "None: DATUM only"],
  unknown: ["", "Not known"],
};

export function evidence(r) {
  switch (r.why) {
    case "stratum-endpoint": return "Live session on the pool's stratum endpoint";
    case "pool-stratum-work": return r.stratumPct != null ? `${r.stratumPct}% of its work in the pool's window is stratum` : "Stratum work in the pool's window";
    case "pool-live-stratum": return "Hashing on the pool's stratum product now";
    case "pool-stratum-block": return `The pool names it the finder of block ${n(r.evidenceHeight)}, which the pool built`;
    case "custodial-block": return "Paid by a block the pool built and paid out alone";
    case "split-payee": return `Paid by ${plural(r.paidBlocks, "split block", "split blocks")}. The pool built ${r.share}% of its blocks itself (${n(r.stratumBlocks)} of ${n(r.stratumBlocks + r.gatewayBlocks)}, by its own block list)`;
    case "behind-pool-gateway": return `Hashing on ${r.pool}'s stratum. ${r.pool} runs a gateway of its own into ${r.host || "Lazarus"}'s DATUM window${r.gatewayKey ? ` (gateway ${r.gatewayKey})` : ""}, connected from the server behind its public stratum port, so ${r.pool}'s node builds this address's blocks and ${r.host || "Lazarus"}'s coinbase pays it. Seen behind that gateway ${r.firstSeen ? `since ${when(r.firstSeen)}` : ""}${r.lastSeen ? `, last ${when(r.lastSeen)}` : ""}`;
    case "hosted-gateway": return `Paid by ${plural(r.paidBlocks, "block", "blocks")} built on gateways the pool runs itself${r.gatewayNames && r.gatewayNames.length ? ` (${r.gatewayNames.join(", ")})` : ""}: ${r.share}% of its gateway blocks (${n(r.hostedBlocks)} of ${n(r.hostedBlocks + r.otherGatewayBlocks)}), each tied to a job its public stratum port handed out`;
    case "no-gateway-tag": return "On the pool's miner list with no gateway tag";
    default: return r.list === "nice" ? "Was on stratum. Now on its own node" : "";
  }
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

function table(id, heads, rows, empty) {
  if (!rows.length) return `<p class="status" id="${id}">${esc(empty)}</p>`;
  return `<div class="table-wrap"><table class="list" id="${id}"><thead><tr>${heads.map((h) => `<th${h.num ? ' class="num"' : ""}>${esc(h.t)}</th>`).join("")}</tr></thead><tbody>
${rows.join("\n")}
</tbody></table></div>`;
}

/** The list page. `view` is the document the cron stores; `pool` filters to one pool's rows. */
export function renderList(view, { pool = "" } = {}) {
  const pools = view.pools || [];
  const known = pools.find((p) => p.pool === pool);
  const only = known ? known.pool : "";
  const pick = (rows) => (only ? rows.filter((r) => r.pool === only) : rows);
  const naughty = pick(view.naughty || []), suspects = pick(view.suspects || []), nice = pick(view.nice || []);
  const finders = only && only !== "dxpool" ? [] : (view.finders || []);
  const title = only ? `Naughty list: stratum hashers on ${only} | Lazarus` : "Naughty list: who is still hashing on stratum, on every pool | Lazarus";
  const description = "Addresses hashing on a pool's stratum endpoint on Bitcoin BLAKE2b (XBT), from each pool's own public data. Confirmed and suspected are kept apart. Works without JavaScript.";
  const walk = view.windowDone ? "Fourteen days of coinbases are read" : `Coinbases are still being read back from the tip${view.scannedTo ? ` (through block ${n(view.scannedTo)})` : ""}`;
  const withData = pools.filter((p) => p.reader).length;

  const poolRows = pools.map((p) => {
    const [cls, label] = STRATUM[p.stratum] || STRATUM.unknown;
    const own = !p.own ? "—" : p.own.d == null ? n(p.own.s) : `${n(p.own.s)} of ${n(p.own.s + p.own.d)}`;
    const read = !p.reader ? (p.stratum === "no" ? "Nothing to read" : "No usable list") : p.err ? `Did not answer${p.ok ? `. Last read ${when(p.ok)}` : ""}` : p.ok ? when(p.ok).slice(11) : "Not read yet";
    return `<tr${p.pool === only ? ' aria-current="true"' : ""}>
<td><a href="/naughtylist/?pool=${q(p.pool)}">${esc(p.pool)}</a></td>
<td>${cls || label !== "Not known" ? `<span class="pill ${cls}">${esc(label)}</span>` : "Not checked"}</td>
<td class="num">${p.blocks ? n(p.blocks) : "—"}</td>
<td class="num">${p.blocks ? n(p.d) : "—"}</td>
<td class="num">${own}</td>
<td class="num">${p.confirmed ? n(p.confirmed) : "—"}</td>
<td class="num">${p.suspects ? n(p.suspects) : "—"}</td>
<td class="num">${esc(read)}</td>
</tr>`;
  });

  const naughtyRows = naughty.map((r) => `<tr>${addrCell(r.address)}${poolCell(r.pool)}<td>${esc(evidence(r))}</td><td class="num">${hr(r.hrGhs)}</td></tr>`);
  const suspectRows = suspects.map((r) => `<tr>${addrCell(r.address)}${poolCell(r.pool)}<td>${esc(evidence(r))}</td><td class="num">${r.lastStratumHeight ? n(r.lastStratumHeight) : "—"}</td></tr>`);
  const niceRows = nice.map((r) => `<tr>${addrCell(r.address)}${poolCell(r.pool)}<td>${esc(evidence(r))}</td><td class="num">${hr(r.hrGhs)}</td></tr>`);
  const finderRows = finders.map((f) => `<tr><td class="num">${n(f.h)}</td><td class="num">${when(f.t)}</td><td class="addr">${esc(f.name)}</td></tr>`);
  const how = pools.filter((p) => p.reads).map((p) => `<dt>${p.site ? `<a href="${safeUrl(p.site)}" rel="noopener nofollow external">${esc(p.pool)}</a>` : esc(p.pool)}</dt><dd>${esc(p.reads)}${p.hosted ? ` Proved so far, last 14 days: ${n(p.hosted.hostedBlocks)} of its ${n(p.hosted.hostedBlocks + p.hosted.otherGatewayBlocks)} gateway blocks (${p.hosted.share}%) were built on gateways it runs itself, under the names ${esc(p.hosted.names.join(", "))}.` : ""}</dd>`).join("\n");

  return `${HEAD(title, description, "https://lazarus-xbt.xyz/naughtylist/", "/naughtylist/", "/assets/naughty.css?v=1")}
    <p class="kicker">Bitcoin BLAKE2b · every pool</p>
    <h1>Still hashing on stratum.</h1>
    <p class="lede">A stratum hasher lets the pool write the block. A DATUM miner writes it on their own node. An address is listed here when a pool's own public data shows it on that pool's stratum endpoint. ${n(withData)} pools publish enough to tell, and each row says what it rests on.</p>
    <div class="callout">
      <p>The chain alone cannot settle it: a split pays stratum hashers and gateway operators alike, and some gateways write no tag in the coinbase. So <strong>confirmed</strong> means a pool's data names the address, and <strong>suspected</strong> means the odds only. An address leaves when the same source shows it on its own node.</p>
    </div>
    <p class="status">As of ${when(view.asOf)}. ${esc(walk)}, chain tip ${n(view.tip)}. ${plural((view.naughty || []).length, "address", "addresses")} confirmed on stratum, ${n((view.suspects || []).length)} suspected, ${n((view.nice || []).length)} moved to their own node.</p>
${LOOKUP()}
    <p class="jump">${only ? `Showing <strong>${esc(only)}</strong> only. <a href="/naughtylist/">Show every pool</a> · ` : ""}<a href="#confirmed">Confirmed (${n(naughty.length)})</a> · <a href="#suspected">Suspected (${n(suspects.length)})</a> · <a href="#nice">Nice list (${n(nice.length)})</a> · <a href="#how">How each pool is read</a> · <a href="/api/naughtylist">JSON</a></p>

    <h2 id="pools">Pools</h2>
    <p class="note">Every pool with blocks in the last fourteen days, and every pool we have checked. Choose a pool to see only its addresses. A block with a gateway tag was built on a miner's node. One without may be the pool's stratum or a gateway that writes no tag, so nothing is classified from that column.</p>
    ${table("pool-table", [{ t: "Pool" }, { t: "Stratum endpoint" }, { t: "Blocks, 14 days", num: 1 }, { t: "With a gateway tag", num: 1 }, { t: "Pool-built, by its own list", num: 1 }, { t: "Confirmed", num: 1 }, { t: "Suspected", num: 1 }, { t: "Pool data read", num: 1 }], poolRows, "No pool has been read yet.")}

    <h2 id="confirmed">Confirmed on stratum <span class="count">(${n(naughty.length)})</span></h2>
    ${table("naughty-table", [{ t: "Address" }, { t: "Pool" }, { t: "What shows it" }, { t: "Stratum hashrate", num: 1 }], naughtyRows, only ? `No address on ${only} is confirmed on stratum in this window.` : "No address is confirmed on stratum in this window.")}

    <h2 id="suspected">Suspected <span class="count">(${n(suspects.length)})</span></h2>
    <p class="note">Not proof. The pool does not say which protocol these addresses use, so a miner on their own node can be in this table.</p>
    ${table("suspect-table", [{ t: "Address" }, { t: "Pool" }, { t: "Why it is suspected" }, { t: "Last split block", num: 1 }], suspectRows, only ? `No suspect on ${only}.` : "No suspects in this window.")}
${finderRows.length ? `
    <h2 id="finders">dxpool stratum finders, named in part <span class="count">(${n(finderRows.length)})</span></h2>
    <p class="note">dxpool's stratum pool publishes the finder of each block as the first and last characters of <code>address.worker</code> or of an account name. These are its stratum blocks of the last fourteen days.</p>
    ${table("finder-table", [{ t: "Block", num: 1 }, { t: "Found", num: 1 }, { t: "Finder, as published" }], finderRows, "")}` : ""}

    <h2 id="nice">Nice list <span class="count">(${n(nice.length)})</span></h2>
    <p class="note">Addresses we saw on stratum that the same source now shows on their own node. A miner who only ever ran a node is on neither list.</p>
    ${table("nice-table", [{ t: "Address" }, { t: "Pool" }, { t: "What shows it" }, { t: "Hashrate", num: 1 }], niceRows, "No converted address in this window yet.")}

    <h2 id="how">How each pool is read</h2>
    <dl class="how">
${how}
    </dl>
    <p class="note">A pool not named here is listed in the table above as soon as it finds a block, marked not checked, and none of its payees are classified until its data has been read. Corrections: <a href="https://github.com/AwokenLazarus/Bitcoin/issues" rel="noopener">open an issue</a>.</p>
${FOOT}`;
}

/** One address: what each source says, and the coinbase outputs that paid it. */
export function renderAddress(d) {
  const address = d.address;
  const title = `${address} on the naughty list | Lazarus`;
  const head = HEAD(title, "What each Bitcoin BLAKE2b pool's public data says about one address, and the coinbase outputs that paid it.", "https://lazarus-xbt.xyz/naughtylist/", "/naughtylist/", "/assets/naughty.css?v=1");
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
  const verdict = !listed ? "Not on either list."
    : listed.list === "naughty" ? `Confirmed on stratum at ${listed.pool}.`
    : listed.list === "suspect" ? `Suspected, not confirmed, at ${listed.pool}.`
    : `On the nice list at ${listed.pool}.`;
  const says = [];
  if (listed) says.push(`<li><strong>${esc(listed.pool)}.</strong> ${esc(evidence(listed))}.</li>`);
  for (const s of d.stored || []) {
    if (listed && s.pool === listed.pool) continue;
    says.push(`<li><strong>${esc(s.pool)}.</strong> ${esc(s.text)}</li>`);
  }
  for (const a of d.asked || []) {
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
  const payRows = (d.pays || []).map((p) => {
    const label = p.via ? `Earned on ${esc(p.via)}'s stratum, behind its gateway on ${esc(p.pool)}` : p.kind === "datum-block" ? "Gateway-tagged block" : p.kind === "custodial" ? "Block paid to the pool alone" : "Split with no gateway tag";
    return `<tr><td class="num">${n(p.height)}</td><td class="num">${when(p.ts)}</td><td class="num">${xbt(p.sats)}</td><td>${esc(p.pool || "—")}</td><td class="${p.kind === "datum-block" && !p.via ? "kind-d" : "kind-s"}">${label}</td><td>${p.unspent ? "unspent" : "spent"}</td></tr>`;
  });
  return `${head}
    <p class="kicker">Naughty list · one address</p>
    <h1 class="addr-title">${esc(address)}</h1>
    <p class="lede">${esc(verdict)}</p>
    <h2>What the pools say</h2>
    ${says.length ? `<ul class="says">${says.join("\n")}</ul>` : `<p class="status">No pool we read shows this address in its current data.</p>`}
    <p class="note">B2Pool and dxpool answer for one address at a time, so they are asked when this page is opened. A pool that hides its miner list cannot be asked.</p>
    <h2>Coinbase outputs</h2>
    <p class="note">${plural(d.unspentCount || 0, "unspent coinbase output", "unspent coinbase outputs")}, ${xbt(d.unspentSats || 0)} still unspent, in the fourteen days of blocks indexed. A block's kind describes who built the block, not this address: a split pays everyone in the pool's window. <a href="${safeUrl(d.explorer)}" rel="noopener">Open the address on the explorer</a>.</p>
    ${table("pay-table", [{ t: "Block", num: 1 }, { t: "When", num: 1 }, { t: "Amount", num: 1 }, { t: "Pool" }, { t: "Block" }, { t: "Output" }], payRows, "No coinbase output for this address is in the index. The chain read covers fourteen days back from the tip.")}
${LOOKUP()}
    <p><a href="/naughtylist/">Back to the list</a></p>
${FOOT}`;
}
