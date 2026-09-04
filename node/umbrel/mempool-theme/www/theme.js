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

  function el(tag, attrs, html) {
    var e = document.createElement(tag);
    Object.keys(attrs || {}).forEach(function (k) { e.setAttribute(k, attrs[k]); });
    if (html != null) e.innerHTML = html;
    return e;
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
    a.appendChild(el('span', { class: 'lz-mark', 'aria-hidden': 'true' }, 'P'));
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
      ['/mining/pool/' + POOL_SLUG, 'Blocks found by the pool'],
      [POOL + '/#connect', 'Point a DATUM gateway at the pool'],
      [REPO, 'Source on GitHub']
    ];
    links.forEach(function (l) {
      var p = el('p');
      var a = el('a', { href: l[0] }, l[1]);
      if (l[0].charAt(0) !== '/') { a.setAttribute('target', '_blank'); a.setAttribute('rel', 'noopener'); }
      p.appendChild(a);
      col.appendChild(p);
    });
    // In front of "Legal" so the reading order stays Explore, Learn, Tools, Lazarus, Legal.
    var cols = tree.querySelectorAll('.links');
    var legal = cols.length ? cols[cols.length - 1] : null;
    if (legal) tree.insertBefore(col, legal); else tree.appendChild(col);
  }

  var scheduled = false;
  function apply() {
    scheduled = false;
    try { nav(); footer(); } catch (e) { /* never break the explorer */ }
  }
  function schedule() {
    if (scheduled) return;
    scheduled = true;
    (window.requestAnimationFrame || setTimeout)(apply);
  }

  function start() {
    apply();
    new MutationObserver(schedule).observe(document.body, { childList: true, subtree: true });
  }
  if (document.body) start(); else document.addEventListener('DOMContentLoaded', start);
})();
