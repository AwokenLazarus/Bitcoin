#!/usr/bin/env node
// Drive headless Chrome over the DevTools protocol: load a page, run pointer/keyboard steps,
// take screenshots. No dependencies (Node 22+ has WebSocket).
//   node dev/shot.mjs '{"url":"http://127.0.0.1:8899/","w":1400,"h":900,"steps":[["wait",1500],["shot","/tmp/a.png"]]}'
// steps: ["wait",ms] ["shot",path,{full?}] ["move",x,y] ["click",x,y] ["key","k"] ["eval","js"] ["scroll","#id"] ["hoverSel","css",dx,dy] ["clickSel","css",dx,dy]
import { spawn } from "node:child_process";
import { mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

const job = JSON.parse(process.argv[2]);
const port = 9300 + Math.floor(Math.random() * 500);
const chrome = spawn("google-chrome", ["--headless=new", "--no-sandbox", "--disable-gpu", `--remote-debugging-port=${port}`,
  `--user-data-dir=${mkdtempSync(join(tmpdir(), "lzshot-"))}`, "--hide-scrollbars", ...(job.args || []), "about:blank"], { stdio: "ignore" });
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
let ws, id = 0;
const pending = new Map(), logs = [];
const send = (method, params = {}) => new Promise((res, rej) => { const i = ++id; pending.set(i, { res, rej }); ws.send(JSON.stringify({ id: i, method, params })); });
try {
  let target;
  for (let i = 0; i < 50 && !target; i++) { await sleep(200); try { target = (await (await fetch(`http://127.0.0.1:${port}/json`)).json()).find((t) => t.type === "page"); } catch {} }
  ws = new WebSocket(target.webSocketDebuggerUrl);
  await new Promise((r) => (ws.onopen = r));
  ws.onmessage = (m) => {
    const d = JSON.parse(m.data);
    if (d.id && pending.has(d.id)) { const p = pending.get(d.id); pending.delete(d.id); d.error ? p.rej(new Error(d.error.message)) : p.res(d.result); }
    else if (d.method === "Runtime.exceptionThrown") logs.push("EXCEPTION " + (d.params.exceptionDetails.exception?.description || d.params.exceptionDetails.text));
    else if (d.method === "Runtime.consoleAPICalled" && ["error", "warning"].includes(d.params.type)) logs.push(d.params.type + " " + d.params.args.map((a) => a.value ?? a.description).join(" "));
    else if (d.method === "Network.responseReceived" && d.params.response.status >= 400) logs.push(`HTTP ${d.params.response.status} ${d.params.response.url}`);
  };
  await send("Runtime.enable"); await send("Network.enable"); await send("Page.enable");
  await send("Emulation.setDeviceMetricsOverride", { width: job.w || 1400, height: job.h || 900, deviceScaleFactor: job.dpr || 1, mobile: !!job.mobile });
  if (job.mobile) await send("Emulation.setTouchEmulationEnabled", { enabled: true });
  await send("Page.navigate", { url: job.url });
  await sleep(job.settle || 2500);
  const ev = async (expression) => (await send("Runtime.evaluate", { expression, returnByValue: true, awaitPromise: true })).result.value;
  const rect = (sel) => ev(`(()=>{const e=document.querySelector(${JSON.stringify(sel)});if(!e)return null;e.scrollIntoView({block:"center"});const r=e.getBoundingClientRect();return {x:r.x,y:r.y,w:r.width,h:r.height}})()`);
  const mouse = (type, x, y, extra = {}) => send("Input.dispatchMouseEvent", { type, x, y, button: "none", ...extra });
  for (const [op, a, b, c] of job.steps || []) {
    if (op === "wait") await sleep(a);
    else if (op === "move") await mouse("mouseMoved", a, b);
    else if (op === "click") { await mouse("mouseMoved", a, b); await mouse("mousePressed", a, b, { button: "left", clickCount: 1 }); await mouse("mouseReleased", a, b, { button: "left", clickCount: 1 }); }
    else if (op === "hoverSel" || op === "clickSel") {
      const r = await rect(a); if (!r) { logs.push("no element " + a); continue; }
      await sleep(150);
      const r2 = await ev(`(()=>{const r=document.querySelector(${JSON.stringify(a)}).getBoundingClientRect();return {x:r.x,y:r.y,w:r.width,h:r.height}})()`);
      const x = r2.x + (b <= 1 ? r2.w * b : b), y = r2.y + (c <= 1 ? r2.h * c : c);
      await mouse("mouseMoved", x, y);
      if (op === "clickSel") { await mouse("mousePressed", x, y, { button: "left", clickCount: 1 }); await mouse("mouseReleased", x, y, { button: "left", clickCount: 1 }); }
    }
    else if (op === "key") { await send("Input.dispatchKeyEvent", { type: "keyDown", key: a, text: a.length === 1 ? a : undefined }); await send("Input.dispatchKeyEvent", { type: "keyUp", key: a }); }
    else if (op === "type") await send("Input.insertText", { text: a });
    else if (op === "eval") console.log("eval:", JSON.stringify(await ev(a)));
    else if (op === "scroll") await ev(`document.querySelector(${JSON.stringify(a)})?.scrollIntoView({block:"start"})`);
    else if (op === "shot") {
      const o = b || {};
      const shot = await send("Page.captureScreenshot", { format: "png", captureBeyondViewport: !!o.full, ...(o.full ? { clip: { x: 0, y: 0, width: job.w || 1400, height: Math.min(await ev("document.documentElement.scrollHeight"), o.maxH || 6000), scale: 1 } } : {}) });
      writeFileSync(a, Buffer.from(shot.data, "base64"));
    }
  }
  if (logs.length) console.log("page log:\n" + [...new Set(logs)].join("\n")); else console.log("page log: clean");
} finally { try { ws?.close(); } catch {} chrome.kill("SIGKILL"); }
