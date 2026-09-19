// Pool hashrate explorer: seven time ranges, every block the pool found marked on the timeline,
// hover for the reading at any moment, click a block for its payout split.
// Depends on window.LZ (shared.js) for formatting and window.LZ_I18N for strings. No libraries.
(function () {
  "use strict";
  const LZ = window.LZ;
  if (!LZ) return;
  const t = (k, v) => LZ.t("app.chart." + k, v);
  const RANGES = ["1h", "6h", "24h", "3d", "7d", "30d", "all"];
  const REFRESH_S = { "1h": 15, "6h": 30, "24h": 30, "3d": 120, "7d": 120, "30d": 300, all: 300 };
  const LANE_H = 30; // block lane under the plot
  const PAD = { l: 8, r: 78, t: 14, b: 22 };
  const CLUSTER_PX = 15;

  const css = (name) => getComputedStyle(document.documentElement).getPropertyValue(name).trim();
  // Canvas wants plain colours with alpha; the palette is oklch custom properties. Paint one pixel
  // with the property's value and read it back as rgb, once per colour.
  const _rgb = {};
  let _probe = null;
  function rgba(name, a) {
    if (!_rgb[name]) {
      if (!_probe) { const c = document.createElement("canvas"); c.width = c.height = 1; _probe = c.getContext("2d", { willReadFrequently: true }); }
      _probe.clearRect(0, 0, 1, 1); _probe.fillStyle = "#000"; _probe.fillStyle = css(name) || "#dbb565"; _probe.fillRect(0, 0, 1, 1);
      const d = _probe.getImageData(0, 0, 1, 1).data; _rgb[name] = [d[0], d[1], d[2]];
    }
    const c = _rgb[name];
    return `rgba(${c[0]},${c[1]},${c[2]},${a})`;
  }
  const el = (tag, cls, html) => {
    const n = document.createElement(tag);
    if (cls) n.className = cls;
    if (html != null) n.innerHTML = html;
    return n;
  };

  function niceMax(v) {
    if (!(v > 0)) return 1;
    const p = Math.pow(10, Math.floor(Math.log10(v)));
    for (const m of [1, 1.2, 1.5, 2, 2.5, 3, 4, 5, 6, 8, 10]) if (m * p >= v) return m * p;
    return 10 * p;
  }

  class HashChart {
    // mode "pool": reads /api/history. mode "miner": is handed one address's day of samples and
    // the blocks that paid it (setData), and cuts the shorter ranges out of that day itself.
    constructor(root, mode) {
      this.root = root;
      this.mode = mode === "miner" ? "miner" : "pool";
      this.ranges = this.mode === "miner" ? ["1h", "6h", "24h"] : RANGES;
      this.storeKey = "lz.chart.range." + this.mode;
      this.range = "24h";
      try {
        const saved = localStorage.getItem(this.storeKey);
        if (this.ranges.includes(saved)) this.range = saved;
      } catch (e) { /* private mode */ }
      this.data = null;
      this.cache = {};
      this.hover = null; // {i} point index or {c} cluster
      this.pinned = null;
      this.showMiners = false;
      this.build();
      this.observe();
      if (this.mode === "pool") this.load();
    }

    // Strings: a miner chart uses app.chart.m.* where one exists, and the pool's wording otherwise.
    t(k, v) {
      // data-neutral: the standalone /miner/<address> page, where the address may be anyone's.
      if (this.mode === "miner" && this.root.dataset.neutral != null && window.LZ_I18N && window.LZ_I18N.raw("app.chart.n." + k) != null) return LZ.t("app.chart.n." + k, v);
      if (this.mode === "miner" && window.LZ_I18N && window.LZ_I18N.raw("app.chart.m." + k) != null) return LZ.t("app.chart.m." + k, v);
      return LZ.t("app.chart." + k, v);
    }

    // Miner mode: `m` is the /api/miner/<address> document. Called on every refresh.
    setData(m) {
      this.address = m.address || this.address || "";
      this.raw = {
        history: (m.history || []).map((h) => [Number(h.ts), Number(h.hr_ghs) || 0, 0]),
        blocks: (m.blocks_found || []).filter((b) => Number(b.ts) > 0).map((b) => ({
          height: b.height, hash: b.hash, ts: Number(b.ts), reward_btc: Number(b.miner_btc) || 0, status: b.status || "", kind: "", gateway: "",
        })).sort((a, b) => a.ts - b.ts),
      };
      const first = !this.data;
      this.slice();
      if (first) this.animateIn(); else this.draw();
    }
    slice() {
      if (!this.raw) return;
      const span = { "1h": 3600, "6h": 21600, "24h": 86400 }[this.range], step = { "1h": 60, "6h": 120, "24h": 300 }[this.range];
      const until = Math.floor(Date.now() / 1000), since = until - span, buckets = new Map();
      for (const [ts, hr] of this.raw.history) {
        if (ts < since) continue;
        const k = Math.floor(ts / step) * step, b = buckets.get(k) || [0, 0];
        b[0] += hr; b[1]++;
        buckets.set(k, b);
      }
      const points = [...buckets.entries()].sort((a, b) => a[0] - b[0]).map(([ts, b]) => [ts, b[0] / b[1], 0]);
      this.data = { range: this.range, since, until, step_s: step, points, blocks: this.raw.blocks.filter((b) => b.ts >= (points.length ? points[0][0] : since)) };
      this.prepare();
      this.renderStats();
    }

    build() {
      const r = this.root;
      r.classList.add("hx");
      r.innerHTML = "";
      const head = el("div", "hx-head");
      this.titleEl = el("h3", "hx-title");
      const tools = el("div", "hx-tools");
      this.pills = el("div", "hx-ranges");
      this.pills.setAttribute("role", "group");
      for (const k of this.ranges) {
        const b = el("button", "hx-range");
        b.type = "button";
        b.dataset.range = k;
        b.addEventListener("click", () => this.setRange(k));
        this.pills.appendChild(b);
      }
      this.minersBtn = el("button", "hx-toggle");
      this.minersBtn.type = "button";
      this.minersBtn.addEventListener("click", () => {
        this.showMiners = !this.showMiners;
        this.minersBtn.setAttribute("aria-pressed", String(this.showMiners));
        this.draw();
      });
      tools.append(this.pills);
      if (this.mode === "pool") tools.append(this.minersBtn);
      head.append(this.titleEl, tools);

      this.statsEl = el("dl", "hx-stats");
      const stage = el("div", "hx-stage");
      this.canvas = el("canvas", "hx-canvas");
      this.canvas.tabIndex = 0;
      this.canvas.setAttribute("role", "img");
      this.tip = el("div", "hx-tip");
      this.tip.hidden = true;
      this.pop = el("div", "hx-pop");
      this.pop.hidden = true;
      this.pop.setAttribute("role", "dialog");
      this.empty = el("p", "hx-empty");
      this.empty.hidden = true;
      stage.append(this.canvas, this.tip, this.pop, this.empty);
      this.hint = el("p", "hx-hint");
      r.append(head, this.statsEl, stage, this.hint);
      this.stage = stage;
      this.ctx = this.canvas.getContext("2d");

      this.canvas.addEventListener("pointermove", (e) => this.onMove(e));
      this.canvas.addEventListener("pointerleave", () => { this.hover = null; this.tip.hidden = true; this.draw(); });
      this.canvas.addEventListener("click", (e) => this.onClick(e));
      this.canvas.addEventListener("keydown", (e) => this.onKey(e));
      document.addEventListener("pointerdown", (e) => {
        if (!this.pop.hidden && !this.pop.contains(e.target) && e.target !== this.canvas) this.closePop();
      });
      document.addEventListener("keydown", (e) => { if (e.key === "Escape") this.closePop(); });
      document.addEventListener("lz:i18n", () => { this.labels(); this.renderStats(); this.draw(); });
      this.labels();
    }

    labels() {
      this.titleEl.textContent = this.t("title");
      this.pills.setAttribute("aria-label", this.t("rangeAria"));
      for (const b of this.pills.children) {
        b.textContent = this.t("range." + b.dataset.range);
        b.setAttribute("aria-pressed", String(b.dataset.range === this.range));
        b.disabled = !!this.legacy && b.dataset.range !== "24h";
        b.title = b.disabled ? this.t("rangeSoon") : "";
      }
      this.minersBtn.textContent = this.t("miners");
      this.minersBtn.setAttribute("aria-pressed", String(this.showMiners));
      this.hint.textContent = this.t("hint");
      this.empty.textContent = this.t("empty");
    }

    observe() {
      if ("ResizeObserver" in window) new ResizeObserver(() => this.draw()).observe(this.stage);
      else window.addEventListener("resize", () => this.draw());
      this.visible = true;
      if ("IntersectionObserver" in window) {
        new IntersectionObserver((es) => {
          this.visible = es[0].isIntersecting;
          if (this.visible) this.maybeRefresh();
        }).observe(this.root);
      }
      document.addEventListener("visibilitychange", () => this.maybeRefresh());
      setInterval(() => this.maybeRefresh(), 5000);
    }

    maybeRefresh() {
      if (this.mode !== "pool" || document.hidden || !this.visible || this.loading) return;
      const c = this.cache[this.range];
      if (!c || Date.now() - c.at > REFRESH_S[this.range] * 1000) this.load();
    }

    setRange(k) {
      if (k === this.range) return;
      this.range = k;
      try { localStorage.setItem(this.storeKey, k); } catch (e) { /* private mode */ }
      this.closePop();
      this.hover = null;
      this.tip.hidden = true;
      this.labels();
      if (this.mode === "miner") { this.slice(); this.animateIn(); return; }
      const c = this.cache[k];
      if (c) { this.data = c.data; this.prepare(); this.renderStats(); this.animateIn(); }
      this.load();
    }

    async load() {
      const k = this.range;
      this.loading = true;
      this.root.classList.add("is-loading");
      try {
        let data;
        if (this.legacy) data = await this.legacyData();
        else {
          const r = await fetch("/api/history?range=" + k);
          if (r.status === 404) {
            // A node that predates /api/history: draw the day it does publish, from the endpoints
            // it has, and leave the other ranges off until it is updated.
            this.legacy = true;
            this.range = "24h";
            this.labels();
            data = await this.legacyData();
          } else {
            if (!r.ok) throw new Error("HTTP " + r.status);
            data = await r.json();
          }
        }
        this.cache[this.range] = { at: Date.now(), data };
        if (!this.legacy && k !== this.range) return;
        const first = !this.data;
        this.data = data;
        this.prepare();
        this.renderStats();
        if (first) this.animateIn(); else this.draw();
      } catch (e) {
        if (!this.data) { this.empty.hidden = false; this.empty.textContent = this.t("error"); }
      } finally {
        this.loading = false;
        this.root.classList.remove("is-loading");
      }
    }

    async legacyData() {
      const [pool, pays] = await Promise.all([
        fetch("/api/pool").then((r) => r.json()),
        fetch("/api/payouts").then((r) => r.json()).catch(() => ({})),
      ]);
      const step = 300, buckets = new Map();
      for (const h of pool.history || []) {
        const k = Math.floor(h.ts / step) * step, b = buckets.get(k) || [0, 0, 0];
        b[0] += h.hr_ghs; b[1] += h.miners || 0; b[2]++;
        buckets.set(k, b);
      }
      const points = [...buckets.entries()].sort((a, b) => a[0] - b[0]).map(([ts, b]) => [ts, b[0] / b[2], Math.round(b[1] / b[2])]);
      const since = points.length ? points[0][0] : 0, seen = new Set(), blocks = [];
      for (const row of pays.payouts || []) {
        if (!row.hash || seen.has(row.hash) || !(row.ts >= since)) continue;
        seen.add(row.hash);
        blocks.push({ height: row.height, hash: row.hash, ts: row.ts, reward_btc: row.reward_btc || 0, kind: row.kind || "", gateway: row.gateway || "", status: row.block_status || "" });
      }
      blocks.sort((a, b) => a.height - b.height);
      return { range: "24h", since, until: Math.floor(Date.now() / 1000), step_s: step, points, blocks };
    }

    prepare() {
      const d = this.data;
      const pts = (d.points || []).filter((p) => p[1] > 0);
      this.pts = pts;
      this.blocks = d.blocks || [];
      this.t0 = pts.length ? pts[0][0] : d.since;
      this.t1 = Math.max(d.until || 0, pts.length ? pts[pts.length - 1][0] : 0);
      if (this.t1 <= this.t0) this.t1 = this.t0 + 1;
      let max = 0, sum = 0, maxM = 0, peakI = 0;
      pts.forEach((p, i) => { if (p[1] > max) { max = p[1]; peakI = i; } sum += p[1]; if (p[2] > maxM) maxM = p[2]; });
      this.yMax = niceMax(max * 1.08);
      this.mMax = niceMax(maxM * 1.15);
      this.summary = {
        now: pts.length ? pts[pts.length - 1][1] : 0,
        avg: pts.length ? sum / pts.length : 0,
        peak: max,
        peakTs: pts.length ? pts[peakI][0] : 0,
        blocks: this.blocks.length,
        perDay: this.blocks.length / Math.max((this.t1 - this.t0) / 86400, 1 / 24),
        reward: this.blocks.reduce((a, b) => a + (b.reward_btc || 0), 0),
      };
      this.empty.hidden = pts.length > 1;
      if (pts.length <= 1) this.empty.textContent = this.t("empty");
    }

    renderStats() {
      if (!this.summary) return;
      const s = this.summary;
      const cell = (k, v, sub) => `<div><dt>${LZ.esc(this.t(k))}</dt><dd>${v}${sub ? `<small>${sub}</small>` : ""}</dd></div>`;
      this.statsEl.innerHTML =
        cell("now", LZ.fmtHr(s.now)) +
        cell("avg", LZ.fmtHr(s.avg)) +
        cell("peak", LZ.fmtHr(s.peak), s.peakTs ? LZ.esc(this.fmtTime(s.peakTs, true)) : "") +
        cell("blocks", LZ.num(s.blocks), LZ.esc(this.t("perDay", { n: s.perDay >= 10 ? Math.round(s.perDay) : s.perDay.toFixed(1) }))) +
        cell("reward", LZ.amt(s.reward));
      this.canvas.setAttribute("aria-label", this.t("aria", {
        range: this.t("range." + this.range), now: LZ.fmtHr(s.now), avg: LZ.fmtHr(s.avg), peak: LZ.fmtHr(s.peak), blocks: s.blocks,
      }));
    }

    animateIn() {
      if (matchMedia("(prefers-reduced-motion: reduce)").matches) { this.reveal = 1; this.draw(); return; }
      const start = performance.now();
      const step = (now) => {
        const p = Math.min(1, (now - start) / 520);
        this.reveal = 1 - Math.pow(1 - p, 3);
        this.draw();
        if (p < 1) this.raf = requestAnimationFrame(step);
      };
      cancelAnimationFrame(this.raf);
      this.raf = requestAnimationFrame(step);
    }

    // ---- geometry
    size() {
      const w = Math.max(280, this.stage.clientWidth);
      const h = w < 560 ? 250 : 330;
      const padR = w < 560 ? 60 : PAD.r;
      return { w, h, pw: w - PAD.l - padR, ph: h - PAD.t - PAD.b - LANE_H, top: PAD.t, left: PAD.l };
    }
    x(ts, g) { return g.left + ((ts - this.t0) / (this.t1 - this.t0)) * g.pw; }
    y(v, g) { return g.top + g.ph - (v / this.yMax) * g.ph; }

    clusters(g) {
      const out = [];
      for (const b of this.blocks) {
        const bx = this.x(b.ts, g);
        if (bx < g.left - 1 || bx > g.left + g.pw + 1) continue;
        const last = out[out.length - 1];
        if (last && bx - last.x1 <= CLUSTER_PX) { last.blocks.push(b); last.x1 = bx; last.x = (last.x0 + last.x1) / 2; }
        else out.push({ x: bx, x0: bx, x1: bx, blocks: [b] });
      }
      return out;
    }

    fmtTime(ts, withDay) {
      const long = this.t1 - this.t0 > 36 * 3600;
      const loc = window.LZ_I18N ? window.LZ_I18N.locale() : undefined;
      const o = long || withDay
        ? { month: "short", day: "numeric", hour: "numeric", minute: "2-digit" }
        : { hour: "numeric", minute: "2-digit" };
      if (this.t1 - this.t0 > 20 * 86400 && !withDay) { delete o.hour; delete o.minute; }
      return new Date(ts * 1000).toLocaleString(loc, o);
    }

    // ---- drawing
    draw() {
      if (!this.pts) return;
      const g = (this.g = this.size());
      const dpr = Math.min(window.devicePixelRatio || 1, 2.5);
      const c = this.canvas, ctx = this.ctx;
      if (c.width !== Math.round(g.w * dpr) || c.height !== Math.round(g.h * dpr)) {
        c.width = Math.round(g.w * dpr); c.height = Math.round(g.h * dpr);
        c.style.width = g.w + "px"; c.style.height = g.h + "px";
      }
      ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
      ctx.clearRect(0, 0, g.w, g.h);
      const brass = rgba("--brass", 1), line = rgba("--line", 1), faint = rgba("--faint", 1), muted = rgba("--muted", 1), ink = rgba("--ink", 1), good = rgba("--good", 1);
      const mono = "11px " + (css("--mono") || "monospace");
      const pts = this.pts;
      const reveal = this.reveal == null ? 1 : this.reveal;

      // grid + y labels
      ctx.font = mono; ctx.textBaseline = "middle"; ctx.textAlign = "left";
      for (let i = 0; i <= 4; i++) {
        const v = (this.yMax / 4) * i, yy = Math.round(this.y(v, g)) + 0.5;
        ctx.strokeStyle = line; ctx.globalAlpha = i === 0 ? 1 : 0.55; ctx.lineWidth = 1;
        ctx.beginPath(); ctx.moveTo(g.left, yy); ctx.lineTo(g.left + g.pw, yy); ctx.stroke();
        ctx.globalAlpha = 1; ctx.fillStyle = faint;
        if (i > 0) ctx.fillText(LZ.fmtHr(v).replace(/\.0+(?=\s)/, ""), g.left + g.pw + 8, yy);
      }
      // x labels
      const nTicks = g.w < 560 ? 2 : 6;
      ctx.textBaseline = "top";
      for (let i = 0; i <= nTicks; i++) {
        const ts = this.t0 + ((this.t1 - this.t0) * i) / nTicks;
        const xx = this.x(ts, g);
        ctx.textAlign = i === 0 ? "left" : i === nTicks ? "right" : "center";
        ctx.fillStyle = faint;
        ctx.fillText(this.fmtTime(ts), xx, g.top + g.ph + LANE_H + 6);
      }

      if (pts.length > 1) {
        const gapS = (this.data.step_s || 60) * 3.5;
        const clipW = g.pw * reveal;
        ctx.save();
        ctx.beginPath(); ctx.rect(g.left, 0, clipW, g.h); ctx.clip();
        // area segments, broken where the pool has no samples
        const segs = [];
        let cur = [];
        pts.forEach((p, i) => {
          if (i && p[0] - pts[i - 1][0] > gapS) { segs.push(cur); cur = []; }
          cur.push(p);
        });
        segs.push(cur);
        const grad = ctx.createLinearGradient(0, g.top, 0, g.top + g.ph);
        grad.addColorStop(0, rgba("--brass", 0.34));
        grad.addColorStop(1, rgba("--brass", 0.02));
        for (const s of segs) {
          if (s.length < 2) continue;
          const path = new Path2D();
          s.forEach((p, i) => {
            const px = this.x(p[0], g), py = this.y(p[1], g);
            if (!i) path.moveTo(px, py);
            else {
              const q = s[i - 1], qx = this.x(q[0], g), qy = this.y(q[1], g), mx = (qx + px) / 2;
              path.bezierCurveTo(mx, qy, mx, py, px, py);
            }
          });
          const area = new Path2D(path);
          area.lineTo(this.x(s[s.length - 1][0], g), g.top + g.ph);
          area.lineTo(this.x(s[0][0], g), g.top + g.ph);
          area.closePath();
          ctx.fillStyle = grad; ctx.fill(area);
          ctx.strokeStyle = brass; ctx.lineWidth = 1.8; ctx.lineJoin = "round"; ctx.stroke(path);
        }
        if (this.showMiners && this.mMax > 0) {
          ctx.beginPath();
          pts.forEach((p, i) => {
            const px = this.x(p[0], g), py = g.top + g.ph - (p[2] / this.mMax) * g.ph;
            if (!i) ctx.moveTo(px, py); else ctx.lineTo(px, py);
          });
          ctx.setLineDash([4, 4]); ctx.strokeStyle = muted; ctx.lineWidth = 1.2; ctx.stroke(); ctx.setLineDash([]);
        }
        ctx.restore();
        // live dot on the newest sample
        if (reveal >= 1) {
          const lp = pts[pts.length - 1], lx = this.x(lp[0], g), ly = this.y(lp[1], g);
          ctx.fillStyle = rgba("--good", 0.3);
          ctx.beginPath(); ctx.arc(lx, ly, 7, 0, 7); ctx.fill();
          ctx.fillStyle = good; ctx.beginPath(); ctx.arc(lx, ly, 3, 0, 7); ctx.fill();
        }
      }

      // block lane
      const laneY = g.top + g.ph + LANE_H / 2 + 2;
      ctx.strokeStyle = line; ctx.lineWidth = 1;
      ctx.beginPath(); ctx.moveTo(g.left, Math.round(laneY) + 0.5); ctx.lineTo(g.left + g.pw, Math.round(laneY) + 0.5); ctx.stroke();
      this.cl = this.clusters(g);
      const active = this.pinned || (this.hover && this.hover.c);
      for (const c of this.cl) {
        if (c.x > g.left + g.pw * reveal) continue;
        const on = active && active.blocks[0].height === c.blocks[0].height;
        if (on) {
          ctx.strokeStyle = rgba("--brass", 0.55); ctx.setLineDash([2, 3]);
          ctx.beginPath(); ctx.moveTo(c.x, g.top); ctx.lineTo(c.x, laneY - 8); ctx.stroke(); ctx.setLineDash([]);
        }
        const n = c.blocks.length;
        if (n === 1) {
          const r = on ? 6.5 : 5;
          ctx.beginPath(); ctx.moveTo(c.x, laneY - r); ctx.lineTo(c.x + r, laneY); ctx.lineTo(c.x, laneY + r); ctx.lineTo(c.x - r, laneY); ctx.closePath();
          ctx.fillStyle = on ? ink : brass; ctx.fill();
        } else {
          const label = String(n), wv = Math.max(18, ctx.measureText(label).width + 10), hv = 16;
          ctx.beginPath();
          if (ctx.roundRect) ctx.roundRect(c.x - wv / 2, laneY - hv / 2, wv, hv, 8); else ctx.rect(c.x - wv / 2, laneY - hv / 2, wv, hv);
          ctx.fillStyle = on ? ink : brass; ctx.fill();
          ctx.fillStyle = rgba("--brass-ink", 1); ctx.textAlign = "center"; ctx.textBaseline = "middle"; ctx.font = "600 " + mono;
          ctx.fillText(label, c.x, laneY + 0.5); ctx.font = mono;
        }
      }

      // crosshair
      if (this.hover && this.hover.i != null && pts[this.hover.i]) {
        const p = pts[this.hover.i], px = this.x(p[0], g), py = this.y(p[1], g);
        ctx.strokeStyle = rgba("--ink", 0.35); ctx.lineWidth = 1;
        ctx.beginPath(); ctx.moveTo(Math.round(px) + 0.5, g.top); ctx.lineTo(Math.round(px) + 0.5, g.top + g.ph); ctx.stroke();
        ctx.fillStyle = rgba("--bg", 1); ctx.beginPath(); ctx.arc(px, py, 5.5, 0, 7); ctx.fill();
        ctx.fillStyle = brass; ctx.beginPath(); ctx.arc(px, py, 3.5, 0, 7); ctx.fill();
      }
    }

    // ---- interaction
    locate(e) {
      const r = this.canvas.getBoundingClientRect();
      return { x: e.clientX - r.left, y: e.clientY - r.top };
    }
    nearestPoint(px) {
      const g = this.g, pts = this.pts;
      if (!pts.length) return null;
      const ts = this.t0 + ((px - g.left) / g.pw) * (this.t1 - this.t0);
      let lo = 0, hi = pts.length - 1;
      while (hi - lo > 1) { const m = (lo + hi) >> 1; if (pts[m][0] < ts) lo = m; else hi = m; }
      return Math.abs(pts[lo][0] - ts) <= Math.abs(pts[hi][0] - ts) ? lo : hi;
    }
    clusterAt(pos) {
      const g = this.g;
      if (!this.cl || !g) return null;
      const laneTop = g.top + g.ph + 2;
      if (pos.y < laneTop - 6 || pos.y > laneTop + LANE_H + 6) return null;
      let best = null, bd = 14;
      for (const c of this.cl) { const d = Math.abs(c.x - pos.x); if (d < bd) { bd = d; best = c; } }
      return best;
    }
    onMove(e) {
      if (!this.g || !this.pts) return;
      const pos = this.locate(e);
      const c = this.clusterAt(pos);
      if (c) { this.hover = { c }; this.canvas.style.cursor = "pointer"; this.showTip(c.x, this.g.top + this.g.ph - 6, this.clusterTip(c)); }
      else {
        this.canvas.style.cursor = "crosshair";
        const i = this.nearestPoint(Math.min(Math.max(pos.x, this.g.left), this.g.left + this.g.pw));
        if (i == null) return;
        this.hover = { i };
        const p = this.pts[i];
        this.showTip(this.x(p[0], this.g), this.y(p[1], this.g) - 10, this.pointTip(p));
      }
      this.draw();
    }
    onKey(e) {
      if (!this.pts || !this.pts.length) return;
      if (e.key === "ArrowLeft" || e.key === "ArrowRight") {
        e.preventDefault();
        let i = this.hover && this.hover.i != null ? this.hover.i : this.pts.length - 1;
        i = Math.min(this.pts.length - 1, Math.max(0, i + (e.key === "ArrowLeft" ? -1 : 1) * (e.shiftKey ? 10 : 1)));
        this.hover = { i };
        const p = this.pts[i];
        this.showTip(this.x(p[0], this.g), this.y(p[1], this.g) - 10, this.pointTip(p));
        this.draw();
      }
    }
    pointTip(p) {
      const near = this.blocks.filter((b) => Math.abs(b.ts - p[0]) <= (this.data.step_s || 60) / 2 + 1);
      return `<b>${LZ.fmtHr(p[1])}</b><span>${LZ.esc(this.fmtTime(p[0], true))}</span>` +
        (this.mode === "pool" ? `<span>${LZ.esc(this.t("minersN", { n: LZ.num(p[2]) }))}</span>` : "") +
        (near.length ? `<span class="hx-tip-block">◆ ${LZ.esc(this.t("foundHere", { n: near.length, h: LZ.num(near[near.length - 1].height) }))}</span>` : "");
    }
    clusterTip(c) {
      const bs = c.blocks;
      if (bs.length === 1) {
        const b = bs[0];
        return `<b>◆ ${LZ.esc(this.t("block", { h: LZ.num(b.height) }))}</b><span>${LZ.esc(this.fmtTime(b.ts, true))} · ${LZ.esc(LZ.ago(b.ts))}</span>` +
          `<span>${this.mode === "miner" ? LZ.esc(this.t("toYou", { amt: "" })) : ""}${LZ.amt(b.reward_btc)}${b.kind ? " · " + LZ.esc(this.via(b)) : ""}${this.mode === "miner" && b.status === "immature" ? " · " + LZ.esc(this.t("immature")) : ""}</span><span class="hx-tip-cta">${LZ.esc(this.t("clickSplit"))}</span>`;
      }
      const sum = bs.reduce((a, b) => a + (b.reward_btc || 0), 0);
      return `<b>◆ ${LZ.esc(this.t("nBlocks", { n: bs.length }))}</b><span>${LZ.num(bs[0].height)} – ${LZ.num(bs[bs.length - 1].height)}</span>` +
        `<span>${LZ.amt(sum)}</span><span class="hx-tip-cta">${LZ.esc(this.t("clickList"))}</span>`;
    }
    via(b) {
      if (b.kind === "datum" || b.gateway) return b.gateway ? this.t("viaGw", { gw: b.gateway }) : this.t("viaDatum");
      if (b.kind) return this.t("viaStratum");
      return "";
    }
    showTip(px, py, html) {
      const tip = this.tip;
      tip.innerHTML = html; tip.hidden = false;
      const w = tip.offsetWidth, h = tip.offsetHeight, sw = this.stage.clientWidth;
      let left = px - w / 2; left = Math.max(4, Math.min(sw - w - 4, left));
      let top = py - h - 8; if (top < 0) top = py + 16;
      tip.style.transform = `translate(${Math.round(left)}px, ${Math.round(top)}px)`;
    }
    onClick(e) {
      const c = this.clusterAt(this.locate(e));
      if (!c) { this.closePop(); return; }
      this.pinned = c; this.tip.hidden = true;
      if (c.blocks.length === 1) this.openBlock(c.blocks[0], c); else this.openList(c);
      this.draw();
    }
    placePop(c) {
      const pop = this.pop, sw = this.stage.clientWidth;
      pop.hidden = false;
      const w = pop.offsetWidth;
      let left = c.x - w / 2; left = Math.max(4, Math.min(sw - w - 4, left));
      pop.style.left = Math.round(left) + "px";
      pop.style.bottom = (LANE_H + PAD.b + 14) + "px";
    }
    closePop() {
      if (this.pop.hidden) return;
      this.pop.hidden = true; this.pinned = null; this.draw();
    }
    openList(c) {
      const rows = c.blocks.slice().reverse().slice(0, 12).map((b) =>
        `<li><button type="button" data-h="${LZ.esc(b.hash)}"><b>${LZ.num(b.height)}</b><span>${LZ.esc(this.fmtTime(b.ts, true))}</span><span>${LZ.amt(b.reward_btc)}</span></button></li>`).join("");
      this.pop.innerHTML = `<header><b>${LZ.esc(this.t("nBlocks", { n: c.blocks.length }))}</b><button type="button" class="hx-x" aria-label="${LZ.esc(this.t("close"))}">×</button></header><ul class="hx-list">${rows}</ul>` +
        (c.blocks.length > 12 ? `<p class="hx-more">${LZ.esc(this.t("moreZoom", { n: c.blocks.length - 12 }))}</p>` : "");
      this.pop.querySelector(".hx-x").addEventListener("click", () => this.closePop());
      this.pop.querySelectorAll("[data-h]").forEach((btn) => btn.addEventListener("click", () => {
        const b = c.blocks.find((x) => x.hash === btn.dataset.h);
        if (b) this.openBlock(b, c, true);
      }));
      this.placePop(c);
    }
    async openBlock(b, c, fromList) {
      const head = `<header>${fromList ? `<button type="button" class="hx-back" aria-label="${LZ.esc(this.t("back"))}">←</button>` : ""}<b>${LZ.esc(this.t("block", { h: LZ.num(b.height) }))}</b><button type="button" class="hx-x" aria-label="${LZ.esc(this.t("close"))}">×</button></header>`;
      const meta = `<dl class="hx-meta"><div><dt>${LZ.esc(this.t("found"))}</dt><dd>${LZ.esc(LZ.when(b.ts))}<small>${LZ.esc(LZ.ago(b.ts))}</small></dd></div>` +
        `<div><dt>${LZ.esc(this.t("rewardOne"))}</dt><dd>${LZ.amt(b.reward_btc)}${this.mode === "miner" && b.status ? `<small>${LZ.esc(b.status === "immature" ? this.t("immature") : this.t("spendable"))}</small>` : ""}</dd></div>` +
        (this.via(b) ? `<div><dt>${LZ.esc(this.t("foundVia"))}</dt><dd>${LZ.esc(this.via(b))}</dd></div>` : "") + `</dl>`;
      const foot = `<footer><a href="${LZ.EXPLORER}/block/${LZ.esc(b.hash)}" target="_blank" rel="noreferrer">${LZ.esc(this.t("explorer"))} ↗</a></footer>`;
      const bind = () => {
        this.pop.querySelector(".hx-x").addEventListener("click", () => this.closePop());
        const back = this.pop.querySelector(".hx-back");
        if (back) back.addEventListener("click", () => this.openList(c));
      };
      this.pop.innerHTML = head + meta + `<p class="hx-loading">${LZ.esc(this.t("loadingSplit"))}</p>` + foot;
      bind(); this.placePop(c);
      try {
        const r = await fetch("/api/found/" + b.hash);
        if (!r.ok) throw new Error("HTTP " + r.status);
        const d = await r.json();
        if (this.pinned !== c) return;
        const outs = (d.outputs || []).slice().sort((p, q) => q.btc - p.btc);
        const shown = outs.slice(0, 5);
        const mine = this.address ? outs.find((o) => o.address === this.address) : null;
        if (mine && !shown.includes(mine)) shown[4] = mine;
        const top = shown.map((o) =>
          `<li${o === mine ? ' class="is-you"' : ""}><a href="${location.pathname.startsWith("/miner") || /^\/zh\/miner/.test(location.pathname) ? "/#" : "#"}${LZ.esc(o.address)}">${LZ.esc(LZ.short(o.address))}</a><span class="hx-bar"><i style="width:${Math.max(2, Math.round((o.share || 0) * 100))}%"></i></span><span>${LZ.amt(o.btc)}</span></li>`).join("");
        const split = `<p class="hx-split-head">${LZ.esc(this.t("paidTo", { n: LZ.num(outs.length) }))}</p><ol class="hx-split">${top}</ol>` +
          (outs.length > 5 ? `<p class="hx-more">${LZ.esc(this.t("moreOutputs", { n: LZ.num(outs.length - 5) }))}</p>` : "");
        this.pop.innerHTML = head + meta + split + foot;
        bind(); this.placePop(c);
      } catch (err) {
        const l = this.pop.querySelector(".hx-loading");
        if (l) l.textContent = this.t("splitError");
      }
    }
  }

  function boot() {
    const root = document.getElementById("hashchart");
    if (root && !root.__hx) root.__hx = new HashChart(root);
  }
  if (document.readyState === "loading") document.addEventListener("DOMContentLoaded", boot); else boot();
  HashChart.mount = (root, mode) => (root ? root.__hx || (root.__hx = new HashChart(root, mode)) : null);
  window.LZ_HashChart = HashChart;
})();
