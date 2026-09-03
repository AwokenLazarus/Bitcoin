(() => {
  const $ = (id) => document.getElementById(id);

  async function copyValue(text) {
    if (!text) return false;
    try {
      await navigator.clipboard.writeText(text);
      return true;
    } catch (e) {
      const ta = document.createElement("textarea");
      ta.value = text;
      ta.setAttribute("readonly", "");
      ta.style.position = "fixed";
      ta.style.left = "-9999px";
      document.body.appendChild(ta);
      ta.select();
      const ok = document.execCommand("copy");
      ta.remove();
      return ok;
    }
  }

  document.addEventListener("click", async (e) => {
    const btn = e.target.closest(".copy-btn");
    if (!btn) return;
    const from = btn.getAttribute("data-copy-from");
    const text = (from ? ($(from)?.textContent || "") : (btn.getAttribute("data-copy") || "")).trim();
    if (!(await copyValue(text))) return;
    btn.setAttribute("data-copied", "");
    const prev = btn.getAttribute("aria-label") || "Copy";
    btn.setAttribute("aria-label", "Copied");
    clearTimeout(btn._copyT);
    btn._copyT = setTimeout(() => {
      btn.removeAttribute("data-copied");
      btn.setAttribute("aria-label", prev.replace(/^Copied$/, "Copy"));
    }, 1400);
  });

  // ---------------------------------------------------------------- tabs
  const tabs = [...document.querySelectorAll('[data-tabs] [role="tab"]')];
  function selectTab(id) {
    if (!tabs.length) return;
    const want = tabs.some((t) => t.id === id) ? id : tabs[0].id;
    for (const t of tabs) {
      const on = t.id === want;
      t.setAttribute("aria-selected", on ? "true" : "false");
      t.tabIndex = on ? 0 : -1;
      const pane = $(t.getAttribute("aria-controls"));
      if (pane) pane.hidden = !on;
    }
  }
  tabs.forEach((t, i) => {
    t.addEventListener("click", () => selectTab(t.id));
    t.addEventListener("keydown", (e) => {
      const map = { ArrowRight: i + 1, ArrowLeft: i - 1, Home: 0, End: tabs.length - 1 };
      if (!(e.key in map)) return;
      e.preventDefault();
      const next = tabs[(map[e.key] + tabs.length) % tabs.length];
      selectTab(next.id);
      next.focus();
    });
  });
  if (tabs.length) selectTab(tabs[0].id);

  // ------------------------------------------------------------ formatting
  const esc = (s) => String(s ?? "").replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]));
  const pathLabel = (via) => {
    if (via === "prime" || via === "gateway") return "own gateway";
    if (via === "both") return "stratum + gateway";
    return "public stratum";
  };
  const isPrimePath = (via) => via === "prime" || via === "gateway";
  const sessCell = (via, n) => (isPrimePath(via) ? "\u2014" : num(n ?? 0));

  const fmtHr = (ghs) => {
    if (ghs == null || Number.isNaN(ghs)) return "—";
    if (Number(ghs) < 1e-6) return "0 H/s";
    const hs = Number(ghs) * 1e9;
    if (hs >= 1e15) return (hs / 1e15).toFixed(2) + " PH/s";
    if (hs >= 1e12) return (hs / 1e12).toFixed(2) + " TH/s";
    if (hs >= 1e9) return (hs / 1e9).toFixed(2) + " GH/s";
    if (hs >= 1e6) return (hs / 1e6).toFixed(2) + " MH/s";
    if (hs >= 1e3) return (hs / 1e3).toFixed(1) + " KH/s";
    if (hs > 0) return Math.round(hs) + " H/s";
    return "0 H/s";
  };

  const num = (n) => (n == null || Number.isNaN(Number(n)) ? "—" : Number(n).toLocaleString());
  const short = (a) => (a && a.length > 20 ? a.slice(0, 10) + "\u2026" + a.slice(-8) : a || "\u2014");
  const pct = (n, d = 1) => (Number.isFinite(Number(n)) ? Number(n).toFixed(d) + "%" : "\u2014");
  const ago = (ts) => {
    if (!ts) return "\u2014";
    const s = Math.max(0, Date.now() / 1000 - ts);
    return agoS(s);
  };
  const agoS = (s) => {
    if (s == null || !Number.isFinite(Number(s))) return "\u2014";
    s = Number(s);
    if (s < 90) return Math.round(s) + "s ago";
    if (s < 3600) return Math.round(s / 60) + "m ago";
    if (s < 86400) return (s / 3600).toFixed(1) + "h ago";
    return (s / 86400).toFixed(1) + "d ago";
  };
  const dur = (s) => {
    if (s == null || !Number.isFinite(Number(s))) return "\u2014";
    s = Number(s);
    if (s < 90) return Math.round(s) + " s";
    if (s < 3600) return (s / 60).toFixed(0) + " min";
    if (s < 86400) return (s / 3600).toFixed(1) + " hours";
    if (s < 86400 * 60) return (s / 86400).toFixed(1) + " days";
    return (s / 86400 / 365).toFixed(1) + " years";
  };
  const btc = (n) => {
    if (n == null || Number.isNaN(Number(n))) return "\u2014";
    const x = Number(n);
    if (Math.abs(x) < 1e-12) return "0";
    if (Math.abs(x) >= 0.01) return x.toFixed(4);
    return x.toExponential(2);
  };
  const sats = (n) => (n == null || Number.isNaN(Number(n)) ? "\u2014" : (Number(n) / 1e8).toFixed(8).replace(/0+$/, "").replace(/\.$/, ".0"));
  const expPct = (n) => {
    const x = Number(n);
    if (!Number.isFinite(x) || Math.abs(x) < 1e-12) return "0";
    if (Math.abs(x) >= 0.01) return x.toFixed(2);
    return x.toExponential(2);
  };
  const when = (ts) => (ts ? new Date(ts * 1000).toLocaleString([], { dateStyle: "medium", timeStyle: "short" }) : "");
  const j = async (url) => {
    const r = await fetch(url);
    if (!r.ok) throw new Error(url + " " + r.status);
    return r.json();
  };
  const kindPill = (kind) => {
    const k = String(kind || "").toLowerCase();
    const label = k === "split" ? "split" : k === "partial" ? "partial split" : k === "pool-only" ? "pool only" : k || "\u2014";
    const cls = k === "split" ? "ok" : k === "partial" ? "warn" : k === "pool-only" ? "warn" : "";
    return k ? `<span class="pill ${cls}">${esc(label)}</span>` : "\u2014";
  };
  const statusPill = (status) => {
    const s = String(status || "").toLowerCase();
    const cls = s === "in chain" || s === "paid" || s === "submitted" ? "ok" : s === "orphaned" || s === "rejected" ? "bad" : s === "immature" || s === "pending" ? "warn" : "";
    return s ? `<span class="pill ${cls}">${esc(s)}</span>` : "\u2014";
  };

  // ------------------------------------------------------------------ pool
  function stats(p) {
    const pr = p.prime || {};
    const tot = pr.totals || {};
    const net = p.network_hr_hs ? (p.network_hr_hs / 1e15).toFixed(2) + " PH/s" : "\u2014";
    const luck = p.luck_percent == null ? "\u2014" : p.luck_percent.toFixed(0) + "%";
    const nblocks = Number(p.window_multiple) || 8;
    const fill = Number(p.window_fill_percent);
    const fillTxt = Number.isFinite(fill) ? fill.toFixed(0) + "%" : "\u2014";
    const inWindow = Number((pr.window || {}).identities) || 0;
    const rejPct = tot.shares_accepted + tot.shares_rejected > 0 ? (100 * tot.shares_rejected / (tot.shares_accepted + tot.shares_rejected)).toFixed(2) + "% rejected" : "no rejects";
    const gws = Number(pr.gateways_online) || 0;
    const remote = Number(pr.gateways_remote) || 0;
    const cells = [
      ["Hashrate", fmtHr(p.pool_hr_ghs), (p.miners_online || 0) + " worker" + (p.miners_online === 1 ? "" : "s") + " online"],
      ["Miners", String(inWindow || p.miners_online || 0), inWindow ? "holding work in the window · " + (p.miners_seen ?? "\u2014") + " ever" : (p.miners_seen ?? "\u2014") + " ever"],
      ["Shares", num(tot.shares_accepted), pr.reachable ? "verified by Prime since start · " + rejPct : "Prime unreachable"],
      ["Window", fillTxt + " full", nblocks + " network-blocks of work"],
      ["Gateways", String(gws), gws ? (remote ? remote + " remote + our stratum" : "our public stratum") + " · Prime up " + dur(pr.uptime_s) : "none connected"],
      ["To block", dur(p.ttf_seconds), expPct((p.pool_share || 0) * 100) + "% of the network"],
      ["Found", String(p.blocks_found ?? 0), p.luck_percent == null ? "luck pending" : luck + " luck"],
      ["Network", net, "tip " + (p.height || "\u2014") + " · diff " + (p.difficulty ? Number(p.difficulty).toExponential(3) : "\u2014")],
    ];
    $("stats").innerHTML = cells
      .map(([k, v, s]) => `<div><dt>${k}</dt><dd>${v}<small>${s}</small></dd></div>`)
      .join("");

    $("stratum").textContent = p.stratum;

    const d = p.datum || {};
    const host = d.pool_host || "stratum.awokenlazarus.xyz";
    const port = d.pool_port || 28915;
    const pubkey = d.pool_pubkey || pr.pubkey || "";
    const gw = $("gateway-config");
    if (gw) {
      gw.textContent = JSON.stringify({ datum: {
        pool_host: host,
        pool_port: port,
        pool_pubkey: pubkey || "<primed pubkey>",
        pool_pass_workers: true,
        pool_pass_full_users: true,
        pooled_mining_only: true,
      }}, null, 2);
    }
    if ($("datum-host")) $("datum-host").textContent = host;
    if ($("datum-port")) $("datum-port").textContent = String(port);
    if ($("datum-pubkey")) $("datum-pubkey").textContent = pubkey || "\u2014";

    if ($("live-hr")) $("live-hr").textContent = fmtHr(p.pool_hr_ghs);
    if ($("live-tip")) $("live-tip").textContent = p.height || "\u2014";
    if ($("live-window")) $("live-window").textContent = fillTxt;
    if ($("live-gateways")) $("live-gateways").textContent = String(gws);
    const off = $("prime-offline");
    if (off) off.hidden = !!pr.reachable;
    const live = $("live-chip");
    if (live) live.classList.toggle("stale", !pr.reachable);

    const meter = $("window-meter");
    const bar = $("window-fill");
    if (meter && bar) {
      const w = Math.max(0, Math.min(100, Number.isFinite(fill) ? fill : 0));
      bar.style.width = w + "%";
      meter.setAttribute("aria-valuenow", String(Math.round(w)));
    }
    if ($("window-size")) $("window-size").textContent = nblocks + " net blocks";
    if ($("window-filled")) $("window-filled").textContent = fillTxt;
    if ($("window-filled-inline")) $("window-filled-inline").textContent = fillTxt;
    if ($("window-miners")) $("window-miners").textContent = inWindow ? String(inWindow) : "\u2014";
    if ($("window-shares")) $("window-shares").textContent = num((pr.window || {}).shares ?? p.window_shares);
    if ($("window-explain")) {
      $("window-explain").textContent =
        "TIDES keeps a rolling window of " + nblocks +
        " network-blocks of accepted work. A found block splits the reward by who holds that window — not by who has the highest hashrate right now. Plug in a 19 TH/s box and your window % starts near zero; it climbs as your shares accumulate and older miners’ work ages out. The bar is how full the pool’s window is of that " +
        nblocks + "-block target.";
    }
    const build = $("prime-build");
    if (build) build.textContent = pr.name ? "Prime: " + pr.name + (pr.version ? " " + pr.version : "") + (pr.uptime_s ? " · up " + dur(pr.uptime_s) : "") : "";
    draw($("poolchart"), p.history || [], "hr_ghs");
  }

  // ------------------------------------------------------------- coinbase
  let coinbaseExpanded = false;
  const COINBASE_ROWS = 8;
  function coinbase(cb) {
    const el = $("coinbase");
    if (!el) return;
    const miners = (cb.miners || []).filter((o) => o.to !== "pool");
    const pool = (cb.miners || []).find((o) => o.to === "pool");
    const value = Number(cb.value) || 0;
    const shown = coinbaseExpanded ? miners : miners.slice(0, COINBASE_ROWS);
    const rows = shown.map((o, i) =>
      `<tr><td class="num faint">${i + 1}</td><td><a href="#${esc(o.address)}">${short(o.address)}</a></td><td class="num">${pct(o.share_percent)}</td><td class="num">${sats(o.sats)}</td><td class="num faint">${value ? pct(100 * o.sats / value, 2) : "\u2014"}</td></tr>`
    );
    if (!coinbaseExpanded && miners.length > COINBASE_ROWS) {
      const rest = miners.slice(COINBASE_ROWS);
      const restSats = rest.reduce((a, o) => a + Number(o.sats || 0), 0);
      rows.push(`<tr class="faint"><td class="num"></td><td>${rest.length} more miner output${rest.length === 1 ? "" : "s"}</td><td class="num">${pct(rest.reduce((a, o) => a + Number(o.share_percent || 0), 0))}</td><td class="num">${sats(restSats)}</td><td class="num">${value ? pct(100 * restSats / value, 2) : "\u2014"}</td></tr>`);
    }
    if (pool) {
      rows.push(`<tr class="pool-row"><td class="num faint">${miners.length + 1}</td><td>pool <span class="faint">fee${cb.unplaced_sats > 1000 ? " + unplaced" : ""}</span> · <a href="#${esc(pool.address)}">${short(pool.address)}</a></td><td class="num">\u2014</td><td class="num">${sats(pool.sats)}</td><td class="num faint">${value ? pct(100 * pool.sats / value, 2) : "\u2014"}</td></tr>`);
    }
    el.innerHTML =
      '<thead><tr><th class="num">#</th><th>Output</th><th class="num">Window</th><th class="num">BTC</th><th class="num">Of block</th></tr></thead><tbody>' +
      (rows.length ? rows.join("") : '<tr><td colspan="5" class="empty">Prime has not issued a split yet</td></tr>') +
      "</tbody>";
    const sum = $("coinbase-summary");
    if (sum) sum.textContent = cb.outputs ? `${cb.outputs} outputs · ${btc(value / 1e8)} BTC at the base subsidy · ${sats(cb.miner_sats)} to ${cb.miner_outputs} miner${cb.miner_outputs === 1 ? "" : "s"} · ${sats(cb.pool_sats)} to the pool (${pct(cb.fee_percent, 1)} fee)` : "\u2014";
    const more = $("coinbase-more");
    if (more) {
      more.hidden = miners.length <= COINBASE_ROWS;
      more.textContent = coinbaseExpanded ? "Show fewer" : `Show all ${miners.length} miner outputs`;
      more.onclick = () => { coinbaseExpanded = !coinbaseExpanded; coinbase(cb); };
    }
    const unpaid = cb.unpaid || [];
    const line = $("coinbase-more-line");
    if (line) {
      const old = line.querySelector(".unpaid-note");
      if (old) old.remove();
      if (unpaid.length) {
        const s = document.createElement("span");
        s.className = "unpaid-note faint";
        s.textContent = ` ${unpaid.length} address${unpaid.length === 1 ? "" : "es"} in the window earn${unpaid.length === 1 ? "s" : ""} less than the minimum output right now; that share stays with the pool until it clears.`;
        line.appendChild(s);
      }
    }
  }

  // ------------------------------------------------------------- gateways
  function gateways(pr) {
    const el = $("gwtable");
    if (!el) return;
    const rows = (pr.gateways || []).map((g) => {
      const who = g.own ? `<span class="pill brass">Lazarus public stratum</span>` : `<span class="mono">${esc(g.gateway || "")}</span>`;
      const client = `${esc(g.user_agent || "\u2014")} <span class="faint">· ${esc(g.generation || "")}</span>`;
      const rej = g.rejected ? `${num(g.rejected)} <span class="faint">· ${esc(g.last_reject || "")}</span>` : "0";
      const ident = g.own ? "\u2014" : (g.identity ? `<a href="#${esc(g.identity)}">${short(g.identity)}</a>` : "\u2014");
      return [
        who,
        client,
        ident,
        dur(g.connected_s),
        num(g.accepted),
        rej,
        num(g.work),
        g.last_share_s == null ? "\u2014" : agoS(g.last_share_s),
        String(g.block_candidates || 0),
      ];
    });
    table(el, ["Gateway", "Client", "Pays to", "Connected", "Accepted", "Rejected", "Work", "Last share", "Blocks"], rows, [null, null, null, "num", "num", "num", "num", "num", "num"], "No gateway connected");
    const note = $("gateway-note");
    if (note) {
      const t = pr.totals || {};
      note.textContent = pr.reachable
        ? `Since Prime started ${dur(pr.uptime_s)} ago: ${num(t.connections)} connection${t.connections === 1 ? "" : "s"}, ${num(t.coinbasers)} coinbase splits issued, ${num(t.shares_accepted)} shares verified, ${num(t.block_candidates)} block candidate${t.block_candidates === 1 ? "" : "s"}. “Rejected” right after a Prime restart is a gateway still on the previous instance’s split — it clears with its next template.`
        : "Prime is unreachable; this is the last list it published.";
    }
  }

  // `align` is an optional array of "num" markers, one per column, so numeric
  // columns line up on the right with tabular figures.
  function table(el, headers, rows, align, empty) {
    if (!el) return;
    const cls = (i) => (align && align[i] === "num" ? ' class="num"' : "");
    el.innerHTML =
      "<thead><tr>" +
      headers.map((h, i) => "<th" + cls(i) + ">" + h + "</th>").join("") +
      "</tr></thead><tbody>" +
      (rows.length
        ? rows.map((r) => "<tr>" + r.map((c, i) => "<td" + cls(i) + ">" + c + "</td>").join("") + "</tr>").join("")
        : '<tr><td colspan="' + headers.length + '" class="empty">' + (empty || "None yet") + "</td></tr>") +
      "</tbody>";
  }

  // ---------------------------------------------------------------- blocks
  function foundBlocks(pays, p) {
    const el = $("found");
    if (!el) return;
    const byHeight = new Map();
    for (const r of pays.payouts || []) {
      const key = r.hash || String(r.height);
      const b = byHeight.get(key) || { height: r.height, hash: r.hash, ts: r.ts, outputs: [], miner_btc: 0, pool_btc: 0, status: r.status, kind: r.kind, block_status: r.block_status, owed_sats: r.owed_sats, found_by: r.found_by, reward: r.reward_btc };
      const isPool = r.to === "pool";
      b.outputs.push({ address: r.finder, btc: Number(r.miner_btc) || 0, share: r.share, pool: isPool });
      if (isPool) b.pool_btc += Number(r.miner_btc) || 0;
      else b.miner_btc += Number(r.miner_btc) || 0;
      byHeight.set(key, b);
    }
    const chainHashes = new Set([...byHeight.values()].map((b) => b.hash));
    // Candidates Prime saw that the chain scan has not confirmed (pending, orphaned).
    for (const pb of pays.prime_blocks || []) {
      if (!pb.hash || chainHashes.has(pb.hash)) continue;
      if (pb.status === "in chain") continue;
      byHeight.set(pb.hash, { height: pb.height, hash: pb.hash, ts: pb.ts, outputs: (pb.split || []).map((o) => ({ address: o.address, btc: o.sats / 1e8 })), miner_btc: (pb.split || []).reduce((a, o) => a + o.sats, 0) / 1e8, pool_btc: (pb.pool_sats || 0) / 1e8, status: pb.status, kind: pb.kind, block_status: pb.status, owed_sats: pb.owed_sats, found_by: pb.finder, reward: pb.coinbase_value / 1e8, prime_only: true });
    }
    const blocks = [...byHeight.values()].sort((a, b) => (b.height || 0) - (a.height || 0));
    const headers = ["Height", "Hash", "Coinbase", "Outputs", "Miners paid", "Pool", "Status", "Found by", "Time"];
    const rows = blocks.map((b, i) => {
      const poolBtc = b.pool_btc || (b.reward ? Math.max(0, b.reward - b.miner_btc) : null);
      const kind = b.kind || (b.outputs.length > 1 ? "split" : b.outputs.length === 1 ? "" : "");
      const st = b.prime_only ? b.block_status : (b.status === "unsplit" ? "pool only" : b.status);
      const outs = b.outputs.slice().sort((x, y) => y.btc - x.btc);
      const detail = outs.map((o) => `<tr><td></td><td colspan="2"><a href="#${esc(o.address)}">${esc(o.address)}</a>${o.pool ? ' <span class="pill brass">pool</span>' : ""}</td><td class="num">${btc(o.btc)}</td><td class="num faint">${b.reward ? pct(100 * o.btc / b.reward, 2) : ""}</td><td colspan="4"></td></tr>`).join("");
      const owed = b.owed_sats ? `<div class="faint">owed to window ${sats(b.owed_sats)}</div>` : "";
      return `<tr class="block-row" data-i="${i}" tabindex="0" aria-expanded="false">
        <td class="num"><span class="disclose"></span>${b.height ?? "\u2014"}</td>
        <td>${b.hash ? `<a href="${esc(p.explorer)}/block/${esc(b.hash)}" target="_blank" rel="noreferrer">${short(b.hash)}</a>` : "\u2014"}</td>
        <td>${kindPill(kind)}${owed}</td>
        <td class="num">${b.outputs.length}</td>
        <td class="num">${btc(b.miner_btc)}</td>
        <td class="num">${poolBtc == null ? "\u2014" : btc(poolBtc)}</td>
        <td>${statusPill(st)}</td>
        <td>${b.found_by ? `<a href="#${esc(b.found_by)}">${short(b.found_by)}</a>` : "\u2014"}</td>
        <td>${when(b.ts)}</td>
      </tr>
      <tr class="block-detail" hidden><td colspan="9"><table class="inner"><thead><tr><th></th><th colspan="2">Coinbase output</th><th class="num">BTC</th><th class="num">Of block</th><th colspan="4"></th></tr></thead><tbody>${detail || '<tr><td colspan="9" class="empty">No outputs recorded</td></tr>'}</tbody></table></td></tr>`;
    });
    el.innerHTML =
      "<thead><tr>" + headers.map((h, i) => `<th${[0, 3, 4, 5].includes(i) ? ' class="num"' : ""}>${h}</th>`).join("") + "</tr></thead><tbody>" +
      (rows.length ? rows.join("") : '<tr><td colspan="9" class="empty">No blocks found yet</td></tr>') +
      "</tbody>";
    el.querySelectorAll(".block-row").forEach((tr) => {
      const toggle = () => {
        const det = tr.nextElementSibling;
        const open = det && det.hidden;
        if (det) det.hidden = !open;
        tr.setAttribute("aria-expanded", open ? "true" : "false");
      };
      tr.addEventListener("click", (e) => { if (!e.target.closest("a")) toggle(); });
      tr.addEventListener("keydown", (e) => { if (e.key === "Enter" || e.key === " ") { e.preventDefault(); toggle(); } });
    });
  }

  function continuous(hist, key, bucketSec) {
    const raw = [];
    for (const item of hist || []) {
      const ts = Number(item.ts);
      const v = Number(item[key]);
      if (!Number.isFinite(ts) || !Number.isFinite(v) || v <= 0) continue;
      raw.push({ ts, v });
    }
    raw.sort((a, b) => a.ts - b.ts);
    if (!raw.length) return [];
    const sorted = raw.map((p) => p.v).sort((a, b) => a - b);
    const typical = sorted[Math.floor(sorted.length * 0.85)] || sorted[sorted.length - 1];
    const floor = typical > 0 ? typical * 0.03 : 0;
    const src = raw.filter((p) => p.v >= floor);
    const use = src.length >= 2 ? src : raw;
    const start = Math.floor(use[0].ts / bucketSec) * bucketSec;
    const end = Math.floor(use[use.length - 1].ts / bucketSec) * bucketSec;
    const sums = new Map();
    for (const p of use) {
      const b = Math.floor(p.ts / bucketSec) * bucketSec;
      const rec = sums.get(b) || { sum: 0, n: 0 };
      rec.sum += p.v;
      rec.n += 1;
      sums.set(b, rec);
    }
    const series = [];
    let last = use[0].v;
    for (let t = start; t <= end; t += bucketSec) {
      const rec = sums.get(t);
      if (rec) last = rec.sum / rec.n;
      series.push({ ts: t, [key]: last });
    }
    return series;
  }

  function draw(c, hist, key) {
    if (!c) return;
    try { c.dataset.last = JSON.stringify(hist || []); } catch (e) {}
    const ctx = c.getContext("2d");
    const dpr = Math.max(1, window.devicePixelRatio || 1);
    const cssW = c.clientWidth || c.width;
    const cssH = c.clientHeight || 148;
    if (c.width !== Math.round(cssW * dpr) || c.height !== Math.round(cssH * dpr)) {
      c.width = Math.round(cssW * dpr);
      c.height = Math.round(cssH * dpr);
    }
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.clearRect(0, 0, cssW, cssH);
    if (!hist || hist.length < 2) return;
    const span = hist[hist.length - 1].ts - hist[0].ts;
    const bucket = span > 12 * 3600 ? 120 : 60;
    const series = continuous(hist, key, bucket);
    if (series.length < 2) return;
    const ys = series.map((h) => h[key]);
    const minY = 0;
    const maxY = Math.max(...ys, 0.01);
    const minX = series[0].ts;
    const maxX = series[series.length - 1].ts || minX + 1;
    const pad = { l: 6, r: 52, t: 22, b: 18 };
    const w = cssW - pad.l - pad.r;
    const h = cssH - pad.t - pad.b;
    const pt = (item) => {
      const x = pad.l + ((item.ts - minX) / (maxX - minX)) * w;
      const y = pad.t + h - ((item[key] || 0) - minY) / (maxY - minY) * h;
      return [x, y];
    };
    ctx.save();
    ctx.strokeStyle = "rgba(212,180,90,0.16)";
    ctx.lineWidth = 1;
    ctx.setLineDash([2, 3]);
    for (let i = 1; i <= 3; i++) {
      const y = pad.t + (h * i) / 4;
      ctx.beginPath();
      ctx.moveTo(pad.l, y);
      ctx.lineTo(pad.l + w, y);
      ctx.stroke();
    }
    ctx.setLineDash([]);
    ctx.restore();
    const fill = ctx.createLinearGradient(0, pad.t, 0, pad.t + h);
    fill.addColorStop(0, "rgba(212,180,90,0.28)");
    fill.addColorStop(1, "rgba(212,180,90,0)");
    ctx.beginPath();
    series.forEach((item, i) => {
      const [x, y] = pt(item);
      i ? ctx.lineTo(x, y) : ctx.moveTo(x, y);
    });
    const last = pt(series[series.length - 1]);
    const first = pt(series[0]);
    ctx.lineTo(last[0], pad.t + h);
    ctx.lineTo(first[0], pad.t + h);
    ctx.closePath();
    ctx.fillStyle = fill;
    ctx.fill();
    ctx.beginPath();
    series.forEach((item, i) => {
      const [x, y] = pt(item);
      i ? ctx.lineTo(x, y) : ctx.moveTo(x, y);
    });
    ctx.strokeStyle = "#d4b45a";
    ctx.lineWidth = 1.75;
    ctx.lineJoin = "round";
    ctx.lineCap = "round";
    ctx.stroke();
    ctx.fillStyle = "#8a7d62";
    ctx.font = "10px IBM Plex Mono, ui-monospace, monospace";
    ctx.textAlign = "right";
    ctx.textBaseline = "top";
    ctx.fillText(fmtHr(maxY), cssW - 4, 6);
    ctx.textAlign = "left";
    ctx.textBaseline = "bottom";
    const t0 = new Date(minX * 1000);
    const t1 = new Date(maxX * 1000);
    const fmtT = (d) => d.toLocaleTimeString([], { hour: "numeric", minute: "2-digit" });
    ctx.fillText(fmtT(t0), pad.l, cssH - 2);
    ctx.textAlign = "right";
    ctx.fillText(fmtT(t1), pad.l + w, cssH - 2);
  }

  const showChart = (on) => {
    const w = $("chart-wrap");
    if (w) w.hidden = !on;
  };

  // ----------------------------------------------------------------- miner
  async function showMiner(addr) {
    if (!addr) {
      $("miner").innerHTML = "";
      showChart(false);
      draw($("chart"), [], "hr_ghs");
      return;
    }
    const m = await j("/api/miner/" + encodeURIComponent(addr));
    if (!m.known) {
      $("miner").innerHTML = '<p class="note callout">No stats for that address yet. Connect a miner first.</p>';
      showChart(false);
      return;
    }
    const workers = (m.workers || [])
      .map(
        (w) =>
          `<tr><td>${esc(w.worker || "\u2014")}</td><td>${pathLabel(w.via)}</td><td class="num">${fmtHr(w.hr_ghs)}</td><td class="num">${sessCell(w.via, w.shares_session)}</td><td class="num">${num(w.shares_lifetime ?? w.shares_acc ?? w.window_work)}</td><td class="num">${w.window_percent != null ? pct(w.window_percent) : "\u2014"}</td><td class="num">${num(w.shares_rej)}</td><td class="num">${Number.isFinite(Number(w.last_share_s)) ? Number(w.last_share_s).toFixed(0) + "s" : "\u2014"}</td></tr>`
      )
      .join("");
    const payStatus = (b) => {
      const s = String(b.status || "").toLowerCase();
      const rs = String(b.round_status || "").toLowerCase();
      if (s === "immature") return "immature";
      if (s === "carried" || rs === "unsplit") return "carried";
      if (s === "paid" || s === "unpaid") return "paid";
      return s || rs || "—";
    };
    const pays = (m.blocks_found || [])
      .map((b) => `<tr><td class="num">${b.height}</td><td class="num">${btc(b.miner_btc)}</td><td>${statusPill(payStatus(b))}</td><td>${when(b.ts)}</td></tr>`)
      .join("");
    const status = !m.online
      ? "offline"
      : isPrimePath(m.via)
        ? "online · own gateway"
        : m.via === "both"
          ? "online · stratum + gateway"
          : "online · public stratum";
    const wp = (m.round_share || 0) * 100;
    const hp = Number(m.hashrate_pool_percent) || 0;
    const nblocks = Number(m.window_multiple) || 8;
    let windowNote;
    if (m.online && hp > 1 && wp < hp * 0.5) {
      windowNote = `<p class="note callout">Your hashrate is ${hp.toFixed(1)}% of the pool right now, but you hold ${wp.toFixed(1)}% of the ${nblocks}-block payout window. New hash ramps in as work accumulates and older work ages out — that gap is expected, not a missing payout.</p>`;
    } else {
      windowNote = `<p class="note callout">Next-block pay is the window % (${wp.toFixed(1)}%), not hashrate. The window is ${nblocks} network-blocks of accepted work (TIDES). A newly connected high-hashrate miner does not take a matching slice of the next block.</p>`;
    }
    $("miner").innerHTML = `
      <div class="panel miner-card">
        <div class="addr-line">
          <span class="copyable"><span class="mono">${esc(m.address)}</span><button type="button" class="copy-btn" data-copy="${esc(m.address)}" aria-label="Copy address" title="Copy"></button></span>
          <span class="status-pill ${m.online ? "ok" : "bad"}">${status}</span>
        </div>
        ${windowNote}
        <dl class="ticker">
          <div><dt>Hashrate</dt><dd>${fmtHr(m.hr_ghs || 0)}<small>best ${fmtHr(m.best_hr_ghs || 0)} · ${hp.toFixed(1)}% of pool now</small></dd></div>
          <div><dt>Accepted</dt><dd>${num(m.shares_lifetime ?? m.shares_acc)}<small>stays with this address on either path</small></dd></div>
          <div><dt>This session</dt><dd>${sessCell(m.via, m.shares_session)}<small>${isPrimePath(m.via) ? "own gateway · Prime credits the window directly" : "public stratum only · resets on reconnect"}</small></dd></div>
          <div><dt>Payout window</dt><dd>${wp.toFixed(1)}%<small>${num(m.window_work)} work · what the next block pays</small></dd></div>
          <div><dt>Est. / day</dt><dd>${btc(m.est_btc_day)}<small>if the window already matched this hashrate</small></dd></div>
          <div><dt>Next block</dt><dd>${btc(m.block_payout_btc)}<small>your output in the coinbase Prime dictates now</small></dd></div>
          <div><dt>Immature</dt><dd>${btc(m.immature_btc)}<small>in a coinbase, under 100 confs</small></dd></div>
          <div><dt>Paid</dt><dd>${btc(m.paid_btc)}<small>in a coinbase, 100+ confs</small></dd></div>
        </dl>
        <div>
          <p class="kicker table-label">Workers</p>
          <p class="note">“Public stratum” is our gateway; “own gateway” is share credit arriving through a DATUM gateway you run.</p>
          <div class="scroll"><table><thead><tr><th>Worker</th><th>Path</th><th class="num">Hashrate</th><th class="num">Session</th><th class="num">Accepted</th><th class="num">Window %</th><th class="num">Rejects</th><th class="num">Last</th></tr></thead><tbody>${workers || '<tr><td colspan="8" class="empty">Offline</td></tr>'}</tbody></table></div>
        </div>
        <div>
          <p class="kicker table-label">Your payouts</p>
          <div class="scroll"><table><thead><tr><th class="num">Height</th><th class="num">Amount</th><th>Status</th><th>Time</th></tr></thead><tbody>${pays || '<tr><td colspan="4" class="empty">No blocks found yet</td></tr>'}</tbody></table></div>
        </div>
      </div>`;
    showChart(true);
    draw($("chart"), m.history || [], "hr_ghs");
  }

  // --------------------------------------------------------------- refresh
  async function refresh() {
    const [p, miners, blocks, pays, cb] = await Promise.all([
      j("/api/pool"),
      j("/api/miners"),
      j("/api/blocks"),
      j("/api/payouts"),
      j("/api/coinbaser").catch(() => ({})),
    ]);
    stats(p);
    coinbase(cb || {});
    gateways(p.prime || {});
    table(
      $("online"),
      ["Address", "Worker", "Path", "Hashrate", "Session", "Accepted", "Window %", "Last share"],
      (miners.online || []).filter((m) => m.address).map((m, i, arr) => [
        `<a href="#${esc(m.address)}">${short(m.address)}</a>`,
        esc(m.worker || "\u2014"),
        pathLabel(m.via),
        fmtHr((arr.findIndex((x) => x.address === m.address) === i) ? (m.credited_hr_ghs || m.hr_ghs) : m.hr_ghs),
        sessCell(m.via, m.shares_session),
        num(m.shares_lifetime ?? m.shares_acc ?? m.window_work),
        (arr.findIndex((x) => x.address === m.address) === i) && m.window_percent != null ? pct(m.window_percent) : "\u2014",
        Number.isFinite(Number(m.last_share_s)) ? Number(m.last_share_s).toFixed(0) + "s" : "\u2014",
      ]),
      [null, null, null, "num", "num", "num", "num", "num"]
    );
    table(
      $("seen"),
      ["Address", "Hashrate", "Accepted", "Window %", "Last seen"],
      (miners.seen || []).filter((m) => m.address).map((m) => [
        `<a href="#${esc(m.address)}">${short(m.address)}</a>`,
        fmtHr(m.hr_ghs || 0),
        num(m.shares_lifetime ?? m.shares_acc ?? m.window_work),
        m.window_percent != null ? pct(m.window_percent) : "\u2014",
        ago(m.last_ts),
      ]),
      [null, "num", "num", "num", "num"]
    );
    foundBlocks(pays, p);
    table(
      $("blocktable"),
      ["Height", "Miner tag", "Time", "Txs", ""],
      (blocks.blocks || []).slice(0, 16).map((b) => [
        b.height,
        esc(b.pool || "Unknown"),
        when(b.timestamp),
        b.tx_count || "",
        b.explorer ? `<a href="${esc(b.explorer)}" target="_blank" rel="noreferrer">explorer</a>` : "",
      ]),
      ["num", null, null, "num", null]
    );
  }

  // Section anchors never trigger an address lookup. "mine" and "datum" are
  // retired anchors kept so old bookmarks still land somewhere sensible.
  const SECTIONS = new Set([
    "", "top", "status", "window", "connect", "dashboard", "miners", "gateways",
    "blocks", "payouts", "how", "datum", "mine",
  ]);

  function fromHash() {
    const a = location.hash.slice(1);
    if (a === "datum") {
      selectTab("tab-datum");
      $("connect")?.scrollIntoView();
      return;
    }
    if (a && !SECTIONS.has(a)) {
      $("lookup").value = a;
      showMiner(a);
    }
  }

  $("go").onclick = () => {
    const a = $("lookup").value.trim();
    location.hash = a;
    showMiner(a);
  };
  $("lookup").addEventListener("keydown", (e) => {
    if (e.key === "Enter") $("go").click();
  });
  window.addEventListener("hashchange", fromHash);
  fromHash();
  refresh().catch((e) => console.error(e));
  setInterval(() => refresh().catch((e) => console.error(e)), 10000);
  window.addEventListener("resize", () => {
    for (const id of ["poolchart", "chart"]) {
      const c = $(id);
      if (c && c.dataset.last) {
        try { draw(c, JSON.parse(c.dataset.last), "hr_ghs"); } catch (e) {}
      }
    }
  });
})();
