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
  // Formatters and the trend chart live in shared.js (window.LZ), shared with /miner/<addr>.
  const { blockMarks, esc, fmtHr, num, bigNum, short, shortHash, pct, ago, agoS, dur, when, clock, sig4, amt, amtSats, amtExact, kindPill, statusPill, chartLegend, draw, pathLabel, isPrimePath, sessCell, winShareCell, feePct, poolLink, payStatus, minerCard, showMinerTab, MINER_TABS } = window.LZ;

  // Fee schedule as primed reports it: one rate for work through a miner's own DATUM
  // gateway, another for our public stratum. Filled from /api/pool on every refresh.
  const fees = { datum: 0, stratum: 2.0 };
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
      const label = gwName ? `stratum + ${gwName}` : "stratum + gateway";
      return `<span class="pill brass" title="Work arriving on both the public stratum and an own gateway${gwName ? " (" + esc(gwName) + ")" : ""}">${esc(label)}</span>`;
    }
    const own = p !== "stratum";
    const label = own ? (gwName || "own gateway") : "public stratum";
    const title = own
      ? (gwName ? `Own DATUM gateway — ${gwName}` : "Most of this address's window work came through its own DATUM gateway")
      : "Most of this address's window work came through the Lazarus public stratum";
    let html = `<span class="pill ${own ? "brass" : ""}" title="${esc(title)}">${esc(label)}</span> <span class="faint">${feePct(feeForPath(p))}</span>`;
    if (billed && live && live !== billed) {
      const where = live === "both" ? "both paths" : live === "stratum" ? "stratum now" : "gateway now";
      html += ` <span class="faint" title="Connected right now on a different path. The fee follows where most of the window's accepted work arrived, so it moves as the window rolls.">· ${where}</span>`;
    }
    return html;
  };

  // Bitcoin (BTCB2) USD from /api/price (Neoxa + NonKYC last prices, weighted by 24h volume).
  let priceUsd = null;
  const moneyOnly = (usd) => {
    const u = Number(usd);
    if (!Number.isFinite(u)) return "\u2014";
    return "$" + u.toLocaleString(undefined, { maximumFractionDigits: u >= 100 ? 0 : 2 });
  };
  const money = (btcAmt) => {
    const u = Number(btcAmt) * Number(priceUsd);
    if (!Number.isFinite(u) || !Number.isFinite(Number(priceUsd))) return "";
    return " · " + moneyOnly(u);
  };
  // 1 TH/s of continuous work at the current difficulty, base subsidy (no tx fees).
  const thsDay = (p, feePercent) => {
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
    el.innerHTML =
      `<strong>Overflow is on.</strong> Lazarus holds ${Number.isFinite(share) ? share.toFixed(1) : "\u2014"}% of the network hashrate, over the ${Number(ov.enter_pct) || 32}% line we hold ourselves to. ` +
      `Miners already here keep mining here. A <em>new</em> miner pointing at our stratum is relayed to ${ups.length ? ups.join(", ") : "another BLAKE2b pool"} and is paid by that pool, not by us` +
      (ov.proxied_sessions ? ` — ${num(ov.proxied_sessions)} connection${ov.proxied_sessions === 1 ? "" : "s"} relayed right now.` : ".") +
      ` Own-gateway (DATUM) miners are never relayed. New miners come back to Lazarus once we are under ${Number(ov.exit_pct) || 27}%. <a href="#pools">About those pools</a>.`;
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
        share.innerHTML = `Lazarus holds <b>${s.toFixed(1)}%</b> of the network right now` +
          (ov.active ? ` — over the ${Number(ov.enter_pct) || 32}% line, so new stratum miners are being relayed.` : ` — under the ${Number(ov.enter_pct) || 32}% line, so nobody is being relayed.`);
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
        pill.textContent = "not answering";
        pill.title = u.last_error ? String(u.last_error) : "Our gateway could not reach this pool's stratum on its last check";
      } else if (n > 0) {
        pill.className = "pill pool-state warn";
        pill.textContent = `${num(n)} relayed here`;
        pill.title = `${num(n)} of our stratum connection${n === 1 ? " is" : "s are"} being relayed to this pool right now`;
      } else {
        pill.className = "pill pool-state ok";
        pill.textContent = "reachable";
        pill.title = "Our gateway reached this pool's stratum on its last check";
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
      ["Address", "Worker", "Mining on", "Connected", "Accepted there", "From"],
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
    if ($("relayed-count")) $("relayed-count").textContent = `${rows.length} connection${rows.length === 1 ? "" : "s"}`;
    if ($("relayed-note")) {
      $("relayed-note").textContent =
        "These miners connected to our stratum while Lazarus was over its network-share limit, so the gateway passed them straight through to the pool named here. That pool pays them; nothing here is credited to our window. Click the pool to see your stats there. Reconnecting after we drop under the limit brings you back to Lazarus.";
    }
    wrap.hidden = false;
  }

  function stats(p) {
    const pr = p.prime || {};
    const tot = pr.totals || {};
    if (p.fees) {
      if (Number.isFinite(Number(p.fees.datum_percent))) fees.datum = Number(p.fees.datum_percent);
      if (Number.isFinite(Number(p.fees.stratum_percent))) fees.stratum = Number(p.fees.stratum_percent);
    }
    const setText = (id, t) => { const e = $(id); if (e) e.textContent = t; };
    for (const id of ["fee-datum", "tab-fee-datum", "datum-fee-line", "top-fee-datum"]) setText(id, feePct(fees.datum));
    for (const id of ["fee-stratum", "tab-fee-stratum", "stratum-fee-line", "pillar-stratum-fee", "top-fee-stratum"]) setText(id, feePct(fees.stratum));
    if (fees.datum === 0 && fees.stratum > 0) {
      setText("fee-ratio", "no cut versus");
    } else if (fees.datum > 0 && fees.stratum > fees.datum) {
      const r = fees.stratum / fees.datum;
      const words = { 2: "twice", 4: "four times", 5: "five times", 10: "ten times" }[r] || (Number.isInteger(r) ? r + " times" : r.toFixed(1) + "×");
      setText("fee-ratio", words);
    }
    setText("how-window", String(Number(p.window_multiple) || 8));
    setText("how-fees", fees.datum === fees.stratum
      ? `Fee: ${feePct(fees.datum)} of each miner's window share, taken inside the coinbase.`
      : `Fees are taken per miner, inside the coinbase, by the path the work arrived on: ${feePct(fees.datum)} of your window share through your own DATUM gateway, ${feePct(fees.stratum)} on the public stratum. Switching paths keeps your accepted work.`);
    const net = p.network_hr_hs ? (p.network_hr_hs / 1e15).toFixed(2) + " PH/s" : "\u2014";
    const luck = p.luck_percent == null ? "\u2014" : p.luck_percent.toFixed(0) + "%";
    const expected = Number(p.blocks_expected);
    // Luck is measured over the span the hashrate samples cover (a rolling week), at the
    // difficulty in force when each hash was done; the headline count is all-time.
    const luckFound = Number.isFinite(Number(p.luck_blocks_found)) ? Number(p.luck_blocks_found) : (p.blocks_found ?? 0);
    const luckSince = Number(p.luck_since_ts) > 0 ? new Date(Number(p.luck_since_ts) * 1000).toLocaleDateString([], { month: "short", day: "numeric" }) : "";
    const luckSub = p.luck_percent == null
      ? "luck pending"
      : luck + " luck" + (Number.isFinite(expected) && expected > 0 ? " · " + luckFound + " / " + expected.toFixed(1) + " expected" + (luckSince && luckFound !== (p.blocks_found ?? 0) ? " since " + luckSince : "") : "");
    const shareSub = expPct((p.pool_share || 0) * 100) + "% of live hashrate"
      + (p.block_interval_seconds ? " · ~" + dur(p.block_interval_seconds) + " blocks" : "");
    const nblocks = Number(p.window_multiple) || 8;
    const fill = Number(p.window_fill_percent);
    const fillTxt = Number.isFinite(fill) ? fill.toFixed(0) + "%" : "\u2014";
    const inWindow = Number((pr.window || {}).identities) || 0;
    const rejPct = tot.shares_accepted + tot.shares_rejected > 0 ? (100 * tot.shares_rejected / (tot.shares_accepted + tot.shares_rejected)).toFixed(2) + "% rejected" : "no rejects";
    const gws = Number(pr.gateways_online) || 0;
    const remote = Number(pr.gateways_remote) || 0;
    const thsDatum = thsDay(p, fees.datum);
    const thsStratum = thsDay(p, fees.stratum);
    // Rigs (stratum sessions, plus one per own-gateway address) vs. distinct payout addresses.
    const workers = Number(p.workers_online) || Number(p.miners_online) || 0;
    const addrs = Number(p.miners_online) || 0;
    const hrSub = workers + " worker" + (workers === 1 ? "" : "s") + (addrs && addrs !== workers ? " · " + addrs + " address" + (addrs === 1 ? "" : "es") : "") + " online";
    // Lifetime accepted shares survive Prime restarts; the per-run count is what the reject
    // rate is measured on.
    const life = Number(tot.lifetime_shares) || 0;
    const run = Number(tot.shares_accepted) || 0;
    const sharesMain = life > run ? life : run;
    const sharesSub = !pr.reachable
      ? "Prime unreachable"
      : life > run
        ? "accepted, lifetime · " + num(run) + " this Prime run · " + rejPct
        : "verified by Prime since it started · " + rejPct;
    const cells = [
      ["1 TH/s yields (est.)", thsDatum != null ? amt(thsDatum) + "/day" + money(thsDatum) : "\u2014", "at current difficulty, base subsidy · DATUM " + feePct(fees.datum) + " · " + (thsStratum != null ? amt(thsStratum) + "/day" + money(thsStratum) : "\u2014") + " on stratum"],
      ["Hashrate", fmtHr(p.pool_hr_ghs), hrSub],
      ["Miners", String(inWindow || p.miners_online || 0), inWindow ? "holding work in the window · " + (p.miners_seen ?? "\u2014") + " ever" : (p.miners_seen ?? "\u2014") + " ever"],
      ["Shares", num(sharesMain), sharesSub],
      ["Window", fillTxt + " full", nblocks + " network-blocks of work"],
      ["Gateways", String(gws), gws ? (remote ? remote + " remote + our stratum" : "our public stratum") + " · Prime up " + dur(pr.uptime_s) : "none connected"],
      ["To block", dur(p.ttf_seconds), shareSub],
      ["Found", String(p.blocks_found ?? 0), luckSub],
      ["Network", net, "tip " + (p.height ? num(p.height) : "\u2014") + " · difficulty " + bigNum(p.difficulty)],
    ];
    $("stats").innerHTML = cells
      .map(([k, v, s]) => `<div><dt>${k}</dt><dd>${v}<small>${s}</small></dd></div>`)
      .join("");
    overflowNote(p.overflow);
    poolCards(p.overflow);

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
    if ($("live-ths")) $("live-ths").textContent = thsDatum != null ? "~" + amt(thsDatum) + "/day" + money(thsDatum) : "\u2014";
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
    $("payout-reward-sub").textContent = value ? "base subsidy at height " + (p.height ? num(Number(p.height) + 1) : "\u2014") + " · transaction fees add to every output" : "waiting for Prime";
    $("payout-miners-btc").textContent = value ? amtSats(minerSats) : "\u2014";
    $("payout-miners-sub").textContent = value ? `${pct(100 * minerSats / value, 1)} of the block · ${n} address${n === 1 ? "" : "es"} paid directly` : "\u2014";
    $("payout-pool-btc").textContent = value ? amtSats(poolSats) : "\u2014";
    const unplaced = Number(cb.unplaced_sats) || 0;
    const carryPaid = Number(cb.carry_paid_sats) || 0;
    $("payout-pool-sub").textContent = value
      ? `${pct(eff, 2)} blended fee` + (fees.datum !== fees.stratum ? ` (${feePct(fees.datum)} gateway · ${feePct(fees.stratum)} stratum)` : "") + (unplaced > 1000 ? ` + ${amtSats(unplaced)} carried forward` : "") + (carryPaid > 1000 ? ` − ${amtSats(carryPaid)} carry paid back` : "")
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
    if (o.fee_path === "datum") return { text: "own gateway", known: false };
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
        label: `${tail.length} smaller address${tail.length === 1 ? "" : "es"}`,
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
        note: "pool fee",
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
      svg.innerHTML = '<title id="donut-title">Share of the coinbase by payout address</title>';
      if (legend) legend.innerHTML = "";
      if (hole) hole.textContent = "\u2014";
      if (holeLabel) holeLabel.textContent = "no split yet";
      if (desc) desc.textContent = "Prime has not issued a split yet.";
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
        `<title>${esc(s.label)}${s.who ? " \u00b7 " + esc(s.who.text) : ""}${s.note ? " \u00b7 " + s.note : ""} \u2014 ${pct(s.percent, 2)} of the block, ${amtSats(s.sats)}</title>` +
        "</circle>";
      offset -= s.percent;
      return arc;
    });
    svg.innerHTML =
      '<title id="donut-title">Share of the coinbase by payout address</title>' +
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
    if (holeLabel) holeLabel.textContent = n === 1 ? "address to pay" : "addresses to pay";
    if (desc) {
      const named = slices.filter((s) => s.who && s.who.known).length;
      const total = Number(cb.miner_outputs) || 0;
      desc.textContent =
        `Each slice is one payout address's share of the whole coinbase, labelled by the last four characters of its address` +
        (named ? `; ${named} ${named === 1 ? "is a gateway that has" : "are gateways that have"} named ${named === 1 ? "itself" : "themselves"}` : "") +
        (total > DONUT_SLICES && !coinbaseExpanded ? `. The ${total - DONUT_SLICES} smallest are grouped; use “Show all” below to split them out` : "") +
        ".";
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
      rows.push(`<tr class="faint"><td class="num"></td><td>${rest.length} more miner output${rest.length === 1 ? "" : "s"}</td><td></td><td class="num">${restShares ? num(restShares) : "\u2014"}</td><td class="num">${pct(rest.reduce((a, o) => a + Number(o.share_percent || 0), 0))}</td><td class="num">${amtSats(restSats)}</td><td class="num">${value ? pct(100 * restSats / value, 2) : "\u2014"}</td></tr>`);
    }
    if (pool) {
      rows.push(`<tr class="pool-row"><td class="num faint">${miners.length + 1}</td><td>Lazarus <span class="faint">pool fee${cb.unplaced_sats > 1000 ? " + carried forward" : ""}${Number(cb.carry_paid_sats) > 1000 ? " − carry paid back" : ""}</span> · <a href="#${esc(pool.address)}">${short(pool.address)}</a></td><td><span class="pill">pool</span></td><td class="num">\u2014</td><td class="num">\u2014</td><td class="num" title="${amtExact(pool.sats / 1e8)}">${amtSats(pool.sats)}</td><td class="num faint">${value ? pct(100 * pool.sats / value, 2) : "\u2014"}</td></tr>`);
    }
    el.innerHTML =
      '<thead><tr><th class="num">#</th><th>Paid to</th><th>Path · fee</th><th class="num">Window shares</th><th class="num">Window %</th><th class="num">Output</th><th class="num">Of block</th></tr></thead><tbody>' +
      (rows.length ? rows.join("") : '<tr><td colspan="7" class="empty">Prime has not issued a split yet</td></tr>') +
      "</tbody>";
    const sum = $("coinbase-summary");
    if (sum) sum.textContent = cb.outputs ? `${cb.outputs} outputs · ${amtSats(cb.miner_sats)} to ${cb.miner_outputs} miner${cb.miner_outputs === 1 ? "" : "s"} · ${amtSats(cb.pool_sats)} to the pool` : "\u2014";
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
      const carryTotal = Number(cb.carry_total_sats) || 0;
      const carryPaid = Number(cb.carry_paid_sats) || 0;
      if (unpaid.length || carryTotal > 0 || carryPaid > 0) {
        const s = document.createElement("span");
        s.className = "unpaid-note faint";
        const parts = [];
        if (unpaid.length) parts.push(`${unpaid.length} address${unpaid.length === 1 ? "" : "es"} earn${unpaid.length === 1 ? "s" : ""} less than the minimum output this block; that share is carried forward, not forfeited, and rides on the first output that clears the floor.`);
        if (carryPaid > 0) parts.push(`${amtSats(carryPaid)} of carry from earlier blocks is included in these outputs.`);
        if (carryTotal > 0) parts.push(`The pool is holding ${amtSats(carryTotal)} of carry for ${num(cb.carry_holders || 0)} miner${cb.carry_holders === 1 ? "" : "s"}.`);
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
      const who = g.own
        ? `<span class="pill brass">Lazarus public stratum</span>`
        : (() => {
            const tag = String(g.secondary_tag || g.name || "").trim();
            const key = `<span class="mono" title="Gateway signing key (first 16 hex)">${esc(g.gateway || "")}</span>`;
            return tag ? `<div>${esc(tag)}</div><div class="faint">${key}</div>` : key;
          })();
      const ident = g.own ? '<span class="faint">miners’ own addresses</span>' : (g.identity ? `<a href="#${esc(g.identity)}">${short(g.identity)}</a>` : '<span class="faint">no share yet</span>');
      const hashing = !g.offline && g.last_share_s != null && g.last_share_s < 180 && g.accepted > 0;
      const state = g.offline
        ? `<div><span class="pill">offline</span></div><div class="faint">${num(g.block_candidates || 0)} block${(g.block_candidates || 0) === 1 ? "" : "s"} on record</div>`
        : `<div>${hashing ? '<span class="pill ok">hashing</span>' : g.accepted > 0 ? '<span class="pill warn">idle</span>' : '<span class="pill">connected</span>'}</div>`
          + (g.accepted > 0 && g.last_share_s != null ? `<div class="faint">last share ${agoS(g.last_share_s)}</div>` : '<div class="faint">no shares yet</div>');
      const shares = `<div>${num(g.accepted)}</div>${g.rejected ? `<div class="faint" title="${esc(g.last_reject || "")}">${num(g.rejected)} rejected</div>` : ""}`;
      return [
        who,
        state,
        clientLabel(g),
        pathPill(g.fee_path || (g.own ? "stratum" : "datum"), null, g.secondary_tag || g.name),
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
      const live = gws.filter((g) => !g.offline);
      const remote = live.filter((g) => !g.own);
      const historic = gws.filter((g) => g.offline);
      const active = remote.filter((g) => g.accepted > 0).length;
      const cells = [
        ["Connected", String(live.length), live.length ? `${remote.length} remote · ${live.length - remote.length} ours` + (historic.length ? ` · ${historic.length} offline with finds` : "") : "none"],
        ["Remote hashing", String(active), remote.length ? `of ${remote.length} remote gateway${remote.length === 1 ? "" : "s"} · ${remote.length - active} connected without shares` : "no remote gateways"],
        ["Splits issued", num(t.coinbasers), "coinbase lists handed out since Prime started"],
        ["Shares verified", num(t.shares_accepted), (t.shares_rejected ? num(t.shares_rejected) + " rejected · " : "") + "since Prime started " + dur(pr.uptime_s) + " ago"],
      ];
      tick.innerHTML = cells.map(([k, v, s]) => `<div><dt>${k}</dt><dd>${v}<small>${s}</small></dd></div>`).join("");
    }
    const note = $("gateway-note");
    if (note) {
      note.textContent = pr.reachable
        ? `A gateway shows “connected” until its miners send work; a stock gateway with no miners attached still requests splits. “Rejected” right after a Prime restart is a gateway still on the previous instance’s split — it clears with its next template. Blocks found is lifetime (survives Prime restarts). Since this start: ${num(t.connections)} connection${t.connections === 1 ? "" : "s"}. ${num(t.block_candidates)} block${t.block_candidates === 1 ? "" : "s"} found in total.`
        : "Prime is unreachable; this is the last list it published.";
    }
  }

  // ------------------------------------------------------------------ solo
  // Solo is its own book. Nothing here is a share of anything: a solo miner is paid only
  // by the block it finds, so the tables show proven work and blocks, never "owed".
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
      ["Port", "For", "Address", "Fee", "Starting difficulty", "State"],
      (s.endpoints || []).map((e) => [
        esc(e.name || ""),
        e.name === "GPU" ? "GPUs and CPUs" : "ASICs",
        `<span class="mono">${esc(e.host || "")}:${esc(String(e.port || ""))}</span>`,
        pctFee(e.fee_percent),
        e.vardiff && e.vardiff.start ? num(e.vardiff.start) : "\u2014",
        e.online ? '<span class="pill ok">up</span>' : '<span class="pill">down</span>',
      ]),
      [null, null, null, "num", "num", null],
      "No solo endpoint is up"
    );

    const rows = (s.miners || []).map((m) => [
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
      ["Address", "Via", "Hashrate", "Workers", "Shares", "Work", "Best share", "Blocks"],
      rows,
      [null, null, "num", "num", "num", "num", "num", "num"],
      "Nobody is mining solo right now"
    );

    table(
      $("soloblocks"),
      ["Height", "Found by", "Reward", "Kept by finder", "Our fee", "Time"],
      (s.blocks || []).map((b) => [
        String(b.height),
        b.finder ? `<a href="#${esc(b.finder)}">${short(b.finder)}</a>` : '<span class="faint">unknown</span>',
        amt(b.reward_btc),
        amt(b.miner_btc),
        amt(b.pool_fee_btc),
        when(b.ts),
      ]),
      ["num", null, "num", "num", "num", null],
      "No solo block yet"
    );

    const tick = $("solo-ticker");
    if (tick) {
      const up = (s.endpoints || []).filter((e) => e.online).length;
      const cells = [
        ["Solo hashrate", fmtHr(s.hashrate_ghs), s.miner_count ? `${s.miner_count} address${s.miner_count === 1 ? "" : "es"} chasing a whole block` : "nobody mining solo"],
        ["Blocks found solo", String(s.blocks_found || 0), "each one paid its finder in full, less the fee"],
        ["Fee", pctFee(fee), "taken as one output in the block you solve"],
        ["Ports", String(up), up ? "our node builds the templates" : "no solo port is up"],
      ];
      tick.innerHTML = cells.map(([k, v, sub]) => `<div><dt>${k}</dt><dd>${v}<small>${sub}</small></dd></div>`).join("");
    }
    const note = $("solo-note");
    if (note) {
      note.textContent = s.miner_count
        ? "Work here buys no share of any pooled block, and a pooled miner is owed nothing from a block found solo. A solo miner with no block yet has earned nothing — that is what solo means."
        : "No solo miners connected. Work sent to a solo port is kept entirely out of the TIDES window.";
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
        : '<tr><td colspan="' + headers.length + '" class="empty">' + (empty || "None yet") + "</td></tr>") +
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
      const b = byHeight.get(key) || { height: r.height, hash: r.hash, ts: r.ts, outputs: [], miner_btc: 0, pool_btc: 0, status: r.status, kind: r.kind, block_status: r.block_status, owed_sats: r.owed_sats, owed_txid: r.owed_txid, owed_resolved: r.owed_resolved, found_by: r.found_by, reward: r.reward_btc };
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
    const headers = ["Height", "Block", "Coinbase", "Outputs", "Miners paid", "Pool", "Status", "Found by", "Time"];
    const rows = blocks.map((b, i) => {
      const poolBtc = b.pool_btc || (b.reward ? Math.max(0, b.reward - b.miner_btc) : null);
      const kind = b.kind || (b.outputs.length > 1 ? "split" : b.outputs.length === 1 ? "" : "");
      const st = b.prime_only ? b.block_status : (b.status === "unsplit" ? "pool only" : b.status);
      const outs = b.outputs.slice().sort((x, y) => y.btc - x.btc);
      const detail = outs.map((o) => `<tr><td></td><td colspan="2"><a href="#${esc(o.address)}">${esc(o.address)}</a>${o.pool ? ' <span class="pill brass">pool</span>' : ""}</td><td class="num" title="${amtExact(o.btc)}">${amt(o.btc)}</td><td class="num faint">${b.reward ? pct(100 * o.btc / b.reward, 2) : ""}</td><td colspan="4"></td></tr>`).join("");
      const owed = b.owed_txid
        ? `<div class="faint">window paid${b.owed_sats ? " " + amtSats(b.owed_sats) : ""} · <a href="${esc(p.explorer)}/tx/${esc(b.owed_txid)}" target="_blank" rel="noreferrer" title="Transaction that paid the window">tx ${shortHash(b.owed_txid)}</a></div>`
        : (b.owed_sats ? `<div class="faint">owed to window ${amtSats(b.owed_sats)}</div>` : "");
      return `<tr class="block-row" data-i="${i}" tabindex="0" aria-expanded="false">
        <td class="num"><span class="disclose"></span>${b.height ?? "\u2014"}</td>
        <td>${b.hash ? `<a href="${esc(p.explorer)}/block/${esc(b.hash)}" target="_blank" rel="noreferrer" title="Open in the Lazarus Mempool">${shortHash(b.hash)}</a>` : "\u2014"}</td>
        <td>${kindPill(kind)}${owed}</td>
        <td class="num">${b.outputs.length}</td>
        <td class="num" title="${amtExact(b.miner_btc)}">${amt(b.miner_btc)}</td>
        <td class="num" title="${poolBtc == null ? "" : amtExact(poolBtc)}">${poolBtc == null ? "\u2014" : amt(poolBtc)}</td>
        <td>${statusPill(st)}</td>
        <td>${b.found_by ? `<a href="#${esc(b.found_by)}">${short(b.found_by)}</a>` : "\u2014"}</td>
        <td>${when(b.ts)}</td>
      </tr>
      <tr class="block-detail" hidden><td colspan="9"><table class="inner"><thead><tr><th></th><th colspan="2">Coinbase output</th><th class="num">Amount</th><th class="num">Of block</th><th colspan="4"></th></tr></thead><tbody>${detail || '<tr><td colspan="9" class="empty">No outputs recorded</td></tr>'}</tbody></table></td></tr>`;
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
          title: (b.height ? "Block " + num(b.height) : "Block") + " · " + clock(b.ts),
          sub: amt(b.miner_btc) + " to you" + (payStatus(b) === "immature" ? " · immature" : ""),
        }))
    );
    chartLegend($("chart-legend"), $("chart"), "block paid you", "blocks paid you");
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

  // --------------------------------------------------------------- refresh
  let refreshBusy = false;
  async function refresh() {
    if (refreshBusy) return;
    refreshBusy = true;
    const poolP = j("/api/pool");
    const cbP = j("/api/coinbaser").catch(() => ({}));
    const pxP = j("/api/price").catch(() => null);
    const soP = j("/api/solo").catch(() => null);
    const minersP = j("/api/miners").catch(() => ({ online: [], seen: [] }));
    const blocksP = j("/api/blocks").catch(() => ({ blocks: [] }));
    const paysP = j("/api/payouts").catch(() => ({ payouts: [], prime_blocks: [] }));
    try {
      const [p, cb, px, so] = await Promise.all([poolP, cbP, pxP, soP]);
      if (px && Number.isFinite(Number(px.USD))) priceUsd = Number(px.USD);
      if (Number(p.block_interval_seconds) > 0) blockInterval = Number(p.block_interval_seconds);
      stats(p);
      payoutHero(p, cb || {});
      coinbase(cb || {});
      gateways(p.prime || {});
      solo(so);
      draw($("poolchart"), p.history || [], "hr_ghs", $("poolchart")?.__marks || []);
      chartLegend($("poolchart-legend"), $("poolchart"), "block found", "blocks found");

      const miners = await minersP;
      const online = (miners.online || []).filter((m) => m.address);
      const firstOf = (m, i) => online.findIndex((x) => x.address === m.address) === i;
      table(
        $("online"),
        ["Address", "Worker", "Path · fee", "Hashrate", "Session", "Accepted", "Window shares", "Window %", "Next block", "Last share"],
        online.map((m, i) => [
          `<a href="#${esc(m.address)}">${short(m.address)}</a>`,
          m.worker === "window" ? '<span class="faint">via gateway</span>' : (Number(m.sessions) > 1 ? `${num(m.sessions)} workers` : esc(m.worker || "\u2014")),
          pathPill(m.fee_path, m.via, m.gateway_name),
          fmtHr(firstOf(m, i) ? (m.credited_hr_ghs || m.hr_ghs) : m.hr_ghs),
          sessCell(m.via, m.shares_session),
          num(m.shares_lifetime ?? m.shares_acc ?? m.window_work),
          winShareCell(m, firstOf(m, i)),
          firstOf(m, i) && m.window_percent != null ? pct(m.window_percent) : "\u2014",
          firstOf(m, i) && m.window_sats ? amtSats(m.window_sats) : "\u2014",
          Number.isFinite(Number(m.last_share_s)) ? Number(m.last_share_s).toFixed(0) + "s" : "\u2014",
        ]),
        [null, null, null, "num", "num", "num", "num", "num", "num", "num"],
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
        ["Address", "Path · fee", "Accepted", "Window shares", "Window %", "Next block", "Last seen"],
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
        "Every address we have seen is hashing right now"
      );
      relayedTable(miners.relayed, miners.overflow);
      // Rows are one per address; each carries how many sessions (rigs) it rolled up.
      const onlineWorkers = online.reduce((a, m) => a + (Number(m.sessions) || 1), 0);
      if ($("online-count")) $("online-count").textContent = online.length ? `${onlineAddrs.size} address${onlineAddrs.size === 1 ? "" : "es"} · ${onlineWorkers} worker${onlineWorkers === 1 ? "" : "s"}` : "";
      if ($("seen-count")) {
        const holding = seenAll.filter((m) => m.window_sats > 0).length;
        $("seen-count").textContent = seenAll.length
          ? `${seenAll.length > SEEN_ROWS ? `${SEEN_ROWS} of ` : ""}${seenAll.length} address${seenAll.length === 1 ? "" : "es"}${holding ? ` · ${holding} still in the window` : ""}`
          : "";
      }

      // Keep the looked-up card current too (same panel stays open).
      if (minerAddr) showMiner(minerAddr).catch(() => {});
      const [blocks, pays] = await Promise.all([blocksP, paysP]);
      const marks = blockMarks(foundBlocks(pays, p));
      draw($("poolchart"), p.history || [], "hr_ghs", marks);
      chartLegend($("poolchart-legend"), $("poolchart"), "block found", "blocks found");
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
    } finally {
      refreshBusy = false;
    }
  }

  // Section anchors never trigger an address lookup. "mine" and "datum" are
  // retired anchors kept so old bookmarks still land somewhere sensible.
  const SECTIONS = new Set([
    "", "top", "fees", "status", "payout", "window", "connect", "gw-pick", "dashboard", "miners", "gateways",
    "blocks", "payouts", "pools", "how", "datum", "mine", "solo",
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
  if (!minerAddr) {
    try {
      const last = localStorage.getItem("lz.addr");
      if (last && !SECTIONS.has(last)) navPayouts(last);
    } catch (e) { /* private mode */ }
  }
  refresh().catch((e) => console.error(e));
  setInterval(() => refresh().catch((e) => console.error(e)), 10000);
  window.addEventListener("resize", () => {
    for (const [id, legend, one, many] of [
      ["poolchart", "poolchart-legend", "block found", "blocks found"],
      ["chart", "chart-legend", "block paid you", "blocks paid you"],
    ]) {
      const c = $(id);
      if (!c || !c.__hist) continue;
      draw(c, c.__hist, "hr_ghs", c.__marks);
      chartLegend($(legend), c, one, many);
    }
  });

  // ---------------------------------------------------------------- chrome
  // Top bar gets a hairline once the page has scrolled; the nav highlights the section in
  // view; sections rise in as they enter. All decorative — nothing here touches data — and
  // all of it degrades to nothing when the browser lacks IntersectionObserver.
  (() => {
    const header = document.querySelector(".site-header");
    if (header) {
      let raf = 0;
      const onScroll = () => {
        if (raf) return;
        raf = requestAnimationFrame(() => {
          raf = 0;
          header.toggleAttribute("data-scrolled", window.scrollY > 8);
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
