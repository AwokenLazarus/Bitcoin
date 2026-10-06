// One walk of the last 14 days of coinbases into a naughtylist KV document.
// The cron does the same walk 48 blocks at a time; this fills it in one go.
//
//   node scripts/naughty-backfill.mjs > /tmp/naughtylist.json
import { readFileSync, writeFileSync, existsSync } from "node:fs";
import { emptyState, noteBlock, outputsOf, prune } from "../src/naughtylist.js";
import { FORK_HEIGHT } from "../src/galaxy.js";

const SAVE = "/tmp/naughtylist-state.json";

const EXPLORER = "https://mempool.lazarus-xbt.xyz";
const WINDOW = 14 * 86400;
const UA = { "User-Agent": "lazarus-naughtylist/0.1", Accept: "application/json" };

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

async function pool(items, n, fn) {
  const out = new Array(items.length);
  let i = 0;
  async function worker() {
    while (i < items.length) {
      const k = i++;
      out[k] = await fn(items[k], k);
    }
  }
  await Promise.all(Array.from({ length: n }, worker));
  return out;
}

const now = Math.floor(Date.now() / 1000);
const tip = Number(await get(`${EXPLORER}/api/blocks/tip/height`));
const state = existsSync(SAVE) ? JSON.parse(readFileSync(SAVE, "utf8")) : emptyState();
let cursor = state.scannedTo == null ? tip : state.scannedTo - 1;
let aged = false;
while (!aged && cursor >= FORK_HEIGHT) {
  const page = await get(`${EXPLORER}/api/v1/blocks/${cursor}`);
  if (!Array.isArray(page) || !page.length) break;
  const fresh = page.filter((b) => b && b.height >= FORK_HEIGHT && !(b.timestamp && now - b.timestamp > WINDOW));
  if (fresh.length < page.length) aged = true;
  await pool(fresh, 4, async (b) => {
    const txs = await get(`${EXPLORER}/api/block/${b.id}/txs/0`);
    const coinbase = Array.isArray(txs) ? txs[0] : null;
    const outputs = coinbase ? outputsOf(coinbase) : [];
    noteBlock(state, {
      h: b.height, t: b.timestamp, id: b.id, txid: coinbase && coinbase.txid || "",
      script: (b.extras && b.extras.coinbaseRaw) || "",
      explorerPool: (b.extras && b.extras.pool && b.extras.pool.name) || "",
      outputs,
    });
  });
  cursor = page[page.length - 1].height - 1;
  state.scannedTo = cursor + 1;
  writeFileSync(SAVE, JSON.stringify(state));
  console.error(`through ${state.scannedTo} addresses ${Object.keys(state.addrs).length}`);
}
state.tip = tip;
state.windowDone = true;
prune(state, now);
process.stdout.write(JSON.stringify(state));
