// /miner/<address> — one address, full page. Rendering is shared with the front-page card
// (shared.js: minerCard); this file fetches, fills the summary strip and drives the tabs.
(() => {
  const $ = (id) => document.getElementById(id);
  const { esc, fmtHr, num, pctSmart, clock, amt, amtExact, draw, chartLegend, payStatus, minerCard, showMinerTab, MINER_TABS, EXPLORER, t: _t } = window.LZ;
  const t = (k, vars) => (window.LZ_I18N ? LZ_I18N.t(k, vars) : (_t ? _t(k, vars) : k));
  const loc = () => (window.LZ_I18N && LZ_I18N.locale()) || undefined;

  // Address from the path: /miner/<addr>. Fall back to ?addr= and #<addr> for old links.
  const m = location.pathname.match(/^\/miner\/([^/]+)/);
  const addr = decodeURIComponent((m && m[1]) || new URLSearchParams(location.search).get("addr") || "").trim();
  const tabFromHash = () => {
    const h = location.hash.slice(1);
    return MINER_TABS.some(([k]) => k === h) ? h : "overview";
  };
  let tab = tabFromHash();

  // Same copy-to-clipboard as the front page.
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
    const text = (btn.getAttribute("data-copy") || "").trim();
    if (!(await copyValue(text))) return;
    btn.setAttribute("data-copied", "");
    btn.setAttribute("aria-label", t("copied"));
    clearTimeout(btn._copyT);
    btn._copyT = setTimeout(() => {
      btn.removeAttribute("data-copied");
      btn.setAttribute("aria-label", t("copy"));
    }, 1400);
  });

  const fees = { datum: 0, stratum: 10 };
  let priceUsd = null;
  let blockInterval = 0;
  const money = (btcAmt) => {
    if (priceUsd == null) return "";
    const u = Number(btcAmt) * Number(priceUsd);
    if (!Number.isFinite(u) || !Number.isFinite(Number(priceUsd))) return "";
    return " · $" + u.toLocaleString(loc(), { maximumFractionDigits: u >= 100 ? 0 : 2 });
  };
  const j = async (url) => {
    const r = await fetch(url);
    if (!r.ok) throw new Error(url + " " + r.status);
    return r.json();
  };
  const chip = (id, value, label) => {
    const el = $(id);
    if (!el) return;
    el.querySelector("b").textContent = value;
    if (label) el.querySelector("span").textContent = label;
  };

  function setTab(next, push) {
    tab = showMinerTab($("miner"), next) || "overview";
    document.querySelectorAll('.nav a[data-mtab]').forEach((a) => {
      if (a.getAttribute("data-mtab") === tab) a.setAttribute("aria-current", "true");
      else a.removeAttribute("aria-current");
    });
    if (push && location.hash.slice(1) !== tab) history.replaceState(null, "", tab === "overview" ? location.pathname : "#" + tab);
  }
  // Card tabs and top-nav tabs both just switch panels.
  document.addEventListener("click", (e) => {
    const t = e.target.closest("[data-mtab]");
    if (!t) return;
    e.preventDefault();
    setTab(t.getAttribute("data-mtab"), true);
    if (t.closest(".nav")) $("miner")?.scrollIntoView({ block: "start", behavior: "smooth" });
  });
  window.addEventListener("hashchange", () => setTab(tabFromHash(), false));

  function summary(d) {
    const online = !!d.online;
    const relayed = (d.relayed || []).length;
    $("miner-status-word").textContent = !d.known && !d.solo ? t("miner.unknown") : online ? t("miner.online") : relayed ? t("miner.relayed") : t("miner.offline");
    const hp = Number(d.hashrate_pool_percent) || 0;
    const wp = (Number(d.round_share) || 0) * 100;
    const nPending = Number(d.immature_blocks) || (d.blocks_found || []).filter((b) => payStatus(b) === "immature").length;
    chip("mc-hr", fmtHr(d.hr_ghs || 0));
    chip("mc-avg", Number(d.hr_1h_ghs) > 0 ? fmtHr(d.hr_1h_ghs) : "\u2014");
    $("mc-avg").title = t("miner.avgTitle") + (Number(d.hr_24h_ghs) > 0 ? t("miner.avgTitle24", { hr: fmtHr(d.hr_24h_ghs) }) : "");
    chip("mc-share", pctSmart(hp));
    chip("mc-window", pctSmart(wp));
    chip("mc-pending", amt(d.immature_btc), nPending ? t(nPending === 1 ? "miner.pendingN" : "miner.pendingNs", { n: nPending }) : t("miner.pending"));
    chip("mc-paid", amt(d.paid_btc));
    $("mc-pending").title = t("miner.pendingExact", { amt: amtExact(d.immature_btc), n: nPending, s: nPending === 1 ? "" : "s", need: num(d.maturity_confs || 100) });
    $("mc-paid").title = t("miner.paidExact", { amt: amtExact(d.paid_btc) });
    $("nav-live").hidden = false;
  }

  let busy = false;
  async function refresh() {
    if (busy) return;
    busy = true;
    try {
      const [d, p, px] = await Promise.all([
        j("/api/miner/" + encodeURIComponent(addr)),
        j("/api/pool").catch(() => ({})),
        j("/api/price").catch(() => null),
      ]);
      if (px && Number.isFinite(Number(px.USD))) priceUsd = Number(px.USD);
      if (p && p.fees) {
        if (Number.isFinite(Number(p.fees.datum_percent))) fees.datum = Number(p.fees.datum_percent);
        if (Number.isFinite(Number(p.fees.stratum_percent))) fees.stratum = Number(p.fees.stratum_percent);
      }
      if (Number(p.block_interval_seconds) > 0) blockInterval = Number(p.block_interval_seconds);
      if (p && p.pool_hr_ghs != null) $("nav-hr").textContent = fmtHr(p.pool_hr_ghs);
      summary(d);
      $("miner").innerHTML = minerCard(d, { fees, money, blockInterval, tab, full: true });
      setTab(tab, false);
      const hasPool = !!(d.via || Number(d.window_work) > 0 || Number(d.shares_lifetime) > 0 || Number(d.round_share) > 0);
      $("chart-wrap").hidden = !(d.known && hasPool);
      draw(
        $("chart"),
        d.history || [],
        "hr_ghs",
        (d.blocks_found || [])
          .filter((b) => Number(b.ts) > 0)
          .map((b) => ({
            ts: Number(b.ts),
            title: (b.height ? t("chart.blockN", { h: num(b.height) }) : t("chart.block")) + " · " + clock(b.ts),
            sub: t("chart.toAddr", { amt: amt(b.miner_btc) }) + (payStatus(b) === "immature" ? t("chart.immature") : ""),
          }))
      );
      chartLegend($("chart-legend"), $("chart"), t("chart.blockPaid", { n: 1 }), t("chart.blockPaid", { n: 2 }));
    } finally {
      busy = false;
    }
  }

  if (!addr) {
    const empty = () => {
      $("miner-title").textContent = t("miner.noAddr");
      $("miner-status-word").textContent = t("miner.nothing");
      $("miner").innerHTML = `<p class="note callout">${t("miner.openAs")}</p>`;
      $("chart-wrap").hidden = true;
    };
    empty();
    document.addEventListener("lz:i18n", () => {
      if (window.LZ_I18N) LZ_I18N.apply();
      empty();
    });
    return;
  }
  document.title = t("meta.minerTitleAddr", { short: addr.slice(0, 10) + "\u2026" + addr.slice(-6) });
  $("miner-title").innerHTML = `<span class="copyable"><span class="mono">${esc(addr)}</span><button type="button" class="copy-btn" data-copy="${esc(addr)}" aria-label="${esc(t("connect.copyAddr"))}" title="${esc(t("copy"))}"></button></span>`;
  $("explorer-addr").href = EXPLORER + "/address/" + encodeURIComponent(addr);
  $("api-link").href = "/api/miner/" + encodeURIComponent(addr);
  try { localStorage.setItem("lz.addr", addr); } catch (e) { /* private mode */ }

  refresh().catch((e) => {
    console.error(e);
    $("miner").innerHTML = `<p class="note callout">${t("miner.fail")}</p>`;
  });
  setInterval(() => refresh().catch((e) => console.error(e)), 10000);
  document.addEventListener("lz:i18n", () => {
    LZ_I18N.apply();
    document.title = t("meta.minerTitleAddr", { short: addr.slice(0, 10) + "\u2026" + addr.slice(-6) });
    refresh().catch((e) => console.error(e));
  });
  window.addEventListener("resize", () => {
    const c = $("chart");
    if (c && c.__hist) {
      draw(c, c.__hist, "hr_ghs", c.__marks);
      chartLegend($("chart-legend"), c, t("chart.blockPaid", { n: 1 }), t("chart.blockPaid", { n: 2 }));
    }
  });

  // Top bar hairline once scrolled, same as the front page.
  const header = document.querySelector(".site-header");
  const mast = document.querySelector(".site-mast") || header;
  if (header) {
    let raf = 0;
    const onScroll = () => {
      if (raf) return;
      raf = requestAnimationFrame(() => {
        raf = 0;
        const on = window.scrollY > 8;
        header.toggleAttribute("data-scrolled", on);
        if (mast && mast !== header) mast.toggleAttribute("data-scrolled", on);
      });
    };
    window.addEventListener("scroll", onScroll, { passive: true });
    onScroll();
  }
})();
