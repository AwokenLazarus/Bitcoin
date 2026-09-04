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
  // The fee cards are the "why"; clicking one opens the matching "how" tab.
  document.querySelectorAll(".fee-card[data-tab]").forEach((card) => {
    card.addEventListener("click", () => {
      selectTab(card.getAttribute("data-tab"));
      $(card.getAttribute("data-tab"))?.focus({ preventScroll: true });
    });
  });

  // ------------------------------------------------------------ formatting
  const esc = (s) => String(s ?? "").replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]));
  const pathLabel = (via) => {
    if (via === "prime" || via === "gateway") return "own gateway";
    if (via === "both") return "stratum + gateway";
    return "public stratum";
  };
  const isPrimePath = (via) => via === "prime" || via === "gateway";
  const sessCell = (via, n) => (isPrimePath(via) ? "\u2014" : num(n ?? 0));

  // Fee schedule as primed reports it: one rate for work through a miner's own DATUM
  // gateway, another for our public stratum. Filled from /api/pool on every refresh.
  const fees = { datum: 0.5, stratum: 5 };
  const feePct = (x) => (Number.isFinite(Number(x)) ? Number(x).toLocaleString(undefined, { maximumFractionDigits: 2 }) + "%" : "\u2014");
  const feeForPath = (path) => (String(path || "").toLowerCase() === "stratum" ? fees.stratum : fees.datum);
  // Which fee schedule an address is on, as a small labelled pill.
  // `feePath` is the schedule primed applies to the identity's window work (the fee that is
  // actually charged); `via` is the live connection (miners table). The pill always shows the
  // billed path so the same address reads the same in every table; when the live connection
  // differs we say so next to it.
  const pathPill = (feePath, via) => {
    const billed = String(feePath || "").toLowerCase();
    let live = "";
    if (via === "both") live = "both";
    else if (via) live = isPrimePath(via) ? "datum" : "stratum";
    const p = billed || live;
    if (!p) return "\u2014";
    if (p === "both") return `<span class="pill brass" title="Work arriving on both the public stratum and an own gateway">stratum + gateway</span>`;
    const own = p !== "stratum";
    let html = `<span class="pill ${own ? "brass" : ""}" title="${own ? "Most of this address's window work came through its own DATUM gateway" : "Most of this address's window work came through the Lazarus public stratum"}">${own ? "own gateway" : "public stratum"}</span> <span class="faint">${feePct(feeForPath(p))}</span>`;
    if (billed && live && live !== billed) {
      const where = live === "both" ? "both paths" : live === "stratum" ? "stratum now" : "gateway now";
      html += ` <span class="faint" title="Connected right now on a different path. The fee follows where most of the window's accepted work arrived, so it moves as the window rolls.">· ${where}</span>`;
    }
    return html;
  };

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
  const short = (a) => (a && a.length > 20 ? a.slice(0, 10) + "\u2026" + a.slice(-8) : a || "\u2014");
  // Block hashes lead with zeros; the tail is what identifies them.
  const shortHash = (h) => (h && h.length > 16 ? "\u2026" + h.slice(-12) : h || "\u2014");
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
    if (p.fees) {
      if (Number.isFinite(Number(p.fees.datum_percent))) fees.datum = Number(p.fees.datum_percent);
      if (Number.isFinite(Number(p.fees.stratum_percent))) fees.stratum = Number(p.fees.stratum_percent);
    }
    const setText = (id, t) => { const e = $(id); if (e) e.textContent = t; };
    for (const id of ["fee-datum", "tab-fee-datum", "datum-fee-line"]) setText(id, feePct(fees.datum));
    for (const id of ["fee-stratum", "tab-fee-stratum", "stratum-fee-line", "pillar-stratum-fee"]) setText(id, feePct(fees.stratum));
    setText("how-window", String(Number(p.window_multiple) || 8));
    setText("how-fees", fees.datum === fees.stratum
      ? `Fee: ${feePct(fees.datum)} of each miner's window share, taken inside the coinbase.`
      : `Fees are taken per miner, inside the coinbase, by the path the work arrived on: ${feePct(fees.datum)} of your window share through your own DATUM gateway, ${feePct(fees.stratum)} on the public stratum. Switching paths keeps your accepted work.`);
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
      ["Network", net, "tip " + (p.height ? num(p.height) : "\u2014") + " · difficulty " + bigNum(p.difficulty)],
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
    if (build) build.textContent = pr.name ? "Pool server: " + pr.name + (pr.version ? " " + pr.version : "") + (pr.uptime_s ? " · up " + dur(pr.uptime_s) : "") + " · coinbase tag " + (pr.tag || "Lazarus") : "";
    draw($("poolchart"), p.history || [], "hr_ghs");
  }

  // ------------------------------------------------------------ payout hero
  // "If a block is found right now": the reward and how the split Prime is issuing
  // divides it. Straight from /api/coinbaser, which mirrors primed's own split.
  function payoutHero(p, cb) {
    if (!$("payout-reward")) return;
    const value = Number(cb.value) || Math.round((Number(p.subsidy_btc) || 0) * 1e8);
    const minerSats = Number(cb.miner_sats) || 0;
    const poolSats = Number(cb.pool_sats) || 0;
    const feeSats = Number(cb.fee_sats) || 0;
    const n = Number(cb.miner_outputs) || 0;
    const eff = Number.isFinite(Number(cb.effective_fee_percent)) ? Number(cb.effective_fee_percent) : (value ? 100 * feeSats / value : 0);
    $("payout-reward").textContent = value ? sats(value) + " BTC" : "\u2014";
    $("payout-reward-sub").textContent = value ? "base subsidy at height " + (p.height ? num(Number(p.height) + 1) : "\u2014") + " · transaction fees add to every output" : "waiting for Prime";
    $("payout-miners-btc").textContent = value ? sats(minerSats) + " BTC" : "\u2014";
    $("payout-miners-sub").textContent = value ? `${pct(100 * minerSats / value, 1)} of the block · ${n} address${n === 1 ? "" : "es"} paid directly` : "\u2014";
    $("payout-pool-btc").textContent = value ? sats(poolSats) + " BTC" : "\u2014";
    const unplaced = Number(cb.unplaced_sats) || 0;
    $("payout-pool-sub").textContent = value
      ? `${pct(eff, 2)} blended fee` + (fees.datum !== fees.stratum ? ` (${feePct(fees.datum)} gateway · ${feePct(fees.stratum)} stratum)` : "") + (unplaced > 1000 ? ` + ${sats(unplaced)} not yet payable` : "")
      : "\u2014";
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
      `<tr><td class="num faint">${i + 1}</td><td><a href="#${esc(o.address)}">${short(o.address)}</a></td><td>${pathPill(o.fee_path)}</td><td class="num">${pct(o.share_percent)}</td><td class="num">${sats(o.sats)}</td><td class="num faint">${value ? pct(100 * o.sats / value, 2) : "\u2014"}</td></tr>`
    );
    if (!coinbaseExpanded && miners.length > COINBASE_ROWS) {
      const rest = miners.slice(COINBASE_ROWS);
      const restSats = rest.reduce((a, o) => a + Number(o.sats || 0), 0);
      rows.push(`<tr class="faint"><td class="num"></td><td>${rest.length} more miner output${rest.length === 1 ? "" : "s"}</td><td></td><td class="num">${pct(rest.reduce((a, o) => a + Number(o.share_percent || 0), 0))}</td><td class="num">${sats(restSats)}</td><td class="num">${value ? pct(100 * restSats / value, 2) : "\u2014"}</td></tr>`);
    }
    if (pool) {
      rows.push(`<tr class="pool-row"><td class="num faint">${miners.length + 1}</td><td>Lazarus <span class="faint">pool fee${cb.unplaced_sats > 1000 ? " + not yet payable" : ""}</span> · <a href="#${esc(pool.address)}">${short(pool.address)}</a></td><td><span class="pill">pool</span></td><td class="num">\u2014</td><td class="num">${sats(pool.sats)}</td><td class="num faint">${value ? pct(100 * pool.sats / value, 2) : "\u2014"}</td></tr>`);
    }
    el.innerHTML =
      '<thead><tr><th class="num">#</th><th>Paid to</th><th>Path · fee</th><th class="num">Window %</th><th class="num">BTC</th><th class="num">Of block</th></tr></thead><tbody>' +
      (rows.length ? rows.join("") : '<tr><td colspan="6" class="empty">Prime has not issued a split yet</td></tr>') +
      "</tbody>";
    const sum = $("coinbase-summary");
    if (sum) sum.textContent = cb.outputs ? `${cb.outputs} outputs · ${sats(cb.miner_sats)} BTC to ${cb.miner_outputs} miner${cb.miner_outputs === 1 ? "" : "s"} · ${sats(cb.pool_sats)} BTC to the pool` : "\u2014";
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
    const gws = (pr.gateways || []).slice().sort((a, b) => (a.own === b.own ? (b.work || 0) - (a.work || 0) : a.own ? 1 : -1));
    // Client string as "<software> <version>", so the table reads without knowing the wire format.
    const clientLabel = (g) => {
      const ua = String(g.user_agent || "");
      if (g.own) return `<div>Lazarus gateway</div><div class="faint">${esc(ua.split("/")[1] || "")}</div>`;
      const gen = String(g.generation || "").toLowerCase();
      const family = gen === "convoy" ? "CONVOY datum_gateway" : "DATUM gateway";
      const ver = (ua.match(/v?(\d+\.\d+[\w.-]*?)(?=[+/]|$)/) || [])[1] || "";
      const flavor = (ua.match(/\+([a-z][\w-]*)/i) || [])[1] || "";
      const hash = (ua.match(/\/([0-9a-f]{7,})/i) || [])[1] || "";
      const bits = [ver, flavor, hash ? hash.slice(0, 7) : ""].filter(Boolean);
      return `<div>${esc(family)}</div><div class="faint" title="${esc(ua)}">${esc(bits.join(" · ") || ua)}</div>`;
    };
    const rows = gws.map((g) => {
      const who = g.own ? `<span class="pill brass">Lazarus public stratum</span>` : `<span class="mono" title="Gateway signing key (first 16 hex)">${esc(g.gateway || "")}</span>`;
      const ident = g.own ? '<span class="faint">miners’ own addresses</span>' : (g.identity ? `<a href="#${esc(g.identity)}">${short(g.identity)}</a>` : '<span class="faint">no share yet</span>');
      const hashing = g.last_share_s != null && g.last_share_s < 180 && g.accepted > 0;
      const state = `<div>${hashing ? '<span class="pill ok">hashing</span>' : g.accepted > 0 ? '<span class="pill warn">idle</span>' : '<span class="pill">connected</span>'}</div>`
        + (g.accepted > 0 && g.last_share_s != null ? `<div class="faint">last share ${agoS(g.last_share_s)}</div>` : '<div class="faint">no shares yet</div>');
      const shares = `<div>${num(g.accepted)}</div>${g.rejected ? `<div class="faint" title="${esc(g.last_reject || "")}">${num(g.rejected)} rejected</div>` : ""}`;
      return [
        who,
        state,
        clientLabel(g),
        pathPill(g.fee_path || (g.own ? "stratum" : "datum")),
        ident,
        shares,
        num(g.work),
        `<div>${dur(g.connected_s)}</div><div class="faint">${num(g.coinbasers)} splits</div>`,
        String(g.block_candidates || 0),
      ];
    });
    table(el, ["Gateway", "State", "Client", "Fee path", "Pays to", "Shares", "Work", "Connected", "Blocks"], rows, [null, null, null, null, null, "num", "num", "num", "num"], "No gateway connected");
    const t = pr.totals || {};
    const tick = $("gateway-ticker");
    if (tick) {
      const remote = gws.filter((g) => !g.own);
      const active = remote.filter((g) => g.accepted > 0).length;
      const cells = [
        ["Connected", String(gws.length), gws.length ? `${remote.length} remote · ${gws.length - remote.length} ours` : "none"],
        ["Remote hashing", String(active), remote.length ? `of ${remote.length} remote gateway${remote.length === 1 ? "" : "s"} · ${remote.length - active} connected without shares` : "no remote gateways"],
        ["Splits issued", num(t.coinbasers), "coinbase lists handed out since Prime started"],
        ["Shares verified", num(t.shares_accepted), (t.shares_rejected ? num(t.shares_rejected) + " rejected · " : "") + "since Prime started " + dur(pr.uptime_s) + " ago"],
      ];
      tick.innerHTML = cells.map(([k, v, s]) => `<div><dt>${k}</dt><dd>${v}<small>${s}</small></dd></div>`).join("");
    }
    const note = $("gateway-note");
    if (note) {
      note.textContent = pr.reachable
        ? `A gateway shows “connected” until its miners send work; a stock gateway with no miners attached still requests splits. “Rejected” right after a Prime restart is a gateway still on the previous instance’s split — it clears with its next template. Since start: ${num(t.connections)} connection${t.connections === 1 ? "" : "s"}, ${num(t.block_candidates)} block candidate${t.block_candidates === 1 ? "" : "s"}.`
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
    const poolAddr = (p.prime && p.prime.address) || "";
    const byPrime = new Map((pays.prime_blocks || []).filter((b) => b.hash).map((b) => [b.hash, b]));
    const partsFor = (hash, combined) => {
      const pb = byPrime.get(hash);
      const total = Number(combined) || 0;
      if (!pb || !poolAddr) {
        const fee = ((p.fee_percent || 0.5) / 100) * (p.subsidy_btc || 3.125);
        return { miner: Math.max(0, total - fee), fee: Math.min(total, fee) };
      }
      const minerSats = (pb.split || []).filter((o) => o.address === poolAddr).reduce((a, o) => a + Number(o.sats || 0), 0);
      if (pb.fee_sats != null) return { miner: minerSats / 1e8, fee: Number(pb.fee_sats) / 1e8 };
      if (minerSats) return { miner: minerSats / 1e8, fee: Math.max(0, (pb.pool_sats || 0) - minerSats) / 1e8 };
      const fee = (p.fee_percent || 0.5) / 100 * ((pb.coinbase_value || 0) / 1e8 || 3.125);
      return { miner: Math.max(0, total - fee), fee: Math.min(total, fee) };
    };
    const byHeight = new Map();
    for (const r of pays.payouts || []) {
      const key = r.hash || String(r.height);
      const b = byHeight.get(key) || { height: r.height, hash: r.hash, ts: r.ts, outputs: [], miner_btc: 0, pool_btc: 0, status: r.status, kind: r.kind, block_status: r.block_status, owed_sats: r.owed_sats, found_by: r.found_by, reward: r.reward_btc };
      const combined = Number(r.miner_btc) || 0;
      if (r.to === "pool" && poolAddr && r.finder === poolAddr) {
        const parts = partsFor(r.hash, combined);
        if (parts.miner > 0) {
          b.outputs.push({ address: r.finder, btc: parts.miner, share: b.reward ? parts.miner / b.reward : r.share, pool: false });
          b.miner_btc += parts.miner;
        }
        if (parts.fee > 0) {
          b.outputs.push({ address: r.finder, btc: parts.fee, share: b.reward ? parts.fee / b.reward : 0, pool: true });
          b.pool_btc += parts.fee;
        }
      } else {
        const isPool = r.to === "pool";
        b.outputs.push({ address: r.finder, btc: combined, share: r.share, pool: isPool });
        if (isPool) b.pool_btc += combined;
        else b.miner_btc += combined;
      }
      byHeight.set(key, b);
    }
    const chainHashes = new Set([...byHeight.values()].map((b) => b.hash));
    // Candidates Prime saw that the chain scan has not confirmed (pending, orphaned).
    for (const pb of pays.prime_blocks || []) {
      if (!pb.hash || chainHashes.has(pb.hash)) continue;
      if (pb.status === "in chain") continue;
      const feeSats = pb.fee_sats != null ? pb.fee_sats : Math.max(0, (pb.pool_sats || 0) - (pb.miner_to_pool_sats || 0));
      byHeight.set(pb.hash, { height: pb.height, hash: pb.hash, ts: pb.ts, outputs: (pb.split || []).map((o) => ({ address: o.address, btc: o.sats / 1e8 })), miner_btc: (pb.split || []).reduce((a, o) => a + o.sats, 0) / 1e8, pool_btc: feeSats / 1e8, status: pb.status, kind: pb.kind, block_status: pb.status, owed_sats: pb.owed_sats, found_by: pb.finder, reward: pb.coinbase_value / 1e8, prime_only: true });
    }
    const blocks = [...byHeight.values()].sort((a, b) => (b.height || 0) - (a.height || 0));
    const headers = ["Height", "Block", "Coinbase", "Outputs", "Miners paid (BTC)", "Pool (BTC)", "Status", "Found by", "Time"];
    const rows = blocks.map((b, i) => {
      const poolBtc = b.pool_btc || (b.reward ? Math.max(0, b.reward - b.miner_btc) : null);
      const kind = b.kind || (b.outputs.length > 1 ? "split" : b.outputs.length === 1 ? "" : "");
      const st = b.prime_only ? b.block_status : (b.status === "unsplit" ? "pool only" : b.status);
      const outs = b.outputs.slice().sort((x, y) => y.btc - x.btc);
      const detail = outs.map((o) => `<tr><td></td><td colspan="2"><a href="#${esc(o.address)}">${esc(o.address)}</a>${o.pool ? ' <span class="pill brass">pool</span>' : ""}</td><td class="num">${btc(o.btc)}</td><td class="num faint">${b.reward ? pct(100 * o.btc / b.reward, 2) : ""}</td><td colspan="4"></td></tr>`).join("");
      const owed = b.owed_sats ? `<div class="faint">owed to window ${sats(b.owed_sats)}</div>` : "";
      return `<tr class="block-row" data-i="${i}" tabindex="0" aria-expanded="false">
        <td class="num"><span class="disclose"></span>${b.height ?? "\u2014"}</td>
        <td>${b.hash ? `<a href="${esc(p.explorer)}/block/${esc(b.hash)}" target="_blank" rel="noreferrer" title="Open in the Lazarus Mempool">${shortHash(b.hash)}</a>` : "\u2014"}</td>
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
    // Per-session rate as the stratum session reports it; the address-level credited rate
    // (what the pool actually counts) is the Hashrate metric above the table. Gateway-window
    // rows have no session, so they carry the credited rate.
    const workerHr = (w) => (w.via === "stratum" && Number(w.firmware_hr_ghs) > 0 ? w.firmware_hr_ghs : w.hr_ghs);
    const workers = (m.workers || [])
      .map(
        (w) =>
          `<tr><td>${esc(w.worker || "\u2014")}</td><td>${pathLabel(w.via)}</td><td class="num">${fmtHr(workerHr(w))}</td><td class="num">${sessCell(w.via, w.shares_session)}</td><td class="num">${num(w.shares_lifetime ?? w.shares_acc ?? w.window_work)}</td><td class="num">${num(w.shares_rej)}</td><td class="num">${Number.isFinite(Number(w.last_share_s)) ? Number(w.last_share_s).toFixed(0) + "s" : "\u2014"}</td></tr>`
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
          <div><dt>Next block</dt><dd>${btc(m.block_payout_btc)}<small>your output in the coinbase Prime dictates now${m.fee_path ? " · your window work is on the " + feePct(m.fee_percent_path != null ? m.fee_percent_path : feeForPath(m.fee_path)) + " " + (m.fee_path === "stratum" ? "public-stratum" : "own-gateway") + " rate" : ""}</small></dd></div>
          <div><dt>Immature</dt><dd>${btc(m.immature_btc)}<small>in a coinbase, under 100 confs</small></dd></div>
          <div><dt>Paid</dt><dd>${btc(m.paid_btc)}<small>in a coinbase, 100+ confs</small></dd></div>
        </dl>
        <div>
          <p class="kicker table-label">Workers</p>
          <p class="note">One row per stratum session. “Public stratum” is our gateway; “own gateway” is share credit arriving through a DATUM gateway you run. Session hashrate is what each connection reports; the credited total is the Hashrate figure above.</p>
          <div class="scroll"><table><thead><tr><th>Worker</th><th>Path</th><th class="num">Hashrate</th><th class="num">Session</th><th class="num">Accepted</th><th class="num">Rejects</th><th class="num">Last</th></tr></thead><tbody>${workers || '<tr><td colspan="7" class="empty">Offline</td></tr>'}</tbody></table></div>
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
    payoutHero(p, cb || {});
    coinbase(cb || {});
    gateways(p.prime || {});
    const online = (miners.online || []).filter((m) => m.address);
    const firstOf = (m, i) => online.findIndex((x) => x.address === m.address) === i;
    table(
      $("online"),
      ["Address", "Worker", "Path · fee", "Hashrate", "Session", "Accepted", "Window %", "Next block", "Last share"],
      online.map((m, i) => [
        `<a href="#${esc(m.address)}">${short(m.address)}</a>`,
        m.worker === "window" ? '<span class="faint">via gateway</span>' : esc(m.worker || "\u2014"),
        pathPill(m.fee_path, m.via),
        fmtHr(firstOf(m, i) ? (m.credited_hr_ghs || m.hr_ghs) : m.hr_ghs),
        sessCell(m.via, m.shares_session),
        num(m.shares_lifetime ?? m.shares_acc ?? m.window_work),
        firstOf(m, i) && m.window_percent != null ? pct(m.window_percent) : "\u2014",
        firstOf(m, i) && m.window_sats ? sats(m.window_sats) : "\u2014",
        Number.isFinite(Number(m.last_share_s)) ? Number(m.last_share_s).toFixed(0) + "s" : "\u2014",
      ]),
      [null, null, null, "num", "num", "num", "num", "num", "num"],
      "No miner is submitting work right now"
    );
    const onlineAddrs = new Set(online.map((m) => m.address));
    // Addresses that have mined here but are not submitting right now. Anyone still holding
    // window work is listed first: they are still paid if a block lands.
    const seenAll = (miners.seen || []).filter((m) => m.address && !onlineAddrs.has(m.address));
    seenAll.sort((a, b) => (Number(b.window_sats || 0) - Number(a.window_sats || 0)) || (Number(b.last_ts || 0) - Number(a.last_ts || 0)));
    const SEEN_ROWS = 25;
    const seen = seenAll.slice(0, SEEN_ROWS);
    table(
      $("seen"),
      ["Address", "Path · fee", "Accepted", "Window %", "Next block", "Last seen"],
      seen.map((m) => [
        `<a href="#${esc(m.address)}">${short(m.address)}</a>`,
        m.window_work ? pathPill(m.fee_path) : "\u2014",
        num(m.shares_lifetime ?? m.shares_acc ?? m.window_work),
        m.window_work ? pct(m.window_percent) : "\u2014",
        m.window_sats ? sats(m.window_sats) : "\u2014",
        ago(m.last_ts),
      ]),
      [null, null, "num", "num", "num", "num"],
      "Every address we have seen is hashing right now"
    );
    if ($("online-count")) $("online-count").textContent = online.length ? `${onlineAddrs.size} address${onlineAddrs.size === 1 ? "" : "es"} · ${online.length} worker${online.length === 1 ? "" : "s"}` : "";
    if ($("seen-count")) {
      const holding = seenAll.filter((m) => m.window_sats > 0).length;
      $("seen-count").textContent = seenAll.length
        ? `${seenAll.length > SEEN_ROWS ? `${SEEN_ROWS} of ` : ""}${seenAll.length} address${seenAll.length === 1 ? "" : "es"}${holding ? ` · ${holding} still in the window` : ""}`
        : "";
    }
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
    "", "top", "status", "payout", "window", "connect", "dashboard", "miners", "gateways",
    "blocks", "payouts", "how", "datum", "mine",
  ]);

  function fromHash() {
    const a = location.hash.slice(1);
    if (a === "datum") {
      selectTab("tab-datum");
      $("connect")?.scrollIntoView();
      return;
    }
    if (a === "window") {
      $("payout")?.scrollIntoView();
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
