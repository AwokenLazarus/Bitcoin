// Fill a v3 naughtylist document in one go: every coinbase of the last 14 days. The cron does the
// same 60 blocks at a time, which from empty takes a few hours; this takes a couple of minutes.
//
//   node scripts/naughty-backfill.mjs blocks.json [naughtylist.json] [hostedgateways.json] > new.json
//
// blocks.json is the map Worker's KV key `blocks`. naughtylist.json is the stored document, if
// there is one: what the pools said and who was once on stratum carry over from it.
// Progress is saved to SAVE, so an interrupted run picks up where it stopped.
import { readFileSync, writeFileSync, existsSync } from "node:fs";
import { ACTIVE_SEC, applyHosted, migrate, noteBlock, outputsOf, prune } from "../src/naughtylist.js";

const [blocksFile, stateFile, hostedFile] = process.argv.slice(2);
if (!blocksFile) { console.error("usage: naughty-backfill.mjs blocks.json [naughtylist.json] [hostedgateways.json]"); process.exit(2); }
const SAVE = process.env.SAVE || "/tmp/naughtylist-v3-state.json";
const EXPLORER = "https://mempool.lazarus-xbt.xyz";
const UA = { "User-Agent": "lazarus-naughtylist/0.2", Accept: "application/json" };

async function get(url) {
  let last;
  for (let n = 0; n < 5; n++) {
    try {
      const r = await fetch(url, { headers: UA, signal: AbortSignal.timeout(30000) });
      if (!r.ok) throw new Error(`${url} -> HTTP ${r.status}`);
      return await r.json();
    } catch (e) {
      last = e;
      await new Promise((resolve) => setTimeout(resolve, 400 * (n + 1)));
    }
  }
  throw last;
}

const now = Math.floor(Date.now() / 1000);
const recs = JSON.parse(readFileSync(blocksFile, "utf8"));
const hosted = hostedFile ? JSON.parse(readFileSync(hostedFile, "utf8")) : null;
const state = existsSync(SAVE) ? JSON.parse(readFileSync(SAVE, "utf8")) : migrate(stateFile ? JSON.parse(readFileSync(stateFile, "utf8")) : null);
const window = recs.filter((r) => r.t >= now - ACTIVE_SEC);
const missing = window.filter((r) => !state.blk[r.h] && state.skip[r.h] == null).reverse();
console.error(`${window.length} blocks in the window, ${missing.length} to read`);
for (let i = 0; i < missing.length; i += 6) {
  const slice = missing.slice(i, i + 6);
  const txs = await Promise.all(slice.map((r) => get(`${EXPLORER}/api/block/${r.id}/txs/0`)));
  slice.forEach((r, k) => {
    const coinbase = Array.isArray(txs[k]) ? txs[k][0] : null;
    if (!coinbase || !coinbase.txid) throw new Error(`no coinbase for ${r.h}`);
    noteBlock(state, {
      h: r.h, t: r.t, id: r.id, txid: coinbase.txid, outputs: outputsOf(coinbase),
      tags: { layout: r.l ? "datum" : "other", primary: r.p || "", secondary: r.s || "" },
      explorerPool: r.x || "",
    }, hosted);
  });
  if (i % 300 === 0) { writeFileSync(SAVE, JSON.stringify(state)); console.error(`read ${i + slice.length} of ${missing.length}`); }
}
state.tip = window.length ? window[window.length - 1].h : state.tip;
applyHosted(state, hosted);
prune(state, now);
writeFileSync(SAVE, JSON.stringify(state));
console.error(`blocks ${Object.keys(state.blk).length}, skipped ${Object.keys(state.skip).length}, addresses ${Object.keys(state.pays).length}`);
process.stdout.write(JSON.stringify(state));
