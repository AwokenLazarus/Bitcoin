(() => {
  const $ = (id) => document.getElementById(id);

  const fmtHr = (ghs) => {
    if (ghs == null || Number.isNaN(ghs)) return "—";
    const hs = Number(ghs) * 1e9;
    if (hs >= 1e15) return (hs / 1e15).toFixed(2) + " PH/s";
    if (hs >= 1e12) return (hs / 1e12).toFixed(2) + " TH/s";
    if (hs >= 1e9) return (hs / 1e9).toFixed(2) + " GH/s";
    if (hs >= 1e6) return (hs / 1e6).toFixed(2) + " MH/s";
    if (hs >= 1e3) return (hs / 1e3).toFixed(1) + " KH/s";
    if (hs > 0) return Math.round(hs) + " H/s";
    return "0 H/s";
  };

  const short = (a) => (a && a.length > 20 ? a.slice(0, 10) + "\u2026" + a.slice(-8) : a || "\u2014");
  const ago = (ts) => {
    if (!ts) return "\u2014";
    const s = Math.max(0, Date.now() / 1000 - ts);
    if (s < 90) return Math.round(s) + "s ago";
    if (s < 3600) return Math.round(s / 60) + "m ago";
    if (s < 86400) return (s / 3600).toFixed(1) + "h ago";
    return (s / 86400).toFixed(1) + "d ago";
  };
  const dur = (s) => {
    if (s == null) return "\u2014";
    if (s < 3600) return (s / 60).toFixed(0) + " min";
    if (s < 86400) return (s / 3600).toFixed(1) + " hours";
    if (s < 86400 * 60) return (s / 86400).toFixed(1) + " days";
    return (s / 86400 / 365).toFixed(1) + " years";
  };
  const btc = (n) => (n == null || Number.isNaN(n) ? "\u2014" : Math.abs(n) >= 0.01 ? n.toFixed(4) : n.toExponential(2));
  const j = async (url) => {
    const r = await fetch(url);
    if (!r.ok) throw new Error(url + " " + r.status);
    return r.json();
  };

  function stats(p) {
    const net = p.network_hr_hs ? (p.network_hr_hs / 1e15).toFixed(2) + " PH/s" : "\u2014";
    const luck = p.luck_percent == null ? "\u2014" : p.luck_percent.toFixed(0) + "%";
    const cells = [
      ["Hashrate", fmtHr(p.pool_hr_ghs), (p.miners_online || 0) + " worker" + (p.miners_online === 1 ? "" : "s")],
      ["Miners", String(p.miners_seen ?? "\u2014"), (p.miners_online || 0) + " online"],
      ["Shares", String(p.shares_accepted ?? 0), (p.shares_rejected || 0) + " rejected"],
      ["Fee", (p.fee_percent ?? 0.5) + "%", "coinbase remainder"],
      ["Est. BTC/day", btc(p.est_btc_day), "vs the whole network"],
      ["Time to block", dur(p.ttf_seconds), ((p.pool_share || 0) * 100).toExponential(2) + "% of net"],
      ["Found", String(p.blocks_found ?? 0), p.luck_percent == null ? "luck pending" : luck + " luck"],
      ["Tip", String(p.height || "\u2014"), "diff " + (p.difficulty ? Number(p.difficulty).toExponential(3) : "\u2014")],
      ["Network", net, "BLAKE2b"],
      ["Round cut", (p.finder_payout_btc || 0).toFixed(3) + " BTC", "99.5% of a block"],
    ];
    $("stats").innerHTML = cells
      .map(([k, v, s]) => `<div><dt>${k}</dt><dd>${v}<small>${s}</small></dd></div>`)
      .join("");
    $("stratum").textContent = p.stratum;
    $("payoutline").textContent = p.payout;
    if ($("live-hr")) $("live-hr").textContent = fmtHr(p.pool_hr_ghs);
    if ($("live-tip")) $("live-tip").textContent = p.height || "\u2014";
    draw($("poolchart"), p.history || [], "hr_ghs");
  }

  function table(el, headers, rows) {
    el.innerHTML =
      "<thead><tr>" +
      headers.map((h) => "<th>" + h + "</th>").join("") +
      "</tr></thead><tbody>" +
      (rows.length
        ? rows.map((r) => "<tr>" + r.map((c) => "<td>" + c + "</td>").join("") + "</tr>").join("")
        : '<tr><td colspan="' + headers.length + '">None yet</td></tr>') +
      "</tbody>";
  }

  function draw(c, hist, key) {
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
    const xs = hist.map((h) => h.ts);
    const ys = hist.map((h) => h[key] || 0);
    const minY = 0;
    const maxY = Math.max(...ys, 0.01);
    const minX = xs[0];
    const maxX = xs[xs.length - 1] || minX + 1;
    const pad = { l: 6, r: 6, t: 22, b: 8 };
    const w = cssW - pad.l - pad.r;
    const h = cssH - pad.t - pad.b;
    const pt = (item) => {
      const x = pad.l + ((item.ts - minX) / (maxX - minX)) * w;
      const y = pad.t + h - ((item[key] || 0) - minY) / (maxY - minY) * h;
      return [x, y];
    };
    ctx.beginPath();
    hist.forEach((item, i) => {
      const [x, y] = pt(item);
      i ? ctx.lineTo(x, y) : ctx.moveTo(x, y);
    });
    const last = pt(hist[hist.length - 1]);
    ctx.lineTo(last[0], pad.t + h);
    ctx.lineTo(pad.l, pad.t + h);
    ctx.closePath();
    const fill = ctx.createLinearGradient(0, pad.t, 0, pad.t + h);
    fill.addColorStop(0, "rgba(212,180,90,0.28)");
    fill.addColorStop(1, "rgba(212,180,90,0)");
    ctx.fillStyle = fill;
    ctx.fill();
    ctx.beginPath();
    hist.forEach((item, i) => {
      const [x, y] = pt(item);
      i ? ctx.lineTo(x, y) : ctx.moveTo(x, y);
    });
    ctx.strokeStyle = "#d4b45a";
    ctx.lineWidth = 1.4;
    ctx.lineJoin = "round";
    ctx.stroke();
  }

  async function showMiner(addr) {
    if (!addr) {
      $("miner").innerHTML = "";
      draw($("chart"), [], "hr_ghs");
      return;
    }
    const m = await j("/api/miner/" + encodeURIComponent(addr));
    if (!m.known) {
      $("miner").innerHTML = '<p class="note">No stats for that address yet. Connect a miner first.</p>';
      return;
    }
    const workers = (m.workers || [])
      .map(
        (w) =>
          `<tr><td>${w.worker || "\u2014"}</td><td>${fmtHr(w.hr_ghs)}</td><td>${w.shares_acc}</td><td>${w.shares_rej}</td><td>${w.vdiff}</td><td>${w.last_share_s ? w.last_share_s.toFixed(0) + "s" : "\u2014"}</td><td>${w.ua || ""}</td></tr>`
      )
      .join("");
    const pays = (m.blocks_found || [])
      .map((b) => `<tr><td>${b.height}</td><td>${btc(b.miner_btc)}</td><td>${b.ts ? new Date(b.ts * 1000).toLocaleString() : ""}</td></tr>`)
      .join("");
    $("miner").innerHTML = `
      <div class="panel miner-card">
        <div class="addr-line">
          <span>${m.address}</span>
          <span class="${m.online ? "ok" : "bad"}">${m.online ? "online" : "offline"}</span>
        </div>
        <dl class="ticker">
          <div><dt>Hashrate</dt><dd>${fmtHr(m.hr_ghs || 0)}<small>best ${fmtHr(m.best_hr_ghs || 0)}</small></dd></div>
          <div><dt>Shares</dt><dd>${m.shares_acc}<small>${m.shares_rej} rejected</small></dd></div>
          <div><dt>This round</dt><dd>${((m.round_share || 0) * 100).toFixed(1)}%<small>of accepted work</small></dd></div>
          <div><dt>Est. / day</dt><dd>${btc(m.est_btc_day)}<small>${btc(m.est_btc_week)} / week</small></dd></div>
          <div><dt>If we find one</dt><dd>${btc(m.block_payout_btc)}<small>from current share</small></dd></div>
          <div><dt>Immature</dt><dd>${btc(m.immature_btc)}</dd></div>
          <div><dt>Unpaid</dt><dd>${btc(m.unpaid_btc)}<small>mature</small></dd></div>
          <div><dt>Paid</dt><dd>${btc(m.paid_btc)}</dd></div>
          <div><dt>Your TTF</dt><dd>${dur(m.ttf_seconds)}</dd></div>
        </dl>
        <div>
          <p class="kicker" style="margin-bottom:0.45rem">Workers</p>
          <div class="scroll"><table><thead><tr><th>Worker</th><th>Hashrate</th><th>Shares</th><th>Rejects</th><th>VDiff</th><th>Last</th><th>Agent</th></tr></thead><tbody>${workers || '<tr><td colspan="7">Offline</td></tr>'}</tbody></table></div>
        </div>
        <div>
          <p class="kicker" style="margin-bottom:0.45rem">Your payouts</p>
          <div class="scroll"><table><thead><tr><th>Height</th><th>Paid</th><th>Time</th></tr></thead><tbody>${pays || '<tr><td colspan="3">No blocks found yet</td></tr>'}</tbody></table></div>
        </div>
      </div>`;
    draw($("chart"), m.history || [], "hr_ghs");
  }

  async function refresh() {
    const [p, miners, blocks, pays] = await Promise.all([
      j("/api/pool"),
      j("/api/miners"),
      j("/api/blocks"),
      j("/api/payouts"),
    ]);
    stats(p);
    table(
      $("online"),
      ["Address", "Worker", "Hashrate", "Shares", "Rejects", "VDiff", "Last share", "Agent"],
      (miners.online || []).map((m) => [
        `<a href="#${m.address}">${short(m.address)}</a>`,
        m.worker || "\u2014",
        fmtHr(m.hr_ghs),
        m.shares_acc,
        m.shares_rej,
        m.vdiff,
        m.last_share_s ? m.last_share_s.toFixed(0) + "s" : "\u2014",
        m.ua || "",
      ])
    );
    table(
      $("seen"),
      ["Address", "Best HR", "Shares", "Last seen"],
      (miners.seen || []).map((m) => [
        `<a href="#${m.address}">${short(m.address)}</a>`,
        fmtHr(m.best_hr_ghs),
        m.shares_acc,
        ago(m.last_ts),
      ])
    );
    const found = (pays.payouts || []).map((b) => [
      b.height ?? "\u2014",
      b.hash ? `<a href="${p.explorer}/block/${b.hash}" target="_blank" rel="noreferrer">${short(b.hash)}</a>` : "\u2014",
      b.finder ? `<a href="#${b.finder}">${short(b.finder)}</a>` : "\u2014",
      btc(b.miner_btc),
      btc(b.pool_fee_btc),
      b.ts ? new Date(b.ts * 1000).toLocaleString() : "",
    ]);
    table($("found"), ["Height", "Hash", "Finder", "Miner paid", "Pool fee", "Time"], found);
    table($("paytable"), ["Height", "Hash", "Finder", "Miner paid", "Pool fee", "Time"], found);
    table(
      $("blocktable"),
      ["Height", "Miner tag", "Time", "Txs", ""],
      (blocks.blocks || []).slice(0, 16).map((b) => [
        b.height,
        b.pool || "Unknown",
        b.timestamp ? new Date(b.timestamp * 1000).toLocaleString() : "",
        b.tx_count || "",
        b.explorer ? `<a href="${b.explorer}" target="_blank" rel="noreferrer">explorer</a>` : "",
      ])
    );
  }

  function syncMineBtn() {
    const btn = $("bm-btn");
    if (!btn) return;
    const on = /stop/i.test(btn.textContent || "");
    if (on) btn.setAttribute("data-on", "");
    else btn.removeAttribute("data-on");
  }

  const sectionRe = /^(mine|connect|dashboard|miners|blocks|payouts|how)$/;
  function fromHash() {
    const a = location.hash.slice(1);
    if (a && !sectionRe.test(a)) {
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
  refresh();
  setInterval(refresh, 10000);
  setInterval(syncMineBtn, 400);
  window.addEventListener("resize", () => {
    const p = document.querySelector("#poolchart");
    if (p && p.dataset.last) {
      try { draw(p, JSON.parse(p.dataset.last), "hr_ghs"); } catch (e) {}
    }
  });
})();
