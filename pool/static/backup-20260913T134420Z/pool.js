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
    const prev = btn.getAttribute("aria-label") || t("copy");
    btn.setAttribute("aria-label", t("copied"));
    clearTimeout(btn._copyT);
    btn._copyT = setTimeout(() => {
      btn.removeAttribute("data-copied");
      btn.setAttribute("aria-label", prev === t("copied") ? t("copy") : prev);
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
  // The fee cards are the "why"; clicking one opens the matching "how" tab. Any link with
  // data-tab (the DATUM calls to action) does the same, so it lands on the setup it promises.
  document.querySelectorAll("[data-tab]").forEach((el) => {
    el.addEventListener("click", () => {
      const id = el.getAttribute("data-tab");
      selectTab(id);
      if (el.classList.contains("fee-card")) $(id)?.focus({ preventScroll: true });
    });
  });

  const PROMO_DISMISS_KEY = "lz.promo.datum";
  const promoDismissed = () => {
    try { return localStorage.getItem(PROMO_DISMISS_KEY) === "1"; } catch (e) { return false; }
  };
  $("promo-dismiss")?.addEventListener("click", () => {
    try { localStorage.setItem(PROMO_DISMISS_KEY, "1"); } catch (e) { /* private mode */ }
    const el = $("datum-promo");
    if (el) el.hidden = true;
  });

  // ------------------------------------------------------------ formatting
  // Formatters and the trend chart live in shared.js (window.LZ), shared with /miner/<addr>.
  const { blockMarks, esc, fmtHr, num, bigNum, short, shortHash, pct, pctSmart, ago, agoS, dur, when, clock, sig4, amt, amtSats, amtExact, kindPill, statusPill, chartLegend, draw, pathLabel, isPrimePath, sessCell, winShareCell, feePct, poolLink, payStatus, minerCard, showMinerTab, MINER_TABS, t: _t } = window.LZ;
  const t = (k, vars) => (window.LZ_I18N ? LZ_I18N.t(k, vars) : (_t ? _t(k, vars) : k));
  const loc = () => (window.LZ_I18N && LZ_I18N.locale()) || undefined;

  // Fee schedule as primed reports it: one rate for work through a miner's own DATUM
  // gateway, another for our public stratum. Filled from /api/pool on every refresh.
  // `rebate` is the points of the stratum fee credited to DATUM work; `uplift` is what that
  // is worth to a DATUM miner at today's split (rebate × stratum work ÷ DATUM work).
  const fees = { datum: 0, stratum: 10, rebate: 5, uplift: 0, datumWorkPct: 0, stratumWorkPct: 0, datumMiners: 0 };
  const feeForPath = (path) => (String(path || "").toLowerCase() === "stratum" ? fees.stratum : fees.datum);
  // Which fee schedule an address is on, as a small labelled pill.
  // `feePath` is the schedule primed applies to the identity's window work (the fee that is
  // actually charged); `via` is the live connection (miners table). The pill always shows the
  // billed path so the same address reads the same in every table; when the live connection
  // differs we say so next to it.
  const pathPill = (feePath, via, name) => {
    const billed = String(feePath || "").toLowerCase();
    let live = "";
    if (via === "both") live = "both";
    else if (via) live = isPrimePath(via) ? "datum" : "stratum";
    const p = billed || live;
    if (!p) return "\u2014";
    const gwName = String(name || "").trim();
    if (p === "both") {
      const label = gwName ? t("path.stratumGw", { gw: gwName }) : t("path.stratumGwBare");
      return `<span class="pill brass" title="${esc(t("path.bothTitle", { gw: gwName ? " (" + gwName + ")" : "" }))}">${esc(label)}</span>`;
    }
    const own = p !== "stratum";
    const label = own ? (gwName || t("path.own")) : t("path.stratum");
    const title = own
      ? (gwName ? t("path.ownTitleNamed", { gw: gwName }) : t("path.ownTitle"))
      : t("path.stratumTitle");
    let html = `<span class="pill ${own ? "brass" : ""}" title="${esc(title)}">${esc(label)}</span> <span class="faint">${feePct(feeForPath(p))}</span>`;
    if (billed && live && live !== billed) {
      const where = live === "both" ? t("path.bothNow") : live === "stratum" ? t("path.stratumNow") : t("path.gwNow");
      html += ` <span class="faint" title="${esc(t("path.liveTitle"))}">· ${where}</span>`;
    }
    return html;
  };

  // Bitcoin (BTCB2) USD from /api/price (Neoxa + NonKYC last prices, weighted by 24h volume).
  let priceUsd = null;
  const moneyOnly = (usd) => {
    const u = Number(usd);
    if (!Number.isFinite(u)) return "\u2014";
    return "$" + u.toLocaleString(loc(), { maximumFractionDigits: u >= 100 ? 0 : 2 });
  };
  const money = (btcAmt) => {
    if (priceUsd == null) return "";
    const u = Number(btcAmt) * Number(priceUsd);
    if (!Number.isFinite(u) || !Number.isFinite(Number(priceUsd))) return "";
    return " · " + moneyOnly(u);
  };
  // 1 TH/s of continuous work at the current difficulty, base subsidy (no tx fees).
  // `key` picks which of primed's pre-computed figures to prefer: with the rebate on,
  // "datum_bonus" is the DATUM rate including the credit at today's work split.
  const thsDay = (p, feePercent, key) => {
    if (key) {
      const pre = Number(p && p["ths_btc_day_" + key]);
      if (Number.isFinite(pre) && pre > 0) return pre;
    }
    const billed = Number(p && p["ths_btc_day_" + (feePercent === fees.stratum ? "stratum" : "datum")]);
    if (Number.isFinite(billed) && billed > 0) return billed;
    const gross = Number(p && p.ths_btc_day);
    if (Number.isFinite(gross) && gross > 0) return gross * (1 - (Number(feePercent) || 0) / 100);
    const diff = Number(p && p.difficulty);
    const sub = Number(p && p.subsidy_btc) || 3.125;
    if (Number.isFinite(diff) && diff > 0) {
      return (1e12 * 86400 / (diff * Math.pow(2, 32))) * sub * (1 - (Number(feePercent) || 0) / 100);
    }
    return null;
  };
  // Percent that can be tiny (a miner's share of the network) without scientific notation.
  const expPct = (n) => {
    const x = Number(n);
    if (!Number.isFinite(x) || Math.abs(x) < 1e-12) return "0";
    if (Math.abs(x) >= 0.01) return x.toFixed(2);
    return sig4(x);
  };
  const j = async (url) => {
    const r = await fetch(url);
    if (!r.ok) throw new Error(url + " " + r.status);
    return r.json();
  };

  // ------------------------------------------------------------------ pool
  // ------------------------------------------------------------- overflow
  // The house stratum relays new miners to other BLAKE2b pools while Lazarus holds more
  // than its share of the network. Miners already here are unaffected; a relayed miner
  // is paid by the pool it was sent to.
  const upstreamList = (ov) => (ov && ov.upstreams ? ov.upstreams : []).filter((u) => u.healthy !== false).map((u) => poolLink(u.name, u.url));
  function overflowNote(ov) {
    const el = $("overflow-note");
    if (!el) return;
    const on = ov && (ov.active || ov.mode === "force");
    if (!on) {
      el.hidden = true;
      return;
    }
    const share = Number(ov.share_pct);
    const ups = upstreamList(ov);
    const now = ov.proxied_sessions ? t("overflow.now", { n: ov.proxied_sessions }) : "";
    el.innerHTML = t("overflow.on", {
      share: Number.isFinite(share) ? share.toFixed(1) : "\u2014",
      enter: Number(ov.enter_pct) || 25,
      ups: ups.length ? ups.join(", ") : t("overflow.upsFallback"),
      now,
      exit: Number(ov.exit_pct) || 22,
    });
    el.hidden = false;
  }
  // The "Other pools" cards are the overflow upstreams. When the gateway is health-checking
  // them, say so on each card: reachable, down, or how many of our relayed miners are there.
  function poolCards(ov) {
    const ups = ov && Array.isArray(ov.upstreams) ? ov.upstreams : [];
    const share = $("pools-share");
    if (share) {
      const s = Number(ov && ov.share_pct);
      share.hidden = !(ov && ov.meter_ok && Number.isFinite(s));
      if (!share.hidden) {
        share.innerHTML = t(ov.active ? "overflow.shareOver" : "overflow.shareUnder", {
          pct: s.toFixed(1),
          enter: Number(ov.enter_pct) || 25,
        });
      }
    }
    document.querySelectorAll(".pool-card[data-pool]").forEach((card) => {
      const pill = card.querySelector("[data-pool-state]");
      if (!pill) return;
      const want = String(card.getAttribute("data-pool") || "").toLowerCase();
      const u = ups.find((x) => String(x.name || "").toLowerCase() === want);
      if (!u || typeof u.healthy !== "boolean") {
        pill.hidden = true;
        return;
      }
      const n = Number(u.sessions) || 0;
      if (u.healthy === false) {
        pill.className = "pill pool-state bad";
        pill.textContent = t("overflow.notAnswering");
        pill.title = u.last_error ? String(u.last_error) : t("overflow.notAnsweringTitle");
      } else if (n > 0) {
        pill.className = "pill pool-state warn";
        pill.textContent = t("overflow.relayedHere", { n: num(n) });
        pill.title = t("overflow.relayedHereTitle", { n: num(n) });
      } else {
        pill.className = "pill pool-state ok";
        pill.textContent = t("overflow.reachable");
        pill.title = t("overflow.reachableTitle");
      }
      pill.hidden = false;
    });
  }
  function relayedTable(rows, ov) {
    const wrap = $("relayed-wrap");
    if (!wrap) return;
    rows = rows || [];
    if (!rows.length) {
      wrap.hidden = true;
      return;
    }
    table(
      $("relayed"),
      [t("miners.thAddress"), t("miners.thWorker"), t("miners.thMiningOn"), t("miners.thConnected"), t("miners.thAcceptedThere"), t("miners.thFrom")],
      rows.map((r) => [
        r.address ? `<a href="#${esc(r.address)}">${short(r.address)}</a>` : "\u2014",
        esc(r.worker || "\u2014"),
        poolLink(r.upstream, r.miner_url || r.upstream_url),
        dur(Number(r.connected_s) || 0),
        num(r.accepted) + (Number(r.submits) ? ` <span class="faint">/ ${num(r.submits)}</span>` : ""),
        esc((r.host || "").replace(/:\d+$/, "")),
      ]),
      [null, null, null, "num", "num", null],
      ""
    );
    if ($("relayed-count")) $("relayed-count").textContent = t("overflow.connections", { n: rows.length });
    if ($("relayed-note")) {
      $("relayed-note").textContent = t("overflow.relayedNote");
    }
    wrap.hidden = false;
  }

  // "one point", "five points" — rebate percentages read as points of the stratum fee when we are
  // talking about a slice of it rather than a rate in its own right. `Pts` starts a sentence.
  const pts = (n) => {
    const x = Number(n) || 0;
    if (x === 0.5) return t("pts.half");
    const list = (window.LZ_I18N && LZ_I18N.raw && LZ_I18N.raw("pts.word")) || ["zero", "one", "two", "three", "four", "five"];
    const s = Number.isInteger(x) && x < list.length ? list[x] : String(sig4(x));
    return t(x === 1 ? "pts.one" : "pts.other", { s });
  };
  const Pts = (n) => { const s = pts(n); return s.charAt(0).toUpperCase() + s.slice(1); };
  // The uplift, as the page shows it: a signed percent with a decimal until it gets big.
  const upliftPct = (n) => "+" + pct(Number(n) || 0, Math.abs(Number(n)) < 100 ? 1 : 0);
  // The DATUM bonus, everywhere it appears. One knob (`datum_rebate_percent`) turns all of it
  // on: with the rebate off every element here is hidden and the page reads as it did before.
  // `uplift` is the number that matters to a miner — what DATUM work earns above its
  // proportional share of a block right now — so it is what we lead with when Prime reports it.
  function rebateCopy(p) {
    const setText = (id, text) => { const e = $(id); if (e) e.textContent = text; };
    const show = (id, on) => { const e = $(id); if (e) e.hidden = !on; };
    const on = fees.rebate > 0;
    const upliftTxt = fees.uplift > 0 ? upliftPct(fees.uplift) : "";
    const upliftPhrase = fees.uplift > 0
      ? t("connect.upliftPhrase", { uplift: upliftTxt })
      : "";
    const upliftBecause = fees.uplift > 0
      ? t("connect.upliftBecause", { datumPct: pctSmart(fees.datumWorkPct), stratumPct: pctSmart(fees.stratumWorkPct) })
      : "";

    show("datum-promo", on && !promoDismissed());
    show("promo-live", on && !!upliftTxt);
    const promo = $("promo-live");
    if (promo && upliftTxt) {
      promo.textContent = t("promo.live", { uplift: upliftTxt });
      promo.title = t("promo.liveTitle", { datumPct: pctSmart(fees.datumWorkPct), stratumPct: pctSmart(fees.stratumWorkPct), pts: pts(fees.rebate), uplift: upliftTxt });
    }
    setText("promo-text", on
      ? t("promo.text", { fee: feePct(fees.stratum), Pts: Pts(fees.rebate) })
      : "");

    show("live-bonus-chip", on && !!upliftTxt);
    setText("live-bonus", upliftTxt || "\u2014");

    setText("lede-rebate-pts", pts(fees.rebate));
    const feesBody = $("fee-lede-body");
    if (feesBody) {
      feesBody.innerHTML = t("hero.feesBody", {
        pts: `<span id="lede-rebate-pts">${esc(pts(fees.rebate))}</span>`,
      });
    }
    show("top-bonus-datum", on && !!upliftTxt);
    setText("top-bonus-datum", upliftTxt ? t("hero.topBonus", { uplift: upliftTxt }) : "");
    setText("top-fee-datum-sub", on ? t("hero.datumSubOn") : t("hero.datumSubOff"));
    setText("top-stratum-rebate", on ? t("hero.stratumRebate", { pts: pts(fees.rebate), fee: feePct(fees.stratum) }) : "");
    show("top-stratum-rebate", on);

    const pillar = $("pillar-rebate");
    if (pillar) {
      pillar.textContent = on ? t("status.pillarRebate", { pts: pts(fees.rebate) }) : "";
      pillar.hidden = !on;
    }

    show("fee-datum-bonus", on && !!upliftTxt);
    setText("fee-datum-bonus", upliftTxt ? t("connect.bonusSmall", { uplift: upliftTxt }) : "");
    setText("fee-datum-copy", on ? t("connect.datumCopyOn", { fee: feePct(fees.stratum) }) : "");
    setText("fee-stratum-copy", on ? t("connect.stratumCopyOn", { pts: pts(fees.rebate) }) : "");
    show("tab-bonus-datum", on && !!upliftTxt);
    setText("tab-bonus-datum", upliftTxt || "");

    const stratumLine = $("stratum-rebate-line");
    if (stratumLine) {
      stratumLine.innerHTML = on ? t("connect.stratumRebate", { pts: pts(fees.rebate) }) : "";
      stratumLine.hidden = !on;
      stratumLine.querySelector("[data-tab]")?.addEventListener("click", () => selectTab("tab-datum"));
    }

    show("datum-bonus-explain", on);
    const explain = $("datum-bonus-explain-text");
    if (explain && on) {
      explain.innerHTML = t("connect.bonusExplain", {
        fee: feePct(fees.stratum),
        Pts: Pts(fees.rebate),
        uplift: upliftPhrase ? upliftPhrase + upliftBecause : "",
      });
    }
    show("datum-bonus-dt", on && !!upliftTxt);
    show("datum-bonus-dd", on && !!upliftTxt);
    const bonusDd = $("datum-bonus-dd");
    if (bonusDd && on && upliftTxt) {
      bonusDd.innerHTML = t("connect.bonusDd", {
        uplift: upliftTxt,
        pts: pts(fees.rebate),
        fee: feePct(fees.stratum),
        n: fees.datumMiners || "\u2014",
        who: t("connect.miner", { n: fees.datumMiners || 0 }),
      });
    }

    show("how-step-bonus", on);
    const howBonus = $("how-bonus-copy");
    if (howBonus && on) {
      howBonus.innerHTML = t("connect.howBonus", {
        Pts: Pts(fees.rebate),
        fee: feePct(fees.stratum),
        upliftDot: upliftPhrase ? upliftPhrase + "." : "",
      });
    }

    const payoutNote = $("payout-rebate-note");
    if (payoutNote) {
      const sample = Number(p.fees && p.fees.sample_rebate_btc) || 0;
      payoutNote.hidden = !on;
      if (on) {
        payoutNote.innerHTML = t("payout.plusBonus", {
          sample: sample > 0 ? `<b>${amt(sample)}</b>${money(sample)}` : t("payout.plusBonusPts", { pts: pts(fees.rebate) }),
        });
        payoutNote.querySelector("[data-tab]")?.addEventListener("click", () => selectTab("tab-datum"));
      }
    }
  }

  function cssColor(name, fallback) {
    const v = getComputedStyle(document.documentElement).getPropertyValue(name).trim();
    return v || fallback;
  }

  function drawPathPie(c, slices) {
    if (!c) return;
    c.__slices = slices || [];
    const ctx = c.getContext("2d");
    const dpr = Math.max(1, window.devicePixelRatio || 1);
    const cssW = c.clientWidth || 168;
    const cssH = c.clientHeight || 168;
    if (c.width !== Math.round(cssW * dpr) || c.height !== Math.round(cssH * dpr)) {
      c.width = Math.round(cssW * dpr);
      c.height = Math.round(cssH * dpr);
    }
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.clearRect(0, 0, cssW, cssH);
    const cx = cssW / 2;
    const cy = cssH / 2;
    const r = Math.min(cssW, cssH) / 2 - 1.5;
    const inner = r * 0.62;
    const total = (slices || []).reduce((sum, sl) => sum + Math.max(0, Number(sl.value) || 0), 0);
    const live = (slices || []).filter((sl) => (Number(sl.value) || 0) > 0);
    ctx.beginPath();
    ctx.arc(cx, cy, r, 0, Math.PI * 2);
    ctx.arc(cx, cy, inner, 0, Math.PI * 2, true);
    ctx.fillStyle = cssColor("--line", "#3a342c");
    ctx.fill();
    if (total <= 0 || !live.length) return;
    let a = -Math.PI / 2;
    const gap = live.length > 1 ? Math.min(0.045, (Math.PI * 2 * 0.08) / live.length) : 0;
    for (const sl of slices) {
      const v = Math.max(0, Number(sl.value) || 0);
      if (v <= 0) continue;
      const sweep = (v / total) * Math.PI * 2 - gap;
      if (sweep <= 0) continue;
      const next = a + sweep;
      ctx.beginPath();
      ctx.arc(cx, cy, r, a, next);
      ctx.arc(cx, cy, inner, next, a, true);
      ctx.closePath();
      ctx.fillStyle = sl.color;
      ctx.fill();
      a = next + gap;
    }
  }

  function pathPct(n) {
    const x = Number(n);
    if (!Number.isFinite(x)) return "\u2014";
    if (x <= 0) return "0%";
    if (x >= 9.95) return x.toFixed(0) + "%";
    return x.toFixed(1) + "%";
  }

  function fillPathHr(p, so) {
    const box = $("path-hr");
    const canvas = $("path-pie");
    if (!box || !canvas) return;
    const d = Number(p.datum_hr_ghs);
    const s = Number(p.stratum_hr_ghs);
    if (!Number.isFinite(d) || !Number.isFinite(s)) {
      box.hidden = true;
      return;
    }
    const soloOn = !!(so && so.enabled);
    const o = soloOn ? Math.max(0, Number(so.hashrate_ghs) || 0) : 0;
    const nSolo = soloOn ? soloHashing(so).length : 0;
    box.hidden = false;
    const brass = cssColor("--brass", "#dbb565");
    const steel = cssColor("--muted", "#9a8f7e");
    const copper = cssColor("--solo", "#c47a4a");
    const slices = [
      { key: "datum", value: Math.max(0, d), color: brass },
      { key: "stratum", value: Math.max(0, s), color: steel },
    ];
    if (soloOn) slices.push({ key: "solo", value: o, color: copper });
    drawPathPie(canvas, slices);
    const den = Math.max(0, d) + Math.max(0, s) + (soloOn ? o : 0);
    const pctOf = (x) => (den > 1e-12 ? (100.0 * x) / den : 0);
    const dp = pctOf(Math.max(0, d));
    const sp = pctOf(Math.max(0, s));
    const op = soloOn ? pctOf(o) : 0;
    if ($("path-pie-pct")) $("path-pie-pct").textContent = pathPct(dp);
    const note = $("path-hr-note");
    if (note) note.textContent = soloOn ? t("status.pathNoteSolo") : t("status.pathNote");
    const legend = $("path-hr-legend");
    if (legend) {
      const rows = [
        ["datum", t("status.pathDatum"), dp, d, p.datum_hr_miners, brass],
        ["stratum", t("status.pathStratum"), sp, s, p.stratum_hr_miners, steel],
      ];
      if (soloOn) rows.push(["solo", t("status.pathSolo"), op, o, nSolo, copper]);
      legend.innerHTML = rows.map(([, label, pctv, hr, n, color]) =>
        `<li><span class="path-hr-swatch" style="background:${color}"></span><span>${label}<small>${t("status.pathMiners", { n: Number(n) || 0 })}</small></span><b>${t("status.pathSlice", { pct: pathPct(pctv), hr: fmtHr(hr) })}</b></li>`
      ).join("");
    }
    let aria = t("status.pathChartAria") + ". " + t("status.pathDatum") + " " + pathPct(dp) + ", " + t("status.pathStratum") + " " + pathPct(sp);
    if (soloOn) aria += ", " + t("status.pathSolo") + " " + pathPct(op);
    canvas.setAttribute("aria-label", aria);
    fillGwHr(p);
  }

  function gwPieColors(n) {
    const brass = cssColor("--brass", "#dbb565");
    const dim = cssColor("--brass-dim", "#a8884a");
    const pal = [
      brass, dim, "#c9a36a", "#8f6f45", "#dcc392", "#6b5640", "#e4c98a",
      "#b08950", "#7a6240", "#c4b08a", "#9a7344", "#e8d4a8",
      "#5c4a32", "#d0a05c", "#a89070", "#cfc09a", "#8a5a32",
    ];
    return Array.from({ length: Math.max(0, n) }, (_, i) => pal[i % pal.length]);
  }

  function fillGwHr(p) {
    const box = $("gw-hr");
    const copy = $("gw-hr-copy");
    const canvas = $("gw-pie");
    if (!box || !canvas) return;
    const rows = Array.isArray(p.datum_gateways) ? p.datum_gateways : [];
    const live = rows.filter((g) => Number(g.hr_ghs) > 1e-9);
    if (!live.length) {
      box.hidden = true;
      if (copy) copy.hidden = true;
      $("path-hr")?.classList.remove("has-gw");
      return;
    }
    box.hidden = false;
    if (copy) copy.hidden = false;
    $("path-hr")?.classList.add("has-gw");
    const otherColor = "#5a5348";
    const colors = gwPieColors(live.length);
    drawPathPie(canvas, live.map((g, i) => ({
      key: g.other ? "other" : (g.gateway || g.name || String(i)),
      value: Math.max(0, Number(g.hr_ghs) || 0),
      color: g.other ? otherColor : colors[i],
    })));
    const n = Number(p.datum_gateway_count);
    if ($("gw-pie-n")) $("gw-pie-n").textContent = Number.isFinite(n) && n > 0 ? String(n) : String(live.filter((g) => !g.other).length);
    const legend = $("gw-hr-legend");
    if (legend) {
      const named = [];
      const folded = [];
      live.forEach((g, i) => {
        const color = g.other ? otherColor : colors[i];
        if (!g.other && named.length < 7) named.push({ g, color });
        else folded.push({ g, color });
      });
      const items = named.map(({ g, color }) => {
        const label = String(g.name || "").trim() || t("status.gwUnnamed");
        return { label, color, pct: g.percent, hr: g.hr_ghs };
      });
      if (folded.length) {
        items.push({
          label: t("status.gwOther"),
          color: otherColor,
          pct: folded.reduce((s, x) => s + (Number(x.g.percent) || 0), 0),
          hr: folded.reduce((s, x) => s + (Number(x.g.hr_ghs) || 0), 0),
        });
      }
      legend.innerHTML = items.map((it) =>
        `<li><span class="path-hr-swatch" style="background:${it.color}"></span><span>${esc(it.label)}</span><b>${t("status.pathSlice", { pct: pathPct(it.pct), hr: fmtHr(it.hr) })}</b></li>`
      ).join("");
    }
    canvas.setAttribute("aria-label", t("status.gwChartAria") + ". " + live.map((g) => {
      const label = g.other ? t("status.gwOther") : (String(g.name || "").trim() || t("status.gwUnnamed"));
      return label + " " + pathPct(g.percent);
    }).join(", "));
  }

  function stats(p, so) {
    const pr = p.prime || {};
    const tot = pr.totals || {};
    if (p.fees) {
      if (Number.isFinite(Number(p.fees.datum_percent))) fees.datum = Number(p.fees.datum_percent);
      if (Number.isFinite(Number(p.fees.stratum_percent))) fees.stratum = Number(p.fees.stratum_percent);
      if (Number.isFinite(Number(p.fees.datum_rebate_percent))) fees.rebate = Number(p.fees.datum_rebate_percent);
      if (Number.isFinite(Number(p.fees.datum_uplift_percent))) fees.uplift = Number(p.fees.datum_uplift_percent);
      if (Number.isFinite(Number(p.fees.datum_work_percent))) fees.datumWorkPct = Number(p.fees.datum_work_percent);
      if (Number.isFinite(Number(p.fees.stratum_work_percent))) fees.stratumWorkPct = Number(p.fees.stratum_work_percent);
      fees.datumMiners = Number(p.fees.datum_miners) || 0;
    }
    const setText = (id, text) => { const e = $(id); if (e) e.textContent = text; };
    for (const id of ["fee-datum", "tab-fee-datum", "datum-fee-line", "top-fee-datum"]) setText(id, feePct(fees.datum));
    for (const id of ["fee-stratum", "tab-fee-stratum", "stratum-fee-line", "pillar-stratum-fee", "top-fee-stratum"]) setText(id, feePct(fees.stratum));
    rebateCopy(p);
    if (fees.datum === 0 && fees.stratum > 0) {
      setText("fee-ratio", t("connect.noCut"));
    } else if (fees.datum > 0 && fees.stratum > fees.datum) {
      const r = fees.stratum / fees.datum;
      const times = ({ 2: t("connect.times.2"), 4: t("connect.times.4"), 5: t("connect.times.5"), 10: t("connect.times.10") }[r])
        || (Number.isInteger(r) ? t("connect.timesN", { n: r }) : t("connect.timesX", { n: r.toFixed(1) }));
      setText("fee-ratio", times);
    }
    setText("how-window", String(Number(p.window_multiple) || 8));
    setText("how-fees", fees.datum === fees.stratum
      ? t("connect.howFeesSame", { fee: feePct(fees.datum) })
      : t("connect.howFeesDiff", { datum: feePct(fees.datum), stratum: feePct(fees.stratum) }));
    const net = p.network_hr_hs ? (p.network_hr_hs / 1e15).toFixed(2) + " PH/s" : "\u2014";
    const luck = p.luck_percent == null ? "\u2014" : p.luck_percent.toFixed(0) + "%";
    const expected = Number(p.blocks_expected);
    // Luck is measured over the span the hashrate samples cover (a rolling week), at the
    // difficulty in force when each hash was done; the headline count is all-time.
    const luckFound = Number.isFinite(Number(p.luck_blocks_found)) ? Number(p.luck_blocks_found) : (p.blocks_found ?? 0);
    const luckSince = Number(p.luck_since_ts) > 0 ? new Date(Number(p.luck_since_ts) * 1000).toLocaleDateString(loc(), { month: "short", day: "numeric" }) : "";
    let luckSub = t("status.luckPending");
    if (p.luck_percent != null) {
      luckSub = t("status.luck", { luck });
      if (Number.isFinite(expected) && expected > 0) {
        const since = luckSince && luckFound !== (p.blocks_found ?? 0)
          ? t("status.luckSinceShort", { date: luckSince })
          : "";
        luckSub += t("status.luckRatio", { found: luckFound, exp: expected.toFixed(1), since });
      }
    }
    const shareSub = t("status.shareOfHr", { pct: expPct((p.pool_share || 0) * 100) })
      + (p.block_interval_seconds ? t("status.blocksEvery", { dur: dur(p.block_interval_seconds) }) : "");
    const nblocks = Number(p.window_multiple) || 8;
    const fill = Number(p.window_fill_percent);
    const fillTxt = Number.isFinite(fill) ? fill.toFixed(0) + "%" : "\u2014";
    const inWindow = Number((pr.window || {}).identities) || 0;
    const rejPct = tot.shares_accepted + tot.shares_rejected > 0
      ? t("status.rejPct", { pct: (100 * tot.shares_rejected / (tot.shares_accepted + tot.shares_rejected)).toFixed(2) + "%" })
      : t("status.noRejects");
    const gws = Number(pr.gateways_online) || 0;
    const remote = Number(pr.gateways_remote) || 0;
    const thsDatum = thsDay(p, fees.datum);
    const thsStratum = thsDay(p, fees.stratum);
    // Headline yield is the DATUM one including the bonus: it is the number we want a miner
    // comparing pools to see, and it is what they actually get through a gateway.
    const thsBonus = fees.rebate > 0 ? thsDay(p, fees.datum, "datum_bonus") : thsDatum;
    // Rigs (stratum sessions, plus one per own-gateway address) vs. distinct payout addresses.
    const workers = Number(p.workers_online) || Number(p.miners_online) || 0;
    const addrs = Number(p.miners_online) || 0;
    const extra = addrs && addrs !== workers ? t("status.hrAddr", { n: addrs }) : "";
    const hrSub = t("status.hrSub", { n: workers, extra });
    const life = Number(tot.lifetime_shares) || 0;
    const run = Number(tot.shares_accepted) || 0;
    const sharesMain = life > run ? life : run;
    const sharesSub = !pr.reachable
      ? t("status.primeUnreachable")
      : life > run
        ? t("status.sharesLifetime", { run: num(run), rej: rejPct })
        : t("status.sharesVerified", { rej: rejPct });
    const cells = [
      [t("status.thsEst", { label: t("hero.ths") }), thsBonus != null ? t("status.perDay", { amt: amt(thsBonus) + money(thsBonus) }) : "\u2014",
        (fees.rebate > 0 ? t("status.thsSubOn") : t("status.thsSubOff"))
        + " · " + (thsStratum != null ? t("status.perDay", { amt: amt(thsStratum) + money(thsStratum) }) : "\u2014") + " · " + feePct(fees.stratum)],
      [t("status.hashrate"), fmtHr(p.pool_hr_ghs), hrSub],
      [t("status.miners"), String(inWindow || p.miners_online || 0), inWindow ? t("status.minersSub", { seen: p.miners_seen ?? "\u2014" }) : t("status.minersEver", { seen: p.miners_seen ?? "\u2014" })],
      [t("status.shares"), num(sharesMain), sharesSub],
      [t("status.window"), t("status.windowFull", { fill: fillTxt }), t("status.windowBlocks", { n: nblocks })],
      [t("status.gateways"), String(gws), gws ? (remote ? t("status.gwRemote", { remote }) : t("status.gwOurs")) + t("status.gwUp", { up: dur(pr.uptime_s) }) : t("status.gwNone")],
      [t("status.toBlock"), dur(p.ttf_seconds), shareSub],
      [t("status.found"), String(p.blocks_found ?? 0), luckSub],
      [t("status.network"), net, t("status.tipDiff", { h: p.height ? num(p.height) : "\u2014", d: bigNum(p.difficulty) })],
    ];
    $("stats").innerHTML = cells
      .map(([k, v, s]) => `<div><dt>${k}</dt><dd>${v}<small>${s}</small></dd></div>`)
      .join("");
    overflowNote(p.overflow);
    poolCards(p.overflow);
    fillPathHr(p, so);

    $("stratum").textContent = p.stratum;

    const d = p.datum || {};
    const host = d.pool_host || "stratum.awokenlazarus.xyz";
    const port = d.pool_port || 28915;
    const pubkey = d.pool_pubkey || pr.pubkey || "";
    const gw = $("gateway-config");
    if (gw) {
      gw.textContent = JSON.stringify({
        stratum: {
          vardiff_min: 4096,
          vardiff_target_shares_min: 8,
        },
        datum: {
          pool_host: host,
          pool_port: port,
          pool_pubkey: pubkey || "<primed pubkey>",
          pool_pass_workers: true,
          pool_pass_full_users: true,
          pooled_mining_only: true,
        },
      }, null, 2);
    }
    if ($("datum-host")) $("datum-host").textContent = host;
    if ($("datum-port")) $("datum-port").textContent = String(port);
    if ($("datum-pubkey")) $("datum-pubkey").textContent = pubkey || "\u2014";

    if ($("live-hr")) $("live-hr").textContent = fmtHr(p.pool_hr_ghs);
    if ($("nav-hr")) $("nav-hr").textContent = fmtHr(p.pool_hr_ghs);
    if ($("nav-live")) $("nav-live").classList.toggle("stale", !pr.reachable);
    if ($("live-price")) $("live-price").textContent = priceUsd != null ? moneyOnly(priceUsd) : "\u2014";
    const priceChip = $("live-price-chip");
    if (priceChip) priceChip.classList.toggle("stale", priceUsd == null);
    if ($("live-ths")) $("live-ths").textContent = thsBonus != null ? "~" + t("status.perDay", { amt: amt(thsBonus) + money(thsBonus) }) : "\u2014";
    calcRate = thsBonus;
    renderCalc();
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
    if ($("window-size")) $("window-size").textContent = t("status.windowSize", { n: nblocks });
    if ($("window-filled")) $("window-filled").textContent = fillTxt;
    if ($("window-filled-inline")) $("window-filled-inline").textContent = fillTxt;
    if ($("window-miners")) $("window-miners").textContent = inWindow ? String(inWindow) : "\u2014";
    if ($("window-shares")) $("window-shares").textContent = num((pr.window || {}).shares ?? p.window_shares);
    if ($("window-explain")) {
      $("window-explain").textContent = t("status.windowExplain", { n: nblocks });
    }
    const build = $("prime-build");
    if (build) build.textContent = pr.name ? t("status.build", { name: pr.name, ver: pr.version ? " " + pr.version : "", up: pr.uptime_s ? t("status.buildUp", { up: dur(pr.uptime_s) }) : "", tag: pr.tag || "Lazarus" }) : "";
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
    $("payout-reward").textContent = value ? amtSats(value) + money(value / 1e8) : "\u2014";
    $("payout-reward-sub").textContent = value ? t("payout.rewardSub", { h: p.height ? num(Number(p.height) + 1) : "\u2014" }) : t("payout.rewardSubWait");
    $("payout-miners-btc").textContent = value ? amtSats(minerSats) : "\u2014";
    $("payout-miners-sub").textContent = value ? t("payout.minersSub", { pct: pct(100 * minerSats / value, 1), n }) : "\u2014";
    $("payout-pool-btc").textContent = value ? amtSats(poolSats) : "\u2014";
    const unplaced = Number(cb.unplaced_sats) || 0;
    const carryPaid = Number(cb.carry_paid_sats) || 0;
    const rebateSats = Number(cb.rebate_sats) || 0;
    $("payout-pool-sub").textContent = value
      ? t("payout.blendedFee", { pct: pct(eff, 2) })
        + (fees.datum !== fees.stratum ? t("payout.feeSplit", { datum: feePct(fees.datum), stratum: feePct(fees.stratum) }) : "")
        + (rebateSats > 0 ? t("payout.rebateCredit", { amt: amtSats(rebateSats) }) : "")
        + (unplaced > 1000 ? t("payout.carryFwdAmt", { amt: amtSats(unplaced) }) : "")
        + (carryPaid > 1000 ? t("payout.carryBackAmt", { amt: amtSats(carryPaid) }) : "")
      : "\u2014";
  }

  // --------------------------------------------------------- payout donut
  // One control drives both views of the split: collapsed groups the long tail, expanded gives
  // every address its own slice and row.
  let coinbaseExpanded = false;

  // The same split as the table below it, as a ring: one slice per payout address, plus the
  // pool's own slice, so the whole coinbase adds up to the circle. Labelled by the last four
  // characters of the address, and by the operator's name where Prime has learned one from the
  // secondary coinbase tag on a block their gateway found.
  const DONUT_SLICES = 12;
  // Circumference 100 at r=15.9155, so a slice's dash length *is* its percentage.
  const DONUT_R = 15.9155;

  // Fan the brass hue across the slices so neighbours stay apart at a glance. Largest slice is
  // full brass; the tail cools towards ember. The pool's slice sits outside the ramp on purpose.
  const donutColour = (i, n) => {
    const t = n > 1 ? i / (n - 1) : 0;
    return `oklch(${(84 - 27 * t).toFixed(1)}% ${(0.115 - 0.03 * t).toFixed(3)} ${(90 - 44 * t).toFixed(1)})`;
  };

  const last4 = (a) => (a && a.length > 4 ? "\u2026" + a.slice(-4) : a || "\u2014");

  // What to call an address in the legend: the gateway operator's own name once we know it,
  // otherwise just a note that this address runs its own gateway.
  const donutWho = (o) => {
    const name = String(o.name || o.gateway_name || "").trim();
    if (name) return { text: name, known: true };
    if (o.fee_path === "datum") return { text: t("path.own"), known: false };
    return null;
  };

  function donutSlices(cb, expanded) {
    const value = Number(cb.value) || 0;
    if (!value) return [];
    const miners = (cb.miners || []).filter((o) => o.to !== "pool");
    const pool = (cb.miners || []).find((o) => o.to === "pool");
    const cut = expanded ? miners.length : DONUT_SLICES;
    const head = miners.slice(0, cut);
    const tail = miners.slice(cut);
    const out = head.map((o, i) => ({
      address: o.address,
      label: last4(o.address),
      who: donutWho(o),
      sats: Number(o.sats) || 0,
      percent: (100 * (Number(o.sats) || 0)) / value,
      colour: donutColour(i, Math.max(head.length, 2)),
    }));
    if (tail.length) {
      const s = tail.reduce((a, o) => a + (Number(o.sats) || 0), 0);
      out.push({
        label: t("payout.smaller", { n: tail.length }),
        tail: tail.length,
        sats: s,
        percent: (100 * s) / value,
        colour: "oklch(45% 0.03 80)",
      });
    }
    if (pool) {
      out.push({
        address: pool.address,
        label: "Lazarus",
        pool: true,
        note: t("payout.poolFee"),
        sats: Number(pool.sats) || 0,
        percent: (100 * (Number(pool.sats) || 0)) / value,
        colour: "oklch(32% 0.022 78)",
      });
    }
    return out;
  }

  function donut(cb) {
    const svg = $("payout-donut");
    if (!svg) return;
    const slices = donutSlices(cb, coinbaseExpanded);
    const legend = $("donut-legend");
    const hole = $("donut-hole-value");
    const holeLabel = $("donut-hole-label");
    const desc = $("donut-desc");
    const value = Number(cb.value) || 0;
    if (!slices.length) {
      svg.innerHTML = `<title id="donut-title">${t("payout.donutTitle")}</title>`;
      if (legend) legend.innerHTML = "";
      if (hole) hole.textContent = "\u2014";
      if (holeLabel) holeLabel.textContent = t("payout.noSplit");
      if (desc) desc.textContent = t("payout.noSplitDesc");
      return;
    }
    // Slices are drawn as dashes on one circle: offset walks backwards because a positive
    // dashoffset rotates the dash anticlockwise.
    let offset = 25; // start at twelve o'clock
    const arcs = slices.map((s) => {
      // Keep a hairline between slices, but never eat a slice that is thinner than the gap.
      // Dust outputs are left at their true width rather than padded, so the ring stays honest.
      const gap = s.percent > 1.2 ? 0.4 : 0;
      const len = s.percent >= 0.08 ? Math.max(0.1, s.percent - gap) : s.percent;
      const arc =
        `<circle class="donut-arc" r="${DONUT_R}" cx="20" cy="20" fill="none"` +
        ` stroke="${s.colour}" stroke-width="5"` +
        ` stroke-dasharray="${len.toFixed(3)} ${(100 - len).toFixed(3)}"` +
        ` stroke-dashoffset="${offset.toFixed(3)}">` +
        `<title>${esc(s.label)}${s.who ? " \u00b7 " + esc(s.who.text) : ""}${s.note ? " \u00b7 " + s.note : ""} \u2014 ${t("payout.sliceTitle", { pct: pct(s.percent, 2), amt: amtSats(s.sats) })}</title>` +
        "</circle>";
      offset -= s.percent;
      return arc;
    });
    svg.innerHTML =
      '<title id="donut-title">' + t("payout.donutTitle") + "</title>" +
      `<circle r="${DONUT_R}" cx="20" cy="20" fill="none" stroke="var(--bg-inset)" stroke-width="5"></circle>` +
      arcs.join("");

    if (legend) {
      legend.innerHTML = slices
        .map((s) => {
          const name = s.who
            ? `<span class="${s.who.known ? "donut-name" : "faint"}">${esc(s.who.text)}</span>`
            : s.note
              ? `<span class="faint">${esc(s.note)}</span>`
              : "";
          const label = s.address
            ? `<a class="mono" href="#${esc(s.address)}" title="${esc(s.address)}">${esc(s.label)}</a>`
            : `<span class="faint">${esc(s.label)}</span>`;
          return (
            '<li class="donut-item">' +
            `<span class="donut-swatch" style="background:${s.colour}" aria-hidden="true"></span>` +
            `<span class="donut-label">${label}${name ? " " + name : ""}</span>` +
            `<b class="donut-pct">${pct(s.percent, s.percent < 1 ? 2 : 1)}</b>` +
            "</li>"
          );
        })
        .join("");
    }
    const n = Number(cb.miner_outputs) || 0;
    if (hole) hole.textContent = n ? num(n) : "\u2014";
    if (holeLabel) holeLabel.textContent = t("payout.holeLabel", { n });
    if (desc) {
      const named = slices.filter((s) => s.who && s.who.known).length;
      const total = Number(cb.miner_outputs) || 0;
      desc.textContent = t("payout.donutDesc", { n, named: named ? t("payout.donutNamed", { n: named }) : "" });
    }
  }

  // ------------------------------------------------------------- coinbase
  const COINBASE_ROWS = 8;
  function coinbase(cb) {
    donut(cb);
    const el = $("coinbase");
    if (!el) return;
    const miners = (cb.miners || []).filter((o) => o.to !== "pool");
    const pool = (cb.miners || []).find((o) => o.to === "pool");
    const value = Number(cb.value) || 0;
    const shown = coinbaseExpanded ? miners : miners.slice(0, COINBASE_ROWS);
    const rows = shown.map((o, i) =>
      `<tr><td class="num faint">${i + 1}</td><td><a href="#${esc(o.address)}">${short(o.address)}</a></td><td>${pathPill(o.fee_path, null, o.name || o.gateway_name)}</td><td class="num">${winShareCell(o)}</td><td class="num">${pct(o.share_percent)}</td><td class="num" title="${amtExact(o.sats / 1e8)}">${amtSats(o.sats)}</td><td class="num faint">${value ? pct(100 * o.sats / value, 2) : "\u2014"}</td></tr>`
    );
    if (!coinbaseExpanded && miners.length > COINBASE_ROWS) {
      const rest = miners.slice(COINBASE_ROWS);
      const restSats = rest.reduce((a, o) => a + Number(o.sats || 0), 0);
      const restShares = rest.reduce((a, o) => a + (Number(o.window_shares) > 0 ? Number(o.window_shares) : 0), 0);
      rows.push(`<tr class="faint"><td class="num"></td><td>${t("payout.moreOutputs", { n: rest.length })}</td><td></td><td class="num">${restShares ? num(restShares) : "\u2014"}</td><td class="num">${pct(rest.reduce((a, o) => a + Number(o.share_percent || 0), 0))}</td><td class="num">${amtSats(restSats)}</td><td class="num">${value ? pct(100 * restSats / value, 2) : "\u2014"}</td></tr>`);
    }
    if (pool) {
      rows.push(`<tr class="pool-row"><td class="num faint">${miners.length + 1}</td><td>Lazarus <span class="faint">${t("payout.poolFee")}${cb.unplaced_sats > 1000 ? t("payout.poolCarry") : ""}${Number(cb.carry_paid_sats) > 1000 ? t("payout.poolCarryBack") : ""}</span> · <a href="#${esc(pool.address)}">${short(pool.address)}</a></td><td><span class="pill">${t("payout.poolPill")}</span></td><td class="num">\u2014</td><td class="num">\u2014</td><td class="num" title="${amtExact(pool.sats / 1e8)}">${amtSats(pool.sats)}</td><td class="num faint">${value ? pct(100 * pool.sats / value, 2) : "\u2014"}</td></tr>`);
    }
    el.innerHTML =
      `<th class="num">${t("payout.thNum")}</th><th>${t("payout.thPaidTo")}</th><th>${t("payout.thPath")}</th><th class="num">${t("payout.thWinShares")}</th><th class="num">${t("payout.thWinPct")}</th><th class="num">${t("payout.thOutput")}</th><th class="num">${t("payout.thOfBlock")}</th></tr></thead><tbody>` +
      (rows.length ? rows.join("") : `<tr><td colspan="7" class="empty">${t("payout.emptySplit")}</td></tr>`) +
      "</tbody>";
    const sum = $("coinbase-summary");
    if (sum) sum.textContent = cb.outputs ? t("payout.summary", { outputs: cb.outputs, minersAmt: amtSats(cb.miner_sats), n: cb.miner_outputs, poolAmt: amtSats(cb.pool_sats) }) : "\u2014";
    const more = $("coinbase-more");
    if (more) {
      more.hidden = miners.length <= COINBASE_ROWS;
      more.textContent = coinbaseExpanded ? t("payout.showFewer") : t("payout.showAll", { n: miners.length });
      more.onclick = () => { coinbaseExpanded = !coinbaseExpanded; coinbase(cb); };
    }
    const unpaid = cb.unpaid || [];
    const line = $("coinbase-more-line");
    if (line) {
      const old = line.querySelector(".unpaid-note");
      if (old) old.remove();
      const carryTotal = Number(cb.carry_total_sats) || 0;
      const carryPaid = Number(cb.carry_paid_sats) || 0;
      if (unpaid.length || carryTotal > 0 || carryPaid > 0) {
        const s = document.createElement("span");
        s.className = "unpaid-note faint";
        const parts = [];
        if (unpaid.length) parts.push(t("payout.unpaid", { n: unpaid.length }));
        if (carryPaid > 0) parts.push(t("payout.carryIn", { amt: amtSats(carryPaid) }));
        if (carryTotal > 0) parts.push(t("payout.carryHold", { amt: amtSats(carryTotal), n: num(cb.carry_holders || 0) }));
        s.textContent = " " + parts.join(" ");
        line.appendChild(s);
      }
    }
  }

  // ------------------------------------------------------------- gateways
  function gateways(pr) {
    const el = $("gwtable");
    if (!el) return;
    const gws = (pr.gateways || []).slice().sort((a, b) => {
      if (!!a.offline !== !!b.offline) return a.offline ? 1 : -1;
      if (a.own !== b.own) return a.own ? 1 : -1;
      return (b.block_candidates || 0) - (a.block_candidates || 0) || (b.work || 0) - (a.work || 0);
    });
    // Client string as "<software> <version>", so the table reads without knowing the wire format.
    const clientLabel = (g) => {
      const ua = String(g.user_agent || "");
      if (g.own) return `<div>${t("gws.lazGw")}</div><div class="faint">${esc(ua.split("/")[1] || "")}</div>`;
      const gen = String(g.generation || "").toLowerCase();
      const family = gen === "convoy" ? t("gws.convoyGw") : t("gws.datumGw");
      const ver = (ua.match(/v?(\d+\.\d+[\w.-]*?)(?=[+/]|$)/) || [])[1] || "";
      const flavor = (ua.match(/\+([a-z][\w-]*)/i) || [])[1] || "";
      const hash = (ua.match(/\/([0-9a-f]{7,})/i) || [])[1] || "";
      const bits = [ver, flavor, hash ? hash.slice(0, 7) : ""].filter(Boolean);
      return `<div>${esc(family)}</div><div class="faint" title="${esc(ua)}">${esc(bits.join(" · ") || ua)}</div>`;
    };
    const rows = gws.map((g) => {
      const who = g.own
        ? `<span class="pill brass">${t("gws.public")}</span>`
        : (() => {
            const tag = String(g.secondary_tag || g.name || "").trim();
            const key = `<span class="mono" title="${esc(t("gws.keyTitle"))}">${esc(g.gateway || "")}</span>`;
            return tag ? `<div>${esc(tag)}</div><div class="faint">${key}</div>` : key;
          })();
      const ident = g.own ? `<span class="faint">${t("gws.ownMiners")}</span>` : (g.identity ? `<a href="#${esc(g.identity)}">${short(g.identity)}</a>` : `<span class="faint">${t("gws.noShare")}</span>`);
      const hashing = !g.offline && g.last_share_s != null && g.last_share_s < 180 && g.accepted > 0;
      const state = g.offline
        ? `<div><span class="pill">${t("gws.offline")}</span></div><div class="faint">${t("gws.blocksOnRecord", { n: g.block_candidates || 0 })}</div>`
        : `<div>${hashing ? `<span class="pill ok">${t("gws.hashing")}</span>` : g.accepted > 0 ? `<span class="pill warn">${t("gws.idle")}</span>` : `<span class="pill">${t("gws.connected")}</span>`}</div>`
          + (g.accepted > 0 && g.last_share_s != null ? `<div class="faint">${t("gws.lastShare", { ago: agoS(g.last_share_s) })}</div>` : `<div class="faint">${t("gws.noShares")}</div>`);
      const shares = `<div>${num(g.accepted)}</div>${g.rejected ? `<div class="faint" title="${esc(g.last_reject || "")}">${t("gws.rejected", { n: num(g.rejected) })}</div>` : ""}`;
      return [
        who,
        state,
        clientLabel(g),
        pathPill(g.fee_path || (g.own ? "stratum" : "datum"), null, g.secondary_tag || g.name),
        ident,
        shares,
        num(g.work),
        `<div>${dur(g.connected_s)}</div><div class="faint">${t("gws.splits", { n: num(g.coinbasers) })}</div>`,
        String(g.block_candidates || 0),
      ];
    });
    table(el, [t("gws.thGw"), t("gws.thState"), t("gws.thClient"), t("gws.thFeePath"), t("gws.thPays"), t("gws.thShares"), t("gws.thWork"), t("gws.thConnected"), t("gws.thBlocks")], rows, [null, null, null, null, null, "num", "num", "num", "num"], t("gws.empty"));
    const tot = pr.totals || {};
    const tick = $("gateway-ticker");
    if (tick) {
      const live = gws.filter((g) => !g.offline);
      const remote = live.filter((g) => !g.own);
      const historic = gws.filter((g) => g.offline);
      const active = remote.filter((g) => g.accepted > 0).length;
      const cells = [
        [t("gws.tickConnected"), String(live.length), live.length ? t("gws.tickConnSub", { remote: remote.length, ours: live.length - remote.length, hist: historic.length ? t("gws.tickHist", { n: historic.length }) : "" }) : t("gws.tickNone")],
        [t("gws.tickRemote"), String(active), remote.length ? t("gws.tickRemoteSub", { n: remote.length, idle: remote.length - active }) : t("gws.tickNoRemote")],
        [t("gws.tickSplits"), num(tot.coinbasers), t("gws.tickSplitsSub")],
        [t("gws.tickShares"), num(tot.shares_accepted), t("gws.tickSharesSub", { rej: tot.shares_rejected ? t("gws.tickRej", { n: num(tot.shares_rejected) }) : "", up: dur(pr.uptime_s) })],
      ];
      tick.innerHTML = cells.map(([k, v, s]) => `<div><dt>${k}</dt><dd>${v}<small>${s}</small></dd></div>`).join("");
    }
    const note = $("gateway-note");
    if (note) {
      note.textContent = pr.reachable
        ? t("gws.noteOk", { conn: t("gws.nConn", { n: tot.connections }), blocks: t("gws.nBlocks", { n: tot.block_candidates }) })
        : t("gws.noteOff");
    }
  }

  // ------------------------------------------------------------------ solo
  // Solo is its own book. Nothing here is a share of anything: a solo miner is paid only
  // by the block it finds, so the tables show proven work and blocks, never "owed".
  function soloHashing(s) {
    return (s && s.miners || []).filter((m) => (Number(m.hashrate_ghs) || 0) > 1e-9 || (Number(m.workers) || 0) > 0);
  }

  function solo(s) {
    const on = !!(s && s.enabled);
    for (const id of ["solo", "nav-solo", "fee-card-solo", "tab-solo"]) {
      const el = $(id);
      if (el) el.hidden = !on;
    }
    if (!on) return;
    const fee = s.fee_percent;
    setText("fee-solo", pctFee(fee));
    setText("tab-fee-solo", pctFee(fee));
    setText("solo-fee-line", pctFee(fee));

    table(
      $("solo-endpoints"),
      [t("solo.thPort"), t("solo.thFor"), t("solo.thAddress"), t("solo.thFee"), t("solo.thDiff"), t("solo.thState")],
      (s.endpoints || []).map((e) => [
        esc(e.name || ""),
        e.name === "GPU" ? t("solo.forGpu") : t("solo.forAsic"),
        `<span class="mono">${esc(e.host || "")}:${esc(String(e.port || ""))}</span>`,
        pctFee(e.fee_percent),
        e.vardiff && e.vardiff.start ? num(e.vardiff.start) : "\u2014",
        e.online ? `<span class="pill ok">${t("solo.up")}</span>` : `<span class="pill">${t("solo.down")}</span>`,
      ]),
      [null, null, null, "num", "num", null],
      t("solo.emptyEp")
    );

    const hashing = soloHashing(s);
    const rows = hashing.map((m) => [
      `<a href="#${esc(m.address)}">${short(m.address)}</a>`,
      esc(m.via || "\u2014"),
      fmtHr(m.hashrate_ghs),
      num(m.workers),
      num(m.shares),
      num(m.work),
      m.best_diff ? num(m.best_diff) : "\u2014",
      String(m.blocks_onchain || m.blocks || 0),
    ]);
    table(
      $("solotable"),
      [t("solo.thAddress"), t("solo.thVia"), t("solo.thHr"), t("solo.thWorkers"), t("solo.thShares"), t("solo.thWork"), t("solo.thBest"), t("solo.thBlocks")],
      rows,
      [null, null, "num", "num", "num", "num", "num", "num"],
      t("solo.emptyMiners")
    );

    table(
      $("soloblocks"),
      [t("solo.thHeight"), t("solo.thFoundBy"), t("solo.thReward"), t("solo.thKept"), t("solo.thOurFee"), t("solo.thTime")],
      (s.blocks || []).map((b) => [
        String(b.height),
        b.finder ? `<a href="#${esc(b.finder)}">${short(b.finder)}</a>` : `<span class="faint">${t("solo.unknown")}</span>`,
        amt(b.reward_btc),
        amt(b.miner_btc),
        amt(b.pool_fee_btc),
        when(b.ts),
      ]),
      ["num", null, "num", "num", "num", null],
      t("solo.emptyBlocks")
    );

    const tick = $("solo-ticker");
    if (tick) {
      const up = (s.endpoints || []).filter((e) => e.online).length;
      const cells = [
        [t("solo.tickHr"), fmtHr(s.hashrate_ghs), hashing.length ? t("solo.tickHrSub", { n: hashing.length }) : t("solo.tickHrNone")],
        [t("solo.tickFound"), String(s.blocks_found || 0), t("solo.tickFoundSub")],
        [t("solo.tickFee"), pctFee(fee), t("solo.tickFeeSub")],
        [t("solo.tickPorts"), String(up), up ? t("solo.tickPortsUp") : t("solo.tickPortsDown")],
      ];
      tick.innerHTML = cells.map(([k, v, sub]) => `<div><dt>${k}</dt><dd>${v}<small>${sub}</small></dd></div>`).join("");
    }
    const note = $("solo-note");
    if (note) {
      note.textContent = hashing.length
        ? t("solo.noteSome")
        : t("solo.noteNone");
    }
  }

  const pctFee = feePct;
  function setText(id, text) {
    const el = $(id);
    if (el) el.textContent = text;
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
        : '<tr><td colspan="' + headers.length + '" class="empty">' + (empty || t("gws.empty")) + "</td></tr>") +
      "</tbody>";
  }

  // ---------------------------------------------------------------- blocks
  function foundBlocks(pays, p) {
    const el = $("found");
    if (!el) return [];
    const poolAddr = (p.prime && p.prime.address) || "";
    const byPrime = new Map((pays.prime_blocks || []).filter((b) => b.hash).map((b) => [b.hash, b]));
    const partsFor = (hash, combined) => {
      const pb = byPrime.get(hash);
      const total = Number(combined) || 0;
      if (!pb || !poolAddr) {
        // No issued split: a pool-address output is fee/remainder, not a miner.
        return { miner: 0, fee: total };
      }
      const minerSats = (pb.split || []).filter((o) => o.address === poolAddr).reduce((a, o) => a + Number(o.sats || 0), 0);
      if (pb.fee_sats != null) return { miner: minerSats / 1e8, fee: Number(pb.fee_sats) / 1e8 };
      if (minerSats) return { miner: minerSats / 1e8, fee: Math.max(0, (pb.pool_sats || 0) - minerSats) / 1e8 };
      return { miner: 0, fee: total };
    };
    const byHeight = new Map();
    for (const r of pays.payouts || []) {
      const key = r.hash || String(r.height);
      // Compact shape: one object per block with outputs[]. Legacy was one row per coinbase output.
      if (Array.isArray(r.outputs) || r.output_count != null) {
        const outs = Array.isArray(r.outputs)
          ? r.outputs.map((o) => ({
              address: o.address,
              btc: Number(o.btc != null ? o.btc : o.miner_btc) || 0,
              share: o.share,
              pool: o.to === "pool" || o.pool,
            }))
          : [];
        byHeight.set(key, {
          height: r.height, hash: r.hash, ts: r.ts, outputs: outs,
          output_count: Number(r.output_count) || outs.length,
          miner_btc: r.miner_btc != null ? Number(r.miner_btc) : outs.filter((o) => !o.pool).reduce((a, o) => a + o.btc, 0),
          pool_btc: r.pool_btc != null ? Number(r.pool_btc) : outs.filter((o) => o.pool).reduce((a, o) => a + o.btc, 0),
          status: r.status, kind: r.kind, block_status: r.block_status, owed_sats: r.owed_sats, owed_txid: r.owed_txid, owed_resolved: r.owed_resolved, found_by: r.found_by, reward: r.reward_btc, confirmations: r.confirmations,
        });
        continue;
      }
      const b = byHeight.get(key) || { height: r.height, hash: r.hash, ts: r.ts, outputs: [], miner_btc: 0, pool_btc: 0, status: r.status, kind: r.kind, block_status: r.block_status, owed_sats: r.owed_sats, owed_txid: r.owed_txid, owed_resolved: r.owed_resolved, found_by: r.found_by, reward: r.reward_btc, confirmations: r.confirmations };
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
      byHeight.set(pb.hash, { height: pb.height, hash: pb.hash, ts: pb.ts, outputs: (pb.split || []).map((o) => ({ address: o.address, btc: o.sats / 1e8 })), miner_btc: (pb.split || []).reduce((a, o) => a + o.sats, 0) / 1e8, pool_btc: feeSats / 1e8, status: pb.status, kind: pb.kind, block_status: pb.status, owed_sats: pb.owed_sats, owed_txid: pb.owed_txid, owed_resolved: pb.owed_resolved, found_by: pb.finder, reward: pb.coinbase_value / 1e8, prime_only: true });
    }
    const blocks = [...byHeight.values()].sort((a, b) => (b.height || 0) - (a.height || 0));
    const tip = Number(pays.tip || p.height || 0);
    const need = Number(pays.maturity_blocks || 100);
    const foundStatus = (b) => {
      const bs = String(b.block_status || "").toLowerCase();
      if (bs === "orphaned" || bs === "rejected" || bs === "pending") return bs;
      if (b.prime_only) return bs || b.status || "pending";
      const h = Number(b.height);
      const confs = Number(b.confirmations) || (tip && h ? tip - h + 1 : 0);
      if (h && confs < need) return "immature";
      if (h && confs >= need) return "spendable";
      return b.status === "unsplit" ? "pool_only" : (b.status || bs || "\u2014");
    };
    for (const b of blocks) {
      if (!b.confirmations && tip && b.height) b.confirmations = tip - b.height + 1;
    }
    const headers = [t("blocks.thHeight"), t("blocks.thBlock"), t("blocks.thCoinbase"), t("blocks.thOutputs"), t("blocks.thMiners"), t("blocks.thPool"), t("blocks.thStatus"), t("blocks.thFoundBy"), t("blocks.thTime")];
    const detailHtml = (b) => {
      const outs = (b.outputs || []).slice().sort((x, y) => y.btc - x.btc);
      const rows = outs.map((o) => `<tr><td></td><td colspan="2"><a href="#${esc(o.address)}">${esc(o.address)}</a>${o.pool ? ` <span class="pill brass">${t("blocks.poolPill")}</span>` : ""}</td><td class="num" title="${amtExact(o.btc)}">${amt(o.btc)}</td><td class="num faint">${b.reward ? pct(100 * o.btc / b.reward, 2) : ""}</td><td colspan="4"></td></tr>`).join("");
      return rows || `<tr><td colspan="9" class="empty">${t("blocks.emptyOut")}</td></tr>`;
    };
    const rows = blocks.map((b, i) => {
      const poolBtc = b.pool_btc != null ? b.pool_btc : (b.reward ? Math.max(0, b.reward - b.miner_btc) : null);
      const nOut = Number(b.output_count) || (b.outputs || []).length;
      const kind = b.kind || (nOut > 1 ? "split" : nOut === 1 ? "" : "");
      const st = foundStatus(b);
      const confs = Number(b.confirmations) || 0;
      const stTitle = st === "immature" && confs
        ? t("blocks.confImm", { c: confs, need })
        : (st === "spendable" && confs ? t("blocks.confOk", { c: confs }) : "");
      const owed = b.owed_txid
        ? `<div class="faint">${t("blocks.windowPaid", { amt: b.owed_sats ? " " + amtSats(b.owed_sats) : "" })} · <a href="${esc(p.explorer)}/tx/${esc(b.owed_txid)}" target="_blank" rel="noreferrer" title="${esc(t("blocks.txPaidTitle"))}">tx ${shortHash(b.owed_txid)}</a></div>`
        : (b.owed_sats ? `<div class="faint">${t("blocks.owed", { amt: amtSats(b.owed_sats) })}</div>` : "");
      return `<tr class="block-row" data-i="${i}" tabindex="0" aria-expanded="false">
        <td class="num"><span class="disclose"></span>${b.height ?? "\u2014"}</td>
        <td>${b.hash ? `<a href="${esc(p.explorer)}/block/${esc(b.hash)}" target="_blank" rel="noreferrer" title="${esc(t("blocks.openMempool"))}">${shortHash(b.hash)}</a>` : "\u2014"}</td>
        <td>${kindPill(kind)}${owed}</td>
        <td class="num">${b.output_count || (b.outputs || []).length}</td>
        <td class="num" title="${amtExact(b.miner_btc)}">${amt(b.miner_btc)}</td>
        <td class="num" title="${poolBtc == null ? "" : amtExact(poolBtc)}">${poolBtc == null ? "\u2014" : amt(poolBtc)}</td>
        <td title="${esc(stTitle)}">${statusPill(st)}</td>
        <td>${b.found_by ? `<a href="#${esc(b.found_by)}">${short(b.found_by)}</a>` : "\u2014"}</td>
        <td>${when(b.ts)}</td>
      </tr>
      <tr class="block-detail" hidden data-empty="1"><td colspan="9"><table class="inner"><thead><tr><th></th><th colspan="2">${t("blocks.thOut")}</th><th class="num">${t("blocks.thAmt")}</th><th class="num">${t("blocks.thOf")}</th><th colspan="4"></th></tr></thead><tbody></tbody></table></td></tr>`;
    });
    el.innerHTML =
      "<thead><tr>" + headers.map((h, i) => `<th${[0, 3, 4, 5].includes(i) ? ' class="num"' : ""}>${h}</th>`).join("") + "</tr></thead><tbody>" +
      (rows.length ? rows.join("") : `<tr><td colspan="9" class="empty">${t("blocks.emptyFound")}</td></tr>`) +
      "</tbody>";
    el.querySelectorAll(".block-row").forEach((tr) => {
      const toggle = () => {
        const det = tr.nextElementSibling;
        const open = det && det.hidden;
        if (open && det && det.dataset.empty === "1") {
          const tb = det.querySelector("tbody");
          const i = Number(tr.getAttribute("data-i"));
          const b = blocks[i];
          const fill = () => { if (tb) tb.innerHTML = detailHtml(b); det.dataset.empty = "0"; };
          if (b && !(b.outputs || []).length && b.hash) {
            if (tb) tb.innerHTML = `<tr><td colspan="9" class="empty">${t("gws.empty")}</td></tr>`;
            j("/api/found/" + encodeURIComponent(b.hash)).then((d) => {
              b.outputs = (d.outputs || []).map((o) => ({
                address: o.address,
                btc: Number(o.btc) || 0,
                share: o.share,
                pool: o.to === "pool" || o.pool,
              }));
              fill();
            }).catch(fill);
          } else {
            fill();
          }
        }
        if (det) det.hidden = !open;
        tr.setAttribute("aria-expanded", open ? "true" : "false");
      };
      tr.addEventListener("click", (e) => { if (!e.target.closest("a")) toggle(); });
      tr.addEventListener("keydown", (e) => { if (e.key === "Enter" || e.key === " ") { e.preventDefault(); toggle(); } });
    });
    return blocks;
  }

  // Legend + screen-reader text for the marks that landed inside the drawn window.

  const showChart = (on) => {
    const w = $("chart-wrap");
    if (w) w.hidden = !on;
  };

  // ----------------------------------------------------------------- miner
  // Block interval from /api/pool, for the "spendable in" ETA on pending payouts.
  let blockInterval = 0;
  // Which panel of the miner card is showing; follows the URL as #<addr>/<tab>.
  let minerTab = "overview";
  let minerAddr = "";
  async function showMiner(addr, tab) {
    if (tab) minerTab = tab;
    if (!addr) {
      minerAddr = "";
      $("miner").innerHTML = "";
      showChart(false);
      draw($("chart"), [], "hr_ghs");
      navPayouts("");
      return;
    }
    const m = await j("/api/miner/" + encodeURIComponent(addr));
    minerAddr = addr;
    try { localStorage.setItem("lz.addr", addr); } catch (e) { /* private mode */ }
    navPayouts(addr);
    const hasPool = !!(m.via || Number(m.window_work) > 0 || Number(m.shares_lifetime) > 0 || Number(m.round_share) > 0);
    $("miner").innerHTML = minerCard(m, { fees, money, blockInterval, tab: minerTab, full: false });
    if (!m.known || !hasPool) {
      showChart(false);
      return;
    }
    showChart(true);
    // Same trend markers, but from this address's side: the blocks its work was paid in.
    draw(
      $("chart"),
      m.history || [],
      "hr_ghs",
      (m.blocks_found || [])
        .filter((b) => Number(b.ts) > 0)
        .map((b) => ({
          ts: Number(b.ts),
          title: (b.height ? t("chart.blockN", { h: num(b.height) }) : t("chart.block")) + " · " + clock(b.ts),
          sub: t("chart.toYou", { amt: amt(b.miner_btc) }) + (payStatus(b) === "immature" ? t("chart.immature") : ""),
        }))
    );
    chartLegend($("chart-legend"), $("chart"), t("chart.blockPaidYou", {n:1}), t("chart.blockPaidYou", {n:2}));
  }
  // Tab clicks inside the card switch panels and update the URL without a reload.
  $("miner").addEventListener("click", (e) => {
    const t = e.target.closest(".mtab[data-mtab]");
    if (!t) return;
    e.preventDefault();
    minerTab = showMinerTab($("miner"), t.getAttribute("data-mtab")) || "overview";
    if (minerAddr) history.replaceState(null, "", "#" + minerAddr + (minerTab === "overview" ? "" : "/" + minerTab));
  });
  // "Payouts" in the top nav goes straight to this address's payouts panel.
  function navPayouts(addr) {
    const a = $("nav-payouts");
    if (!a) return;
    if (!addr) {
      a.hidden = true;
      return;
    }
    a.hidden = false;
    a.href = "#" + addr + "/payouts";
  }

  // ------------------------------------------------------------- calculator
  // Same per-TH/s figure the status panel shows, so the calculator can never disagree with the
  // rest of the page: whatever the DATUM rate including the bonus is right now, times your
  // hashrate. Null until the first /api/pool lands.
  let calcRate = null;

  function renderCalc() {
    const input = $("calc-hr");
    if (!input) return;
    const unit = Number(($("calc-unit") || {}).value || 1);
    const ths = Number(input.value) / (unit || 1);
    const perDay = calcRate != null && Number.isFinite(ths) && ths >= 0 ? calcRate * ths : null;
    const set = (id, btc) => {
      const el = $(id);
      if (!el) return;
      el.textContent = btc == null ? "\u2014" : amt(btc);
      if (btc != null) el.title = amtExact(btc);
    };
    const setUsd = (id, btc) => {
      const el = $(id);
      if (el) el.textContent = btc == null ? "" : money(btc);
    };
    set("calc-xbt", perDay);
    setUsd("calc-usd", perDay);
    set("calc-xbt-30", perDay == null ? null : perDay * 30);
    setUsd("calc-usd-30", perDay == null ? null : perDay * 30);
  }

  // BT-Miners BTCB2 catalog: price + Lazarus earnings at current difficulty.
  function hardware(doc) {
    const grid = $("hardware-grid");
    const empty = $("hardware-empty");
    const meta = $("hardware-meta");
    if (!grid) return;
    const miners = (doc && doc.miners) || [];
    if (meta) {
      meta.textContent = miners.length ? t("hardware.source", { shop: "BT-Miners" }) : "";
    }
    if (!miners.length) {
      grid.innerHTML = "";
      if (empty) empty.hidden = false;
      return;
    }
    if (empty) empty.hidden = true;
    const moneyDay = (usd) => {
      const u = Number(usd);
      if (!Number.isFinite(u)) return "\u2014";
      if (u >= 100) return "$" + u.toLocaleString(loc(), { maximumFractionDigits: 0 });
      if (u >= 1) return "$" + u.toLocaleString(loc(), { minimumFractionDigits: 2, maximumFractionDigits: 2 });
      return "$" + u.toLocaleString(loc(), { minimumFractionDigits: 2, maximumFractionDigits: 3 });
    };
    const price = (usd) => {
      const u = Number(usd);
      if (!Number.isFinite(u)) return "\u2014";
      return "$" + u.toLocaleString(loc(), { maximumFractionDigits: u >= 100 ? 0 : 2 });
    };
    grid.innerHTML = miners.map((m) => {
      const href = esc(m.url || "https://bt-miners.com/collections/btcb2-miners/");
      const name = esc(m.name || m.model || "Miner");
      const hr = m.ths != null ? fmtHr(Number(m.ths) * 1e3) : "\u2014";
      const watts = Number(m.watts);
      const spec = Number.isFinite(watts) && watts > 0 ? `${hr} · ${num(Math.round(watts))} W` : hr;
      const stock = m.in_stock ? t("hardware.inStock") : t("hardware.outOfStock");
      const stockCls = m.in_stock ? "ok" : "warn";
      const cond = /used/i.test(m.condition || "") ? `<span class="pill">${esc(t("hardware.used"))}</span>` : "";
      const img = m.image
        ? `<img class="hw-img" src="${esc(m.image)}" alt="" width="160" height="160" loading="lazy">`
        : `<span class="hw-img hw-img-empty" aria-hidden="true"></span>`;
      return `<li>
        <a class="hw-card${m.in_stock ? "" : " oos"}" href="${href}" target="_blank" rel="noreferrer" title="${esc(t("hardware.buyTitle", { name: m.name || m.model || "" }))}">
          ${img}
          <div class="hw-body">
            <div class="hw-head">
              <h3>${name}</h3>
              <span class="pill ${stockCls}">${esc(stock)}</span>
              ${cond}
            </div>
            <p class="hw-spec">${esc(spec)}</p>
            <dl class="hw-facts">
              <div><dt>${esc(t("hardware.price"))}</dt><dd>${esc(price(m.price_usd))}</dd></div>
              <div><dt>${esc(t("hardware.xbtDay"))}</dt><dd title="${esc(amtExact(m.xbt_day))}">${amt(m.xbt_day)}</dd></div>
              <div><dt>${esc(t("hardware.usdDay"))}</dt><dd>${esc(moneyDay(m.usd_day))}</dd></div>
            </dl>
          </div>
        </a>
      </li>`;
    }).join("");
  }

  // --------------------------------------------------------------- refresh
  let refreshBusy = false;
  let lastHeavy = 0;
  let lastPays = { payouts: [], prime_blocks: [] };
  let lastBlocks = { blocks: [] };
  async function refresh() {
    if (refreshBusy) return;
    refreshBusy = true;
    try {
      // Stats first. Payouts used to be a 7 MB Found-by-Lazarus dump that stalled /api/pool.
      const [p, cb, px, so] = await Promise.all([
        j("/api/pool"),
        j("/api/coinbaser").catch(() => ({})),
        j("/api/price").catch(() => null),
        j("/api/solo").catch(() => null),
      ]);
      if (px && Number.isFinite(Number(px.USD))) priceUsd = Number(px.USD);
      if (Number(p.block_interval_seconds) > 0) blockInterval = Number(p.block_interval_seconds);
      stats(p, so);
      payoutHero(p, cb || {});
      coinbase(cb || {});
      gateways(p.prime || {});
      solo(so);
      draw($("poolchart"), p.history || [], "hr_ghs", $("poolchart")?.__marks || []);
      chartLegend($("poolchart-legend"), $("poolchart"), t("chart.blockFound", {n:1}), t("chart.blockFound", {n:2}));

      const miners = await j("/api/miners").catch(() => ({ online: [], seen: [] }));
      const online = (miners.online || []).filter((m) => m.address);
      const firstOf = (m, i) => online.findIndex((x) => x.address === m.address) === i;
      table(
        $("online"),
        [t("miners.thAddress"), t("miners.thWorker"), t("miners.thPath"), t("miners.thHr"), t("miners.thSession"), t("miners.thAccepted"), t("miners.thWinShares"), t("miners.thWinPct"), t("miners.thNext"), t("miners.thLast")],
        online.map((m, i) => [
          `<a href="#${esc(m.address)}">${short(m.address)}</a>`,
          m.worker === "window" ? `<span class="faint">${t("miners.viaGw")}</span>` : (Number(m.sessions) > 1 ? t("miners.nWorkers", { n: num(m.sessions) }) : esc(m.worker || "\u2014")),
          pathPill(m.fee_path, m.via, m.gateway_name),
          fmtHr(firstOf(m, i) ? (m.credited_hr_ghs || m.hr_ghs) : m.hr_ghs),
          sessCell(m.via, m.shares_session),
          num(m.shares_lifetime ?? m.shares_acc ?? m.window_work),
          winShareCell(m, firstOf(m, i)),
          firstOf(m, i) && m.window_percent != null ? pct(m.window_percent) : "\u2014",
          firstOf(m, i) && m.window_sats ? amtSats(m.window_sats) : "\u2014",
          Number.isFinite(Number(m.last_share_s)) ? Number(m.last_share_s).toFixed(0) + t("miner.s") : "\u2014",
        ]),
        [null, null, null, "num", "num", "num", "num", "num", "num", "num"],
        t("miners.emptyOnline")
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
        [t("miners.thAddress"), t("miners.thPath"), t("miners.thAccepted"), t("miners.thWinShares"), t("miners.thWinPct"), t("miners.thNext"), t("miners.thLastSeen")],
        seen.map((m) => [
          `<a href="#${esc(m.address)}">${short(m.address)}</a>`,
          m.window_work ? pathPill(m.fee_path, null, m.gateway_name) : "\u2014",
          num(m.shares_lifetime ?? m.shares_acc ?? m.window_work),
          winShareCell(m),
          m.window_work ? pct(m.window_percent) : "\u2014",
          m.window_sats ? amtSats(m.window_sats) : "\u2014",
          ago(m.last_ts),
        ]),
        [null, null, "num", "num", "num", "num", "num"],
        t("miners.emptySeen")
      );
      relayedTable(miners.relayed, miners.overflow);
      // Rows are one per address; each carries how many sessions (rigs) it rolled up.
      const onlineWorkers = online.reduce((a, m) => a + (Number(m.sessions) || 1), 0);
      if ($("online-count")) $("online-count").textContent = online.length ? t("miners.onlineCount", { addr: onlineAddrs.size, workers: t("miners.nWorkers", { n: onlineWorkers }) }) : "";
      if ($("seen-count")) {
        const holding = seenAll.filter((m) => m.window_sats > 0).length;
        $("seen-count").textContent = seenAll.length
          ? t("miners.seenCount", { shown: seenAll.length > SEEN_ROWS ? t("miners.seenOf", { n: SEEN_ROWS }) : "", n: seenAll.length, hold: holding ? t("miners.seenHold", { n: holding }) : "" })
          : "";
      }

      // Keep the looked-up card current too (same panel stays open).
      if (minerAddr) showMiner(minerAddr).catch(() => {});
      const hw = await j("/api/hardware").catch(() => null);
      hardware(hw);

      const now = Date.now();
      if (now - lastHeavy > 60000 || !(lastPays.payouts || []).length) {
        const [blocks, pays] = await Promise.all([
          j("/api/blocks").catch(() => ({ blocks: [] })),
          j("/api/payouts").catch(() => ({ payouts: [], prime_blocks: [] })),
        ]);
        lastBlocks = blocks;
        lastPays = pays;
        lastHeavy = now;
      }
      const blocks = lastBlocks;
      const pays = lastPays;
      const marks = blockMarks(foundBlocks(pays, p));
      draw($("poolchart"), p.history || [], "hr_ghs", marks);
      chartLegend($("poolchart-legend"), $("poolchart"), t("chart.blockFound", {n:1}), t("chart.blockFound", {n:2}));
      table(
        $("blocktable"),
        [t("blocks.thHeight"), t("blocks.thTag"), t("blocks.thTime"), t("blocks.thTxs"), ""],
        (blocks.blocks || []).slice(0, 16).map((b) => [
          b.height,
          esc(b.pool || t("blocks.unknown")),
          when(b.timestamp),
          b.tx_count || "",
          b.explorer ? `<a href="${esc(b.explorer)}" target="_blank" rel="noreferrer">${t("blocks.explorer")}</a>` : "",
        ]),
        ["num", null, null, "num", null]
      );
    } finally {
      refreshBusy = false;
    }
  }

  // Section anchors never trigger an address lookup. "mine" and "datum" are
  // retired anchors kept so old bookmarks still land somewhere sensible.
  const SECTIONS = new Set([
    "", "top", "fees", "status", "payout", "window", "connect", "hardware", "gw-pick", "dashboard", "miners", "gateways",
    "blocks", "payouts", "pools", "how", "datum", "mine", "solo", "calc",
  ]);

  function fromHash() {
    const a = location.hash.slice(1);
    if (a === "datum") {
      selectTab("tab-datum");
      $("connect")?.scrollIntoView();
      return;
    }
    if (a === "gw-pick") {
      selectTab("tab-datum");
      return;
    }
    if (a === "window") {
      $("payout")?.scrollIntoView();
      return;
    }
    if (a && !SECTIONS.has(a)) {
      const [addr, tab] = a.split("/");
      $("lookup").value = addr;
      showMiner(addr, tab || "overview");
      // No element carries the address as its id, so the browser has nothing to scroll to;
      // take the reader to the lookup card ourselves.
      anchorDashboard();
    }
  }
  // Scroll to the lookup card, and keep it pinned there while the sections above it fill
  // with data (tiles, coinbase table, miners) — each one growing pushes the card down and
  // would otherwise leave the reader looking at whatever slid into the viewport. Stops as
  // soon as the reader scrolls on their own.
  let anchorUntil = 0;
  let userScrolled = false;
  const stopAnchor = () => { userScrolled = true; };
  for (const ev of ["wheel", "touchstart", "keydown", "mousedown"]) window.addEventListener(ev, stopAnchor, { passive: true });
  function anchorDashboard() {
    const el = $("dashboard");
    if (!el) return;
    userScrolled = false;
    anchorUntil = performance.now() + 6000;
    // Instant: a smooth scroll restarted every correction would crawl for seconds.
    el.scrollIntoView({ block: "start", behavior: "instant" });
    let last = -1;
    const tick = () => {
      if (userScrolled || performance.now() > anchorUntil) return;
      const top = el.getBoundingClientRect().top;
      if (Math.abs(top - last) > 1) {
        el.scrollIntoView({ block: "start", behavior: "instant" });
        last = el.getBoundingClientRect().top;
      }
      requestAnimationFrame(tick);
    };
    requestAnimationFrame(tick);
  }

  $("go").onclick = () => {
    const a = $("lookup").value.trim();
    location.hash = a;
    showMiner(a);
  };
  $("lookup").addEventListener("keydown", (e) => {
    if (e.key === "Enter") $("go").click();
  });
  for (const id of ["calc-hr", "calc-unit"]) {
    $(id)?.addEventListener("input", renderCalc);
  }
  $("calc")?.addEventListener("submit", (e) => {
    e.preventDefault();
    renderCalc();
  });
  window.addEventListener("hashchange", fromHash);
  // Clicking the address that is already in the URL fires no hashchange; handle it here.
  document.addEventListener("click", (e) => {
    const a = e.target.closest('a[href^="#"]');
    if (!a) return;
    const target = a.getAttribute("href").slice(1);
    if (target && !SECTIONS.has(target) && location.hash.slice(1) === target) {
      e.preventDefault();
      fromHash();
    }
  });
  fromHash();
  // Pretty URLs (/hardware, /mine-xbt, /bip110 …) serve this same page. The server marks the
  // section to open in data-scroll so a crawler-visible URL still lands in the right place.
  if (!location.hash) {
    const dest = document.documentElement.getAttribute("data-scroll");
    if (dest) $(dest)?.scrollIntoView({ block: "start", behavior: "instant" });
  }
  if (!minerAddr) {
    try {
      const last = localStorage.getItem("lz.addr");
      if (last && !SECTIONS.has(last)) navPayouts(last);
    } catch (e) { /* private mode */ }
  }
  refresh().catch((e) => console.error(e));
  setInterval(() => refresh().catch((e) => console.error(e)), 10000);
  document.addEventListener("lz:i18n", () => {
    LZ_I18N.apply();
    refresh().catch((e) => console.error(e));
  });
  window.addEventListener("resize", () => {
    for (const [id, legend, one, many] of [
      ["poolchart", "poolchart-legend", t("chart.blockFound", {n:1}), t("chart.blockFound", {n:2})],
      ["chart", "chart-legend", t("chart.blockPaidYou", {n:1}), t("chart.blockPaidYou", {n:2})],
    ]) {
      const c = $(id);
      if (!c || !c.__hist) continue;
      draw(c, c.__hist, "hr_ghs", c.__marks);
      chartLegend($(legend), c, one, many);
    }
    const pie = $("path-pie");
    if (pie && pie.__slices) drawPathPie(pie, pie.__slices);
    const gwPie = $("gw-pie");
    if (gwPie && gwPie.__slices) drawPathPie(gwPie, gwPie.__slices);
  });

  // ---------------------------------------------------------------- chrome
  // Top bar gets a hairline once the page has scrolled; the nav highlights the section in
  // view; sections rise in as they enter. All decorative — nothing here touches data — and
  // all of it degrades to nothing when the browser lacks IntersectionObserver.
  (() => {
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
    if (!("IntersectionObserver" in window)) return;

    const navLinks = [...document.querySelectorAll('.nav a[href^="#"]')];
    const byId = new Map(navLinks.map((a) => [a.getAttribute("href").slice(1), a]));
    const targets = [...byId.keys()].map((id) => $(id)).filter(Boolean);
    if (targets.length) {
      const visible = new Map();
      const spy = new IntersectionObserver(
        (entries) => {
          for (const e of entries) visible.set(e.target.id, e.isIntersecting ? e.intersectionRatio : 0);
          // The first section (in document order) with anything on screen is the current one,
          // so the highlight walks down the nav as the page scrolls rather than jumping to
          // whichever section happens to fill most of the viewport.
          let current = null;
          for (const t of targets) {
            if (t.hidden) continue;
            if ((visible.get(t.id) || 0) > 0) { current = t.id; break; }
          }
          for (const [id, a] of byId) {
            if (id === current) a.setAttribute("aria-current", "true");
            else a.removeAttribute("aria-current");
          }
          if (current && byId.get(current)) {
            const a = byId.get(current);
            const nav = a.closest(".nav");
            if (nav && nav.scrollWidth > nav.clientWidth) {
              const left = a.offsetLeft - nav.clientWidth / 2 + a.offsetWidth / 2;
              nav.scrollTo({ left, behavior: "smooth" });
            }
          }
        },
        { rootMargin: "-40% 0px -55% 0px", threshold: [0, 0.01] }
      );
      targets.forEach((t) => spy.observe(t));
    }

    const reduce = window.matchMedia && window.matchMedia("(prefers-reduced-motion: reduce)").matches;
    if (!reduce) {
      const sections = [...document.querySelectorAll("main .section")];
      const io = new IntersectionObserver(
        (entries) => {
          for (const e of entries) {
            if (!e.isIntersecting) continue;
            e.target.classList.add("in");
            io.unobserve(e.target);
          }
        },
        { rootMargin: "0px 0px -8% 0px" }
      );
      for (const s of sections) {
        // Anything already on screen at load stays put; only sections below the fold animate.
        if (s.getBoundingClientRect().top < window.innerHeight) continue;
        s.classList.add("reveal");
        io.observe(s);
      }
    }
  })();
})();
