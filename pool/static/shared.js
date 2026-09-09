// Shared formatting and chart code for the Lazarus pool pages (index + /miner/<addr>).
// Loaded before pool.js / miner.js; everything hangs off window.LZ.
(() => {
  const esc = (s) => String(s ?? "").replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]));
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
  const bigNum = (n) => {
    const x = Number(n);
    if (!Number.isFinite(x)) return "\u2014";
    if (x >= 1e12) return (x / 1e12).toFixed(2) + "T";
    if (x >= 1e9) return (x / 1e9).toFixed(2) + "G";
    if (x >= 1e6) return (x / 1e6).toFixed(1) + "M";
    if (x >= 1e3) return (x / 1e3).toFixed(1) + "k";
    return x.toFixed(x < 10 ? 2 : 0);
  };
  // Every use of these is HTML text, and the inputs are miner-supplied identities, so
  // they escape here rather than trusting each call site to remember.
  const short = (a) => esc(a && a.length > 20 ? a.slice(0, 10) + "\u2026" + a.slice(-8) : a || "\u2014");
  // Block hashes lead with zeros; the tail is what identifies them.
  const shortHash = (h) => esc(h && h.length > 16 ? "\u2026" + h.slice(-12) : h || "\u2014");
  const pct = (n, d = 1) => (Number.isFinite(Number(n)) ? Number(n).toFixed(d) + "%" : "\u2014");
  // Percent that stays legible for small miners: 23.6% / 1.25% / 0.032% — never "0.0%".
  const pctSmart = (n) => {
    const x = Number(n);
    if (!Number.isFinite(x)) return "\u2014";
    if (Math.abs(x) >= 10) return x.toFixed(1) + "%";
    if (Math.abs(x) >= 1) return x.toFixed(2) + "%";
    if (Math.abs(x) < 1e-9) return "0%";
    return sig4(x) + "%";
  };
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
  // ---- amounts: XBT or sats, never scientific notation, at most 4 significant figures.
  // >= 0.01 reads in XBT ("0.6266 XBT", "3.125 XBT", "124.2 XBT"); below that in sats
  // ("612,300 sats", "42 sats"). Trailing zeros are dropped so 3.1000 reads "3.1".
  const TICKER = "XBT";
  const sig4 = (x) => {
    if (x === 0) return "0";
    if (Math.abs(x) >= 10000) return Math.round(x).toLocaleString();
    let s = Number(x.toPrecision(4));
    return s.toLocaleString(undefined, { maximumFractionDigits: 8 });
  };
  const amt = (btcValue) => {
    if (btcValue == null || Number.isNaN(Number(btcValue))) return "\u2014";
    const x = Number(btcValue);
    if (Math.abs(x) < 5e-9) return "0 sats";
    if (Math.abs(x) >= 0.01) return sig4(x) + " " + TICKER;
    const s = x * 1e8;
    const r = Math.abs(s) >= 10000 ? Number(s.toPrecision(4)) : Math.round(s);
    return r.toLocaleString() + " sats";
  };
  // Same, from an integer sat count.
  const amtSats = (sats) => (sats == null || Number.isNaN(Number(sats)) ? "\u2014" : amt(Number(sats) / 1e8));
  // Exact figure for tooltips: full 8-place XBT plus the sat count.
  const amtExact = (btcValue) => {
    if (btcValue == null || Number.isNaN(Number(btcValue))) return "";
    const x = Number(btcValue);
    return x.toFixed(8).replace(/0+$/, "").replace(/\.$/, "") + " " + TICKER + " (" + Math.round(x * 1e8).toLocaleString() + " sats)";
  };
  const when = (ts) => (ts ? new Date(ts * 1000).toLocaleString([], { dateStyle: "medium", timeStyle: "short" }) : "");
  const clock = (ts) => (ts ? new Date(ts * 1000).toLocaleTimeString([], { hour: "numeric", minute: "2-digit" }) : "");
  const kindPill = (kind) => {
    // primed prefixes orphaned records ("orphan:split"); the Status column carries that, this pill says what the coinbase did.
    const k = String(kind || "").toLowerCase().replace(/^orphan:/, "");
    const label = k === "split" ? "split" : k === "partial" ? "partial split" : k === "pool-only" ? "pool only" : k || "\u2014";
    const cls = k === "split" ? "ok" : k === "partial" ? "warn" : k === "pool-only" ? "warn" : "";
    return k ? `<span class="pill ${cls}">${esc(label)}</span>` : "\u2014";
  };
  const statusPill = (status) => {
    const s = String(status || "").toLowerCase();
    const cls = s === "in chain" || s === "paid" || s === "submitted" || s === "spendable" ? "ok" : s === "orphaned" || s === "rejected" ? "bad" : s === "immature" || s === "pending" ? "warn" : "";
    return s ? `<span class="pill ${cls}">${esc(s)}</span>` : "\u2014";
  };
  function chartLegend(el, c, one, many) {
    const n = (c && c.__hits ? c.__hits : []).reduce((a, hit) => a + hit.items.length, 0);
    const label = n ? `${n} ${n === 1 ? one : many}` : "";
    if (el) {
      // The noun is dropped in a narrow container (see .chart-legend-noun), where the
      // caption has no room for it; the arrow and the count carry the meaning.
      el.innerHTML = n ? `${n}<span class="chart-legend-noun"> ${esc(n === 1 ? one : many)}</span>` : "";
      el.hidden = !n;
    }
    if (c) {
      const base = c.dataset.label || c.getAttribute("aria-label") || "";
      c.dataset.label = base;
      c.setAttribute("aria-label", n ? `${base}, with ${label} marked` : base);
    }
  }

  // One trend marker per found block. Orphans are marked too and say so, since the pool did
  // find them; the reward just did not stick.
  function blockMarks(blocks) {
    return (blocks || [])
      .filter((b) => Number(b.ts) > 0)
      .map((b) => {
        const reward = Number(b.reward) || (Number(b.miner_btc) || 0) + (Number(b.pool_btc) || 0);
        const st = String(b.prime_only ? b.block_status : b.status || "").toLowerCase();
        const note = st === "orphaned" || st === "rejected" ? " · " + st : "";
        return {
          ts: Number(b.ts),
          title: (b.height ? "Block " + num(b.height) : "Block found") + " · " + clock(b.ts),
          sub: (reward ? amt(reward) : "") + note,
        };
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

  // Markers on the trend: one tick per block found, hover for height and amount. `marks` is
  // [{ts, title, sub}]; ticks closer together than a few pixels share one arrow and tooltip.
  const CHART_MARK = "oklch(76% 0.13 155)";
  const MARK_GAP_PX = 7;

  function chartTip(c) {
    const wrap = c.parentElement;
    if (!wrap) return null;
    let tip = wrap.querySelector(".chart-tip");
    if (!tip) {
      tip = document.createElement("div");
      tip.className = "chart-tip";
      tip.hidden = true;
      wrap.appendChild(tip);
    }
    return tip;
  }

  function markHover(c) {
    if (c.__markHover) return;
    c.__markHover = true;
    const hide = () => {
      const tip = chartTip(c);
      if (tip) tip.hidden = true;
    };
    const show = (e) => {
      const hits = c.__hits || [];
      const tip = chartTip(c);
      if (!tip) return;
      if (!hits.length) return hide();
      const r = c.getBoundingClientRect();
      const x = e.clientX - r.left;
      let best = null;
      for (const hit of hits) {
        const d = Math.abs(hit.x - x);
        if (d <= 11 && (!best || d < Math.abs(best.x - x))) best = hit;
      }
      if (!best) return hide();
      tip.innerHTML = best.html;
      tip.hidden = false;
      // Keep the tooltip inside the plot; flip it left of the tick near the right edge.
      const tw = tip.offsetWidth;
      tip.style.left = Math.max(2, Math.min(r.width - tw - 2, best.x - tw / 2)) + "px";
    };
    c.addEventListener("pointermove", show);
    c.addEventListener("pointerdown", show);
    c.addEventListener("pointerleave", hide);
  }

  function draw(c, hist, key, marks) {
    if (!c) return;
    c.__hist = hist || [];
    c.__marks = marks || [];
    c.__hits = [];
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
    // Marks get their own row between the caption and the plot, so the arrows never sit on
    // top of the caption or the line. No marks in view, no row.
    const inView = c.__marks
      .filter((m) => m && Number.isFinite(Number(m.ts)) && m.ts >= minX && m.ts <= maxX)
      .sort((a, b) => a.ts - b.ts); // clustering below walks left to right
    const pad = { l: 6, r: 52, t: inView.length ? 30 : 22, b: 18 };
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
    // Cluster the marks by pixel so a burst of blocks reads as one arrow, then draw the
    // ticks under the area fill and the arrows over the line.
    const hits = [];
    for (const m of inView) {
      const x = pad.l + ((m.ts - minX) / (maxX - minX)) * w;
      const prev = hits[hits.length - 1];
      if (prev && x - prev.x <= MARK_GAP_PX) prev.items.push(m);
      else hits.push({ x, items: [m] });
    }
    for (const hit of hits) {
      const n = hit.items.length;
      const head = n === 1 ? "" : `<b>${n} blocks</b>`;
      const rows = hit.items
        .slice(0, 4)
        .map((m) => `<span>${esc(m.title || "")}</span>${m.sub ? `<span class="faint">${esc(m.sub)}</span>` : ""}`)
        .join("");
      hit.html = head + rows + (n > 4 ? `<span class="faint">+${n - 4} more</span>` : "");
    }
    c.__hits = hits;
    if (hits.length) {
      ctx.save();
      ctx.strokeStyle = CHART_MARK;
      ctx.globalAlpha = 0.3;
      ctx.lineWidth = 1;
      ctx.setLineDash([2, 3]);
      for (const hit of hits) {
        ctx.beginPath();
        ctx.moveTo(hit.x, pad.t);
        ctx.lineTo(hit.x, pad.t + h);
        ctx.stroke();
      }
      ctx.restore();
      markHover(c);
    }
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
    for (const hit of c.__hits || []) {
      ctx.save();
      ctx.fillStyle = CHART_MARK;
      ctx.beginPath();
      ctx.moveTo(hit.x - 3.5, pad.t - 7);
      ctx.lineTo(hit.x + 3.5, pad.t - 7);
      ctx.lineTo(hit.x, pad.t - 2);
      ctx.closePath();
      ctx.fill();
      ctx.restore();
    }
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

  // ------------------------------------------------------------ miner card
  // One renderer for the "Your stats" card on the front page and the /miner/<addr> page.
  // `ctx` carries what differs between the two: fee schedule, USD price, the block
  // interval (for maturity ETAs) and whether this is the full page.
  const EXPLORER = "https://mempool.awokenlazarus.xyz";
  const pathLabel = (via, name) => {
    const gw = String(name || "").trim();
    if (via === "prime" || via === "gateway") return gw || "own gateway";
    if (via === "both") return gw ? `stratum + ${gw}` : "stratum + gateway";
    return "public stratum";
  };
  const isPrimePath = (via) => via === "prime" || via === "gateway";
  const sessCell = (via, n) => (isPrimePath(via) ? "\u2014" : num(n ?? 0));
  const winShareCell = (m, show) => {
    if (show === false) return "\u2014";
    const n = Number(m.window_shares);
    return Number.isFinite(n) && n > 0 ? num(n) : "\u2014";
  };
  const feePct = (x) => (Number.isFinite(Number(x)) ? Number(x).toLocaleString(undefined, { maximumFractionDigits: 2 }) + "%" : "\u2014");
  const poolLink = (name, url) => (url ? `<a href="${esc(url)}" target="_blank" rel="noreferrer">${esc(name || url)}</a>` : esc(name || "\u2014"));
  const payStatus = (b) => {
    const s = String(b.status || "").toLowerCase();
    const rs = String(b.round_status || "").toLowerCase();
    if (s === "immature") return "immature";
    if (s === "carried" || rs === "unsplit") return "carried";
    if (s === "paid" || s === "unpaid") return "paid";
    return s || rs || "\u2014";
  };
  const blockLink = (b) => {
    const h = b.height != null ? String(b.height) : "\u2014";
    return b.hash ? `<a href="${EXPLORER}/block/${esc(b.hash)}" target="_blank" rel="noreferrer" title="${esc(b.hash)}">${h}</a>` : h;
  };
  const addrLine = (address, cls, status) => `
        <div class="addr-line">
          <span class="copyable"><span class="mono">${esc(address)}</span><button type="button" class="copy-btn" data-copy="${esc(address)}" aria-label="Copy address" title="Copy"></button></span>
          <span class="status-pill ${cls}">${status}</span>
        </div>`;

  function soloCard(m, so) {
    if (!so) return "";
    const hashing = Number(so.hashrate_ghs) > 1e-6;
    const status = hashing
      ? `online · solo${so.via ? " " + so.via : ""}`
      : so.shares || so.work
        ? "idle · solo"
        : "solo";
    const blocks = so.blocks_list || [];
    const pays = blocks
      .map((b) => `<tr><td class="num">${blockLink(b)}</td><td class="num" title="${amtExact(b.miner_btc)}">${amt(b.miner_btc)}</td><td class="num">${amt(b.pool_fee_btc)}</td><td>${when(b.ts)}</td></tr>`)
      .join("");
    return `
      <div class="panel miner-card">
        ${addrLine(m.address || so.address, hashing ? "ok" : "bad", status)}
        <p class="note callout">This address is on a solo port. Shares here buy no TIDES window share. A block you find pays this address in that block’s coinbase, less the solo fee.</p>
        <dl class="ticker">
          <div><dt>Hashrate</dt><dd>${fmtHr(so.hashrate_ghs)}<small>${so.workers ? so.workers + " worker" + (so.workers === 1 ? "" : "s") : "no workers hashing"}</small></dd></div>
          <div><dt>Accepted</dt><dd>${num(so.shares)}<small>${num(so.work)} work on this solo book</small></dd></div>
          <div><dt>Best share</dt><dd>${so.best_diff ? num(so.best_diff) : "\u2014"}<small>highest difficulty accepted</small></dd></div>
          <div><dt>Fee</dt><dd>${feePct(so.fee_percent)}<small>taken in the coinbase if you find a block</small></dd></div>
          <div><dt>Time to a block</dt><dd>${dur(so.ttf_seconds)}<small>at this hashrate vs current network difficulty</small></dd></div>
          <div><dt>Blocks found</dt><dd>${num(so.blocks_onchain || so.blocks || blocks.length)}<small>paid in the block itself</small></dd></div>
        </dl>
        <div>
          <p class="kicker table-label">Solo blocks</p>
          <div class="scroll"><table><thead><tr><th class="num">Height</th><th class="num">Amount</th><th class="num">Fee</th><th>Time</th></tr></thead><tbody>${pays || '<tr><td colspan="4" class="empty">No solo block yet</td></tr>'}</tbody></table></div>
        </div>
      </div>`;
  }

  // Pending (immature) coinbase outputs, alphapool-style: how far each one is from
  // spendable, in confirmations and blocks, with an ETA from the pool's block interval.
  function pendingTable(m, ctx) {
    const need = Number(m.maturity_confs) || 100;
    const tip = Number(m.tip_height) || 0;
    const interval = Number(ctx.blockInterval) || 0;
    const rows = (m.blocks_found || []).filter((b) => payStatus(b) === "immature");
    if (!rows.length) return "";
    const body = rows
      .map((b) => {
        const confs = Number.isFinite(Number(b.confirmations)) ? Number(b.confirmations) : Math.max(0, tip - Number(b.height) + 1);
        const left = Number.isFinite(Number(b.blocks_to_mature)) ? Number(b.blocks_to_mature) : Math.max(0, need - confs + 1);
        const p = Math.max(0, Math.min(100, (100 * confs) / need));
        const eta = interval && left ? "~" + dur(left * interval) : left ? "\u2014" : "next block";
        return `<tr>
          <td class="num">${blockLink(b)}</td>
          <td class="num" title="${amtExact(b.miner_btc)}">${amt(b.miner_btc)}${ctx.money ? ctx.money(b.miner_btc) : ""}</td>
          <td class="num"><span class="mature"><span>${num(confs)} / ${num(need)}</span><span class="mature-bar" style="--p:${p.toFixed(1)}%"></span></span></td>
          <td class="num">${num(left)}</td>
          <td>${eta}</td>
          <td>${statusPill(left ? "immature" : "maturing")}</td>
          <td>${when(b.ts)}</td>
        </tr>`;
      })
      .join("");
    return `
        <div>
          <p class="kicker table-label">Pending · ${rows.length} block${rows.length === 1 ? "" : "s"} maturing</p>
          <p class="note">A coinbase output can be spent ${need} blocks after the block that carries it. Yours is already on chain; it just is not spendable yet.</p>
          <div class="scroll${ctx.full ? "" : " tall"}"><table class="pending"><thead><tr><th class="num">Block</th><th class="num">Amount</th><th class="num">Confirmations</th><th class="num">Blocks to go</th><th>Spendable in</th><th>Status</th><th>Found</th></tr></thead><tbody>${body}</tbody></table></div>
        </div>`;
  }

  function paidTable(m, ctx) {
    const rows = (m.blocks_found || []).filter((b) => payStatus(b) !== "immature");
    const body = rows
      .map((b) => `<tr><td class="num">${blockLink(b)}</td><td class="num" title="${amtExact(b.miner_btc)}">${amt(b.miner_btc)}${ctx.money ? ctx.money(b.miner_btc) : ""}</td><td class="num">${Number.isFinite(Number(b.share)) ? pct(100 * Number(b.share), 2) : "\u2014"}</td><td>${statusPill(payStatus(b))}</td><td>${when(b.ts)}</td></tr>`)
      .join("");
    return `
        <div>
          <p class="kicker table-label">Paid · matured coinbase outputs</p>
          <div class="scroll tall"><table><thead><tr><th class="num">Block</th><th class="num">Amount</th><th class="num">Of block</th><th>Status</th><th>Found</th></tr></thead><tbody>${body || '<tr><td colspan="5" class="empty">Nothing matured yet</td></tr>'}</tbody></table></div>
        </div>`;
  }

  const MINER_TABS = [
    ["overview", "Overview"],
    ["workers", "Workers"],
    ["payouts", "Payouts"],
  ];

  // Returns the card's HTML. `ctx`: { fees:{datum,stratum}, money(btc)->" · $x", blockInterval,
  // tab, full, href(tab)->url }. Tabs are anchors when `href` is given (so the URL follows), else buttons.
  function minerCard(m, ctx) {
    ctx = ctx || {};
    const so = m.solo;
    const hasSolo = !!so;
    const hasPool = !!(m.via || Number(m.window_work) > 0 || Number(m.shares_lifetime) > 0 || Number(m.round_share) > 0);
    if (!m.known && !hasSolo) return '<p class="note callout">No stats for that address yet. Connect a miner first.</p>';
    if (!hasPool && hasSolo) return soloCard(m, so);
    const fees = ctx.fees || { datum: 0, stratum: 3 };
    const money = ctx.money || (() => "");
    const feeForPath = (path) => (String(path || "").toLowerCase() === "stratum" ? fees.stratum : fees.datum);
    const tab = MINER_TABS.some(([k]) => k === ctx.tab) ? ctx.tab : "overview";

    const workerHr = (w) => (w.via === "stratum" ? (Number(w.firmware_hr_ghs) > 0 ? w.firmware_hr_ghs : null) : w.hr_ghs);
    const relayed = m.relayed || [];
    const workers = (m.workers || [])
      .map(
        (w) =>
          `<tr><td>${esc(w.worker || "\u2014")}</td><td>${pathLabel(w.via, w.gateway_name || m.gateway_name)}</td><td class="num">${fmtHr(workerHr(w))}</td><td class="num">${sessCell(w.via, w.shares_session)}</td><td class="num">${num(w.shares_lifetime ?? w.shares_acc ?? w.window_work)}</td><td class="num">${num(w.shares_rej)}</td><td class="num">${Number.isFinite(Number(w.last_share_s)) ? Number(w.last_share_s).toFixed(0) + "s" : "\u2014"}</td></tr>`
      )
      .concat(
        relayed.map(
          (r) =>
            `<tr><td>${esc(r.worker || "\u2014")}</td><td>relayed &rarr; ${poolLink(r.upstream, r.miner_url || r.upstream_url)}</td><td class="num">\u2014</td><td class="num">\u2014</td><td class="num">${num(r.accepted)} <span class="faint">there</span></td><td class="num">\u2014</td><td class="num">${dur(Number(r.connected_s) || 0)}</td></tr>`
        )
      )
      .join("");
    let relayNote = "";
    if (relayed.length) {
      const pools = [...new Map(relayed.map((r) => [r.upstream, poolLink(r.upstream, r.miner_url || r.upstream_url)])).values()];
      relayNote = `<p class="note callout"><strong>${relayed.length === 1 ? "This worker is" : relayed.length + " of your workers are"} mining on ${pools.join(" and ")}, not on Lazarus.</strong> Lazarus was over its network-share limit when ${relayed.length === 1 ? "it" : "they"} connected, so our stratum passed the connection straight through. ${pools.length === 1 ? pools[0] : "That pool"} pays for that work under this address; nothing from it lands in our window. Follow the link for your stats there. Reconnecting after we drop under the limit brings the worker back here.</p>`;
    }
    const gwName = String(m.gateway_name || "").trim();
    const status = !m.online
      ? (relayed.length ? "relayed · " + esc(relayed[0].upstream || "other pool") : "offline")
      : isPrimePath(m.via)
        ? (gwName ? "online · " + gwName : "online · own gateway")
        : m.via === "both"
          ? (gwName ? "online · stratum + " + gwName : "online · stratum + gateway")
          : "online · public stratum";
    const wp = (m.round_share || 0) * 100;
    const hp = Number(m.hashrate_pool_percent) || 0;
    const nblocks = Number(m.window_multiple) || 8;
    let windowNote;
    if (m.online && hp > 1 && wp < hp * 0.5) {
      windowNote = `<p class="note callout">Your hashrate is ${pctSmart(hp)} of the pool right now, but you hold ${pctSmart(wp)} of the ${nblocks}-block payout window. New hash ramps in as work accumulates and older work ages out — that gap is expected, not a missing payout.</p>`;
    } else {
      windowNote = `<p class="note callout">Next-block pay is the window % (${pctSmart(wp)}), not hashrate. The window is ${nblocks} network-blocks of accepted work (TIDES). A newly connected high-hashrate miner does not take a matching slice of the next block.</p>`;
    }
    const billedFee = m.est_fee_percent != null ? m.est_fee_percent : (m.fee_percent_path != null ? m.fee_percent_path : feeForPath(m.fee_path));
    const carryBtc = Number(m.carry_btc) || 0;
    const floorBtc = Number(m.min_payout_btc) || 0;
    // The "Next block" figure is Prime's own output for this address. Zero with work in the
    // window means the share is under the floor this block; it accrues as carry, never lost.
    const nextBlockNote = (m) => {
      const path = m.fee_path ? " · your window work is on the " + feePct(m.fee_percent_path != null ? m.fee_percent_path : feeForPath(m.fee_path)) + " " + (m.fee_path === "stratum" ? "public-stratum" : "own-gateway") + " rate" : "";
      if (m.next_block_exact && !(Number(m.block_payout_btc) > 0) && Number(m.window_work) > 0) {
        return `under the ${floorBtc > 0 ? amt(floorBtc) + " " : ""}minimum output this block · your share is carried forward, not forfeited${path}`;
      }
      if (m.next_block_exact && carryBtc > 0 && Number(m.block_payout_btc) > 0) {
        return `your output in the coinbase Prime dictates now · includes carry from earlier blocks as room allows${path}`;
      }
      return `your output in the coinbase Prime dictates now${path}`;
    };
    const carryCell = (m) =>
      carryBtc > 0
        ? `<div><dt>Carried forward</dt><dd title="${amtExact(carryBtc)}">${amt(carryBtc)}${money(carryBtc)}<small>earned in earlier blocks, under the minimum output · paid on top of your next output that clears it</small></dd></div>`
        : "";
    // The DATUM bonus for this address. On the gateway path it is money already accruing, so
    // it gets a ticker cell; on the stratum path it is money being left on the table, so it
    // gets a callout with what switching would pay. Both vanish when the rebate is off.
    const onStratum = String(m.fee_path || "").toLowerCase() === "stratum";
    const upliftPct = Number(m.datum_uplift_percent) || 0;
    const rebatePct = Number(m.datum_rebate_percent) || 0;
    const bonusDay = Number(m.est_bonus_btc_day) || 0;
    const datumDay = Number(m.est_datum_btc_day) || 0;
    const bonusCell =
      rebatePct > 0 && !onStratum && (Number(m.rebate_btc) > 0 || upliftPct > 0)
        ? `<div><dt>DATUM bonus</dt><dd title="${amtExact(m.rebate_btc)}">${amt(m.rebate_btc)}${money(m.rebate_btc)}<small>credited to your balance by the next block${upliftPct > 0 ? " · " + pctSmart(upliftPct) + " above your proportional share" : ""} · your cut of the public stratum's fee, paid with your next output</small></dd></div>`
        : "";
    // A gateway miner's daily estimate already includes the bonus; a stratum miner's does not,
    // and the pitch below says what it would be worth.
    const withBonus = rebatePct > 0 && !onStratum && upliftPct > 0 && datumDay > 0;
    const estDay = withBonus ? datumDay : Number(m.est_btc_day) || 0;
    const estDayNote = withBonus
      ? `at current difficulty, ${feePct(billedFee)} fee and the ${pctSmart(upliftPct)} DATUM bonus included · once the window matches this hashrate`
      : `at current difficulty, after the ${feePct(billedFee)} fee · once the window matches this hashrate`;
    const switchPitch =
      rebatePct > 0 && onStratum && upliftPct > 0
        ? `<p class="note callout rebate-callout"><strong>You are paying ${feePct(fees.stratum)} and funding the DATUM bonus.</strong> Of that fee, ${feePct(rebatePct)} of your work's value is not kept by the pool at all — it is credited to the miners who build their own blocks. Point this hashrate through your own DATUM gateway and you stop paying the fee <em>and</em> start collecting that credit: about <b>${amt(datumDay)}</b>${money(datumDay)} a day at this hashrate instead of ${amt(m.est_btc_day)}${money(m.est_btc_day)}, and ${pctSmart(upliftPct)} of that is the bonus alone (${amt(bonusDay)}${money(bonusDay)} a day). Your accepted work comes with you. <a href="/#connect">Set up a gateway</a>.</p>`
        : "";
    const nWorkers = (m.workers || []).length + relayed.length;
    const nPending = Number(m.immature_blocks) || (m.blocks_found || []).filter((b) => payStatus(b) === "immature").length;
    const nPaid = (m.blocks_found || []).length - (m.blocks_found || []).filter((b) => payStatus(b) === "immature").length;

    const tabBtn = ([key, label], count) => {
      const on = key === tab;
      const inner = `${label}${count ? ` <b>${count}</b>` : ""}`;
      if (ctx.href) return `<a role="tab" class="mtab" data-mtab="${key}" aria-selected="${on}" href="${esc(ctx.href(key))}">${inner}</a>`;
      return `<button type="button" role="tab" class="mtab" data-mtab="${key}" aria-selected="${on}">${inner}</button>`;
    };
    const counts = { workers: nWorkers, payouts: nPending ? `${nPending} pending` : "" };
    const tablist = `<div class="mtabs" role="tablist" aria-label="Miner views">${MINER_TABS.map((t) => tabBtn(t, counts[t[0]])).join("")}${ctx.full ? "" : `<a class="mtab-more" href="/miner/${encodeURIComponent(m.address)}">Full page &rarr;</a>`}</div>`;

    const overview = `
      <div class="mpanel" data-mpanel="overview" ${tab === "overview" ? "" : "hidden"}>
        ${relayNote}
        ${switchPitch}
        ${windowNote}
        <dl class="ticker">
          <div><dt>Hashrate</dt><dd>${fmtHr(m.hr_ghs || 0)}<small>${Number(m.hr_1h_ghs) > 0 ? "1h avg " + fmtHr(m.hr_1h_ghs) + " · 24h avg " + fmtHr(m.hr_24h_ghs) : "best " + fmtHr(m.best_hr_ghs || 0)} · ${pctSmart(hp)} of pool</small></dd></div>
          <div><dt>Accepted</dt><dd>${num(m.shares_lifetime ?? m.shares_acc)}<small>stays with this address on either path</small></dd></div>
          <div><dt>This session</dt><dd>${sessCell(m.via, m.shares_session)}<small>${isPrimePath(m.via) ? "own gateway · Prime credits the window directly" : "public stratum only · resets on reconnect"}</small></dd></div>
          <div><dt>This window</dt><dd>${winShareCell(m)}<small>${Number(m.window_shares) > 0 ? num(m.window_work) + " work still in the TIDES window" : "no accepted shares in the current window"}</small></dd></div>
          <div><dt>Payout window</dt><dd>${pctSmart(wp)}<small>${num(m.window_work)} work · what the next block pays</small></dd></div>
          <div><dt>Est. / day</dt><dd title="${amtExact(estDay)}">${amt(estDay)}${money(estDay)}<small>${estDayNote}</small></dd></div>
          ${bonusCell}
          <div><dt>Next block</dt><dd title="${amtExact(m.block_payout_btc)}">${amt(m.block_payout_btc)}${money(m.block_payout_btc)}<small>${nextBlockNote(m)}</small></dd></div>
          ${carryCell(m)}
          <div><dt>Pending</dt><dd title="${amtExact(m.immature_btc)}">${amt(m.immature_btc)}${money(m.immature_btc)}<small>${nPending ? nPending + " block" + (nPending === 1 ? "" : "s") + " maturing · see Payouts" : "nothing waiting to mature"}</small></dd></div>
          <div><dt>Paid</dt><dd title="${amtExact(m.paid_btc)}">${amt(m.paid_btc)}${money(m.paid_btc)}<small>matured coinbase outputs, lifetime</small></dd></div>
        </dl>
      </div>`;

    const workersPanel = `
      <div class="mpanel" data-mpanel="workers" ${tab === "workers" ? "" : "hidden"}>
        ${relayNote}
        <p class="note">One row per stratum session. “Public stratum” is our gateway; “own gateway” is share credit arriving through a DATUM gateway you run. Session hashrate is what each connection reports; the credited total is the Hashrate figure on Overview.</p>
        <div class="scroll tall"><table><thead><tr><th>Worker</th><th>Path</th><th class="num">Hashrate</th><th class="num">Session</th><th class="num">Accepted</th><th class="num">Rejects</th><th class="num">Last</th></tr></thead><tbody>${workers || '<tr><td colspan="7" class="empty">Offline</td></tr>'}</tbody></table></div>
      </div>`;

    const payoutsPanel = `
      <div class="mpanel" data-mpanel="payouts" ${tab === "payouts" ? "" : "hidden"}>
        <dl class="ticker slim four">
          <div><dt>Pending</dt><dd title="${amtExact(m.immature_btc)}">${amt(m.immature_btc)}${money(m.immature_btc)}<small>${nPending ? "in " + nPending + " block" + (nPending === 1 ? "" : "s") + " under " + num(m.maturity_confs || 100) + " confirmations" : "no coinbase outputs maturing"}</small></dd></div>
          <div><dt>Paid</dt><dd title="${amtExact(m.paid_btc)}">${amt(m.paid_btc)}${money(m.paid_btc)}<small>${nPaid} matured block${nPaid === 1 ? "" : "s"}, lifetime</small></dd></div>
          <div><dt>Next block</dt><dd title="${amtExact(m.block_payout_btc)}">${amt(m.block_payout_btc)}${money(m.block_payout_btc)}<small>${pctSmart(wp)} of the window · in the next coinbase${Number(m.carry_btc) > 0 ? " · plus carry as room allows" : ""}</small></dd></div>
          <div><dt>Est. / day</dt><dd title="${amtExact(estDay)}">${amt(estDay)}${money(estDay)}<small>${withBonus ? `${feePct(billedFee)} fee and the ${pctSmart(upliftPct)} bonus, at current difficulty` : `after the ${feePct(billedFee)} fee at current difficulty`}</small></dd></div>
        </dl>
        <p class="note callout">There is no pool balance and nothing to withdraw. Every block the pool finds pays this address directly in its coinbase; the output becomes spendable ${num(m.maturity_confs || 100)} blocks later.${Number(m.carry_btc) > 0 ? ` <strong>${amt(m.carry_btc)}</strong> you earned in earlier blocks was under the minimum output and is carried forward: it is added to your next output that clears the floor, paid out of the pool's share.` : ""}</p>
        ${pendingTable(m, ctx)}
        ${paidTable(m, ctx)}
      </div>`;

    return `
      <div class="panel miner-card" data-miner="${esc(m.address)}">
        ${addrLine(m.address, m.online ? "ok" : relayed.length ? "warn" : "bad", status)}
        ${tablist}
        ${overview}${workersPanel}${payoutsPanel}
      </div>${hasSolo ? soloCard(m, so) : ""}`;
  }

  // Switch the visible panel inside a rendered card. Returns the tab that is now shown.
  function showMinerTab(root, key) {
    if (!root) return null;
    const want = MINER_TABS.some(([k]) => k === key) ? key : "overview";
    root.querySelectorAll(".mtab[data-mtab]").forEach((t) => t.setAttribute("aria-selected", t.getAttribute("data-mtab") === want ? "true" : "false"));
    root.querySelectorAll(".mpanel[data-mpanel]").forEach((p) => { p.hidden = p.getAttribute("data-mpanel") !== want; });
    return want;
  }

  // Keep in-page anchors from landing under the sticky nav (+ DATUM banner). --mast is the
  // live height of .site-mast; CSS scroll-margin/padding read it.
  (() => {
    const mast = document.querySelector(".site-mast") || document.querySelector(".site-header");
    if (!mast) return;
    const sync = () => {
      document.documentElement.style.setProperty("--mast", `${Math.round(mast.getBoundingClientRect().height)}px`);
    };
    sync();
    if (typeof ResizeObserver === "function") new ResizeObserver(sync).observe(mast);
    window.addEventListener("resize", sync, { passive: true });
  })();

  window.LZ = {
    blockMarks, esc, fmtHr, num, bigNum, short, shortHash, pct, pctSmart, ago, agoS, dur, when, clock, sig4, amt, amtSats, amtExact,
    kindPill, statusPill, chartLegend, continuous, chartTip, markHover, draw, CHART_MARK,
    EXPLORER, pathLabel, isPrimePath, sessCell, winShareCell, feePct, poolLink, payStatus, blockLink, soloCard, minerCard, showMinerTab, MINER_TABS,
  };
})();
