// App shell for the pool site. The page is one document (every section stays in the HTML, so a
// crawler or a browser without JS reads all of it); this turns it into views so a visitor sees
// one subject at a time instead of one very long scroll.
//
// Loaded before pool.js on purpose: the view has to be on screen before pool.js scrolls to the
// section the URL asked for, and this file's hashchange listener has to run before pool.js's.
(function () {
  "use strict";
  const $ = (id) => document.getElementById(id);
  const T = (k, v) => (window.LZ_I18N ? window.LZ_I18N.t("app." + k, v) : k);

  // view -> the <main> sections it shows. `home` is first: it is what "/" opens.
  const VIEWS = {
    home: ["fees", "status"],
    mine: ["connect", "hardware"],
    payouts: ["payout", "blocks"],
    network: ["miners", "gateways", "solo"],
    me: ["dashboard"],
    learn: ["learn", "how", "pools"],
  };
  // Hash targets that are not themselves a top-level section.
  const ALIAS = { "": "fees", top: "fees", datum: "connect", mine: "connect", window: "payout", payouts: "dashboard" };
  const SECTION_VIEW = {};
  for (const [v, ids] of Object.entries(VIEWS)) for (const id of ids) SECTION_VIEW[id] = v;

  const root = document.documentElement;
  let current = null;

  // Each crawlable URL (/tides, /hardware, ...) is this same page with that topic's article put
  // at the top of <main> by the server. The article belongs to the view its topic lives in, and
  // that view is what the URL opens.
  const PATH_VIEW = {
    "/how": "learn", "/bip110": "learn", "/tides": "learn", "/non-custodial": "learn", "/self-cap": "learn", "/pools": "learn",
    "/datum-subsidy": "learn", "/api": "learn",
    "/connect": "mine", "/mine-xbt": "mine", "/hardware": "mine", "/profitability": "mine", "/calculator": "mine",
    "/blocks": "payouts",
  };
  const pagePath = location.pathname.replace(/^\/zh(?=\/|$)/, "").replace(/\/$/, "") || "/";
  const article = document.querySelector("main > section.seo-intro");
  const articleView = article && pagePath !== "/" ? PATH_VIEW[pagePath] || "learn" : null;
  if (article) {
    if (!article.id) article.id = "article";
    SECTION_VIEW[article.id] = articleView || "home";
  }

  function viewFor(target) {
    if (target in ALIAS) target = ALIAS[target];
    if (SECTION_VIEW[target]) return SECTION_VIEW[target];
    const node = target ? $(target) : null;
    const sec = node && node.closest("main > section");
    if (sec && SECTION_VIEW[sec.id]) return SECTION_VIEW[sec.id];
    // Anything else in the hash is a payout address: that is the "my stats" view.
    return target ? "me" : "home";
  }

  // A keyword page carries only its article and the sections it is about (server.py _SEO_KEEP).
  // A view it does not carry, asked for by a link built in the browser (an address, a nav tab),
  // lives on the homepage, so go there with the same hash rather than show an empty view.
  const homeURL = pagePath === "/" ? null : (/^\/zh(\/|$)/.test(location.pathname) ? "/zh/" : "/");

  function show(view, opts) {
    if (!VIEWS[view]) view = "home";
    if (homeURL && view !== articleView && !VIEWS[view].some((id) => $(id))) {
      location.assign(homeURL + location.hash);
      return;
    }
    const changed = view !== current;
    current = view;
    root.dataset.view = view;
    for (const sec of document.querySelectorAll("main > section")) {
      const on = SECTION_VIEW[sec.id] === view;
      sec.classList.toggle("view-off", !on);
      if (on && changed) sec.classList.add("view-in");
    }
    for (const a of document.querySelectorAll("[data-view-link]")) {
      const on = a.dataset.viewLink === view;
      a.classList.toggle("is-active", on);
      if (on) a.setAttribute("aria-current", "page"); else a.removeAttribute("aria-current");
    }
    if (changed) {
      // Canvases drawn while their view was hidden measured zero width; pool.js redraws on resize.
      requestAnimationFrame(() => window.dispatchEvent(new Event("resize")));
      if (!(opts && opts.keepScroll)) window.scrollTo({ top: 0, behavior: "instant" });
      document.dispatchEvent(new CustomEvent("lz:view", { detail: { view } }));
    }
  }

  function route(keepScroll) {
    const hash = decodeURIComponent(location.hash.slice(1));
    const scroll = root.getAttribute("data-scroll") || "";
    const target = hash || scroll;
    // No hash on an article URL: open the article's own view, at the article.
    show(!target && articleView ? articleView : viewFor(target), { keepScroll });
    const tops = new Set(Object.values(VIEWS).map((ids) => ids[0]));
    if (hash && !tops.has(ALIAS[hash] || hash)) {
      const node = $(ALIAS[hash] || hash);
      if (node) requestAnimationFrame(() => node.scrollIntoView({ block: "start", behavior: "instant" }));
    }
  }

  // ------------------------------------------------------------ sub-navigation
  // A view with more than one section gets a slim bar of its parts under the header.
  const SUB = {
    home: [["fees", "app.sub.intro"], ["hashchart", "app.sub.chart"], ["status", "app.sub.live"]],
    mine: [["connect", "nav.connect"], ["hardware", "nav.hardware"], ["calc", "app.sub.calc"]],
    payouts: [["payout", "nav.payout"], ["blocks", "nav.blocks"]],
    network: [["miners", "nav.miners"], ["gateways", "nav.gateways"], ["solo", "nav.solo"]],
    learn: [["learn", "app.sub.basics"], ["how", "nav.how"], ["pools", "nav.pools"]],
  };
  let subnav = null;
  function renderSubnav() {
    const mast = document.querySelector(".site-mast");
    if (!mast) return;
    if (!subnav) {
      subnav = document.createElement("nav");
      subnav.className = "subnav";
      subnav.innerHTML = '<div class="wrap"></div>';
      mast.appendChild(subnav);
      // The mast is sticky and its height changes (banner dismissed, sub-bar shown, phone width), so
      // anchor jumps take their offset from what it measures now.
      const setH = () => {
        root.style.setProperty("--mast-h", (mast.querySelector(".site-header")?.offsetHeight || 58) + "px");
        root.style.setProperty("--mast-full", mast.offsetHeight + "px");
      };
      setH();
      window.addEventListener("resize", setH);
      if ("ResizeObserver" in window) new ResizeObserver(setH).observe(mast);
    }
    const items = (SUB[current] || []).filter(([id]) => { const n = $(id); return n && !n.hidden && !n.closest("[hidden]"); });
    subnav.hidden = items.length < 2;
    subnav.setAttribute("aria-label", T("sub.aria"));
    const box = subnav.querySelector(".wrap");
    box.innerHTML = "";
    for (const [id, key] of items) {
      const a = document.createElement("a");
      a.href = "#" + (id === "fees" ? "top" : id);
      a.dataset.sub = id;
      a.textContent = window.LZ_I18N ? window.LZ_I18N.t(key) : id;
      box.appendChild(a);
    }
    spy();
  }
  let spyIO = null;
  function spy() {
    if (spyIO) spyIO.disconnect();
    if (!subnav || subnav.hidden || !("IntersectionObserver" in window)) return;
    const links = [...subnav.querySelectorAll("a")];
    const vis = new Map();
    spyIO = new IntersectionObserver((es) => {
      for (const e of es) vis.set(e.target.id, e.isIntersecting ? e.intersectionRatio : 0);
      let best = links[0]?.dataset.sub, top = Infinity;
      for (const l of links) {
        const n = $(l.dataset.sub);
        if (!n || !vis.get(l.dataset.sub)) continue;
        const y = Math.abs(n.getBoundingClientRect().top - 130);
        if (y < top) { top = y; best = l.dataset.sub; }
      }
      links.forEach((l) => l.classList.toggle("is-on", l.dataset.sub === best));
    }, { rootMargin: "-120px 0px -45% 0px", threshold: [0, 0.01, 0.2] });
    links.forEach((l) => { const n = $(l.dataset.sub); if (n) spyIO.observe(n); });
    links.forEach((l, i) => l.classList.toggle("is-on", i === 0));
  }
  document.addEventListener("lz:view", renderSubnav);
  document.addEventListener("lz:i18n", renderSubnav);

  window.addEventListener("hashchange", () => route(false));
  // A click on the link for the view already open still means "take me to the top of it".
  document.addEventListener("click", (e) => {
    const a = e.target.closest("[data-view-link]");
    if (a && a.dataset.viewLink === current && a.getAttribute("href") === location.hash) {
      window.scrollTo({ top: 0, behavior: "smooth" });
    }
  });

  // ------------------------------------------------------------ scrollable bars
  // The header tabs and the sub-bar scroll sideways when they do not fit. A hidden scrollbar is
  // only reachable by touch or trackpad, so a mouse gets three more ways: arrows that appear at
  // whichever edge has more to show, the wheel, and drag. The open tab is always brought into view.
  function scroller(bar) {
    if (!bar || bar.__scroller) return;
    bar.__scroller = true;
    const host = document.createElement("div");
    host.className = "scroller" + (bar.classList.contains("views") ? " scroller-views" : "");
    bar.parentNode.insertBefore(host, bar);
    host.appendChild(bar);
    const mk = (dir) => {
      const b = document.createElement("button");
      b.type = "button";
      b.className = "scroller-btn scroller-" + dir;
      b.setAttribute("aria-label", T(dir === "prev" ? "scroll.prev" : "scroll.next"));
      b.addEventListener("click", () => bar.scrollBy({ left: (dir === "prev" ? -1 : 1) * Math.max(160, bar.clientWidth * 0.6), behavior: "smooth" }));
      host.appendChild(b);
      return b;
    };
    const prev = mk("prev"), next = mk("next");
    const update = () => {
      const max = bar.scrollWidth - bar.clientWidth;
      host.classList.toggle("can-prev", bar.scrollLeft > 4);
      host.classList.toggle("can-next", max - bar.scrollLeft > 4);
      prev.tabIndex = bar.scrollLeft > 4 ? 0 : -1;
      next.tabIndex = max - bar.scrollLeft > 4 ? 0 : -1;
    };
    bar.addEventListener("scroll", update, { passive: true });
    bar.addEventListener("wheel", (e) => {
      if (bar.scrollWidth <= bar.clientWidth || Math.abs(e.deltaX) > Math.abs(e.deltaY)) return;
      e.preventDefault();
      bar.scrollLeft += e.deltaY;
    }, { passive: false });
    let drag = null;
    bar.addEventListener("pointerdown", (e) => { if (e.pointerType === "mouse" && e.button === 0) drag = { x: e.clientX, left: bar.scrollLeft, moved: false }; });
    window.addEventListener("pointermove", (e) => {
      if (!drag) return;
      const dx = e.clientX - drag.x;
      if (Math.abs(dx) > 5) drag.moved = true;
      if (drag.moved) bar.scrollLeft = drag.left - dx;
    });
    window.addEventListener("pointerup", () => { if (drag && drag.moved) bar.__dragged = Date.now(); drag = null; });
    bar.addEventListener("click", (e) => { if (bar.__dragged && Date.now() - bar.__dragged < 120) { e.preventDefault(); e.stopPropagation(); } }, true);
    if ("ResizeObserver" in window) new ResizeObserver(update).observe(bar);
    window.addEventListener("resize", update);
    document.addEventListener("lz:i18n", () => setTimeout(update, 0));
    bar.__update = update;
    update();
  }
  function reveal(bar, sel) {
    const a = bar && bar.querySelector(sel);
    if (!a || bar.scrollWidth <= bar.clientWidth) return;
    const pad = 44, l = a.offsetLeft - pad, r = a.offsetLeft + a.offsetWidth + pad;
    if (l < bar.scrollLeft) bar.scrollTo({ left: Math.max(0, l), behavior: "smooth" });
    else if (r > bar.scrollLeft + bar.clientWidth) bar.scrollTo({ left: r - bar.clientWidth, behavior: "smooth" });
    bar.__update && bar.__update();
  }
  document.addEventListener("lz:view", () => requestAnimationFrame(() => {
    reveal(document.querySelector(".nav.views"), "a.is-active");
    const sb = document.querySelector(".subnav .wrap");
    if (sb) { scroller(sb); sb.scrollLeft = 0; sb.__update && sb.__update(); }
  }));

  // ------------------------------------------------------------ live touches
  // Numbers that change flash once, so a refresh is felt without anything moving.
  function watchBumps() {
    if (!("MutationObserver" in window)) return;
    const seen = new WeakMap();
    const mo = new MutationObserver((muts) => {
      for (const m of muts) {
        const el = m.target.nodeType === 1 ? m.target : m.target.parentElement;
        const host = el && el.closest("[data-bump], .stats-grid dd, .hero-stats b, .hx-stats dd");
        if (!host) continue;
        const txt = host.textContent;
        if (seen.get(host) === txt) continue;
        const first = !seen.has(host);
        seen.set(host, txt);
        if (first || txt.includes("—")) continue;
        host.classList.remove("is-bump");
        void host.offsetWidth;
        host.classList.add("is-bump");
      }
    });
    for (const n of document.querySelectorAll(".hero-stats, #stats, #hashchart")) {
      mo.observe(n, { subtree: true, childList: true, characterData: true });
    }
  }

  // A block found while the page is open is the best news this site has; say so.
  let lastFound = null;
  document.addEventListener("lz:pool", (e) => {
    const p = e.detail || {};
    const n = Number(p.blocks_found);
    if (Number.isFinite(n)) {
      if (lastFound != null && n > lastFound) toast(T("toast.block", { h: Number(p.height || 0).toLocaleString() }), "#blocks");
      lastFound = n;
    }
  });

  function toast(text, href) {
    let box = $("lz-toasts");
    if (!box) {
      box = document.createElement("div");
      box.id = "lz-toasts";
      box.setAttribute("role", "status");
      box.setAttribute("aria-live", "polite");
      document.body.appendChild(box);
    }
    const n = document.createElement(href ? "a" : "div");
    n.className = "lz-toast";
    if (href) n.href = href;
    n.innerHTML = '<span class="lz-toast-mark" aria-hidden="true"></span><span></span>';
    n.lastChild.textContent = text;
    box.appendChild(n);
    requestAnimationFrame(() => n.classList.add("is-in"));
    setTimeout(() => { n.classList.remove("is-in"); setTimeout(() => n.remove(), 400); }, 9000);
  }

  // ------------------------------------------------------------ command palette
  const ADDR = /^(bc1[a-z0-9]{20,90}|[13][a-km-zA-HJ-NP-Z1-9]{25,40})$/;
  let pal = null;

  function destinations() {
    const d = [
      ["home", "#status"], ["chart", "#hashchart"], ["connect", "#connect"], ["datum", "#datum"], ["hardware", "#hardware"],
      ["calc", "#calc"], ["payout", "#payout"], ["blocks", "#blocks"], ["miners", "#miners"], ["gateways", "#gateways"],
      ["me", "#dashboard"], ["learn", "#learn"], ["how", "#how"], ["pools", "#pools"],
    ].map(([k, href]) => ({ label: T("pal.go." + k), href }));
    for (const a of document.querySelectorAll(".nav-out a")) d.push({ label: a.textContent.trim() + " ↗", href: a.href, ext: true });
    return d;
  }

  function openPalette() {
    if (!pal) {
      pal = document.createElement("div");
      pal.className = "lz-pal";
      pal.innerHTML = '<div class="lz-pal-box" role="dialog" aria-modal="true"><input type="text" spellcheck="false" autocomplete="off"><ul role="listbox"></ul><p class="lz-pal-foot"></p></div>';
      document.body.appendChild(pal);
      pal.addEventListener("pointerdown", (e) => { if (e.target === pal) closePalette(); });
      const input = pal.querySelector("input");
      input.addEventListener("input", renderPalette);
      input.addEventListener("keydown", (e) => {
        const items = [...pal.querySelectorAll("li")];
        let i = items.findIndex((li) => li.classList.contains("is-on"));
        if (e.key === "ArrowDown" || e.key === "ArrowUp") {
          e.preventDefault();
          if (!items.length) return;
          i = (i + (e.key === "ArrowDown" ? 1 : -1) + items.length) % items.length;
          items.forEach((li, k) => li.classList.toggle("is-on", k === i));
          items[i].scrollIntoView({ block: "nearest" });
        } else if (e.key === "Enter") {
          e.preventDefault();
          (items[i] || items[0])?.click();
        } else if (e.key === "Escape") closePalette();
      });
    }
    pal.hidden = false;
    pal.querySelector(".lz-pal-box").setAttribute("aria-label", T("pal.aria"));
    const input = pal.querySelector("input");
    input.placeholder = T("pal.placeholder");
    pal.querySelector(".lz-pal-foot").textContent = T("pal.foot");
    input.value = "";
    renderPalette();
    root.classList.add("pal-open");
    input.focus();
  }
  function closePalette() {
    if (pal) pal.hidden = true;
    root.classList.remove("pal-open");
  }
  function renderPalette() {
    const q = pal.querySelector("input").value.trim();
    const ul = pal.querySelector("ul");
    ul.innerHTML = "";
    const add = (label, sub, fn, on) => {
      const li = document.createElement("li");
      li.setAttribute("role", "option");
      if (on) li.classList.add("is-on");
      li.innerHTML = "<b></b><span></span>";
      li.firstChild.textContent = label;
      li.lastChild.textContent = sub || "";
      li.addEventListener("click", () => { closePalette(); fn(); });
      ul.appendChild(li);
    };
    if (ADDR.test(q)) {
      add(T("pal.addr"), q.slice(0, 12) + "…" + q.slice(-8), () => { location.hash = q; }, true);
      return;
    }
    const ql = q.toLowerCase();
    let n = 0;
    try {
      const last = localStorage.getItem("lz.addr");
      if (last && ADDR.test(last) && (!ql || last.toLowerCase().includes(ql))) {
        add(T("pal.last"), last.slice(0, 12) + "…" + last.slice(-8), () => { location.hash = last; }, n++ === 0);
      }
    } catch (e) { /* private mode */ }
    for (const d of destinations()) {
      if (ql && !d.label.toLowerCase().includes(ql)) continue;
      add(d.label, "", () => { if (d.ext) window.open(d.href, "_blank", "noreferrer"); else location.hash = d.href; }, n++ === 0);
    }
    if (!n) add(T("pal.none"), "", () => {}, true);
  }

  document.addEventListener("keydown", (e) => {
    const typing = /^(INPUT|TEXTAREA|SELECT)$/.test((e.target.tagName || "")) || e.target.isContentEditable;
    if ((e.key === "k" && (e.metaKey || e.ctrlKey)) || (e.key === "/" && !typing)) {
      e.preventDefault();
      if (pal && !pal.hidden) closePalette(); else openPalette();
    }
  });
  document.addEventListener("click", (e) => {
    if (e.target.closest("[data-palette]")) { e.preventDefault(); openPalette(); }
  });

  // ------------------------------------------------------------ ask AI (footer)
  // Each assistant opens a new chat with a question about the pool already typed in. The question
  // is askai.prompt in the dictionary, so /zh/ asks in Chinese and points at the Chinese page; the
  // hrefs in the HTML are the English fallback for a browser without JS.
  const ASK_AI = {
    chatgpt: "https://chatgpt.com/?prompt=",
    claude: "https://claude.ai/new?q=",
    grok: "https://grok.com/?q=",
  };
  function askLinks() {
    const prompt = window.LZ_I18N ? window.LZ_I18N.t("askai.prompt") : "";
    if (!prompt || prompt === "askai.prompt") return;
    for (const a of document.querySelectorAll("a[data-ask-ai]")) {
      const base = ASK_AI[a.dataset.askAi];
      if (base) a.href = base + encodeURIComponent(prompt);
    }
  }
  document.addEventListener("lz:i18n", askLinks);

  // ------------------------------------------------------------ boot
  root.classList.add("app");
  // Thumbs get the bottom tab bar; a mouse keeps the header tabs however narrow the window is.
  if (navigator.maxTouchPoints > 0 && matchMedia("(pointer: coarse)").matches) root.classList.add("is-touch");
  route(true);
  const ready = () => {
    route(true);
    renderSubnav();
    scroller(document.querySelector(".nav.views"));
    scroller(document.querySelector(".subnav .wrap"));
    reveal(document.querySelector(".nav.views"), "a.is-active");
    watchBumps();
    askLinks();
  };
  if (document.readyState === "loading") document.addEventListener("DOMContentLoaded", ready); else ready();
  window.LZ_APP = { show, toast, openPalette };
})();
