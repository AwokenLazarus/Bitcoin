/* Lazarus in-tab miner: WebSocket stratum + Web Workers. */
(function () {
  const B = window.Blake2b;
  const UI = {};
  let ws = null, workers = [], running = false;
  let en1 = "", en2size = 8, en2 = "", diff = 1;
  let accepted = 0, rejected = 0, submitted = 0, hashes = 0, t0 = 0;
  let sid = 20, addr = "", job = null, pendingNotify = null;
  let hrTimer = null, lastReport = 0;

  function log(s) { if (UI.st) UI.st.textContent = s; }
  function fmtHs(hs) {
    if (hs >= 1e9) return (hs / 1e9).toFixed(2) + " GH/s";
    if (hs >= 1e6) return (hs / 1e6).toFixed(2) + " MH/s";
    if (hs >= 1e3) return (hs / 1e3).toFixed(1) + " KH/s";
    return (hs | 0) + " H/s";
  }
  function send(o) {
    if (ws && ws.readyState === 1) ws.send(JSON.stringify(o) + "\n");
  }
  function updateShares() {
    if (UI.sh) UI.sh.textContent = accepted + " / " + submitted + (rejected ? "  (" + rejected + " rej)" : "");
  }
  function validAddr(a) {
    return /^(bc1|[13])[a-zA-HJ-NP-Z0-9]{20,90}$/.test(a);
  }
  function makeWorker() {
    const n = new Uint8Array(3);
    crypto.getRandomValues(n);
    let s = "web";
    for (let i = 0; i < n.length; i++) s += n[i].toString(16).padStart(2, "0");
    return s;
  }
  function sessionHs() {
    const dt = (Date.now() - t0) / 1000;
    return dt > 0 ? hashes / dt : 0;
  }
  function reportHr() {
    if (!running || !addr) return;
    const now = Date.now();
    if (now - lastReport < 2000) return;
    lastReport = now;
    fetch("/api/browser-stat", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ user: addr, hs: sessionHs() }),
      keepalive: true,
    }).catch(function () {});
  }

  function makeEn2() {
    const n = new Uint8Array(en2size || 8);
    crypto.getRandomValues(n);
    return B.bytesToHex(n);
  }

  function dispatch(notify) {
    if (!en1) {
      pendingNotify = notify;
      return;
    }
    pendingNotify = null;
    const jobId = notify[0], prev = notify[1], coinb1h = notify[2], ntime = notify[7];
    en2 = makeEn2();
    const e1 = B.hexToBytes(en1);
    const e2 = B.hexToBytes(en2);
    const ex = new Uint8Array(12);
    ex.set(e1.subarray(0, 12), 0);
    ex.set(e2.subarray(0, Math.max(0, 12 - e1.length)), e1.length);
    const coinb1 = B.hexToBytes(coinb1h);
    if (coinb1.length !== 39) {
      log("bad job (coinb1 " + coinb1.length + "b)");
      return;
    }
    const root = B.workRoot(coinb1, ex);
    const target = B.shareTarget(diff);
    job = {
      jobId: jobId,
      prev: prev,
      ntime: ntime.length === 16 ? ntime : ntime,
      wroot: B.bytesToHex(root),
      targetHex: B.bytesToHex(target),
      en2: en2,
    };
    const n = workers.length || 1;
    for (let i = 0; i < workers.length; i++) {
      workers[i].postMessage({
        type: "job",
        job: Object.assign({}, job, { base: String(i), step: String(n) }),
      });
    }
    log("mining job " + String(jobId).slice(0, 8) + "…  diff " + diff);
  }

  function handle(line) {
    let m;
    try { m = JSON.parse(line); } catch (e) { return; }
    if (m.method === "mining.set_difficulty") {
      diff = Math.max(1, parseInt(m.params[0], 10) || 1);
      return;
    }
    if (m.method === "mining.notify") {
      dispatch(m.params);
      return;
    }
    if (m.id === 1 && m.result) {
      en1 = m.result[1];
      en2size = m.result[2] || 8;
      log("subscribed");
      if (pendingNotify) dispatch(pendingNotify);
      return;
    }
    if (m.id === 2) {
      if (m.result === true) log("authorized — waiting for work");
      else log("authorize failed");
      return;
    }
    if (m.id >= 20) {
      if (m.error) {
        rejected++;
        log("share rejected: " + (Array.isArray(m.error) ? m.error[1] : m.error));
      } else if (m.result === true) {
        accepted++;
        log("share accepted");
      } else if (m.result === false) {
        rejected++;
        log("share rejected");
      }
      updateShares();
    }
  }

  function onWorker(ev) {
    const m = ev.data || {};
    if (m.type === "progress") {
      hashes += m.hashes || 0;
      return;
    }
    if (m.type === "found") {
      submitted++;
      updateShares();
      send({
        id: sid++,
        method: "mining.submit",
        params: [addr, m.jobId, m.en2, m.ntime, m.nonce],
      });
    }
  }

  function start() {
    if (running) { stop(); return; }
    const a = (UI.addr.value || "").trim();
    if (!validAddr(a.split(".")[0])) {
      log("enter a Bitcoin address first");
      return;
    }
    addr = a.indexOf(".") >= 0 ? a : a + "." + makeWorker();
    running = true;
    accepted = rejected = submitted = hashes = 0;
    t0 = Date.now();
    lastReport = 0;
    pendingNotify = null;
    en1 = "";
    updateShares();
    UI.btn.textContent = "STOP";
    log("starting workers…");
    const n = Math.max(1, Math.min(4, (navigator.hardwareConcurrency || 2) - 0 || 1));
    for (let i = 0; i < n; i++) {
      const w = new Worker("/static/miner-worker.js");
      w.onmessage = onWorker;
      workers.push(w);
    }
    const url = (location.protocol === "https:" ? "wss://" : "ws://") + location.host + "/mine";
    ws = new WebSocket(url);
    let buf = "";
    ws.onopen = function () {
      send({ id: 1, method: "mining.subscribe", params: ["lazarus-web/0.1"] });
      send({ id: 2, method: "mining.authorize", params: [addr, "x"] });
      log("connected — authorizing");
    };
    ws.onmessage = function (ev) {
      buf += ev.data;
      let i;
      while ((i = buf.indexOf("\n")) >= 0) {
        const ln = buf.slice(0, i);
        buf = buf.slice(i + 1);
        if (ln.trim()) handle(ln.trim());
      }
      if (buf.trim() && buf.trim().startsWith("{")) {
        try { JSON.parse(buf.trim()); handle(buf.trim()); buf = ""; } catch (e) {}
      }
    };
    ws.onclose = function () {
      if (running) log("disconnected from pool");
    };
    ws.onerror = function () {
      log("cannot reach mining bridge");
    };
    hrTimer = setInterval(function () {
      if (UI.hr) UI.hr.textContent = fmtHs(sessionHs());
      reportHr();
    }, 1000);
  }

  function stop() {
    running = false;
    UI.btn.textContent = "START MINING";
    workers.forEach(function (w) {
      try { w.postMessage({ type: "stop" }); w.terminate(); } catch (e) {}
    });
    workers = [];
    if (ws) { try { ws.close(); } catch (e) {} ws = null; }
    if (hrTimer) { clearInterval(hrTimer); hrTimer = null; }
    if (UI.hr) UI.hr.textContent = "—";
    log("stopped");
  }

  function boot() {
    UI.btn = document.getElementById("bm-btn");
    if (!UI.btn) return;
    UI.addr = document.getElementById("bm-addr");
    UI.hr = document.getElementById("bm-hr");
    UI.sh = document.getElementById("bm-sh");
    UI.st = document.getElementById("bm-st");
    UI.btn.addEventListener("click", start);
    const look = document.getElementById("lookup");
    if (look && UI.addr && look.value && !UI.addr.value) UI.addr.value = look.value;
  }
  if (document.readyState !== "loading") boot();
  else document.addEventListener("DOMContentLoaded", boot);
})();
