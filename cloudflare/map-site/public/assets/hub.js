// lazarus-xbt.xyz — hub behaviour: live network figures, the live pool directory, and the small
// interactions. Everything here is progressive: the page reads correctly with no JavaScript, and
// each fetch failure leaves the static fallback text in place.

const $ = (sel, root = document) => root.querySelector(sel);
const $$ = (sel, root = document) => [...root.querySelectorAll(sel)];

/* ---------- formatting ---------- */
const UNITS = ["H/s", "kH/s", "MH/s", "GH/s", "TH/s", "PH/s", "EH/s", "ZH/s"];
function hashrate(hs) {
  if (!Number.isFinite(hs) || hs <= 0) return "—";
  let i = 0, v = hs;
  while (v >= 1000 && i < UNITS.length - 1) { v /= 1000; i++; }
  return `${v >= 100 ? Math.round(v) : v.toFixed(v >= 10 ? 1 : 2)} ${UNITS[i]}`;
}
const num = (n, d = 0) => (Number.isFinite(n) ? n.toLocaleString("en-US", { minimumFractionDigits: d, maximumFractionDigits: d }) : "—");
const pct = (n, d = 1) => (Number.isFinite(n) ? `${n.toFixed(d)}%` : "—");
function ago(ts) {
  const s = Math.max(0, Math.floor(Date.now() / 1000 - ts));
  if (s < 90) return `${s}s ago`;
  if (s < 5400) return `${Math.round(s / 60)} min ago`;
  return `${Math.round(s / 3600)} h ago`;
}
function duration(s) {
  if (!Number.isFinite(s) || s <= 0) return "—";
  const d = Math.floor(s / 86400), h = Math.round((s % 86400) / 3600);
  if (d >= 1) return `${d}d ${h}h`;
  const m = Math.round((s % 3600) / 60);
  return s >= 3600 ? `${Math.floor(s / 3600)}h ${m}m` : `${Math.max(1, Math.round(s / 60))} min`;
}
const set = (id, text, sub) => {
  const el = document.getElementById(id);
  if (!el || text == null) return;
  el.textContent = text;
  if (sub != null) {
    let small = el.querySelector("small");
    if (!small) { small = document.createElement("small"); el.appendChild(small); }
    small.textContent = sub;
  }
};

/* ---------- live figures ---------- */
async function stats() {
  let d;
  try {
    const r = await fetch("/api/stats", { headers: { Accept: "application/json" } });
    if (!r.ok) throw new Error(String(r.status));
    d = await r.json();
  } catch {
    $$(".live").forEach((el) => { el.classList.add("stale"); el.textContent = "live figures unavailable"; });
    return;
  }
  const p = d.pool || {}, n = d.network || {}, g = d.galaxy || {};
  set("s-nethash", hashrate(n.hashrate));
  set("s-height", num(n.height));
  set("s-pool", pct(p.sharePercent), `${num(p.gateways)} DATUM gateways`);
  set("s-blocks", num(p.blocksFound), "blocks found by Lazarus");
  set("s-systems", num(g.systems), `${num(g.blocks7d)} blocks in 7 days`);
  set("s-interval", duration(n.blockInterval), "average time between blocks");
  set("s-difficulty", num(n.difficulty / 1e9, 2) + " G", n.retargetChange != null ? `${n.retargetChange >= 0 ? "+" : ""}${n.retargetChange.toFixed(1)}% in ${num(n.retargetBlocks)} blocks` : "");
  set("s-miners", num(p.miners), "miners on Lazarus");
  // fee figures quoted in prose
  $$("[data-fee-stratum]").forEach((el) => { if (Number.isFinite(p.stratumFeePercent)) el.textContent = `${p.stratumFeePercent}%`; });
  $$("[data-fee-rebate]").forEach((el) => { if (Number.isFinite(p.datumRebatePercent)) el.textContent = `${p.datumRebatePercent}%`; });
  $$(".live").forEach((el) => { el.textContent = `live · ${ago(d.asOf)}`; el.classList.remove("stale"); });
}

