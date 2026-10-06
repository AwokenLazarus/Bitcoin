// Run the pool readers over a saved naughtylist document with a large request budget, and print
// what the lists would be. The cron does the same a few requests at a time.
//
//   node scripts/naughty-pools.mjs state.json [out.json] [budget]
import { readFileSync, writeFileSync } from "node:fs";
import { buildLists, migrate, rememberStratum } from "../src/naughtylist.js";
import { refreshPools } from "../src/pools.js";

const [file, out, budgetArg] = process.argv.slice(2);
const UA = { "User-Agent": "lazarus-map/1.0 (+https://lazarus-xbt.xyz/map)" };
const fetchOk = async (url) => {
  const r = await fetch(url, { headers: UA, signal: AbortSignal.timeout(30000) });
  if (!r.ok) throw new Error(`${url} -> HTTP ${r.status}`);
  return r;
};
const get = async (url) => (await fetchOk(url)).json();
const getText = async (url) => (await fetchOk(url)).text();

const state = migrate(JSON.parse(readFileSync(file, "utf8")));
const now = Math.floor(Date.now() / 1000);
for (const x of Object.values(state.ext)) x.tried = 0;
const report = await refreshPools(state, { get, getText, now, budget: Number(budgetArg) || 600 });
rememberStratum(state, now);
console.error("pools", JSON.stringify(report));
const POOL = "https://pool.lazarus-xbt.xyz";
const [miners, gateways] = await Promise.all([get(`${POOL}/api/miners`).catch(() => null), get(`${POOL}/api/gateways`).catch(() => null)]);
const lists = buildLists(state, (miners && miners.online) || [], now, (gateways && gateways.gateways) || []);
const tally = (rows) => { const t = {}; for (const r of rows) { const k = `${r.pool} · ${r.why || r.list}`; t[k] = (t[k] || 0) + 1; } return t; };
console.error("naughty", lists.naughty.length, tally(lists.naughty));
console.error("suspects", lists.suspects.length, tally(lists.suspects));
console.error("nice", lists.nice.length, tally(lists.nice));
if (out) writeFileSync(out, JSON.stringify(state));
