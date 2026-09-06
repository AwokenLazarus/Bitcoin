/* Lazarus additions to the mempool frontend: a "Lazarus Pool" entry in the top nav and a
 * "Lazarus" column in the footer link tree. The app is an Angular SPA that re-renders the
 * shell on navigation, so both are (re)inserted from a MutationObserver. Idempotent. */
/* Fee colours. The mempool blocks, the fee-priority bar and the Mempool Goggles WebGL
 * treemap all take their colours from one constant array (defaultMempoolFeeColors in
 * app.constants.ts, 39 hex strings from green through red to purple) that the theme service
 * hands out and that the goggles module maps once at load. It is not reachable from CSS,
 * so this replaces the array's contents the moment webpack evaluates the constants module:
 * a chunk pushed onto webpackChunkmempool with a runtime callback yields __webpack_require__,
 * and a Proxy on its module registry lets us wrap each factory as it is registered. Runs as
 * a deferred head script, i.e. before runtime.js. If the bundle ever stops using webpack the
 * hook is simply a no-op and the stock colours show. */
(function () {
  'use strict';
  // Low fee -> high fee: dark bronze, brass, amber, red, wine. Index 0 is "< 1 sat/vB".
  var RAMP = ['4f4823','715e1a','77631c','7d681f','846c21','8b7123','917626','987b28','9f802b','a6852d','ad8a30','b48e32','bb9335','c29838','c99d3a','cc9a35','ce9631','d0922c','d28f29','d48b25','d68723','d88322','d97f21','db7b22','dc7724','dd7226','de6e29','d8682d','d3612f','cd5b32','c75534','c14f36','ba4938','b44439','ad3e3b','a7393c','a0333c','992e3d','92293d'];
  // Categorical series palette (mining-pool pie, per-pool hashrate lines, ...): the stock
  // list is Material reds/purples/blues/greens. Same lightness band, hue spread kept wide
  // enough to tell 16 pools apart, but warm-leaning and muted like the pool UI.
  var SERIES = ['#dbb565','#e47164','#65c98c','#e5974c','#c586b7','#7aa3c8','#a9ab54','#c26576','#63b4b8','#d2764a','#9bba7d','#a08dc3','#68b5a6','#937636','#a34a40','#4b6d8a'];

  function looksLikeFeeColors(v) {
    return Array.isArray(v) && v.length >= 30 && v.length <= 60 &&
      v.every(function (c) { return typeof c === 'string' && /^[0-9a-f]{6}$/i.test(c); });
  }
  function looksLikeSeriesColors(v) {
    return Array.isArray(v) && v.length >= 10 && v.length <= 40 &&
      v.every(function (c) { return typeof c === 'string' && /^#[0-9a-f]{6}$/i.test(c); });
  }
  function fill(arr, src, cycle) {
    var out = [];
    for (var i = 0; i < arr.length; i++) out.push(src[cycle ? i % src.length : Math.min(i, src.length - 1)]);
    arr.splice.apply(arr, [0, arr.length].concat(out));
  }

  /* --- chart colours: recolour canvas drawing through OKLCH hue bands -------------------
   * The ECharts graphs build their options from literal colour strings scattered through the
   * components and the echarts namespace is an internal concatenated module, so the hook sits
   * one level down, where colours reach the DOM: SVG colour attributes (mempool renders its
   * charts with the SVG renderer) and 2D canvas fill/stroke/gradient stops. Each colour is
   * moved to the nearest palette hue (red / amber / brass / green / steel / plum) with its
   * lightness kept and its chroma capped. Neutral greys and
   * text pass through; dark navy chart chrome is warmed to match the page. The mapping is
   * idempotent. Canvases that are not charts (QR codes) are black-and-white and unaffected. */
  function srgbToLinear(c) { c /= 255; return c <= 0.04045 ? c / 12.92 : Math.pow((c + 0.055) / 1.055, 2.4); }
  function linearToSrgb(c) { c = Math.max(0, Math.min(1, c)); return Math.round(255 * (c <= 0.0031308 ? 12.92 * c : 1.055 * Math.pow(c, 1 / 2.4) - 0.055)); }
  function rgbToOklch(r, g, b) {
    var lr = srgbToLinear(r), lg = srgbToLinear(g), lb = srgbToLinear(b);
    var l = Math.cbrt(0.4122214708 * lr + 0.5363325363 * lg + 0.0514459929 * lb);
    var m = Math.cbrt(0.2119034982 * lr + 0.6806995451 * lg + 0.1073969566 * lb);
    var s = Math.cbrt(0.0883024619 * lr + 0.2817188376 * lg + 0.6299787005 * lb);
    var L = 0.2104542553 * l + 0.7936177850 * m - 0.0040720468 * s;
    var a = 1.9779984951 * l - 2.4285922050 * m + 0.4505937099 * s;
    var bb = 0.0259040371 * l + 0.7827717662 * m - 0.8086757660 * s;
    var C = Math.sqrt(a * a + bb * bb), h = Math.atan2(bb, a) * 180 / Math.PI;
    return [L, C, h < 0 ? h + 360 : h];
  }
  function oklchToRgb(L, C, h) {
    var a = C * Math.cos(h * Math.PI / 180), b = C * Math.sin(h * Math.PI / 180);
    var l_ = L + 0.3963377774 * a + 0.2158037573 * b, m_ = L - 0.1055613458 * a - 0.0638541728 * b, s_ = L - 0.0894841775 * a - 1.2914855480 * b;
    var l = l_ * l_ * l_, m = m_ * m_ * m_, s = s_ * s_ * s_;
    return [
      linearToSrgb(4.0767416621 * l - 3.3077115913 * m + 0.2309699292 * s),
      linearToSrgb(-1.2684380046 * l + 2.6097574011 * m - 0.3413193965 * s),
      linearToSrgb(-0.0041960863 * l - 0.7034186147 * m + 1.7076147010 * s)
    ];
  }
  function band(L, C, h) {
    if (C < 0.012) return null;                                   // true greys / white / black
    if (C < 0.06 || (L < 0.35 && C < 0.08)) {
      // near-neutral (navy chart chrome, slate slider tracks): keep tone, warm the tint
      return [L, Math.min(C, 0.015), 78];
    }
    if (h >= 40 && h < 75) return [L, Math.min(C, 0.14), 60];    // orange -> amber
    if (h >= 75 && h < 120) return [Math.min(L, 0.82), Math.min(C, 0.12), 84]; // yellow -> brass (no neon)
    if (h >= 120 && h < 190) return [L, Math.min(C, 0.12), 152]; // green
    if (h >= 190 && h < 275) return [L, Math.min(C, 0.07), 245]; // cyan/blue -> muted steel
    if (h >= 275 && h < 335) return [L, Math.min(C, 0.10), 330]; // purple -> plum
    return [L, Math.min(C, 0.14), 28];                            // magenta/red -> coral
  }
  var colorCache = {};
  function recolor(str) {
    if (typeof str !== 'string' || str.length > 40) return str;
    if (colorCache.hasOwnProperty(str)) return colorCache[str];
    var r, g, b, alpha = null, m, out = str;
    if ((m = /^#([0-9a-f]{3,8})$/i.exec(str))) {
      var x = m[1];
      if (x.length === 3 || x.length === 4) x = x.split('').map(function (c) { return c + c; }).join('');
      if (x.length === 6 || x.length === 8) {
        r = parseInt(x.slice(0, 2), 16); g = parseInt(x.slice(2, 4), 16); b = parseInt(x.slice(4, 6), 16);
        if (x.length === 8) alpha = x.slice(6, 8);
      }
    } else if ((m = /^rgba?\(\s*(\d+)\s*,\s*(\d+)\s*,\s*(\d+)\s*(?:,\s*([0-9.]+)\s*)?\)$/i.exec(str))) {
      r = +m[1]; g = +m[2]; b = +m[3]; alpha = m[4] != null ? m[4] : null;
    }
    if (r != null) {
      var lch = rgbToOklch(r, g, b), t = band(lch[0], lch[1], lch[2]);
      if (t) {
        var rgb = oklchToRgb(t[0], t[1], t[2]);
        if (str.charAt(0) === '#') {
          out = '#' + rgb.map(function (c) { return (c < 16 ? '0' : '') + c.toString(16); }).join('') + (alpha || '');
        } else {
          out = alpha != null ? 'rgba(' + rgb.join(', ') + ', ' + alpha + ')' : 'rgb(' + rgb.join(', ') + ')';
        }
      }
    }
    colorCache[str] = out;
    return out;
  }
  function hookCanvas() {
    if (typeof CanvasRenderingContext2D === 'undefined') return;
    var P = CanvasRenderingContext2D.prototype;
    ['fillStyle', 'strokeStyle', 'shadowColor'].forEach(function (prop) {
      var d = Object.getOwnPropertyDescriptor(P, prop);
      if (!d || !d.set || !d.configurable) return;
      Object.defineProperty(P, prop, {
        configurable: true, enumerable: d.enumerable, get: d.get,
        set: function (v) { d.set.call(this, typeof v === 'string' ? recolor(v) : v); }
      });
    });
    if (typeof CanvasGradient !== 'undefined') {
      var add = CanvasGradient.prototype.addColorStop;
      CanvasGradient.prototype.addColorStop = function (offset, color) {
        return add.call(this, offset, typeof color === 'string' ? recolor(color) : color);
      };
    }
    debug.canvas = true;
  }
  // The ECharts graphs use the SVG renderer: zrender writes fill/stroke/stop-color through
  // setAttribute, so the same recolouring is applied there. Only colour attributes are touched.
  var SVG_COLOR_ATTRS = { fill: 1, stroke: 1, 'stop-color': 1, 'flood-color': 1, 'lighting-color': 1 };
  function hookSvg() {
    if (typeof Element === 'undefined') return;
    var setAttr = Element.prototype.setAttribute;
    Element.prototype.setAttribute = function (name, value) {
      if (SVG_COLOR_ATTRS[name] === 1 && typeof value === 'string' && value.length <= 40 &&
          this.namespaceURI === 'http://www.w3.org/2000/svg') {
        value = recolor(value);
      }
      return setAttr.call(this, name, value);
    };
    var setAttrNS = Element.prototype.setAttributeNS;
    Element.prototype.setAttributeNS = function (ns, name, value) {
      if (SVG_COLOR_ATTRS[name] === 1 && typeof value === 'string' && value.length <= 40) value = recolor(value);
      return setAttrNS.call(this, ns, name, value);
    };
    debug.svg = true;
  }
  var debug = self.__lazarusTheme = { canvas: false, svg: false, feeColors: 0, seriesColors: 0, modules: 0 };
  try { hookCanvas(); } catch (_) { /* stock chart colours */ }
  try { hookSvg(); } catch (_) { /* stock chart colours */ }

  function wrap(factory) {
    if (typeof factory !== 'function' || factory.__lz) return factory;
    var w = function (module, exports, req) {
      var r = factory.apply(this, arguments);
      debug.modules++;
      try {
        var e = (module && module.exports) || exports;
        if (e && typeof e === 'object') {
          var keys = Object.keys(e);
          if (keys.length && keys.length <= 40) {
            keys.forEach(function (k) {
              try {
                var v = e[k];
                if (looksLikeFeeColors(v)) { fill(v, RAMP); debug.feeColors++; }
                else if (looksLikeSeriesColors(v)) { fill(v, SERIES, true); debug.seriesColors++; }
              } catch (_) { /* getter not ready; not our module */ }
            });
          }
        }
      } catch (_) { /* leave stock colours */ }
      return r;
    };
    w.__lz = true;
    return w;
  }
  try {
    var chunks = self.webpackChunkmempool = self.webpackChunkmempool || [];
    chunks.push([['lazarus-theme'], {}, function (req) {
      if (!req || !req.m || typeof Proxy !== 'function') return;
      var registry = req.m;
      Object.keys(registry).forEach(function (id) { registry[id] = wrap(registry[id]); });
      req.m = new Proxy(registry, {
        set: function (target, id, f) { target[id] = wrap(f); return true; }
      });
    }]);
  } catch (_) { /* no-op */ }
})();

(function () {
  'use strict';
  var POOL = 'https://pool.awokenlazarus.xyz';
  var REPO = 'https://github.com/AwokenLazarus/Bitcoin';
  var POOL_SLUG = 'lazarus';
  var ELECTRUM = 'electrum.awokenlazarus.xyz:50002';

  function el(tag, attrs, html) {
    var e = document.createElement(tag);
    Object.keys(attrs || {}).forEach(function (k) { e.setAttribute(k, attrs[k]); });
    if (html != null) e.innerHTML = html;
    return e;
  }
  function esc(s) {
    return String(s == null ? '' : s).replace(/[&<>"']/g, function (c) {
      return { '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c];
    });
  }

  function copyButton(text, label) {
    var b = el('button', { type: 'button', class: 'lz-copy-btn', 'aria-label': label || 'Copy', title: 'Copy' });
    b.addEventListener('click', function (ev) {
      ev.preventDefault();
      ev.stopPropagation();
      var done = function () {
        b.setAttribute('data-copied', '');
        clearTimeout(b._t);
        b._t = setTimeout(function () { b.removeAttribute('data-copied'); }, 1400);
      };
      if (navigator.clipboard && navigator.clipboard.writeText) {
        navigator.clipboard.writeText(text).then(done, function () { fallback(); });
      } else fallback();
      function fallback() {
        var ta = document.createElement('textarea');
        ta.value = text; ta.setAttribute('readonly', ''); ta.style.position = 'fixed'; ta.style.left = '-9999px';
        document.body.appendChild(ta); ta.select();
        try { document.execCommand('copy'); done(); } catch (e) { /* no clipboard */ }
        ta.remove();
      }
    });
    return b;
  }

  // Live pool figures for the dashboard card (the pool API allows cross-origin reads).
  var poolStats = { ts: 0, data: null, pending: false };
  function fetchPool(cb) {
    var now = Date.now();
    if (poolStats.data && now - poolStats.ts < 30000) return cb(poolStats.data);
    if (poolStats.pending) return;
    poolStats.pending = true;
    fetch(POOL + '/api/pool', { mode: 'cors' }).then(function (r) { return r.ok ? r.json() : null; })
      .then(function (d) { poolStats = { ts: Date.now(), data: d, pending: false }; if (d) cb(d); })
      .catch(function () { poolStats.pending = false; });
  }
  function fmtHr(ghs) {
    var hs = Number(ghs) * 1e9;
    if (!isFinite(hs) || hs <= 0) return '—';
    if (hs >= 1e15) return (hs / 1e15).toFixed(2) + ' PH/s';
    if (hs >= 1e12) return (hs / 1e12).toFixed(2) + ' TH/s';
    if (hs >= 1e9) return (hs / 1e9).toFixed(2) + ' GH/s';
    return (hs / 1e6).toFixed(1) + ' MH/s';
  }
  function pct(x) { return isFinite(Number(x)) ? Number(x).toLocaleString(undefined, { maximumFractionDigits: 2 }) + '%' : '—'; }

  // Dashboard card: the Electrum endpoint for wallet users and the pool for miners, in the
  // same card grid as the stock widgets. Re-inserted on every route change by apply().
  function dashboard() {
    var row = document.querySelector('app-dashboard .dashboard-container > .row');
    if (!row || row.querySelector('.lz-col')) return;
    var col = el('div', { class: 'col lz-col' });
    col.innerHTML =
      '<div class="card lz-card">' +
        '<div class="card-body">' +
          '<h5 class="card-title">This chain, end to end</h5>' +
          '<div class="lz-grid">' +
            '<section class="lz-block" aria-label="Connect your wallet">' +
              '<p class="lz-kicker">Wallets</p>' +
              '<p class="lz-head">Connect your wallet to our Electrum server</p>' +
              '<p class="lz-endpoint"><code>' + esc(ELECTRUM) + '</code></p>' +
              '<p class="lz-copy">SSL on port 50002, run on the same node this explorer reads from. The chain uses 164-byte BLAKE2b headers, so use a wallet built for it (Electrum protocol 1.8); a stock SHA-256 wallet cannot verify these headers.</p>' +
            '</section>' +
            '<section class="lz-block" aria-label="Mine on this chain">' +
              '<p class="lz-kicker">Miners</p>' +
              '<p class="lz-head">Mine with Lazarus Pool, paid in the block itself</p>' +
              '<p class="lz-stats" id="lz-pool-stats"><span class="lz-dot" aria-hidden="true"></span><span class="lz-stats-text">Loading pool status…</span></p>' +
              '<p class="lz-copy" id="lz-pool-copy">Every block found pays each miner directly in its coinbase by TIDES window share. 0.5% fee through your own DATUM gateway, 2.5% on the public stratum.</p>' +
              '<p class="lz-actions">' +
                '<a class="btn btn-primary btn-sm" href="' + POOL + '" target="_blank" rel="noopener">Open Lazarus Pool ↗</a>' +
                '<a class="btn btn-secondary btn-sm" href="/mining/pool/' + POOL_SLUG + '">Blocks found by Lazarus</a>' +
              '</p>' +
            '</section>' +
          '</div>' +
        '</div>' +
      '</div>';
    var endpoint = col.querySelector('.lz-endpoint');
    endpoint.appendChild(copyButton(ELECTRUM, 'Copy Electrum server address'));
    row.appendChild(col);
    fetchPool(function (d) {
      var t = col.querySelector('.lz-stats-text');
      if (!t || !d) return;
      var pr = d.prime || {};
      var fees = d.fees || {};
      var gws = Number(pr.gateways_online) || 0;
      var inWindow = Number((pr.window || {}).identities) || 0;
      t.textContent = fmtHr(d.pool_hr_ghs) + ' · ' + (d.blocks_found || 0) + ' blocks found · ' + inWindow + ' miner' + (inWindow === 1 ? '' : 's') + ' in window · ' + gws + ' gateway' + (gws === 1 ? '' : 's');
      var c = col.querySelector('#lz-pool-copy');
      if (c && fees.datum_percent != null && fees.stratum_percent != null) {
        c.textContent = 'Every block found pays each miner directly in its coinbase by TIDES window share. ' + pct(fees.datum_percent) + ' fee through your own DATUM gateway, ' + pct(fees.stratum_percent) + ' on the public stratum.';
      }
      col.querySelector('.lz-dot').classList.add(pr.reachable === false ? 'stale' : 'live');
    });
  }

  function nav() {
    var ul = document.querySelector('header ul.navbar-nav');
    if (!ul || ul.querySelector('.lz-pool-item')) return;
    var li = el('li', { class: 'nav-item lz-pool-item', id: 'btn-lazarus-pool' });
    var a = el('a', {
      class: 'nav-link', href: POOL, target: '_blank', rel: 'noopener',
      title: 'Lazarus Pool: TIDES payouts in the coinbase, bring your own DATUM gateway',
      'aria-label': 'Lazarus Pool (opens in a new tab)'
    });
    // Empty: theme.css masks the Chi Rho onto this span.
    a.appendChild(el('span', { class: 'lz-mark', 'aria-hidden': 'true' }));
    a.appendChild(el('span', { class: 'lz-label' }, 'Lazarus Pool'));
    li.appendChild(a);
    ul.appendChild(li);
  }

  function footer() {
    var tree = document.querySelector('app-global-footer .link-tree');
    if (!tree || tree.querySelector('.lz-links')) return;
    var col = el('div', { class: 'links lz-links' });
    col.appendChild(el('p', { class: 'category' }, 'Lazarus'));
    var links = [
      [POOL, 'Lazarus Pool'],
      [POOL + '/#payout', 'What the next block pays'],
      ['/mining/pool/' + POOL_SLUG, 'Blocks found by the pool'],
      [POOL + '/#connect', 'Connect a miner or DATUM gateway'],
      [REPO, 'Source on GitHub']
    ];
    links.forEach(function (l) {
      var p = el('p');
      var a = el('a', { href: l[0] }, l[1]);
      if (l[0].charAt(0) !== '/') { a.setAttribute('target', '_blank'); a.setAttribute('rel', 'noopener'); }
      p.appendChild(a);
      col.appendChild(p);
    });
    // Electrum endpoint for wallet users, with a copy button, under its own heading.
    var wal = el('p', { class: 'category lz-sub-category' }, 'Wallets');
    col.appendChild(wal);
    var ep = el('p', { class: 'lz-footer-endpoint' });
    ep.appendChild(el('code', { title: 'Electrum server, SSL' }, esc(ELECTRUM)));
    ep.appendChild(copyButton(ELECTRUM, 'Copy Electrum server address'));
    col.appendChild(ep);
    col.appendChild(el('p', { class: 'lz-footer-note' }, 'Electrum server, SSL · header-v2 (BLAKE2b) wallets'));
    // In front of "Legal" so the reading order stays Explore, Learn, Tools, Lazarus, Legal.
    var cols = tree.querySelectorAll('.links');
    var legal = cols.length ? cols[cols.length - 1] : null;
    if (legal) tree.insertBefore(col, legal); else tree.appendChild(col);
  }

  // Blocks mined through a DATUM gateway carry the gateway's own tag next to the pool's; the
  // backend exposes it as minerNames[1] and the block badge then shows only that name over a
  // faded pool logo. Prefix the pool so the badge reads "Lazarus - <gateway>" and the reader
  // still sees whose block it is. The pool name comes from the logo's alt text.
  function minerBadges() {
    var badges = document.querySelectorAll('a.badge.miner-name:not([data-lz-pool])');
    for (var i = 0; i < badges.length; i++) {
      var a = badges[i];
      var img = a.querySelector('img.pool-logo');
      var m = img && /^Logo of (.+) mining pool$/.exec(img.getAttribute('alt') || '');
      if (!m) continue;
      var text = null;
      for (var n = a.firstChild; n; n = n.nextSibling) {
        if (n.nodeType === 3 && n.nodeValue.trim()) { text = n; break; }
      }
      if (!text) continue;
      a.setAttribute('data-lz-pool', m[1]);
      a.setAttribute('title', m[1] + ' \u00b7 template by ' + text.nodeValue.trim());
      a.insertBefore(el('span', { class: 'lz-pool-prefix' }, m[1] + ' - '), text);
    }
  }

  /* Address charts. Stock mempool only mounts Balance History + Unspent Outputs when
   * backend$ === 'esplora'. We run Electrum (header-v2 electrs), so the Angular widgets
   * stay hidden even though /api/address/:addr/txs and /utxo work. These two boxes are
   * the same views, drawn here from that data so a backend upgrade is not required. */
  var addrState = { key: '', loading: false, data: null };

  function addrFromPath() {
    var m = /^\/address\/([^/?#]+)/.exec(location.pathname || '');
    return m ? decodeURIComponent(m[1]) : '';
  }
  function fmtBtc(sats) {
    var x = Number(sats) / 1e8;
    if (!isFinite(x)) return '—';
    var n = Math.abs(x) >= 1 ? 4 : Math.abs(x) >= 0.01 ? 6 : 8;
    return (x < 0 ? '−' : '') + Math.abs(x).toFixed(n).replace(/0+$/, '').replace(/\.$/, '') + ' BTC';
  }
  function txTime(tx) {
    var s = tx && tx.status;
    if (s && s.confirmed && s.block_time) return s.block_time;
    return Math.floor(Date.now() / 1000);
  }
  function txNet(tx, addr) {
    var inn = 0, out = 0, i, v, p;
    var vin = tx.vin || [];
    for (i = 0; i < vin.length; i++) {
      p = vin[i].prevout || {};
      if (p.scriptpubkey_address === addr) inn += Number(p.value) || 0;
    }
    var vout = tx.vout || [];
    for (i = 0; i < vout.length; i++) {
      v = vout[i];
      if (v.scriptpubkey_address === addr) out += Number(v.value) || 0;
    }
    return out - inn;
  }
  function fetchAllTxs(addr) {
    var out = [];
    function page(after) {
      var url = '/api/address/' + encodeURIComponent(addr) + '/txs' + (after ? '?after_txid=' + after : '');
      return fetch(url).then(function (r) { return r.ok ? r.json() : []; }).then(function (batch) {
        if (!batch || !batch.length) return out;
        out = out.concat(batch);
        if (out.length >= 500 || batch.length < 10) return out;
        return page(batch[batch.length - 1].txid);
      });
    }
    return page(null);
  }
  function niceTicks(min, max, n) {
    if (!(max > min)) max = min + 1;
    var span = max - min, step = Math.pow(10, Math.floor(Math.log10(span / n)));
    var err = n / (span / step);
    if (err <= 0.15) step *= 10;
    else if (err <= 0.35) step *= 5;
    else if (err <= 0.75) step *= 2;
    var start = Math.ceil(min / step) * step, ticks = [];
    for (var v = start; v <= max + step * 0.01; v += step) ticks.push(v);
    if (!ticks.length) ticks.push(min);
    return ticks;
  }
  function areaSvg(points, period) {
    var W = 720, H = 200, L = 72, R = 16, T = 12, B = 28;
    var now = Date.now();
    var lo = period === '1m' ? now - 30 * 86400 * 1000 : points[0].t;
    var vis = points.filter(function (p) { return p.t >= lo; });
    if (!vis.length) vis = points.slice(-2);
    var startBal = vis[0].bal;
    for (var i = 0; i < points.length; i++) {
      if (points[i].t <= lo) startBal = points[i].bal;
    }
    if (vis[0].t > lo) vis = [{ t: lo, bal: startBal, txid: '' }].concat(vis);
    vis = vis.concat([{ t: now, bal: points[points.length - 1].bal, txid: '' }]);
    var ymin = vis.reduce(function (a, p) { return Math.min(a, p.bal); }, vis[0].bal);
    var ymax = vis.reduce(function (a, p) { return Math.max(a, p.bal); }, vis[0].bal);
    if (ymax === ymin) { ymax += 1e6; ymin = Math.max(0, ymin - 1e6); }
    var pad = (ymax - ymin) * 0.08;
    ymin -= pad; ymax += pad;
    if (ymin < 0 && vis.every(function (p) { return p.bal >= 0; })) ymin = 0;
    function x(t) { return L + (W - L - R) * (t - lo) / Math.max(1, now - lo); }
    function y(b) { return T + (H - T - B) * (1 - (b - ymin) / (ymax - ymin)); }
    var d = '';
    vis.forEach(function (p, i) { d += (i ? 'L' : 'M') + x(p.t).toFixed(1) + ',' + y(p.bal).toFixed(1); });
    var area = d + 'L' + x(vis[vis.length - 1].t).toFixed(1) + ',' + (H - B) + 'L' + x(vis[0].t).toFixed(1) + ',' + (H - B) + 'Z';
    var ticks = niceTicks(ymin, ymax, 4);
    var yaxis = ticks.map(function (v) {
      var yy = y(v);
      return '<line x1="' + L + '" x2="' + (W - R) + '" y1="' + yy + '" y2="' + yy + '" class="lz-grid"/>' +
        '<text x="' + (L - 8) + '" y="' + (yy + 4) + '" class="lz-axis" text-anchor="end">' + esc(fmtBtc(v)) + '</text>';
    }).join('');
    var dots = vis.filter(function (p) { return p.txid; }).map(function (p) {
      return '<a href="/tx/' + encodeURIComponent(p.txid) + '"><circle class="lz-dot-pt" cx="' + x(p.t).toFixed(1) + '" cy="' + y(p.bal).toFixed(1) + '" r="3.2">' +
        '<title>' + esc(fmtBtc(p.bal) + (p.delta ? '  (' + (p.delta > 0 ? '+' : '') + fmtBtc(p.delta) + ')' : '')) + '</title></circle></a>';
    }).join('');
    return '<svg class="lz-area" viewBox="0 0 ' + W + ' ' + H + '" preserveAspectRatio="none" role="img" aria-label="Balance history">' +
      '<defs><linearGradient id="lzBalFill" x1="0" y1="0" x2="0" y2="1">' +
      '<stop offset="0" stop-color="#FDD835" stop-opacity="0.45"/><stop offset="1" stop-color="#FB8C00" stop-opacity="0.04"/>' +
      '</linearGradient><linearGradient id="lzBalStroke" x1="0" y1="0" x2="0" y2="1">' +
      '<stop offset="0" stop-color="#FDD835"/><stop offset="1" stop-color="#FB8C00"/></linearGradient></defs>' +
      yaxis +
      '<path class="lz-area-fill" d="' + area + '" fill="url(#lzBalFill)"/>' +
      '<path class="lz-area-line" d="' + d + '" fill="none" stroke="url(#lzBalStroke)" stroke-width="2" vector-effect="non-scaling-stroke"/>' +
      dots + '</svg>';
  }
  function packBubbles(utxos, W, H) {
    var items = utxos.slice().sort(function (a, b) { return (b.value || 0) - (a.value || 0); }).slice(0, 500);
    if (!items.length) return [];
    var max = items[0].value || 1;
    var i, t, ok, p, dx, dy;
    for (i = 0; i < items.length; i++) {
      items[i].r = 6 + 42 * Math.sqrt((items[i].value || 0) / max);
    }
    items[0].x = 0; items[0].y = 0;
    for (i = 1; i < items.length; i++) {
      var c = items[i], placed = false, ang = 0, dist = items[0].r;
      for (t = 0; t < 900 && !placed; t++) {
        ang += 0.37;
        dist += c.r * 0.045;
        c.x = Math.cos(ang) * dist;
        c.y = Math.sin(ang) * dist * 0.72;
        ok = true;
        for (p = 0; p < i; p++) {
          dx = c.x - items[p].x; dy = c.y - items[p].y;
          if (dx * dx + dy * dy < (c.r + items[p].r) * (c.r + items[p].r) * 0.92) { ok = false; break; }
        }
        placed = ok;
      }
    }
    var minx = Infinity, maxx = -Infinity, miny = Infinity, maxy = -Infinity;
    for (i = 0; i < items.length; i++) {
      minx = Math.min(minx, items[i].x - items[i].r);
      maxx = Math.max(maxx, items[i].x + items[i].r);
      miny = Math.min(miny, items[i].y - items[i].r);
      maxy = Math.max(maxy, items[i].y + items[i].r);
    }
    var sx = (W - 24) / Math.max(1, maxx - minx);
    var sy = (H - 24) / Math.max(1, maxy - miny);
    var s = Math.min(sx, sy);
    for (i = 0; i < items.length; i++) {
      items[i].x = 12 + (items[i].x - minx) * s;
      items[i].y = 12 + (items[i].y - miny) * s;
      items[i].r *= s;
    }
    return items;
  }
  function mixHex(a, b, t) {
    function hex(h) { return [parseInt(h.slice(0, 2), 16), parseInt(h.slice(2, 4), 16), parseInt(h.slice(4, 6), 16)]; }
    var A = hex(a), B = hex(b), o = '#';
    for (var i = 0; i < 3; i++) o += ('0' + Math.round(A[i] + (B[i] - A[i]) * t).toString(16)).slice(-2);
    return o;
  }
  function bubbleSvg(utxos) {
    var W = 720, H = 260;
    var now = Math.floor(Date.now() / 1000);
    var times = utxos.map(function (u) { return (u.status && u.status.block_time) || now; });
    var tmin = Math.min.apply(null, times), tmax = Math.max.apply(null, times);
    var packed = packBubbles(utxos, W, H);
    var circles = packed.map(function (u) {
      var t = (u.status && u.status.block_time) || now;
      var age = tmax === tmin ? 0 : (t - tmin) / (tmax - tmin);
      var fill = u.status && u.status.confirmed ? mixHex('3C39F4', '1BF4AF', age) : '#eba814';
      var href = '/tx/' + encodeURIComponent(u.txid);
      return '<a href="' + href + '"><circle cx="' + u.x.toFixed(1) + '" cy="' + u.y.toFixed(1) + '" r="' + Math.max(2, u.r).toFixed(1) + '" fill="' + fill + '" fill-opacity="0.88" stroke="#00000033" stroke-width="0.6">' +
        '<title>' + esc(fmtBtc(u.value) + (u.status && u.status.block_height ? ' · block ' + u.status.block_height : ' · unconfirmed')) + '</title></circle></a>';
    }).join('');
    return '<svg class="lz-bubbles" viewBox="0 0 ' + W + ' ' + H + '" role="img" aria-label="Unspent outputs">' + circles + '</svg>';
  }
  function renderAddrCharts(host, addr, data) {
    var txs = data.txs || [], utxos = data.utxos || [];
    var chronological = txs.slice().sort(function (a, b) { return txTime(a) - txTime(b); });
    var bal = 0, points = [];
    chronological.forEach(function (tx) {
      var delta = txNet(tx, addr);
      bal += delta;
      points.push({ t: txTime(tx) * 1000, bal: bal, delta: delta, txid: tx.txid });
    });
    var newest = chronological.length ? txTime(chronological[chronological.length - 1]) : 0;
    var showPeriod = newest > Date.now() / 1000 - 30 * 86400;
    var wrap = el('div', { class: 'lz-addr-charts', 'data-lz-addr': addr });
    if (points.length > 2) {
      var hist = el('div', { class: 'lz-addr-block' });
      hist.innerHTML = '<div class="title-tx"><h2 class="text-left">Balance History</h2></div>' +
        '<div class="box lz-chart-box">' +
          (showPeriod ? '<div class="widget-toggler lz-period">' +
            '<a href="#" class="toggler-option" data-period="all"><small>all</small></a>' +
            '<span class="lz-period-bar"> | </span>' +
            '<a href="#" class="toggler-option inactive" data-period="1m"><small>recent</small></a></div>' : '') +
          '<div class="lz-chart-slot" data-slot="area"></div></div>';
      var slot = hist.querySelector('[data-slot="area"]');
      slot.innerHTML = areaSvg(points, 'all');
      if (showPeriod) {
        hist.addEventListener('click', function (ev) {
          var a = ev.target.closest('[data-period]');
          if (!a) return;
          ev.preventDefault();
          hist.querySelectorAll('[data-period]').forEach(function (x) { x.classList.toggle('inactive', x !== a); });
          slot.innerHTML = areaSvg(points, a.getAttribute('data-period'));
        });
      }
      wrap.appendChild(hist);
    }
    if (utxos.length > 2) {
      var uns = el('div', { class: 'lz-addr-block' });
      uns.innerHTML = '<div class="title-tx"><h2 class="text-left">Unspent Outputs</h2></div>' +
        '<div class="box lz-chart-box"><div class="lz-chart-slot">' + bubbleSvg(utxos) + '</div></div>';
      wrap.appendChild(uns);
    }
    var old = host.querySelector('.lz-addr-charts');
    if (old) old.replaceWith(wrap);
    else {
      var txTitle = host.querySelector('.title-tx');
      if (txTitle) host.insertBefore(wrap, txTitle);
      else host.appendChild(wrap);
    }
  }
  function addressCharts() {
    var addr = addrFromPath();
    var host = document.querySelector('app-address');
    if (!addr || !host) return;
    var existing = host.querySelector('.lz-addr-charts');
    if (existing && existing.getAttribute('data-lz-addr') === addr) return;
    if (addrState.data && addrState.key === addr) {
      renderAddrCharts(host, addr, addrState.data);
      return;
    }
    if (addrState.loading === addr) return;
    addrState.loading = addr;
    Promise.all([
      fetchAllTxs(addr),
      fetch('/api/address/' + encodeURIComponent(addr) + '/utxo').then(function (r) { return r.ok ? r.json() : []; })
    ]).then(function (pair) {
      if (addrFromPath() !== addr) { addrState.loading = false; return; }
      addrState = { key: addr, loading: false, data: { txs: pair[0], utxos: pair[1] } };
      var live = document.querySelector('app-address');
      if (live) renderAddrCharts(live, addr, addrState.data);
    }).catch(function () { addrState.loading = false; });
  }

  var scheduled = false;
  function apply() {
    scheduled = false;
    try { nav(); } catch (e) { /* never break the explorer */ }
    try { footer(); } catch (e) { /* never break the explorer */ }
    try { dashboard(); } catch (e) { /* never break the explorer */ }
    try { minerBadges(); } catch (e) { /* never break the explorer */ }
    try { addressCharts(); } catch (e) { /* never break the explorer */ }
  }
  function schedule() {
    if (scheduled) return;
    scheduled = true;
    (window.requestAnimationFrame || setTimeout)(apply);
  }

  function lazChat() {
    if (window.__lazChatBooted) return;
    window.__lazChatBooted = true;
    window.LAZ_CHAT_SOURCE = 'mempool';
    window.LAZ_CHAT_URL = 'https://pool.awokenlazarus.xyz/api/laz/chat';
    if (!document.getElementById('laz-chat-css')) {
      var link = el('link', { id: 'laz-chat-css', rel: 'stylesheet', href: '/lazarus/laz-chat.css' });
      document.head.appendChild(link);
    }
    var s = el('script', { src: '/lazarus/laz-chat.js' });
    document.body.appendChild(s);
  }

  function start() {
    apply();
    try { lazChat(); } catch (e) { /* never break the explorer */ }
    new MutationObserver(schedule).observe(document.body, { childList: true, subtree: true });
  }
  if (document.body) start(); else document.addEventListener('DOMContentLoaded', start);
})();
