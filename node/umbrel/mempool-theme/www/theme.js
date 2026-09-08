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
              '<p class="lz-copy" id="lz-pool-copy">Every block found pays each miner directly in its coinbase by TIDES window share. 0.5% fee through your own DATUM gateway, 1% on the public stratum.</p>' +
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
    if (!tree || tree.querySelector('.lz-links')) return;
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

  var scheduled = false;
  function apply() {
    scheduled = false;
    try { nav(); } catch (e) { /* never break the explorer */ }
    try { fiatChip(); } catch (e) { /* never break the explorer */ }
    try { footer(); } catch (e) { /* never break the explorer */ }
    try { dashboard(); } catch (e) { /* never break the explorer */ }
    try { minerBadges(); } catch (e) { /* never break the explorer */ }
    try { paintClockFiat(); } catch (e) { /* never break the explorer */ }
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
