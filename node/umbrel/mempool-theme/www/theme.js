/* Fonts first, before anything else in this file runs: theme.js is a synchronous <head>
 * script, so the preconnects and the stylesheet go out while the app bundle is still being
 * fetched. This replaces the @import theme.css used to carry (a serial, render-blocking chain
 * with no preconnect). Only the weights the theme uses; display=swap. Idempotent. */
(function () {
  try {
    var head = document.head || document.getElementsByTagName('head')[0];
    if (!head || document.getElementById('lz-fonts')) return;
    var add = function (attrs) {
      var l = document.createElement('link');
      Object.keys(attrs).forEach(function (k) { l.setAttribute(k, attrs[k]); });
      head.appendChild(l);
      return l;
    };
    add({ rel: 'preconnect', href: 'https://fonts.googleapis.com' });
    add({ rel: 'preconnect', href: 'https://fonts.gstatic.com', crossorigin: '' });
    add({
      id: 'lz-fonts', rel: 'stylesheet',
      href: 'https://fonts.googleapis.com/css2?family=IBM+Plex+Mono:wght@400;500;600' +
        '&family=IBM+Plex+Sans:wght@400;500;600' +
        '&family=Newsreader:ital,opsz,wght@0,6..72,500;1,6..72,400&display=swap'
    });
  } catch (_) { /* system fallbacks from the --lz-* stacks */ }
})();

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
  // The gateway bands in the mining-pool pie (second block) step a slice's own colour in
  // lightness, so they share this conversion rather than carrying a second copy of it.
  self.__lzOklch = { to: rgbToOklch, from: oklchToRgb };
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
  var POOL = 'https://pool.lazarus-xbt.xyz';
  var REPO = 'https://github.com/AwokenLazarus/Bitcoin';
  var DISCORD = 'https://discord.gg/fD33dJXnzz';
  var NEOXA = 'https://neoxa.exchange/register?ref=NEXB9423E49';
  var MCP = 'https://mcp.lazarus-xbt.xyz';
  var POOL_SLUG = 'lazarus';
  var ELECTRUM = 'electrum.lazarus-xbt.xyz:50002';
  var RETROPEX = 'https://github.com/Retropex/mempool';
  var MEMPOOL_UPSTREAM = 'https://github.com/mempool/mempool';
  var FORK_HEIGHT = 961640;

  /* BLAKE2b BTC price (BTCB2): volume-weighted Neoxa + NonKYC, from the pool API.
   * Stock mempool still pulls SHA CoinGecko over REST and the websocket. Rewrite both
   * so every sats→fiat figure on this explorer is this chain, not the other one. */
  var blakeConv = null;
  function convFromPrice(d) {
    if (!d || !isFinite(Number(d.USD))) return null;
    var out = { USD: Number(d.USD), time: d.time || Math.floor(Date.now() / 1000) };
    Object.keys(d).forEach(function (k) {
      if (k === 'USD' || k === 'time' || k === 'stale' || k === 'pair' || k === 'sources' || k === 'average_of' || k === 'method') return;
      if (typeof d[k] === 'number' && isFinite(d[k])) out[k] = d[k];
    });
    return out;
  }
  function rewritePriceJson(stock) {
    if (!blakeConv) return stock;
    var out = Object.assign({}, stock && typeof stock === 'object' ? stock : {}, blakeConv);
    return out;
  }
  // Block / tx fiat uses /api/v1/historical-price (SHA prints in the DB, ~$80k).
  // Scale the series so the newest point matches live BTCB2. Skip if already small.
  function scaleHistorical(stock) {
    if (!stock || !blakeConv || !(blakeConv.USD > 0) || !stock.prices || !stock.prices.length) return stock;
    var latest = stock.prices[0], i, p, k, sha, f, q, out;
    for (i = 0; i < stock.prices.length; i++) {
      if ((stock.prices[i].time || 0) >= (latest.time || 0)) latest = stock.prices[i];
    }
    sha = Number(latest.USD);
    if (!(sha > 20000)) return stock;
    f = blakeConv.USD / sha;
    out = Object.assign({}, stock, { prices: stock.prices.map(function (row) {
      q = Object.assign({}, row);
      for (k in q) {
        if (!Object.prototype.hasOwnProperty.call(q, k) || k === 'time') continue;
        if (typeof q[k] === 'number' && q[k] > 0) q[k] = Math.round(q[k] * f * 100) / 100;
      }
      return q;
    })});
    return out;
  }
  function rewriteByUrl(url, stock) {
    if (/historical-price/.test(url)) return scaleHistorical(stock);
    if (/\/api\/v1\/prices(?:\?|$|\/)/.test(url)) return rewritePriceJson(stock);
    return stock;
  }
  function rewriteMessageEvent(ev) {
    if (!ev || typeof ev.data !== 'string') return ev;
    try {
      var data = JSON.parse(ev.data);
      if (!data || typeof data !== 'object') return ev;
      var touched = false;
      // The websocket's opening frame carries the backend name, and the address page mounts
      // Balance History and Unspent Outputs only when it reads "esplora". This node answers
      // the Esplora REST routes both charts read, so report esplora and fill the two gaps
      // the Electrum backend leaves behind (see the address data section below).
      if (typeof data.backend === 'string' && data.backend !== 'esplora') {
        data.backend = 'esplora';
        touched = true;
      }
      if (data.conversions && blakeConv) {
        data.conversions = Object.assign({}, data.conversions, blakeConv);
        touched = true;
      }
      if (!touched) return ev;
      return new MessageEvent(ev.type, { data: JSON.stringify(data), origin: ev.origin, lastEventId: ev.lastEventId });
    } catch (e) { return ev; }
  }
  function deliverWs(fn, ev, ctx) {
    var data;
    try { data = JSON.parse(ev.data); } catch (e) { return fn.call(ctx, ev); }
    if (data && data.conversions && !blakeConv) {
      var tries = 0;
      var t = setInterval(function () {
        tries += 1;
        if (blakeConv || tries > 40) {
          clearInterval(t);
          fn.call(ctx, rewriteMessageEvent(ev));
        }
      }, 100);
      return;
    }
    return fn.call(ctx, rewriteMessageEvent(ev));
  }
  (function hookPriceIO() {
    var ofetch = window.fetch;

    /* Electrum answers /address/:id, /txs and /utxo, but leaves two holes the stock
     * address-page charts need: (1) /txs/summary is 405, (2) funded_txo_count stays 0
     * so chainStats.utxos is 0 and the bubble chart never fetches. Fill both here so
     * app-address-graph and app-utxo-graph can mount as they do on mempool.guide. */
    function parseAddrApi(url) {
      var m = /\/api\/(address|scripthash)\/([^/?#]+)(\/[^?#]*)?/.exec(String(url || ''));
      if (!m) return null;
      var rest = m[3] || '';
      return {
        kind: m[1],
        id: decodeURIComponent(m[2]),
        rest: rest,
        isSummary: rest.indexOf('/txs/summary') === 0,
        isExact: rest === '' || rest === '/'
      };
    }
    function apiUrl(kind, id, suffix) {
      return '/api/' + kind + '/' + encodeURIComponent(id) + suffix;
    }
    function fetchJson(url) {
      return ofetch(url).then(function (r) {
        if (!r.ok) throw new Error(String(r.status));
        return r.json();
      });
    }
    function txNet(tx, addr) {
      if (!addr) return 0;
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
    function fetchAllTxs(kind, id) {
      var out = [];
      function page(after) {
        var url = apiUrl(kind, id, '/txs') + (after ? '?after_txid=' + encodeURIComponent(after) : '');
        return fetchJson(url).catch(function () { return []; }).then(function (batch) {
          if (!batch || !batch.length) return out;
          out = out.concat(batch);
          if (out.length >= 500 || batch.length < 10) return out;
          return page(batch[batch.length - 1].txid);
        });
      }
      return page(null);
    }
    function txsToSummary(txs, addr) {
      return (txs || []).map(function (tx) {
        var s = tx.status || {};
        return {
          txid: tx.txid,
          time: s.confirmed && s.block_time ? s.block_time : Math.floor(Date.now() / 1000),
          value: txNet(tx, addr)
        };
      });
    }
    function patchCounts(info, utxos) {
      if (!info || typeof info !== 'object') return info;
      var n = Array.isArray(utxos) ? utxos.length : 0;
      var cs = info.chain_stats || {};
      var funded = Number(cs.funded_txo_count) || 0;
      var spent = Number(cs.spent_txo_count) || 0;
      if (funded - spent === n) return info;
      return Object.assign({}, info, {
        chain_stats: Object.assign({}, cs, { funded_txo_count: n + spent })
      });
    }
    function jsonResponse(body) {
      return new Response(JSON.stringify(body), {
        status: 200,
        headers: { 'Content-Type': 'application/json' }
      });
    }
    function fulfillXhr(xhr, status, body) {
      try {
        Object.defineProperty(xhr, 'readyState', { configurable: true, get: function () { return 4; } });
        Object.defineProperty(xhr, 'status', { configurable: true, get: function () { return status; } });
        Object.defineProperty(xhr, 'statusText', { configurable: true, get: function () { return status === 200 ? 'OK' : 'Error'; } });
        Object.defineProperty(xhr, 'responseText', { configurable: true, get: function () { return body; } });
        Object.defineProperty(xhr, 'response', { configurable: true, get: function () { return body; } });
      } catch (e) { /* leave native fields */ }
      try { if (typeof xhr.onreadystatechange === 'function') xhr.onreadystatechange(); } catch (e) { /* */ }
      try { xhr.dispatchEvent(new Event('readystatechange')); } catch (e) { /* */ }
      try { xhr.dispatchEvent(new ProgressEvent('load')); } catch (e) {
        try { xhr.dispatchEvent(new Event('load')); } catch (e2) { /* */ }
      }
      try { xhr.dispatchEvent(new ProgressEvent('loadend')); } catch (e) {
        try { xhr.dispatchEvent(new Event('loadend')); } catch (e2) { /* */ }
      }
    }
    function rewriteAddress(url) {
      var p = parseAddrApi(url);
      if (!p) return null;
      if (p.isSummary) {
        return fetchAllTxs(p.kind, p.id).then(function (txs) {
          return txsToSummary(txs, p.kind === 'address' ? p.id : '');
        });
      }
      if (p.isExact) {
        return Promise.all([
          fetchJson(apiUrl(p.kind, p.id, '')),
          fetchJson(apiUrl(p.kind, p.id, '/utxo')).catch(function () { return []; })
        ]).then(function (pair) { return patchCounts(pair[0], pair[1]); });
      }
      return null;
    }
    function addressShim(xhr, url) {
      var pending = rewriteAddress(url);
      if (!pending) return false;
      pending.then(function (body) {
        fulfillXhr(xhr, 200, JSON.stringify(body));
      }).catch(function () {
        fulfillXhr(xhr, 502, '{"error":"address shim failed"}');
      });
      return true;
    }

    if (typeof ofetch === 'function') {
      window.fetch = function (input, init) {
        var url = '';
        // The caller's arguments: inside the .catch callback below, `arguments` would be the
        // callback's own (the Error), not the original request.
        var args = arguments;
        try { url = typeof input === 'string' ? input : (input && input.url) || ''; } catch (e) { url = ''; }
        var addr = rewriteAddress(url);
        if (addr) {
          return addr.then(jsonResponse).catch(function () {
            return ofetch.apply(window, args);
          });
        }
        var p = ofetch.apply(this, arguments);
        if (!/\/api\/v1\/prices(?:\?|$|\/)|historical-price/.test(url)) return p;
        return p.then(function (resp) {
          return resp.clone().json().then(function (stock) {
            return jsonResponse(rewriteByUrl(url, stock));
          }).catch(function () { return resp; });
        });
      };
    }
    // Angular HttpClient in this build uses XHR, not fetch, for REST.
    var xhrProto = window.XMLHttpRequest && XMLHttpRequest.prototype;
    if (xhrProto && !xhrProto._lzPrice) {
      xhrProto._lzPrice = true;
      var xopen = xhrProto.open;
      var xsend = xhrProto.send;
      xhrProto.open = function (method, url) {
        this._lzUrl = String(url || '');
        this._lzMethod = String(method || 'GET').toUpperCase();
        return xopen.apply(this, arguments);
      };
      xhrProto.send = function () {
        var xhr = this;
        if (xhr._lzMethod === 'GET' && addressShim(xhr, xhr._lzUrl || '')) return;
        if (/\/api\/v1\/prices(?:\?|$|\/)|historical-price/.test(xhr._lzUrl || '')) {
          xhr.addEventListener('readystatechange', function () {
            if (xhr.readyState !== 4 || xhr.status < 200 || xhr.status >= 300) return;
            try {
              var stock = JSON.parse(xhr.responseText);
              var body = JSON.stringify(rewriteByUrl(xhr._lzUrl, stock));
              Object.defineProperty(xhr, 'responseText', { configurable: true, value: body });
              Object.defineProperty(xhr, 'response', { configurable: true, value: body });
            } catch (e) { /* leave stock body */ }
          }, true);
        }
        return xsend.apply(this, arguments);
      };
    }
    function wrapSock(ws) {
      if (!ws || ws._lzWrapped) return ws;
      ws._lzWrapped = true;
      var add = ws.addEventListener;
      if (typeof add === 'function') {
        ws.addEventListener = function (type, fn, opt) {
          if (type === 'message' && typeof fn === 'function') {
            return add.call(this, type, function (ev) { return deliverWs(fn, ev, this); }, opt);
          }
          return add.call(this, type, fn, opt);
        };
      }
      return ws;
    }
    function installWsCtor() {
      var Native = window.WebSocket;
      if (!Native || Native._lzHooked) return;
      function Hooked(url, protocols) {
        var ws = protocols !== undefined ? new Native(url, protocols) : new Native(url);
        return wrapSock(ws);
      }
      Hooked.prototype = Native.prototype;
      Hooked.CONNECTING = Native.CONNECTING;
      Hooked.OPEN = Native.OPEN;
      Hooked.CLOSING = Native.CLOSING;
      Hooked.CLOSED = Native.CLOSED;
      Hooked._lzHooked = true;
      window.WebSocket = Hooked;
    }
    var proto = window.WebSocket && WebSocket.prototype;
    if (proto && !proto._lzPrice) {
      proto._lzPrice = true;
      var add = proto.addEventListener;
      if (typeof add === 'function') {
        proto.addEventListener = function (type, fn, opt) {
          if (type === 'message' && typeof fn === 'function') {
            return add.call(this, type, function (ev) { return deliverWs(fn, ev, this); }, opt);
          }
          return add.call(this, type, fn, opt);
        };
      }
      var desc = Object.getOwnPropertyDescriptor(proto, 'onmessage');
      if (desc && desc.set) {
        Object.defineProperty(proto, 'onmessage', {
          configurable: true,
          enumerable: !!desc.enumerable,
          get: function () { return this._lzOnMsg; },
          set: function (fn) {
            this._lzOnMsg = fn;
            desc.set.call(this, typeof fn === 'function' ? function (ev) { return deliverWs(fn, ev, this); } : fn);
          }
        });
      }
    }
    installWsCtor();
    window.addEventListener('load', installWsCtor);
  })();
  function fmtUsd(n) {
    n = Number(n);
    if (!isFinite(n)) return '—';
    return '$' + n.toLocaleString(undefined, { maximumFractionDigits: n >= 100 ? 0 : 2 });
  }
  function paintFiatChip() {
    var node = document.getElementById('lz-btc-price');
    if (!node || !blakeConv) return;
    node.textContent = fmtUsd(blakeConv.USD);
  }
  // Clock /mempool/:n Price is app-fiat(value=1e8) on websocket conversions$.
  // If the socket still carries SHA (~$80k), rewrite the rendered 1-BTC figure.
  function paintClockFiat() {
    if (!blakeConv || !(blakeConv.USD > 0)) return;
    var root = document.querySelector('app-clock');
    if (!root) return;
    var usd = blakeConv.USD;
    var formatted = usd.toLocaleString(undefined, { minimumFractionDigits: 2, maximumFractionDigits: 2 });
    var nodes = root.querySelectorAll('app-fiat span');
    for (var i = 0; i < nodes.length; i++) {
      var span = nodes[i];
      var t = span.textContent || '';
      var m = t.match(/([\d,]+(?:\.\d+)?)/);
      if (!m) continue;
      var n = Number(m[1].replace(/,/g, ''));
      if (!(n > 20000)) continue;
      span.textContent = t.replace(m[1], formatted);
    }
  }
  function pullBlakePrice() {
    fetch(POOL + '/api/price', { mode: 'cors' }).then(function (r) { return r.ok ? r.json() : null; })
      .then(function (d) {
        var c = convFromPrice(d);
        if (c) { blakeConv = c; paintFiatChip(); paintClockFiat(); }
      })
      .catch(function () { /* keep last */ });
  }
  pullBlakePrice();
  setInterval(pullBlakePrice, 60000);

  function fiatChip() {
    var ul = document.querySelector('header ul.navbar-nav');
    if (!ul || ul.querySelector('.lz-btc-price-item')) {
      paintFiatChip();
      return;
    }
    var li = el('li', { class: 'nav-item lz-btc-price-item' });
    var a = el('a', {
      id: 'lz-btc-price',
      class: 'nav-link lz-btc-price',
      href: 'https://neoxa.exchange/trade/BTCB2_USDC',
      target: '_blank',
      rel: 'noopener',
      'aria-label': 'BTCB2 price, opens Neoxa in a new tab',
      title: 'BLAKE2b BTC (BTCB2) · volume-weighted Neoxa BTCB2/USDC and NonKYC BTCB2/USDT'
    }, blakeConv ? fmtUsd(blakeConv.USD) : '…');
    li.appendChild(a);
    ul.insertBefore(li, ul.firstChild);
  }

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
        b.setAttribute('aria-label', 'Copied');
        clearTimeout(b._t);
        b._t = setTimeout(function () {
          b.removeAttribute('data-copied');
          b.setAttribute('aria-label', label || 'Copy');
        }, 1400);
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
              '<p class="lz-copy" id="lz-pool-copy">Every block found pays each miner directly in its coinbase by TIDES window share. 0% fee through your own DATUM gateway, 15% on the public stratum.</p>' +
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
        // With the DATUM bonus on, a gateway miner is paid more than its window share, and
        // the explorer's one paragraph about the pool is the place to say so.
        var uplift = Number(fees.datum_uplift_percent) || 0;
        c.textContent = 'Every block found pays each miner directly in its coinbase by TIDES window share. ' + pct(fees.datum_percent) + ' fee through your own DATUM gateway, ' + pct(fees.stratum_percent) + ' on the public stratum'
          + (Number(fees.datum_rebate_percent) > 0
            ? ' — and ' + pct(fees.datum_rebate_percent) + ' of public-stratum work is credited to DATUM miners in the window on every block'
              + (uplift > 0 ? ', worth +' + pct(uplift) + ' on their share right now.' : '.')
            : '.');
      }
      col.querySelector('.lz-dot').classList.add(pr.reachable === false ? 'stale' : 'live');
    });
  }

  function navOut(ul, cls, href, label, title) {
    if (ul.querySelector('.' + cls)) return;
    var li = el('li', { class: 'nav-item ' + cls });
    var a = el('a', {
      class: 'nav-link', href: href, target: '_blank', rel: 'noopener',
      title: title,
      'aria-label': label + ' (opens in a new tab)'
    });
    a.appendChild(el('span', { class: 'lz-label' }, label));
    li.appendChild(a);
    ul.appendChild(li);
  }

  function nav() {
    var ul = document.querySelector('header ul.navbar-nav');
    if (!ul) return;
    if (!ul.querySelector('.lz-pool-item')) {
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
    navOut(ul, 'lz-discord-item', DISCORD, 'Discord', 'Lazarus Discord');
    navOut(ul, 'lz-neoxa-item', NEOXA, 'Exchange', 'Sign up on Neoxa Exchange to trade BLAKE2b BTC (BTCB2)');
    navOut(ul, 'lz-mcp-item', MCP, 'AI assistant', 'Connect Claude, ChatGPT or any AI assistant to this chain and the pool (MCP server)');
  }

  function footer() {
    var tree = document.querySelector('app-global-footer .link-tree');
    if (tree && !tree.querySelector('.lz-links')) {
    var col = el('div', { class: 'links lz-links' });
    col.appendChild(el('p', { class: 'category' }, 'Lazarus'));
    var links = [
      [POOL, 'Lazarus Pool'],
      [POOL + '/#payout', 'What the next block pays'],
      ['/mining/pool/' + POOL_SLUG, 'Blocks found by the pool'],
      [POOL + '/#connect', 'Connect a miner or DATUM gateway'],
      [MCP, 'Ask your AI assistant (MCP server)'],
      [DISCORD, 'Discord'],
      [NEOXA, 'Exchange'],
      [REPO, 'Lazarus source'],
      [RETROPEX, 'Retropex/mempool'],
      [MEMPOOL_UPSTREAM, 'Mempool Open Source Project'],
      ['/lazarus/NOTICE', 'License notice (AGPL-3.0)']
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
    col.appendChild(el('p', { class: 'lz-footer-note lz-source-note' },
      'Lazarus-styled <a href="' + RETROPEX + '" target="_blank" rel="noopener">Retropex/mempool</a> ' +
      '(AGPL-3.0), itself <a href="' + MEMPOOL_UPSTREAM + '" target="_blank" rel="noopener">The Mempool Open Source Project</a>. ' +
      '<a href="/lazarus/NOTICE">NOTICE</a>'));
    // In front of "Legal" so the reading order stays Explore, Learn, Tools, Lazarus, Legal.
    var cols = tree.querySelectorAll('.links');
    var legal = cols.length ? cols[cols.length - 1] : null;
    if (legal) tree.insertBefore(col, legal); else tree.appendChild(col);
    }
    footerVersion();
  }

  function footerVersion() {
    var p = document.querySelector('footer .row.version p');
    if (!p || p.querySelector('.lz-theme-ver')) return;
    p.appendChild(document.createTextNode(' · '));
    p.appendChild(el('span', { class: 'lz-theme-ver', title: 'Lazarus theme over Retropex/mempool (AGPL-3.0)' },
      'Lazarus · Retropex ' + BUILD));
  }

  /* Replay-protected badge. Ported from Retropex/mempool tx-features +
   * transaction.utils (SIGHASH_UNIFIED 0x20), AGPL-3.0. Stock mempool v3.3.1
   * has no Features-row badge for BLAKE2b replay protection. */
  function isCanonicalDerSig(w) {
    if (!w || w.length < 18 || w.slice(0, 2) !== '30') return false;
    var compound = parseInt(w.slice(2, 4), 16);
    if (w.length !== (compound * 2) + 6 || w.slice(4, 6) !== '02') return false;
    var rLen = parseInt(w.slice(6, 8), 16);
    if (w.length < (rLen * 2) + 10) return false;
    var sEnd = 8 + (rLen * 2);
    if (w.slice(sEnd, sEnd + 2) !== '02') return false;
    var sLen = parseInt(w.slice(sEnd + 2, sEnd + 4), 16);
    return w.length === ((rLen + sLen) * 2) + 14;
  }
  function validSighash(n) {
    return (n >= 0 && n <= 3) || (n > 0x20 && n <= 0x23) || (n > 0x80 && n <= 0x83) || (n > 0xa0 && n <= 0xa3);
  }
  function vinSighashes(vin) {
    var out = [], i, hex, ops, w;
    w = vin.witness || [];
    for (i = 0; i < w.length; i++) {
      hex = w[i];
      if (isCanonicalDerSig(hex)) out.push(parseInt(hex.slice(-2), 16));
      else if (hex && hex.length === 130) out.push(parseInt(hex.slice(-2), 16));
      else if (hex && hex.length === 128) out.push(0);
    }
    ops = String(vin.scriptsig_asm || '').split(' ');
    for (i = 0; i < ops.length - 1; i++) {
      if (ops[i].indexOf('OP_PUSHBYTES_') === 0 && isCanonicalDerSig(ops[i + 1])) {
        out.push(parseInt(ops[i + 1].slice(-2), 16));
      }
    }
    return out.filter(function (n) { return validSighash(n) || n === 0; });
  }
  function classifyReplay(tx) {
    if (!tx || !tx.vin || !tx.vin.length || tx.vin[0].is_coinbase) return null;
    var h = tx.status && tx.status.confirmed ? Number(tx.status.block_height) : Infinity;
    if (h < FORK_HEIGHT) return null;
    var opted = 0, legacy = 0, i, sigs, allU;
    for (i = 0; i < tx.vin.length; i++) {
      sigs = vinSighashes(tx.vin[i]);
      if (!sigs.length) continue;
      allU = sigs.every(function (s) { return (s & 0x20) !== 0; });
      if (allU) opted++; else legacy++;
    }
    if (!opted && !legacy) return null;
    if (!legacy) return 'all';
    if (!opted) return 'none';
    return 'partial';
  }
  var replayCache = {};
  function replayBadge() {
    var host = document.querySelector('app-tx-features');
    if (!host || host.querySelector('.lz-replay')) return;
    var m = /\/tx\/([0-9a-fA-F]{64})/.exec(location.pathname);
    if (!m) return;
    var txid = m[1].toLowerCase();
    function paint(kind) {
      if (!kind || host.querySelector('.lz-replay')) return;
      var span = el('span', { class: 'badge lz-replay lz-replay-' + kind });
      if (kind === 'all') {
        span.className += ' bg-success';
        span.textContent = 'Replay protected';
        span.title = 'Every input is signed with SIGHASH_UNIFIED (0x20). Nodes without the BLAKE2b hardfork cannot verify these signatures, so this transaction cannot be replayed onto the SHA256d chain.';
      } else if (kind === 'partial') {
        span.className += ' bg-warning';
        span.textContent = 'Replay protected (partial)';
        span.title = 'Some inputs use the unified opt-in sighash and some do not. The transaction as a whole cannot be replayed, but the legacy-signed inputs offer no protection on their own.';
      } else {
        span.className += ' bg-danger';
        // Plain words: screen readers do not announce <del>, so a struck-through
        // "Replay protected" read as the opposite of what it meant.
        span.textContent = 'Not replay protected';
        span.title = 'No input is signed with SIGHASH_UNIFIED. This transaction is valid on both chains and can be replayed onto the SHA256d chain.';
      }
      host.appendChild(span);
    }
    if (replayCache[txid] !== undefined) { paint(replayCache[txid]); return; }
    if (replayCache[txid + ':p']) return;
    replayCache[txid + ':p'] = true;
    fetch('/api/tx/' + txid).then(function (r) { return r.ok ? r.json() : null; })
      .then(function (tx) {
        replayCache[txid] = classifyReplay(tx);
        paint(replayCache[txid]);
      })
      .catch(function () { delete replayCache[txid + ':p']; });
  }

  function forkBanner() {
    var title = document.querySelector('.title-block h1, .block-title, h1');
    var onFork = /\/block\/961640\/?$/.test(location.pathname)
      || (title && /\b961640\b/.test(title.textContent || ''));
    if (!onFork) return;
    var wrap = document.querySelector('app-block .container, app-block');
    if (!wrap || wrap.querySelector('.lz-fork-note')) return;
    wrap.insertBefore(el('p', { class: 'lz-fork-note' },
      '<b>BLAKE2b proof of work since block ' + FORK_HEIGHT.toLocaleString('en-US') + ' \u26cf\ufe0f</b> ' +
      'This is the first block mined with BLAKE2b instead of SHA-256. ' +
      'Transactions signed for this chain only are marked \u201cReplay protected\u201d on their Features row. ' +
      'Explorer based on <a href="' + RETROPEX + '" target="_blank" rel="noopener">Retropex/mempool</a>.'),
      wrap.firstChild);
  }

  function specialBlockBlink() {
    var blk = document.getElementById('bitcoin-block-961640');
    if (blk) blk.classList.add('blink-bg');
  }

  function aboutCite() {
    if (!/^\/about\/?$/.test(location.pathname)) return;
    var box = document.querySelector('app-about .about, app-about .container, app-about');
    if (!box || box.querySelector('.lz-about-source')) return;
    var note = el('div', { class: 'lz-about-source' });
    note.innerHTML = '<p><b>Lazarus Mempool</b> is a Lazarus-styled instance of ' +
      '<a href="' + RETROPEX + '" target="_blank" rel="noopener">Retropex/mempool</a> ' +
      '(the BLAKE2b explorer also running at mempool.guide), itself ' +
      '<a href="' + MEMPOOL_UPSTREAM + '" target="_blank" rel="noopener">The Mempool Open Source Project</a>, ' +
      'GNU AGPL-3.0. Trademarks of Mempool Holdings and mempool.guide are not used as this site\u2019s brand. ' +
      'Corresponding source: <a href="' + REPO + '" target="_blank" rel="noopener">AwokenLazarus/Bitcoin</a>, ' +
      '<a href="/lazarus/NOTICE">NOTICE</a>, <a href="/lazarus/LICENSE">LICENSE</a>.</p>';
    box.insertBefore(note, box.firstChild);
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
      // English-only: the alt text is translated in other locales and nothing else on the
      // badge carries the pool's display name, so there the badge simply stays stock.
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

  /* --- gateway bands in the mining-pool pie ---------------------------------------------
   * A slice of the pie on /graphs/mining/pools is one pool's blocks in the chosen window.
   * Pools that speak DATUM hand template building to their miners' own gateways, and every
   * such block carries the gateway operator's tag beside the pool's (see minerBadges), so a
   * slice can be read as the gateways that built it. That is what this draws: inside each
   * slice, one arc band per gateway tag, ordered smallest to largest outward so the biggest
   * gateway is on the rim, with the pool's untagged blocks -- its own stratum -- as the
   * innermost band.
   *
   * Band sizes are blocks per tag over the same window as the pie, from
   * /lazarus/pool-tags.json (written by pools/pools-sync.py with the same coinbase rule the
   * backend patch uses). That is the one metric any mempool instance can compute for any
   * pool, so the geometry is not Lazarus-specific; the live figures appended to the Lazarus
   * tooltip come from the pool's own API and never change a band's size.
   *
   * The chart is an ECharts pie drawn with the SVG renderer, and this bundle mangles the
   * echarts exports past recognition, so rather than reaching into the chart the whole pie
   * is redrawn from the pools API into an overlay SVG above it and the stock SVG is hidden
   * (see drawBands). If either data source is missing the stock pie is left untouched.
   * (An earlier design read the geometry back out of the rendered sectors; that code is gone.)
   */
  var TAGS_URL = '/lazarus/pool-tags.json';
  var SVGNS = 'http://www.w3.org/2000/svg';
  var PIE_START = 270;        // twelve o'clock in SVG angles, where the pie's first slice begins
  var BUILD = '15';
  var bandInfo = (self.__lazarusTheme || {}).bands = { state: 'idle', log: [] };
  function bandState(st) {
    if (bandInfo.state !== st) {
      bandInfo.log.push(new Date().toTimeString().slice(0, 8) + ' ' + st);
      if (bandInfo.log.length > 12) bandInfo.log.shift();
    }
    bandInfo.state = st;
  }
  var httpCache = {};

  // Small GET cache. A fetch that resolves schedules another apply(), so the bands appear as
  // soon as their data does; failures are not retried until the ttl is up.
  function cachedJson(key, url, ttl, opts) {
    var c = httpCache[key] || (httpCache[key] = { ts: 0, data: null, pending: false });
    var now = Date.now();
    if (!c.pending && now - c.ts >= ttl) {
      c.pending = true;
      c.ts = now;
      fetch(url, opts || {}).then(function (r) { return r.ok ? r.json() : null; })
        .then(function (d) {
          c.pending = false;
          if (d) { c.data = d; c.ts = Date.now(); schedule(); }
        })
        .catch(function () { c.pending = false; });
    }
    return c.data;
  }
  function cachedText(key, url, ttl) {
    var c = httpCache[key] || (httpCache[key] = { ts: 0, data: null, pending: false });
    var now = Date.now();
    if (!c.pending && now - c.ts >= ttl) {
      c.pending = true;
      c.ts = now;
      fetch(url).then(function (r) { return r.ok ? r.text() : null; })
        .then(function (d) {
          c.pending = false;
          if (d) { c.data = d.trim(); c.ts = Date.now(); schedule(); }
        })
        .catch(function () { c.pending = false; });
    }
    return c.data;
  }

  function bandWindow() {
    // The /mining dashboard widget is always the 1w luck pie; the graphs page stores its
    // own window in localStorage, which must not leak onto that widget.
    if (isMiningDash()) return '1w';
    try { return localStorage.getItem('miningWindowPreference') || '1w'; } catch (e) { return '1w'; }
  }
  function isPoolsGraph() { return /\/graphs\/mining\/pools/.test(location.pathname); }
  function isMiningDash() { return /^\/mining\/?$/.test(location.pathname); }
  function normTag(s) { return String(s == null ? '' : s).replace(/[^a-z0-9]/gi, '').toLowerCase(); }
  function hash32(s) {
    var h = 5381;
    for (var i = 0; i < s.length; i++) h = ((h << 5) + h + s.charCodeAt(i)) | 0;
    return (h >>> 0).toString(36);
  }
  function polar(cx, cy, r, deg) {
    var a = deg * Math.PI / 180;
    return { x: cx + r * Math.cos(a), y: cy + r * Math.sin(a) };
  }

  function bandSectorPath(cx, cy, r0, r1, a0, span) {
    var a1 = a0 + Math.min(span, 359.9);
    var p0 = polar(cx, cy, r1, a0), p1 = polar(cx, cy, r1, a1);
    var p2 = polar(cx, cy, r0, a1), p3 = polar(cx, cy, r0, a0);
    var big = a1 - a0 > 180 ? 1 : 0;
    function xy(p) { return p.x.toFixed(2) + ' ' + p.y.toFixed(2); }
    return 'M' + xy(p0) + 'A' + r1.toFixed(2) + ' ' + r1.toFixed(2) + ' 0 ' + big + ' 1 ' + xy(p1) +
           'L' + xy(p2) + 'A' + r0.toFixed(2) + ' ' + r0.toFixed(2) + ' 0 ' + big + ' 0 ' + xy(p3) + 'Z';
  }

  // Hashrate per band is the pool table's own estimate split by blocks, so a pool's bands add
  // up to the figure in its row. Upstream only estimates hashrate for the three short
  // windows, and neither do we.
  function windowHashrate(api, win) {
    if (win === '24h') return Number(api.lastEstimatedHashrate) || 0;
    if (win === '3d') return Number(api.lastEstimatedHashrate3d) || 0;
    if (win === '1w') return Number(api.lastEstimatedHashrate1w) || 0;
    return 0;
  }

  /* Live per-gateway figures for the pool's own slice. One coinbase tag can cover several
   * gateway connections -- an operator running more than one, plus sessions that have since
   * dropped -- so the matching rows are added up rather than picked from. Enrichment only: a
   * band's size always comes from blocks, so a renamed or unreachable gateway costs the
   * tooltip a line and nothing else. */
  function primeAgg(match) {
    var d = httpCache.gws && httpCache.gws.data;
    if (!d || !d.gateways) return null;
    var total = 0, work = 0, shares = 0, live = 0, seen = 0, path = '';
    d.gateways.forEach(function (g) {
      var w = g.offline ? 0 : (Number(g.work) || 0);
      total += w;
      if (!match(g)) return;
      seen++;
      if (g.offline) return;
      live++;
      work += w;
      shares += Number(g.accepted) || 0;
      if (g.fee_path) path = g.fee_path;
    });
    if (!seen || !total) return null;
    return { workShare: work / total, shares: shares, live: live, seen: seen, feePath: path };
  }
  function primeTag(tag) {
    var want = normTag(tag);
    if (!want) return null;
    return primeAgg(function (g) { return normTag(g.secondary_tag || g.name) === want; });
  }

  function bandTip(mount) {
    var tip = mount.querySelector('.lz-band-tip');
    if (!tip) {
      tip = el('div', { class: 'lz-band-tip' });
      mount.appendChild(tip);
    }
    return tip;
  }

  /* Sit on the chart SVG's own box, not the host's. The ECharts instance lives in a nested
   * relative div that is already a few pixels down from `.chart`; an overlay at left:0 top:0
   * of `.chart` is therefore that many pixels above the pie, and page zoom scales the gap. */
  function pinOverlay(ov, svg) {
    if (!ov || !svg || !ov.parentNode) return;
    var mb = ov.parentNode.getBoundingClientRect();
    var sb = svg.getBoundingClientRect();
    if (!sb.width) return;
    ov.style.left = (sb.left - mb.left) + 'px';
    ov.style.top = (sb.top - mb.top) + 'px';
    ov.style.width = sb.width + 'px';
    ov.style.height = sb.height + 'px';
  }

  // The chart's own sectors are faded by inline style while a band is hovered, so anything
  // that redraws has to be sure none is left behind.
  function undim(el) { el.style.opacity = ''; el.removeAttribute('data-lz-dim'); }
  function undimAll() {
    var left = document.querySelectorAll('app-pool-ranking [data-lz-dim]');
    for (var i = 0; i < left.length; i++) undim(left[i]);
    // A redraw replaces the overlay the hover handlers belong to, so a tooltip left open by
    // a zoom or a resize would sit there pinned to nothing.
    var tips = document.querySelectorAll('.lz-band-tip[data-shown]');
    for (var j = 0; j < tips.length; j++) tips[j].removeAttribute('data-shown');
  }

  /* Ctrl+wheel zoom and window resizes rewrite the chart's path data in place, which is not
   * a childList mutation, so the observer that drives everything else never hears about it
   * and the bands would sit on the old geometry until the next block or clock tick moved
   * something. A resize is not one event either: the viewport changes, then ECharts resizes
   * its SVG some frames later, so after any of them the overlay is checked against the
   * chart's width every frame for a while and redrawn the frame it stops matching. */
  var watchUntil = 0, watching = false;
  function bandsReflow() {
    bandInfo.sig = null;
    schedule();
    watchUntil = Date.now() + 900;
    if (watching) return;
    watching = true;
    (self.requestAnimationFrame || setTimeout)(watchResize);
  }
  function watchResize() {
    // Not just the size: the pie's centre moves inside an unchanged canvas when the labels
    // relayout. drawBands hashes the sector paths and returns early when they are the same,
    // so calling it per frame costs a hash and redraws only on the frame that moved.
    try { drawBands(); } catch (e) { bandState('error: ' + e); }
    if (Date.now() < watchUntil) (self.requestAnimationFrame || setTimeout)(watchResize);
    else watching = false;
  }

  function dropBands() {
    undimAll();
    var stale = document.querySelectorAll('svg.lz-bands, .lz-band-tip, .lz-bands-note');
    for (var i = 0; i < stale.length; i++) stale[i].remove();
    var hidden = document.querySelectorAll('svg[data-lz-stock="hidden"]');
    for (var h = 0; h < hidden.length; h++) {
      hidden[h].style.opacity = '';
      hidden[h].style.pointerEvents = '';
      hidden[h].removeAttribute('data-lz-stock');
    }
    bandInfo.sig = null;
  }

  /* Retropex/mempool pool-ranking pie (AGPL-3.0): every pool is a slice (no share
   * threshold), DATUM miners are bands in the wedge, hashrate tooltips are TH/s.
   * Stock v3.3.1 still folds small pools into Other and has no miner series, so
   * this draws that pie ourselves and hides the stock SVG. Geometry matches
   * frontend/src/app/components/pool-ranking/pool-ranking.component.ts at e56a2c6. */
  var POOL_COLORS = ['#dbb565','#e47164','#65c98c','#e5974c','#c586b7','#7aa3c8',
    '#a9ab54','#c26576','#63b4b8','#d2764a','#9bba7d','#a08dc3','#68b5a6','#937636','#a34a40','#4b6d8a'];
  var MAX_MINER_BANDS = 8;
  var MIN_BAND_DEPTH = 0.08;
  var BAND_LIGHTEST = 0.84;
  var BAND_DARKEST = 0.58;
  var MIN_LABEL_DEG = 8;

  function fmtTh(hs) {
    if (!(hs > 0)) return '';
    if (hs >= 1e18) return (hs / 1e18).toFixed(2) + ' EH/s';
    if (hs >= 1e15) return (hs / 1e15).toFixed(2) + ' PH/s';
    return (hs / 1e12).toFixed(2) + ' TH/s';
  }
  function retropexBandColor(color, lightness) {
    var hex = String(color || '').replace('#', '');
    if (hex.length === 3) hex = hex.split('').map(function (c) { return c + c; }).join('');
    if (hex.length < 6) return color;
    var r = parseInt(hex.slice(0, 2), 16) / 255, g = parseInt(hex.slice(2, 4), 16) / 255, b = parseInt(hex.slice(4, 6), 16) / 255;
    var max = Math.max(r, g, b), min = Math.min(r, g, b), chroma = max - min;
    var saturation = chroma === 0 ? 0 : chroma / (1 - Math.abs(max + min - 1));
    var hue = 0;
    if (chroma !== 0) {
      if (max === r) hue = ((g - b) / chroma) % 6;
      else if (max === g) hue = (b - r) / chroma + 2;
      else hue = (r - g) / chroma + 4;
      hue = (hue * 60 + 360) % 360;
    }
    var c = (1 - Math.abs(2 * lightness - 1)) * saturation;
    var x = c * (1 - Math.abs(((hue / 60) % 2) - 1));
    var m = lightness - c / 2;
    var rgb = hue < 60 ? [c, x, 0] : hue < 120 ? [x, c, 0] : hue < 180 ? [0, c, x]
      : hue < 240 ? [0, x, c] : hue < 300 ? [x, 0, c] : [c, 0, x];
    return '#' + rgb.map(function (ch) {
      var n = Math.round((ch + m) * 255);
      return (n < 16 ? '0' : '') + Math.max(0, Math.min(255, n)).toString(16);
    }).join('');
  }
  function retropexSliceBands(pool, entry, poolColor, a0, span) {
    var tags = (entry && entry.tags) || {};
    var miners = Object.keys(tags).map(function (n) { return { name: n, blockCount: tags[n] }; });
    miners.sort(function (a, b) { return b.blockCount - a.blockCount || (a.name < b.name ? -1 : 1); });
    if (!miners.length || !pool.blockCount || span <= 0) return [];
    var shown = miners.slice(0, MAX_MINER_BANDS);
    var shownBlocks = shown.reduce(function (s, m) { return s + m.blockCount; }, 0);
    var minerBlockCount = miners.reduce(function (s, m) { return s + m.blockCount; }, 0);
    var otherMinerBlocks = Math.max(0, minerBlockCount - shownBlocks);
    var poolBlocks = Math.max(0, pool.blockCount - shownBlocks - otherMinerBlocks);
    var ordered = [];
    if (poolBlocks > 0) ordered.push({ name: 'Built by the pool', blockCount: poolBlocks, kind: 'stratum' });
    if (otherMinerBlocks > 0) ordered.push({ name: 'Other miners', blockCount: otherMinerBlocks, kind: 'folded' });
    shown.slice().reverse().forEach(function (m) { ordered.push({ name: m.name, blockCount: m.blockCount, kind: 'gateway' }); });
    var totalBlocks = ordered.reduce(function (s, b) { return s + b.blockCount; }, 0);
    if (!totalBlocks) return [];
    var floor = Math.min(MIN_BAND_DEPTH, 0.5 / ordered.length);
    var toShare = 1 - floor * ordered.length;
    var depth = 0;
    return ordered.map(function (band, i) {
      var inner = depth;
      depth += floor + toShare * band.blockCount / totalBlocks;
      return {
        name: band.name, kind: band.kind, blocks: band.blockCount,
        poolName: pool.name, slug: pool.slug, own: pool.slug === POOL_SLUG,
        poolShare: (100 * band.blockCount / pool.blockCount),
        color: retropexBandColor(poolColor, BAND_DARKEST + (i / Math.max(ordered.length - 1, 1)) * (BAND_LIGHTEST - BAND_DARKEST)),
        a0: a0, span: span, inner: inner, outer: depth
      };
    });
  }

  function drawBands() {
    // The full pools graph and the /mining dashboard widget (1w luck pie). Other pies
    // (pool pages, tiny tiles) stay stock.
    if (!isPoolsGraph() && !isMiningDash()) {
      // Every other page: tear down once on the way out, then cost nothing per tick.
      if (bandInfo.state !== 'inactive') { bandState('inactive'); dropBands(); }
      return;
    }
    var host = document.querySelector('app-pool-ranking [_echarts_instance_]');
    var svg = host && host.querySelector('svg:not(.lz-bands)');
    if (!svg) { bandState('no chart yet'); dropBands(); return; }
    if (self.ResizeObserver && !host.__lzRO) {
      host.__lzRO = new self.ResizeObserver(bandsReflow);
      host.__lzRO.observe(host);
      host.__lzRO.observe(svg.parentNode || host);
    }

    var win = bandWindow();
    var api = cachedJson('pools-' + win, '/api/v1/mining/pools/' + win, 120000);
    var tagDoc = cachedJson('tags', TAGS_URL, 120000);
    if (!api || !tagDoc) { bandState('waiting for data'); return; }
    var wdoc = (tagDoc.windows || {})[win];
    if (!wdoc || !wdoc.pools) { bandState('no tag data for ' + win); dropBands(); return; }

    var sig = hash32([win, tagDoc.generated, api.blockCount, host.clientWidth, host.clientHeight,
      svg.getAttribute('width'), svg.getAttribute('height')].join(':'));
    if (bandInfo.sig === sig && host.querySelector('svg.lz-bands')) {
      pinOverlay(host.querySelector('svg.lz-bands'), svg);
      return;
    }

    var pools = api.pools || [];
    var totalBlocks = Number(api.blockCount) || pools.reduce(function (s, p) { return s + (Number(p.blockCount) || 0); }, 0);
    if (!totalBlocks || !pools.length) { bandState('no pools'); return; }

    svg.setAttribute('data-lz-stock', 'hidden');
    svg.style.opacity = '0';
    svg.style.pointerEvents = 'none';

    undimAll();
    var old = host.querySelector('svg.lz-bands');
    if (old) old.remove();
    var mount = svg.parentNode || host;
    var vw = parseFloat(svg.getAttribute('width')) || host.clientWidth || 800;
    var vh = parseFloat(svg.getAttribute('height')) || host.clientHeight || 400;
    var ring = isMiningDash() ? [0.15, 0.60] : [0.20, 0.80];
    var unit = Math.min(vw, vh) / 2;
    var cx = vw / 2, cy = vh / 2;
    var r0pie = ring[0] * unit, r1pie = ring[1] * unit;
    var netHs = windowHashrate(api, win);
    var ownSlice = null;
    pools.forEach(function (p) { if (p.slug === POOL_SLUG && wdoc.pools[POOL_SLUG]) ownSlice = p; });
    if (ownSlice) cachedJson('gws', POOL + '/api/gateways', 60000, { mode: 'cors' });

    var ov = document.createElementNS(SVGNS, 'svg');
    ov.setAttribute('class', 'lz-bands');
    ov.setAttribute('width', vw);
    ov.setAttribute('height', vh);
    ov.setAttribute('viewBox', '0 0 ' + vw + ' ' + vh);

    var cursor = PIE_START, drawn = 0, banded = 0, i;
    var slices = [];
    for (i = 0; i < pools.length; i++) {
      var pool = pools[i];
      var share = (Number(pool.blockCount) || 0) / totalBlocks;
      var span = share * 360;
      var color = POOL_COLORS[i % POOL_COLORS.length];
      var a0 = cursor;
      cursor += span;
      slices.push({ pool: pool, a0: a0, span: span, color: color, share: share });
      var base = document.createElementNS(SVGNS, 'path');
      base.setAttribute('d', bandSectorPath(cx, cy, r0pie, r1pie, a0, Math.max(span, 0.05)));
      base.setAttribute('fill', color);
      base.setAttribute('stroke', 'var(--lz-bg)');
      base.setAttribute('stroke-width', '1');
      base.setAttribute('data-lz-slice', pool.slug || '');
      if (pool.slug) {
        // One tab stop per pool; its bands lead to the same page, so they stay pointer-only.
        base.setAttribute('role', 'link');
        base.setAttribute('tabindex', '0');
        base.setAttribute('aria-label', pool.name + ', ' + (share * 100).toFixed(2) + '% of blocks, ' +
          pool.blockCount + ' block' + (pool.blockCount === 1 ? '' : 's'));
      }
      base.__lzSlice = pool;
      ov.appendChild(base);
      var entry = pool.slug ? wdoc.pools[pool.slug] : null;
      var bands = retropexSliceBands(pool, entry, color, a0, span);
      bands.forEach(function (b) {
        b.windowBlocks = totalBlocks;
        b.netHs = netHs;
        b.r0 = r0pie + b.inner * (r1pie - r0pie);
        b.r1 = r0pie + b.outer * (r1pie - r0pie);
        b.sector = { angle: a0, span: span };
        var path = document.createElementNS(SVGNS, 'path');
        path.setAttribute('d', bandSectorPath(cx, cy, b.r0, b.r1, a0, span));
        path.setAttribute('data-lz-band', b.kind);
        path.style.fill = b.color;
        path.__lzBand = b;
        ov.appendChild(path);
        drawn++;
      });
      if (bands.length) banded++;
      if (span >= MIN_LABEL_DEG) {
        var mid = a0 + span / 2;
        var rim = polar(cx, cy, r1pie + 8, mid);
        var lab = polar(cx, cy, r1pie + 28, mid);
        var right = lab.x >= cx;
        var lead = document.createElementNS(SVGNS, 'polyline');
        lead.setAttribute('class', 'lz-band-lead');
        lead.setAttribute('points', rim.x.toFixed(1) + ',' + rim.y.toFixed(1) + ' ' +
          lab.x.toFixed(1) + ',' + lab.y.toFixed(1));
        var text = document.createElementNS(SVGNS, 'text');
        text.setAttribute('class', 'lz-band-label');
        text.setAttribute('x', (lab.x + (right ? 4 : -4)).toFixed(1));
        text.setAttribute('y', lab.y.toFixed(1));
        text.setAttribute('dy', '0.32em');
        text.setAttribute('text-anchor', right ? 'start' : 'end');
        text.textContent = pool.name + ' (' + (share * 100).toFixed(2) + '%)';
        ov.appendChild(lead);
        ov.appendChild(text);
      }
    }

    mount.appendChild(ov);
    pinOverlay(ov, svg);

    var tip = bandTip(mount);
    var hot = null;
    function sliceTooltip(pool, share) {
      var hs = netHs > 0 ? fmtTh(netHs * share) : '';
      return '<b>' + esc(pool.name) + ' (' + (share * 100).toFixed(2) + '%)</b><span>' +
        (hs ? hs + ' · ' : '') + pool.blockCount + ' block' + (pool.blockCount === 1 ? '' : 's') + '</span>';
    }
    function bandTipHtml(b) {
      var hs = b.netHs > 0 && b.windowBlocks > 0 ? fmtTh(b.netHs * b.blocks / b.windowBlocks) : '';
      var lines = ['<b>' + esc(b.name) + '</b>',
        '<span>' + esc(b.poolName) + ' · ' + pct(b.poolShare) + '</span>',
        '<span>' + b.blocks + ' block' + (b.blocks === 1 ? '' : 's') + (hs ? ' · est. ' + hs : '') + '</span>'];
      if (b.own) {
        var live = b.kind === 'stratum' ? primeAgg(function (g) { return !!g.own; })
          : b.kind === 'gateway' ? primeTag(b.name) : null;
        if (live && live.live) {
          lines.push('<span class="lz-band-live">live: ' + pct(live.workShare * 100) + ' of the pool\'s work now</span>');
        }
      }
      return lines.join('');
    }
    function placeTip(html, x, y) {
      tip.innerHTML = html;
      tip.setAttribute('data-shown', '');
      var tw = tip.offsetWidth, th = tip.offsetHeight;
      tip.style.left = (parseFloat(ov.style.left) || 0) + Math.max(4, Math.min(x, vw - tw - 4)) + 'px';
      tip.style.top = (parseFloat(ov.style.top) || 0) + Math.max(4, Math.min(y, vh - th - 4)) + 'px';
    }
    function clearHot() {
      if (hot) { hot.classList.remove('lz-hot'); hot = null; }
      ov.removeAttribute('data-hover');
      var gone = ov.querySelectorAll('.lz-band-ring');
      for (var j = 0; j < gone.length; j++) gone[j].remove();
      var lit = ov.querySelectorAll('.lz-dim');
      for (var k = 0; k < lit.length; k++) lit[k].classList.remove('lz-dim');
      tip.removeAttribute('data-shown');
    }
    function setHot(p) {
      clearHot();
      hot = p;
      p.classList.add('lz-hot');
      ov.setAttribute('data-hover', '');
      var b = p.__lzBand, sl = p.__lzSlice;
      Array.prototype.forEach.call(ov.querySelectorAll('path'), function (o) {
        var slug = (o.__lzBand && o.__lzBand.slug) || (o.__lzSlice && o.__lzSlice.slug);
        var want = (b && b.slug) || (sl && sl.slug);
        if (slug && want && slug !== want) o.classList.add('lz-dim');
      });
      if (b) {
        var ring = document.createElementNS(SVGNS, 'path');
        ring.setAttribute('class', 'lz-band-ring');
        ring.setAttribute('d', bandSectorPath(cx, cy, Math.max(1, b.r0 - 1.6), b.r1 + 1.6, b.sector.angle, b.sector.span));
        ov.appendChild(ring);
        placeTip(bandTipHtml(b), cx + r1pie + 12, cy - 40);
      } else if (sl) {
        placeTip(sliceTooltip(sl, sl.blockCount / totalBlocks), cx + r1pie + 12, cy - 40);
      }
    }
    ov.addEventListener('mousemove', function (ev) {
      var p = ev.target;
      if (p && (p.__lzBand || p.__lzSlice) && p !== hot) setHot(p);
    });
    ov.addEventListener('mouseleave', clearHot);
    function follow(t) {
      var slug = (t && t.__lzBand && t.__lzBand.slug) || (t && t.__lzSlice && t.__lzSlice.slug);
      if (slug) location.href = '/mining/pool/' + slug;
      return !!slug;
    }
    ov.addEventListener('click', function (ev) { follow(ev.target); });
    // Keyboard: focus shows what hover shows, Enter / Space does what a click does.
    ov.addEventListener('focusin', function (ev) { if (ev.target && ev.target.__lzSlice) setHot(ev.target); });
    ov.addEventListener('focusout', clearHot);
    ov.addEventListener('keydown', function (ev) {
      if (ev.key !== 'Enter' && ev.key !== ' ' && ev.key !== 'Spacebar') return;
      if (follow(ev.target)) ev.preventDefault();
    });

    var wrap = host.parentNode;
    if (isPoolsGraph() && wrap) {
      var note = wrap.querySelector('.lz-bands-note');
      if (!note) {
        note = el('p', { class: 'lz-bands-note' }, '');
        wrap.insertBefore(note, host.nextSibling);
      }
      note.innerHTML = '<b>Pool hashrate pie</b> after <a href="' + RETROPEX + '" target="_blank" rel="noopener">Retropex/mempool</a>: every pool is a slice (no Other-fold), ' +
        'DATUM miners are the outer bands (largest on the rim), the inner band is blocks the pool built itself. ' +
        'Hashrate is block-share \u00d7 estimated network hashrate, in TH/s.';
    }
    bandState('ok');
    bandInfo.pools = banded;
    bandInfo.drawn = drawn;
    bandInfo.slices = slices.length;
    bandInfo.window = win;
    bandInfo.sig = sig;
  }

  /* Retropex common.ts getBlockHeaderV2Fields (AGPL-3.0). Stock extras omit headerV2. */
  function hexLe32(n) {
    return ('00000000' + (n >>> 0).toString(16)).slice(-8);
  }
  function parseHeaderV2(hex) {
    hex = String(hex || '').replace(/\s+/g, '');
    if (hex.length < 328) return null;
    var ver = parseInt(hex.slice(6, 8) + hex.slice(4, 6) + hex.slice(2, 4) + hex.slice(0, 2), 16);
    if ((ver & 0x80000000) === 0) return null;
    function le32(s) { return parseInt(s.slice(6, 8) + s.slice(4, 6) + s.slice(2, 4) + s.slice(0, 2), 16); }
    return {
      nonce2: le32(hex.slice(160, 168)),
      nonce3: le32(hex.slice(168, 176)),
      extranonce: hex.slice(176, 208),
      h1Flags: parseInt(hex.slice(220, 222), 16),
      xorKeyMaskClearBits: parseInt(hex.slice(222, 224), 16),
      xorKey: hex.slice(224, 256)
    };
  }
  function pageBlockHash() {
    var m = /\/block\/([0-9a-fA-F]{64})/.exec(location.pathname);
    if (m) return m[1].toLowerCase();
    var rows = document.querySelectorAll('app-block table tr');
    var firstHex = null;
    for (var i = 0; i < rows.length; i++) {
      var tds = rows[i].querySelectorAll('td');
      if (tds.length < 2) continue;
      var t = (tds[1].textContent || '').replace(/\s+/g, '');
      if (!/^[0-9a-f]{64}$/i.test(t)) continue;
      if (/^\s*Hash\s*$/i.test(tds[0].textContent || '')) return t.toLowerCase();
      if (!firstHex) firstHex = t.toLowerCase();
    }
    // Other locales translate the "Hash" label: the block hash is the first 64-hex value in
    // the block's tables. A wrong guess only costs a 404 on the header fetch, i.e. no rows.
    return firstHex;
  }
  function headerV2Rows() {
    if (!/\/block\//.test(location.pathname)) return;
    var headerCell = null;
    var tds = document.querySelectorAll('app-block table td');
    for (var i = 0; i < tds.length; i++) {
      if (/Block Header Hex/i.test((tds[i].textContent || '').trim())) { headerCell = tds[i]; break; }
    }
    // Locale-robust fallback: the header row is the one whose value is a long run of hex
    // (80 bytes stock, 164 here), whatever its label is called.
    for (var j = 0; !headerCell && j < tds.length; j++) {
      var prev = tds[j].previousElementSibling;
      if (prev && /^[0-9a-f]{160,}$/i.test((tds[j].textContent || '').replace(/\s+/g, ''))) headerCell = prev;
    }
    if (!headerCell) return;
    var tbody = headerCell.parentNode && headerCell.parentNode.parentNode;
    if (!tbody || tbody.querySelector('.lz-header-v2')) return;
    var id = pageBlockHash();
    if (!id) return;
    var hex = cachedText('hdr-' + id, '/api/block/' + id + '/header', 300000);
    var v2 = parseHeaderV2(hex);
    if (!v2) return;
    function row(label, valueHtml) {
      var tr = el('tr', { class: 'lz-header-v2' });
      tr.appendChild(el('td', {}, esc(label)));
      var td = el('td', {});
      td.innerHTML = valueHtml;
      tr.appendChild(td);
      return tr;
    }
    var after = headerCell.parentNode;
    var rows = [
      row('Header version', '2 <span class="badge bg-success ms-1" title="BLAKE2b proof-of-work header">BLAKE2b</span>'),
      row('Nonce2', esc(hexLe32(v2.nonce2))),
      row('Nonce3', esc(hexLe32(v2.nonce3))),
      row('Extranonce', '<p class="break-all">' + esc(v2.extranonce) + '</p>'),
      row('H1 flags', esc(String(v2.h1Flags))),
      row('XOR key', '<p class="break-all">' + esc(v2.xorKey) + '</p>'),
      row('XOR key mask clear bits', esc(String(v2.xorKeyMaskClearBits)))
    ];
    var next = after.nextSibling;
    for (var r = 0; r < rows.length; r++) tbody.insertBefore(rows[r], next);
  }

  /* Retropex mining.service selectedPower = 12 (TH/s). Stock v3.3.1 uses 18 (EH/s)
   * and rounds this network to 0.01 / 0.00 EH/s. */
  function fmtThForced(hs) {
    if (!(hs > 0)) return '0 TH/s';
    return (hs / 1e12).toLocaleString(undefined, { maximumFractionDigits: 2 }) + ' TH/s';
  }
  function miningThs() {
    if (!isPoolsGraph() && !isMiningDash()) return;
    var win = bandWindow();
    var api = cachedJson('pools-' + win, '/api/v1/mining/pools/' + win, 120000);
    if (!api) return;
    var netHs = windowHashrate(api, win);
    var total = Number(api.blockCount) || 0;
    if (!netHs || !total) return;
    var byName = {};
    (api.pools || []).forEach(function (p) { byName[String(p.name).toLowerCase()] = p; });
    var tables = document.querySelectorAll('app-pool-ranking table');
    for (var t = 0; t < tables.length; t++) {
      var ths = tables[t].querySelectorAll('thead th');
      var hi = -1, pi = -1, i;
      for (i = 0; i < ths.length; i++) {
        var h = (ths[i].textContent || '').trim();
        if (h === 'Hashrate') hi = i;
        if (h === 'Pool') pi = i;
      }
      // The header labels above are English; in other locales find the columns by what is
      // in them: the pool column links to /mining/pool/, the hashrate column ends in H/s.
      var probe = tables[t].querySelector('tbody tr');
      for (i = 0; probe && i < probe.children.length; i++) {
        if (pi < 0 && probe.children[i].querySelector('a[href*="/mining/pool/"]')) pi = i;
        if (hi < 0 && /H\/s\s*$/.test(probe.children[i].textContent || '')) hi = i;
      }
      if (hi < 0 || pi < 0) continue;
      var rows = tables[t].querySelectorAll('tbody tr');
      for (var r = 0; r < rows.length; r++) {
        var cells = rows[r].children;
        if (cells.length <= hi) continue;
        var name = (cells[pi].textContent || '').trim();
        var p = byName[name.toLowerCase()];
        if (!p) {
          var keys = Object.keys(byName);
          for (var k = 0; k < keys.length; k++) {
            if (name.toLowerCase().indexOf(keys[k]) !== -1) { p = byName[keys[k]]; break; }
          }
        }
        var label = (name || '').toLowerCase();
        var hs;
        // English-only: the totals row has no link or class to go by.
        if (!p && /all miners|total/.test(label)) hs = netHs;
        else if (p) hs = netHs * (Number(p.blockCount) || 0) / total;
        else continue;
        var sig = (p ? String(p.blockCount) : 'net') + ':' + win;
        if (cells[hi].getAttribute('data-lz-ths') === sig) continue;
        cells[hi].setAttribute('data-lz-ths', sig);
        cells[hi].textContent = fmtThForced(hs);
        cells[hi].title = 'Estimated hashrate after Retropex/mempool (TH/s, not EH/s)';
      }
    }
  }

  var SIGHASH_LABELS = {
    0: 'SIGHASH_DEFAULT', 1: 'SIGHASH_ALL', 2: 'SIGHASH_NONE', 3: 'SIGHASH_SINGLE',
    33: 'SIGHASH_ALL | UNIFIED', 34: 'SIGHASH_NONE | UNIFIED', 35: 'SIGHASH_SINGLE | UNIFIED',
    129: 'SIGHASH_ALL | ACP', 130: 'SIGHASH_NONE | ACP', 131: 'SIGHASH_SINGLE | ACP',
    161: 'SIGHASH_ALL | ACP | UNIFIED', 162: 'SIGHASH_NONE | ACP | UNIFIED', 163: 'SIGHASH_SINGLE | ACP | UNIFIED'
  };
  function sighashKeys() {
    var cells = document.querySelectorAll('app-transactions-list .sig-td');
    if (!cells.length) return;
    var m = /\/tx\/([0-9a-fA-F]{64})/.exec(location.pathname);
    if (!m) return;
    var tx = cachedJson('tx-' + m[1].toLowerCase(), '/api/tx/' + m[1], 300000);
    if (!tx || !tx.vin) return;
    var n = Math.min(cells.length, tx.vin.length);
    for (var i = 0; i < n; i++) {
      if (cells[i].querySelector('.lz-sighash')) continue;
      var sigs = vinSighashes(tx.vin[i]);
      var u = sigs.filter(function (s) { return (s & 0x20) !== 0; });
      if (!u.length) continue;
      var lock = cells[i].querySelector('.sig-no-lock');
      if (lock) lock.remove();
      var span = el('span', {
        class: 'sig sig-key lz-sighash sighash-' + u[0],
        title: SIGHASH_LABELS[u[0]] || ('SIGHASH 0x' + u[0].toString(16))
      }, 'UNIFIED');
      cells[i].appendChild(span);
    }
  }

  /* Document title. Angular writes "<page> - mempool - Bitcoin Explorer" (or just the tail on
   * the dashboard); only that brand tail is rewritten. The result no longer matches the
   * pattern, and nothing is written when the title is already right, so this cannot fight
   * Angular in a loop. */
  var TITLE_RE = /(^|\s-\s)mempool(?:\s-\s.*)?$/;
  function brandTitle() {
    var t = document.title || '';
    var m = TITLE_RE.exec(t);
    if (!m) return;
    var next = t.slice(0, m.index) + m[1] + 'Lazarus Mempool' + (m[1] ? '' : ' - BLAKE2b BTC Explorer');
    if (next !== t) document.title = next;
  }

  /* Fee box: on this chain the recommended tiers are usually all the same (1 sat/vB), and
   * four identical numbers read as a broken widget. Say what it means, only while it is
   * true. Compares the rendered numbers, so it is locale-independent. */
  function feeNote() {
    var boxes = document.querySelectorAll('app-fees-box');
    for (var b = 0; b < boxes.length; b++) {
      var cells = boxes[b].querySelectorAll('.fee-estimation-container:not(.loading-container) .fee-text');
      var same = cells.length >= 3, first = null;
      for (var i = 0; same && i < cells.length; i++) {
        var m = /\d[\d.,\s]*/.exec(cells[i].textContent || '');
        if (!m) same = false;
        else if (first === null) first = m[0].trim();
        else if (m[0].trim() !== first) same = false;
      }
      var note = boxes[b].querySelector('.lz-fee-note');
      if (same && !note) {
        boxes[b].appendChild(el('p', { class: 'lz-fee-note' },
          'Mempool is clear: any fee confirms in the next block.'));
      } else if (!same && note) note.remove();
    }
  }

  /* Text on fee-coloured faces. The projected blocks and the fee-priority bar are painted by
   * inline gradients from the fee ramp with white text on top; on the brass / amber steps
   * white drops under 3:1. Mark a face whose fee colours are bright so theme.css can switch
   * it to dark ink (crossover with the dark ink is relative luminance ~0.197). The grey
   * #554b45 "unfilled" part of a projected block is not a fee colour; a face that is mostly
   * unfilled keeps white. Anything unparseable keeps the stock white. */
  function relLum(r, g, b) {
    function f(c) { c /= 255; return c <= 0.04045 ? c / 12.92 : Math.pow((c + 0.055) / 1.055, 2.4); }
    return 0.2126 * f(r) + 0.7152 * f(g) + 0.0722 * f(b);
  }
  function faceInk() {
    var faces = document.querySelectorAll('.mempool-block.bitcoin-block, app-fees-box .fee-progress-bar');
    for (var i = 0; i < faces.length; i++) {
      var f = faces[i], bg = f.style.backgroundImage || f.style.background || f.style.backgroundColor || '';
      if (f.__lzBg === bg) continue;
      f.__lzBg = bg;
      var re = /rgba?\(\s*(\d+),\s*(\d+),\s*(\d+)[^)]*\)(?:\s+([\d.]+)%)?/g, m, sum = 0, n = 0, empty = 0;
      while ((m = re.exec(bg))) {
        if (+m[1] === 85 && +m[2] === 75 && +m[3] === 69) { if (m[4]) empty = Math.max(empty, +m[4]); continue; }
        sum += relLum(+m[1], +m[2], +m[3]); n++;
      }
      f.classList.toggle('lz-face-bright', n > 0 && empty < 50 && sum / n > 0.197);
    }
  }

  var scheduled = false, dirty = false;
  function apply() {
    scheduled = false;
    try { nav(); } catch (e) { /* never break the explorer */ }
    try { fiatChip(); } catch (e) { /* never break the explorer */ }
    try { footer(); } catch (e) { /* never break the explorer */ }
    try { dashboard(); } catch (e) { /* never break the explorer */ }
    try { minerBadges(); } catch (e) { /* never break the explorer */ }
    try { paintClockFiat(); } catch (e) { /* never break the explorer */ }
    try { replayBadge(); } catch (e) { /* never break the explorer */ }
    try { forkBanner(); } catch (e) { /* never break the explorer */ }
    try { specialBlockBlink(); } catch (e) { /* never break the explorer */ }
    try { aboutCite(); } catch (e) { /* never break the explorer */ }
    try { headerV2Rows(); } catch (e) { /* never break the explorer */ }
    try { miningThs(); } catch (e) { /* never break the explorer */ }
    try { sighashKeys(); } catch (e) { /* never break the explorer */ }
    try { feeNote(); } catch (e) { /* never break the explorer */ }
    try { faceInk(); } catch (e) { /* never break the explorer */ }
    try { drawBands(); } catch (e) { bandState('error: ' + e); }
  }
  // Coalesced: however many DOM mutations a websocket frame causes, apply() runs at most once
  // per animation frame, and not at all in a background tab (it catches up on return).
  function schedule() {
    if (scheduled) return;
    if (document.hidden) { dirty = true; return; }
    scheduled = true;
    (window.requestAnimationFrame || setTimeout)(apply);
  }

  function start() {
    try { localStorage.removeItem('lzdebug'); } catch (e) { /* private mode */ }
    var leftover = document.getElementById('lz-debug');
    if (leftover) leftover.remove();
    apply();
    // Last line of defence for the bands: whatever moves the chart -- a zoom, a resize, a
    // re-render that drops the overlay -- this notices within a tick. drawBands hashes the
    // sector paths and returns immediately when they are unchanged and the overlay is still
    // there, so the pools pages pay a hash four times a second; every other page, and any
    // background tab, pays a route test and nothing else.
    setInterval(function () {
      if (document.hidden) return;
      try { drawBands(); } catch (e) { bandState('error: ' + e); }
    }, 250);
    new MutationObserver(schedule).observe(document.body, { childList: true, subtree: true });
    document.addEventListener('visibilitychange', function () {
      if (!document.hidden && dirty) { dirty = false; schedule(); }
    });
    // <title> lives in <head>, outside the body observer; Angular replaces its text node.
    var titleEl = document.querySelector('head > title');
    if (titleEl) new MutationObserver(brandTitle).observe(titleEl, { childList: true, characterData: true, subtree: true });
    brandTitle();
    self.addEventListener('resize', bandsReflow, { passive: true });
    if (self.visualViewport) self.visualViewport.addEventListener('resize', bandsReflow, { passive: true });
  }
  if (document.body) start(); else document.addEventListener('DOMContentLoaded', start);
})();