/* ---------- the pool directory, from the galaxy model ---------- */
const FACTION = {
  datum: ["rebel", "DATUM"],
  stratum: ["empire", "Stratum only"],
  independent: ["rim", "Solo"],
};
async function pools() {
  const body = $("#pool-rows");
  if (!body && !document.getElementById("latest-blocks")) return;
  let d;
  try {
    const r = await fetch("/map/data.json", { headers: { Accept: "application/json" } });
    if (!r.ok) throw new Error(String(r.status));
    d = await r.json();
  } catch {
    if (!body) return;
    body.closest(".table-wrap").insertAdjacentHTML("afterend", '<p class="note faint">The live directory is not reachable right now; the <a href="/map/">galaxy map</a> has the same data.</p>');
    return;
  }
  latestBlocks(d);
  if (!body) return;
  const rows = (d.systems || [])
    .filter((s) => s.type !== "independent" && (s.blocks7d || 0) > 0)
    .sort((a, b) => (b.share7d || 0) - (a.share7d || 0))
    .slice(0, 14);
  if (!rows.length) return;
  body.innerHTML = rows
    .map((s) => {
      const [cls, label] = FACTION[s.type] || FACTION.independent;
      const name = s.link
        ? `<a href="${escapeAttr(s.link)}" rel="noopener nofollow external" target="_blank">${escapeHtml(s.name)}</a>`
        : escapeHtml(s.name);
      return `<tr>
        <td>${name}</td>
        <td><span class="pill ${cls}">${label}</span></td>
        <td class="num">${pct(s.share7d)}</td>
        <td class="num">${num(s.blocks7d)}</td>
        <td class="num">${hashrate(s.estHashrate)}</td>
        <td class="num">${num(s.gateways)}</td>
      </tr>`;
    })
    .join("");
  const stamp = $("#pool-asof");
  if (stamp) stamp.textContent = `${num(d.counts ? d.counts.datum + d.counts.stratum : rows.length)} pools seen since the fork · ${ago(d.asOf)}`;
}

async function latestBlocks(data) {
  const list = document.getElementById("latest-blocks");
  if (!list) return;
  const rows = (data.recent || []).slice(0, 7);
  if (!rows.length) { list.innerHTML = '<li class="note faint">No blocks to show.</li>'; return; }
  list.innerHTML = rows
    .map((b) => {
      const who = b.planet && b.planet !== b.system ? `${b.system} · ${b.planet}` : b.system || "unknown";
      return `<li><span class="h">${num(b.height)}</span><span class="who" title="${escapeHtml(who)}">${escapeHtml(who)}</span><span class="t">${ago(b.ts)}</span></li>`;
    })
    .join("");
}

const escapeHtml = (s) => String(s).replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]));
const escapeAttr = (s) => (/^https?:\/\//i.test(String(s)) ? escapeHtml(s) : "#");

/* ---------- interactions ---------- */
function nav() {
  const toggle = $(".nav-toggle"), menu = $("#site-nav");
  if (!toggle || !menu) return;
  toggle.addEventListener("click", () => {
    const open = menu.getAttribute("data-open") === "true";
    menu.setAttribute("data-open", String(!open));
    toggle.setAttribute("aria-expanded", String(!open));
  });
  menu.addEventListener("click", (e) => {
    if (e.target.tagName === "A") { menu.setAttribute("data-open", "false"); toggle.setAttribute("aria-expanded", "false"); }
  });
}

function copiers() {
  $$("[data-copy]").forEach((btn) => {
    btn.addEventListener("click", async () => {
      const text = btn.getAttribute("data-copy");
      try {
        await navigator.clipboard.writeText(text);
        const was = btn.textContent;
        btn.textContent = "Copied";
        btn.classList.add("copied");
        setTimeout(() => { btn.textContent = was; btn.classList.remove("copied"); }, 1600);
      } catch { /* clipboard refused: the address is on screen anyway */ }
    });
  });
}

function reveal() {
  const items = $$("[data-reveal]");
  if (!items.length) return;
  if (!("IntersectionObserver" in window) || matchMedia("(prefers-reduced-motion: reduce)").matches) {
    items.forEach((el) => el.classList.add("in"));
    return;
  }
  const io = new IntersectionObserver((entries) => {
    entries.forEach((e) => { if (e.isIntersecting) { e.target.classList.add("in"); io.unobserve(e.target); } });
  }, { rootMargin: "0px 0px -8% 0px" });
  items.forEach((el) => io.observe(el));
}

nav();
copiers();
reveal();
stats();
pools();
setInterval(stats, 60000);
