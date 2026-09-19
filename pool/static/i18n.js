// Language detection, cookie, t(), and data-i18n apply for the Lazarus pool pages.
// Locales are window.LZ_I18N_DICTS.{en,zh-CN}; boot() after those scripts load.
(() => {
  const COOKIE = "lz_lang";
  const SUPPORTED = ["en", "zh-CN"];

  function readCookie() {
    const m = document.cookie.match(/(?:^|; )lz_lang=([^;]*)/);
    return m ? decodeURIComponent(m[1]) : "";
  }
  function writeCookie(lang) {
    document.cookie = `${COOKIE}=${encodeURIComponent(lang)};path=/;max-age=31536000;SameSite=Lax`;
  }
  function normalize(raw) {
    if (!raw) return "";
    const s = String(raw).trim().replace(/_/g, "-");
    if (s === "zh" || /^zh-/i.test(s)) return "zh-CN";
    if (s === "en" || /^en-/i.test(s)) return "en";
    return SUPPORTED.includes(s) ? s : "";
  }
  function detect() {
    const q = new URLSearchParams(location.search).get("lang");
    const fromQ = normalize(q);
    if (fromQ) return fromQ;
    // A /zh URL is as explicit a choice as ?lang=, and it is the canonical spelling, so it has to
    // outrank both the cookie and the browser or the Chinese pages would render in English.
    if (/^\/zh(\/|$)/.test(location.pathname)) return "zh-CN";
    const fromC = normalize(readCookie());
    if (fromC) return fromC;
    const nav = (navigator.languages && navigator.languages.length)
      ? navigator.languages
      : [navigator.language];
    for (const l of nav) {
      const n = normalize(l);
      if (n) return n;
    }
    return "en";
  }

  let lang = detect();

  function locale() {
    return lang === "zh-CN" ? "zh-CN" : "en";
  }
  function dicts() {
    return window.LZ_I18N_DICTS || {};
  }
  function get(obj, path) {
    return String(path).split(".").reduce((o, k) => (o && o[k] != null ? o[k] : undefined), obj);
  }
  function interpolate(str, vars) {
    if (str == null) return "";
    if (!vars) return String(str);
    return String(str).replace(/\{(\w+)\}/g, (_, k) => (vars[k] != null ? String(vars[k]) : `{${k}}`));
  }
  function pick(val, vars) {
    if (val && typeof val === "object" && !Array.isArray(val) && ("one" in val || "other" in val)) {
      const n = Number(vars && vars.n);
      const form = n === 1 ? "one" : "other";
      val = val[form] != null ? val[form] : (val.other || val.one || "");
    }
    return interpolate(val, vars);
  }
  function raw(key) {
    const d = dicts()[lang] || {};
    const fallback = dicts().en || {};
    let val = get(d, key);
    if (val == null) val = get(fallback, key);
    return val;
  }
  function t(key, vars) {
    const d = dicts()[lang] || {};
    const fallback = dicts().en || {};
    let val = get(d, key);
    if (val == null) val = get(fallback, key);
    if (val == null) return vars && vars._default != null ? interpolate(vars._default, vars) : String(key);
    return pick(val, vars);
  }
  function apply(root) {
    const scope = root || document;
    document.documentElement.lang = locale();
    document.documentElement.classList.toggle("lang-zh", lang === "zh-CN");
    scope.querySelectorAll("[data-i18n]").forEach((el) => {
      const v = t(el.getAttribute("data-i18n"));
      if (v) el.textContent = v;
    });
    scope.querySelectorAll("[data-i18n-html]").forEach((el) => {
      const v = t(el.getAttribute("data-i18n-html"));
      if (v) el.innerHTML = v;
    });
    scope.querySelectorAll("[data-i18n-title]").forEach((el) => {
      el.setAttribute("title", t(el.getAttribute("data-i18n-title")));
    });
    scope.querySelectorAll("[data-i18n-placeholder]").forEach((el) => {
      el.setAttribute("placeholder", t(el.getAttribute("data-i18n-placeholder")));
    });
    scope.querySelectorAll("[data-i18n-aria]").forEach((el) => {
      el.setAttribute("aria-label", t(el.getAttribute("data-i18n-aria")));
    });
    const titleEl = document.querySelector("title[data-i18n]");
    if (titleEl) {
      const page = titleEl.getAttribute("data-i18n");
      if (page) document.title = t(page);
    }
    const desc = document.querySelector('meta[name="description"][data-i18n]');
    if (desc) desc.setAttribute("content", t(desc.getAttribute("data-i18n")));
    document.querySelectorAll("[data-lang]").forEach((btn) => {
      btn.setAttribute("aria-pressed", btn.getAttribute("data-lang") === lang ? "true" : "false");
    });
  }
  function setLang(next, persist) {
    const n = normalize(next) || "en";
    lang = n;
    if (persist !== false) writeCookie(n);
    apply();
    document.dispatchEvent(new CustomEvent("lz:i18n", { detail: { lang: n } }));
  }
  // Each language has one canonical URL — English at /x, Chinese at /zh/x — so the switch moves
  // there instead of translating in place, which would leave the reader on a URL whose canonical
  // and <html lang> disagree with what they are reading.
  function switchTo(next) {
    const n = normalize(next) || "en";
    const bare = location.pathname.replace(/^\/zh(?=\/|$)/, "") || "/";
    // The per-address pages are not published in either language; they read the address out of
    // the path, so leave their URL alone and just translate the labels.
    const perAddress = /^\/miner(\/|$)/.test(bare);
    const want = !perAddress && n === "zh-CN" ? "/zh" + (bare === "/" ? "/" : bare) : bare;
    if (perAddress || want === location.pathname) {
      setLang(n);
      return;
    }
    writeCookie(n);
    // ?lang= would fight the path it is being sent to.
    const search = new URLSearchParams(location.search);
    search.delete("lang");
    const qs = search.toString();
    location.assign(want + (qs ? "?" + qs : "") + location.hash);
  }

  function boot() {
    const q = new URLSearchParams(location.search).get("lang");
    if (normalize(q)) writeCookie(lang);
    apply();
    document.querySelectorAll("[data-lang]").forEach((btn) => {
      if (btn._lzLang) return;
      btn._lzLang = true;
      btn.addEventListener("click", () => switchTo(btn.getAttribute("data-lang")));
    });
  }

  window.LZ_I18N = { t, raw, lang: () => lang, locale, setLang, apply, boot, detect };
})();
