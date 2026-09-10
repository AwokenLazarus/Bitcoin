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
  var POOL = 'https://pool.awokenlazarus.xyz';
  var REPO = 'https://github.com/AwokenLazarus/Bitcoin';
  var DISCORD = 'https://discord.gg/fD33dJXnzz';
  var NEOXA = 'https://neoxa.exchange/register?ref=NEXB9423E49';
  var POOL_SLUG = 'lazarus';
  var ELECTRUM = 'electrum.awokenlazarus.xyz:50002';

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
        try { url = typeof input === 'string' ? input : (input && input.url) || ''; } catch (e) { url = ''; }
        var addr = rewriteAddress(url);
        if (addr) {
          return addr.then(jsonResponse).catch(function () {
            return ofetch.apply(window, arguments);
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
  // DATUM bonus is N percentage *points* of the stratum fee: "one point of the 3%"
  // (其中一个百分点). Never a fraction of the fee (pct() of the rebate on "that fee").
  function pointsOfFee(points, feePercent) {
    var n = Number(points);
    var fee = Number(feePercent);
    if (!isFinite(n) || n <= 0 || !isFinite(fee)) return '';
    var rounded = Math.round(n * 1000) / 1000;
    var word = rounded === 1 ? 'one point' : rounded === 2 ? 'two points' : (String(rounded) + ' points');
    return word + ' of the ' + pct(fee);
  }

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
              '<p class="lz-copy" id="lz-pool-copy">Every block found pays each miner directly in its coinbase by TIDES window share. 0% fee through your own DATUM gateway, 3% on the public stratum — and one point of the 3% is credited to the DATUM miners in the window on every block.</p>' +
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
        var bonus = Number(fees.datum_rebate_percent) > 0 ? pointsOfFee(fees.datum_rebate_percent, fees.stratum_percent) : '';
        c.textContent = 'Every block found pays each miner directly in its coinbase by TIDES window share. ' + pct(fees.datum_percent) + ' fee through your own DATUM gateway, ' + pct(fees.stratum_percent) + ' on the public stratum'
          + (bonus
            ? ' — and ' + bonus + ' is credited to the DATUM miners in the window on every block'
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
      [DISCORD, 'Discord'],
      [NEOXA, 'Exchange'],
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
    footerVersion();
  }

  function footerVersion() {
    var p = document.querySelector('footer .row.version p');
    if (!p || p.querySelector('.lz-theme-ver')) return;
    p.appendChild(document.createTextNode(' · '));
    p.appendChild(el('span', { class: 'lz-theme-ver', title: 'Lazarus theme: DATUM gateway bands in the mining pie' },
      'Lazarus bands ' + BUILD));
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
   * echarts exports past recognition, so rather than reaching into the chart the geometry is
   * read back out of the rendered sectors -- centre by circle fit, radii and angles from the
   * path data -- and the bands go into an overlay SVG above it. The reading is checked
   * against the pools API before anything is drawn: if the sectors disagree with the shares
   * the API reports, or either source is missing, the stock pie is left untouched.
   */
  var TAGS_URL = '/lazarus/pool-tags.json';
  var FOLD_SHARE = 0.01;      // gateways under 1% of the pool share one band
  var MIN_BAND_PX = 3.5;      // every band stays visible; the rest is share-proportional
  var MIN_SLICE_DEG = 3;      // narrower slices cannot show a readable band
  var SVGNS = 'http://www.w3.org/2000/svg';
  var PIE_START = 270;        // twelve o'clock in SVG angles, where the pie's first slice begins
  var BUILD = '9';
  var LABEL_STEPS = [0, -15, 15, -30, 30, -46, 46];   // where a hover label may sit, in order
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
  function norm360(a) { a %= 360; return a < 0 ? a + 360 : a; }

  var NUM = '(-?[0-9.]+(?:e[-+]?[0-9]+)?)';
  var ARC_RE = new RegExp('A' + NUM + '[\\s,]+' + NUM + '[\\s,]+' + NUM + '[\\s,]+([01])[\\s,]+([01])[\\s,]+' + NUM + '[\\s,]+' + NUM, 'gi');
  var MOVE_RE = new RegExp('^M[\\s,]*' + NUM + '[\\s,]+' + NUM);

  // Kasa circle fit: solve x^2+y^2 = ax + by + c for the outer-arc points, which all sit on
  // the pie's outer circle. Gives the centre without assuming echarts' default of 50%/50%.
  function fitCircle(pts) {
    var n = pts.length, sx = 0, sy = 0, sxx = 0, syy = 0, sxy = 0, sz = 0, sxz = 0, syz = 0, i, p, z;
    if (n < 3) return null;
    for (i = 0; i < n; i++) {
      p = pts[i]; z = p.x * p.x + p.y * p.y;
      sx += p.x; sy += p.y; sxx += p.x * p.x; syy += p.y * p.y; sxy += p.x * p.y;
      sz += z; sxz += p.x * z; syz += p.y * z;
    }
    var m = [[sxx, sxy, sx], [sxy, syy, sy], [sx, sy, n]], v = [sxz, syz, sz];
    function det3(a) {
      return a[0][0] * (a[1][1] * a[2][2] - a[1][2] * a[2][1])
           - a[0][1] * (a[1][0] * a[2][2] - a[1][2] * a[2][0])
           + a[0][2] * (a[1][0] * a[2][1] - a[1][1] * a[2][0]);
    }
    var d = det3(m);
    if (!isFinite(d) || Math.abs(d) < 1e-9) return null;
    var sol = [0, 1, 2].map(function (col) {
      var a = m.map(function (row, ri) { return row.map(function (x, ci) { return ci === col ? v[ri] : x; }); });
      return det3(a) / d;
    });
    var cx = sol[0] / 2, cy = sol[1] / 2;
    var rr = sol[2] + cx * cx + cy * cy;
    if (!(rr > 0)) return null;
    return { cx: cx, cy: cy, r: Math.sqrt(rr) };
  }

  /* Read the pie back out of its own SVG. Filled closed paths are the sectors. Radii come
   * from each path's own arcs (the 1px corner arcs from itemStyle.borderRadius are skipped).
   * Order comes from start-edge angles, not document order: ECharts moves the hovered
   * sector to the end of the SVG so it paints on top. */
  function readPie(svg) {
    var all = svg.querySelectorAll('path'), sectors = [], outer = [], i, p, d, m, a;
    for (i = 0; i < all.length; i++) {
      p = all[i];
      d = p.getAttribute('d') || '';
      if (!(p.getAttribute('fill') || 'none').match(/^(#|rgb)/i) || !/Z\s*$/.test(d)) continue;
      m = MOVE_RE.exec(d);
      if (!m) return null;
      var arcs = [], lo = Infinity, hi = 0;
      ARC_RE.lastIndex = 0;
      while ((a = ARC_RE.exec(d))) {
        var arc = { r: parseFloat(a[1]), x: parseFloat(a[6]), y: parseFloat(a[7]) };
        arcs.push(arc);
        if (arc.r > 2) { lo = Math.min(lo, arc.r); hi = Math.max(hi, arc.r); }  // skip corner arcs
      }
      if (!arcs.length || !hi) return null;
      sectors.push({ el: p, x: parseFloat(m[1]), y: parseFloat(m[2]), arcs: arcs,
                     r: hi, r0: lo === hi ? 0 : lo });
    }
    if (!sectors.length) return null;

    /* Radii are read per sector and the pie's own radius is the one most sectors share.
     * The sector under the pointer is not the same size as the others -- ECharts scales it
     * on hover -- and mid-resize the chart can briefly hold two sizes at once. Taking the
     * largest radius anywhere would follow the hovered sector and draw every band into the
     * few pixels it grew by; refusing to read at all would blank the bands whenever the
     * pointer crosses the chart. So: the majority sets the pie, and a sector that differs
     * keeps its own radii and has its bands drawn to match. */
    var tally = {}, best = null;
    sectors.forEach(function (s) {
      var k = s.r.toFixed(1);
      tally[k] = (tally[k] || 0) + 1;
      if (!best || tally[k] > tally[best] || (tally[k] === tally[best] && s.r < parseFloat(best))) best = k;
    });
    var r = parseFloat(best), base = sectors.filter(function (s) { return Math.abs(s.r - r) < 0.6; });
    if (base.length * 2 < sectors.length) return null;      // no majority: a half-drawn chart
    var inner = {}, bestIn = null;
    base.forEach(function (s) {
      var k = s.r0.toFixed(1);
      inner[k] = (inner[k] || 0) + 1;
      if (!bestIn || inner[k] > inner[bestIn]) bestIn = k;
    });
    var r0 = parseFloat(bestIn);
    if (!(r > 4) || !(r0 >= 0) || r0 >= r) return null;
    base.forEach(function (s) {
      outer.push({ x: s.x, y: s.y });
      s.arcs.forEach(function (arc) { if (Math.abs(arc.r - r) < 0.5) outer.push({ x: arc.x, y: arc.y }); });
    });
    var fit = fitCircle(outer);
    if (!fit || Math.abs(fit.r - r) > 3) return null;

    /* Order comes from the angles, not from the document: ECharts moves the sector under
     * the pointer to the end of the SVG so it paints on top, and reading spans between
     * whatever paths happen to be adjacent then gives nonsense. Sorted clockwise from
     * twelve o'clock -- where the pie starts -- the sectors are back in data order, which
     * the per-slice share check downstream then confirms. */
    var n = sectors.length;
    sectors.forEach(function (s) { s.angle = norm360(Math.atan2(s.y - fit.cy, s.x - fit.cx) * 180 / Math.PI); });
    sectors.sort(function (a2, b2) { return norm360(a2.angle - PIE_START) - norm360(b2.angle - PIE_START); });
    var sum = 0;
    sectors.forEach(function (s, k) {
      s.span = n === 1 ? 360 : norm360(sectors[(k + 1) % n].angle - s.angle);
      sum += s.span;
    });
    if (Math.abs(sum - 360) > 1) return null;      // not one pie
    return { cx: fit.cx, cy: fit.cy, r: r, r0: r0, sectors: sectors };
  }

  /* What the pools API says the slices are, in the order the chart draws them. The chart
   * keeps the pools above a share threshold that depends on the viewport and sweeps the
   * rest into "Other", but the threshold itself does not have to be known: the API is
   * already sorted by blocks, so n sectors means the first n-1 pools and an Other. Every
   * span is checked against these shares afterwards, which is what actually proves it. */
  function expectedSlices(api, n) {
    var total = Number(api && api.blockCount) || 0;
    if (!total || !api.pools || n < 1 || n > api.pools.length + 1) return null;
    function take(k, withOther) {
      var keep = [], rest = 0;
      api.pools.forEach(function (p, i) {
        var share = p.blockCount / total * 100;
        if (i < k) keep.push({ slug: p.slug, name: p.name, blocks: p.blockCount, share: share });
        else rest += share;
      });
      if (withOther) keep.push({ slug: null, name: 'Other', blocks: null, share: rest });
      return keep.length === n ? keep : null;
    }
    return take(n - 1, true) || take(n, false);
  }

  /* The bands of one pool, innermost first: untagged blocks, then the gateways that are too
   * small to draw on their own, then the rest ascending so the largest ends up on the rim. */
  function poolBands(entry) {
    var blocks = Number(entry && entry.blocks) || 0;
    var names = Object.keys((entry && entry.tags) || {});
    if (!blocks || !names.length) return null;
    var tagged = names.map(function (t) { return { label: t, blocks: entry.tags[t], kind: 'gateway' }; });
    tagged.sort(function (a, b) { return a.blocks - b.blocks || (a.label < b.label ? -1 : 1); });
    var small = tagged.filter(function (b) { return b.blocks / blocks < FOLD_SHARE; });
    var out = [];
    if (entry.untagged > 0) out.push({ label: null, blocks: entry.untagged, kind: 'stratum' });
    if (small.length > 1) {
      tagged = tagged.filter(function (b) { return small.indexOf(b) < 0; });
      out.push({
        label: small.length + ' smaller gateways', kind: 'folded', members: small,
        blocks: small.reduce(function (s, b) { return s + b.blocks; }, 0)
      });
    }
    return out.concat(tagged);
  }

  function parseRgb(css) {
    var m = /^#([0-9a-f]{3}|[0-9a-f]{6})$/i.exec(String(css || ''));
    if (m) {
      var x = m[1];
      if (x.length === 3) x = x.split('').map(function (c) { return c + c; }).join('');
      return [parseInt(x.slice(0, 2), 16), parseInt(x.slice(2, 4), 16), parseInt(x.slice(4, 6), 16)];
    }
    m = /^rgba?\(\s*(\d+)[\s,]+(\d+)[\s,]+(\d+)/i.exec(String(css || ''));
    return m ? [+m[1], +m[2], +m[3]] : null;
  }
  function shade(css, dL) {
    var rgb = parseRgb(css), o = self.__lzOklch;
    if (!rgb || !o) return css;
    var lch = o.to(rgb[0], rgb[1], rgb[2]);
    return 'rgb(' + o.from(Math.max(0.14, Math.min(0.93, lch[0] + dL)), lch[1], lch[2]).join(',') + ')';
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

  function bandTooltipHtml(b) {
    var lines = [];
    var head = esc(b.poolName) + (b.kind === 'stratum' ? ' \u00b7 public stratum'
      : b.kind === 'folded' ? ' \u00b7 ' + esc(b.label) : ' \u00b7 ' + esc(b.label));
    lines.push('<b>' + head + '</b>');
    lines.push('<span>' + b.blocks + ' block' + (b.blocks === 1 ? '' : 's') + ' \u00b7 ' +
      pct(b.share * 100) + ' of the pool \u00b7 ' + pct(b.blocks / b.windowBlocks * 100) + ' of all blocks</span>');
    if (b.netHs > 0 && b.windowBlocks > 0) {
      lines.push('<span>est. ' + fmtHr(b.netHs * b.blocks / b.windowBlocks / 1e9) + '</span>');
    }
    if (b.kind === 'stratum') {
      lines.push('<span class="lz-band-sub">no gateway tag in the coinbase</span>');
    } else if (b.kind === 'folded') {
      lines.push('<span class="lz-band-sub">' + esc(b.members.slice(-4).reverse().map(function (m) { return m.label; }).join(', ')) +
        (b.members.length > 4 ? ' and ' + (b.members.length - 4) + ' more' : '') + '</span>');
    }
    if (!b.own) return lines.join('');
    var live = b.kind === 'stratum' ? primeAgg(function (g) { return !!g.own; })
      : b.kind === 'gateway' ? primeTag(b.label) : null;
    if (live && !live.live) {
      lines.push('<span class="lz-band-live">no gateway with this tag is connected right now</span>');
    } else if (live) {
      lines.push('<span class="lz-band-live">live: ' + pct(live.workShare * 100) + ' of the pool\'s work now \u00b7 ' +
        live.shares.toLocaleString() + ' shares this session' +
        (b.kind === 'gateway'
          ? (live.live > 1 ? ' \u00b7 ' + live.live + ' gateways' : '') +
            (live.feePath ? ' \u00b7 ' + esc(live.feePath) + ' fee path' : '')
          : '') + '</span>');
    }
    return lines.join('');
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
    bandInfo.sig = null;
  }

  function drawBands() {
    // The full pools graph and the /mining dashboard widget (1w luck pie). Other pies
    // (pool pages, tiny tiles) stay stock.
    if (!isPoolsGraph() && !isMiningDash()) { bandState('inactive'); dropBands(); return; }
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

    // Redraw when the pie itself changes (window, resize, new block) and not on every one of
    // the DOM mutations that bring us here.
    var sig = hash32([win, tagDoc.generated, host.clientWidth, host.clientHeight,
      svg.getAttribute('width'), svg.getAttribute('height')].join(':') +
      Array.prototype.map.call(svg.querySelectorAll('path'), function (p) { return p.getAttribute('d'); }).join(''));
    // ECharts rebuilds its container on some resizes and takes the overlay with it, and the
    // sector paths can come back byte-identical, so "nothing changed" is only a reason to
    // skip the redraw while the bands are actually still on the page.
    if (bandInfo.sig === sig && host.querySelector('svg.lz-bands')) {
      pinOverlay(host.querySelector('svg.lz-bands'), svg);
      return;
    }
    undimAll();                       // a redraw ends any hover, so nothing stays faded
    var hadOverlay = !!host.querySelector('svg.lz-bands');

    var pie = readPie(svg);
    // A hovered ECharts sector is a different size and lives at the end of the SVG. If the
    // read still fails, leave whatever overlay is already on the page -- blanking it is
    // how the bands used to vanish the moment the pointer crossed the pie.
    if (!pie) { bandState('sectors unreadable'); return; }
    var slices = expectedSlices(api, pie.sectors.length);
    if (!slices) { bandState('slice count ' + pie.sectors.length + ' unexplained'); return; }
    for (var i = 0; i < slices.length; i++) {
      if (Math.abs(slices[i].share - pie.sectors[i].span / 3.6) > 0.4) {
        bandState('slice ' + i + ' is ' + (pie.sectors[i].span / 3.6).toFixed(2) + '%, API says ' + slices[i].share.toFixed(2) + '%');
        return;
      }
    }

    // The pool's own slice gets live gateway figures in its tooltip; nothing else needs them.
    var ownSlice = null;
    slices.forEach(function (s) { if (s.slug === POOL_SLUG && wdoc.pools[POOL_SLUG]) ownSlice = s; });
    if (ownSlice) cachedJson('gws', POOL + '/api/gateways', 60000, { mode: 'cors' });

    var old = host.querySelector('svg.lz-bands');
    if (old) old.remove();
    var mount = svg.parentNode || host;
    var vw = parseFloat(svg.getAttribute('width')) || host.clientWidth;
    var vh = parseFloat(svg.getAttribute('height')) || host.clientHeight;
    var ov = document.createElementNS(SVGNS, 'svg');
    ov.setAttribute('class', 'lz-bands');
    ov.setAttribute('width', vw);
    ov.setAttribute('height', vh);
    ov.setAttribute('viewBox', svg.getAttribute('viewBox') || ('0 0 ' + vw + ' ' + vh));
    // Own the pie's pointer so ECharts cannot emphasise a sector out from under the bands.
    // Labels and leader lines sit outside this doughnut and stay the chart's.
    var catcher = document.createElementNS(SVGNS, 'path');
    catcher.setAttribute('d', bandSectorPath(pie.cx, pie.cy, pie.r0, pie.r, pie.sectors[0].angle, 359.9));
    catcher.setAttribute('fill', 'transparent');
    catcher.setAttribute('class', 'lz-band-catcher');
    ov.appendChild(catcher);

    var drawn = 0, pools = 0, netHs = windowHashrate(api, win);
    slices.forEach(function (slice, k) {
      var sector = pie.sectors[k];
      if (sector.span < MIN_SLICE_DEG) return;
      var entry = slice.slug ? wdoc.pools[slice.slug] : null;
      var list = entry ? poolBands(entry) : null;
      if (!list) return;
      var base = sector.el.getAttribute('fill');
      // The hovered sector is a different size from the rest, so use its own radii.
      var sr = sector.r > 4 ? sector.r : pie.r, sr0 = sector.r0 > 0 ? sector.r0 : pie.r0;
      if (sr0 >= sr) { sr = pie.r; sr0 = pie.r0; }
      var height = sr - sr0;
      var minT = Math.min(MIN_BAND_PX, height * 0.4 / list.length);
      var free = height - minT * list.length;
      var cursor = sr0;
      var gws = list.filter(function (b) { return b.kind !== 'stratum'; }).length;
      var seen = 0;
      list.forEach(function (b) {
        b.share = b.blocks / entry.blocks;
        b.poolName = slice.name;
        b.slug = slice.slug;
        b.own = slice.slug === POOL_SLUG;
        b.windowBlocks = api.blockCount;
        b.netHs = netHs;
        var r0 = cursor, r1 = cursor + minT + free * b.share;
        cursor = r1;
        b.sector = sector; b.r0 = r0; b.r1 = r1;      // the hover ring and label need these
        // The stratum band keeps the slice's own colour, so a pool with gateways still reads
        // as its slice with rings on it; the gateways step lighter outward.
        var dL = b.kind === 'stratum' ? 0 : 0.05 + 0.17 * (gws <= 1 ? 1 : seen / (gws - 1));
        if (b.kind !== 'stratum') seen++;
        var path = document.createElementNS(SVGNS, 'path');
        path.setAttribute('d', bandSectorPath(pie.cx, pie.cy, r0, r1, sector.angle, sector.span));
        path.setAttribute('data-lz-band', b.kind);
        path.style.fill = shade(base, dL);
        path.__lzBand = b;
        ov.appendChild(path);
        drawn++;
      });
      pools++;
    });

    if (!drawn) { bandState('no gateway blocks in ' + win); bandInfo.sig = sig; dropBands(); return; }
    mount.appendChild(ov);
    pinOverlay(ov, svg);

    /* Hover: the band under the pointer lifts off its slice the way an ECharts sector does,
     * everything belonging to another pool fades back, and the gateway's own tag is written
     * at the rim on a leader line -- a band is a few pixels tall, so the name has to be
     * legible somewhere other than a tooltip that may be across the chart. */
    var tip = bandTip(mount);
    var hot = null, dimmed = [];
    function clearHot() {
      if (hot) { hot.classList.remove('lz-hot'); hot = null; }
      for (var i = 0; i < dimmed.length; i++) undim(dimmed[i]);
      dimmed = [];
      ov.removeAttribute('data-hover');
      var gone = ov.querySelectorAll('.lz-band-label, .lz-band-lead, .lz-band-ring, .lz-band-chip');
      for (var j = 0; j < gone.length; j++) gone[j].remove();
      var lit = ov.querySelectorAll('.lz-dim');
      for (var k = 0; k < lit.length; k++) lit[k].classList.remove('lz-dim');
      tip.removeAttribute('data-shown');
    }
    function setHot(p) {
      clearHot();
      var b = p.__lzBand;
      hot = p;
      p.classList.add('lz-hot');
      ov.setAttribute('data-hover', '');
      var mid = b.sector.angle + b.sector.span / 2;
      // A ring traced just outside the band, rather than moving the band itself: a band can
      // be three pixels tall, and shifting it out from under the pointer would flicker.
      var ring = document.createElementNS(SVGNS, 'path');
      ring.setAttribute('class', 'lz-band-ring');
      ring.setAttribute('d', bandSectorPath(pie.cx, pie.cy, Math.max(1, b.r0 - 1.6), b.r1 + 1.6,
        b.sector.angle, b.sector.span));
      ov.appendChild(ring);
      // Fade the other pools -- their bands here, their sectors in the chart underneath.
      Array.prototype.forEach.call(ov.querySelectorAll('path'), function (o) {
        if (o.__lzBand && o.__lzBand.slug !== b.slug) o.classList.add('lz-dim');
      });
      slices.forEach(function (s, k) {
        if (s.slug === b.slug || !pie.sectors[k]) return;
        pie.sectors[k].el.style.opacity = '0.55';
        pie.sectors[k].el.setAttribute('data-lz-dim', '');
        dimmed.push(pie.sectors[k].el);
      });

      // The tag, at the rim on the band's own bisector, clamped inside the chart box.
      var anchor = polar(pie.cx, pie.cy, (b.r1 + b.r0) / 2, mid);
      var elbow = polar(pie.cx, pie.cy, pie.r + 16, mid);
      var right = elbow.x >= pie.cx;
      var tx = elbow.x + (right ? 9 : -9);
      var lead = document.createElementNS(SVGNS, 'polyline');
      lead.setAttribute('class', 'lz-band-lead');
      lead.setAttribute('points', [anchor.x.toFixed(1) + ',' + anchor.y.toFixed(1),
        elbow.x.toFixed(1) + ',' + elbow.y.toFixed(1), tx.toFixed(1) + ',' + elbow.y.toFixed(1)].join(' '));
      var text = document.createElementNS(SVGNS, 'text');
      text.setAttribute('class', 'lz-band-label');
      text.setAttribute('y', elbow.y.toFixed(1));
      text.setAttribute('dy', '0.34em');
      text.textContent = b.kind === 'stratum' ? b.poolName + ' \u00b7 public stratum' : b.label;
      ov.appendChild(lead);
      ov.appendChild(text);
      var w = text.getComputedTextLength ? text.getComputedTextLength() : 80;
      var x = right ? Math.min(tx + 3, vw - 4 - w) : Math.max(tx - 3, 4 + w);
      text.setAttribute('x', x.toFixed(1));
      text.setAttribute('text-anchor', right ? 'start' : 'end');
      // The pie's own labels crowd both sides, so step the tag off the bisector until it
      // has a clear line to sit on, the way the chart's own leader lines bend.
      var hostBox = host.getBoundingClientRect(), taken = [];
      Array.prototype.forEach.call(svg.querySelectorAll('text'), function (t) {
        var r = t.getBoundingClientRect();
        if (r.width) taken.push({ x0: r.left - hostBox.left, x1: r.right - hostBox.left,
                                  y0: r.top - hostBox.top, y1: r.bottom - hostBox.top });
      });
      var lx0 = right ? x : x - w, lx1 = right ? x + w : x, dy = 0;
      for (var s = 0; s < LABEL_STEPS.length; s++) {
        var ty = elbow.y + LABEL_STEPS[s], clear = true;
        for (var q = 0; q < taken.length; q++) {
          var t2 = taken[q];
          if (lx1 > t2.x0 - 3 && lx0 < t2.x1 + 3 && ty + 7 > t2.y0 && ty - 7 < t2.y1) { clear = false; break; }
        }
        if (clear) { dy = LABEL_STEPS[s]; break; }
      }
      text.setAttribute('y', (elbow.y + dy).toFixed(1));
      lead.setAttribute('points', [anchor.x.toFixed(1) + ',' + anchor.y.toFixed(1),
        elbow.x.toFixed(1) + ',' + elbow.y.toFixed(1),
        tx.toFixed(1) + ',' + (elbow.y + dy).toFixed(1)].join(' '));
      // One side of the pie is a solid stack of the chart's own labels, so there is not
      // always a clear line to move to: the tag sits on a chip and simply covers what it
      // must, for as long as the pointer is on the band.
      if (text.getBBox) {
        var bb = text.getBBox();
        var chip = document.createElementNS(SVGNS, 'rect');
        chip.setAttribute('class', 'lz-band-chip');
        chip.setAttribute('x', (bb.x - 5).toFixed(1));
        chip.setAttribute('y', (bb.y - 3).toFixed(1));
        chip.setAttribute('width', (bb.width + 10).toFixed(1));
        chip.setAttribute('height', (bb.height + 6).toFixed(1));
        chip.setAttribute('rx', '2');
        ov.insertBefore(chip, text);
      }

      // Pinned clear of the pie rather than following the pointer: the bands are fixed
      // shapes, and a tooltip that chases the cursor across a doughnut spends its time
      // covering the band it is describing.
      tip.innerHTML = bandTooltipHtml(b);
      tip.setAttribute('data-shown', '');
      var W = vw, H = vh, tw = tip.offsetWidth, th = tip.offsetHeight;
      var gap = 12, beside = anchor.y - th / 2, spots = [];
      if (pie.cx + pie.r + gap + tw <= W) spots.push([pie.cx + pie.r + gap, beside]);
      if (pie.cx - pie.r - gap - tw >= 0) spots.push([pie.cx - pie.r - gap - tw, beside]);
      spots.push([W - tw - 4, 4], [4, 4], [W - tw - 4, H - th - 4], [4, H - th - 4]);
      // On a narrow screen the pie fills the box and something has to be covered; whatever
      // it is, it must not be the band being described.
      var bb = hot.getBBox ? hot.getBBox() : null, best = spots[0], bestHit = Infinity;
      spots.forEach(function (sp) {
        var sx = Math.max(4, Math.min(sp[0], W - tw - 4)), sy = Math.max(4, Math.min(sp[1], H - th - 4));
        var hit = !bb ? 0 : Math.max(0, Math.min(sx + tw, bb.x + bb.width + 6) - Math.max(sx, bb.x - 6)) *
                            Math.max(0, Math.min(sy + th, bb.y + bb.height + 6) - Math.max(sy, bb.y - 6));
        if (hit < bestHit) { bestHit = hit; best = [sx, sy]; }
      });
      tip.style.left = (parseFloat(ov.style.left) || 0) + best[0] + 'px';
      tip.style.top = (parseFloat(ov.style.top) || 0) + best[1] + 'px';
    }
    ov.addEventListener('mousemove', function (ev) {
      var p = ev.target;
      if (p && p.__lzBand && p !== hot) setHot(p);
    });
    ov.addEventListener('mouseleave', clearHot);
    ov.addEventListener('mouseout', function (ev) {
      if (ev.target && ev.target.__lzBand && !(ev.relatedTarget && ev.relatedTarget.__lzBand)) clearHot();
    });
    ov.addEventListener('click', function (ev) {
      var b = ev.target && ev.target.__lzBand;
      if (b && b.slug) location.href = '/mining/pool/' + b.slug;
    });

    var wrap = host.parentNode;
    if (isPoolsGraph() && wrap && !wrap.querySelector('.lz-bands-note')) {
      wrap.insertBefore(el('p', { class: 'lz-bands-note' },
        '<b>Outer bands: DATUM gateways</b>, largest on the rim, by the tag their coinbase carries. ' +
        'The inner band is the pool\u2019s blocks with no gateway tag \u2014 its own stratum. ' +
        'Sizes are blocks found in this window, so a short window moves a lot.'), host.nextSibling);
    }
    bandState('ok');
    bandInfo.pools = pools;
    bandInfo.drawn = drawn;
    bandInfo.window = win;
    bandInfo.sig = sig;
  }

  var scheduled = false;
  function apply() {
    scheduled = false;
    try { nav(); } catch (e) { /* never break the explorer */ }
    try { fiatChip(); } catch (e) { /* never break the explorer */ }
    try { footer(); } catch (e) { /* never break the explorer */ }
    try { dashboard(); } catch (e) { /* never break the explorer */ }
    try { minerBadges(); } catch (e) { /* never break the explorer */ }
    try { paintClockFiat(); } catch (e) { /* never break the explorer */ }
    try { drawBands(); } catch (e) { bandState('error: ' + e); }
  }
  function schedule() {
    if (scheduled) return;
    scheduled = true;
    (window.requestAnimationFrame || setTimeout)(apply);
  }

  function start() {
    apply();
    // Last line of defence for the bands: whatever moves the chart -- a zoom, a resize, a
    // re-render that drops the overlay -- this notices within a tick. drawBands hashes the
    // sector paths and returns immediately when they are unchanged and the overlay is still
    // there, so an idle page pays a hash four times a second and nothing else.
    setInterval(function () {
      try { drawBands(); } catch (e) { bandState('error: ' + e); }
    }, 250);
    new MutationObserver(schedule).observe(document.body, { childList: true, subtree: true });
    self.addEventListener('resize', bandsReflow, { passive: true });
    if (self.visualViewport) self.visualViewport.addEventListener('resize', bandsReflow, { passive: true });
  }
  if (document.body) start(); else document.addEventListener('DOMContentLoaded', start);
})();
