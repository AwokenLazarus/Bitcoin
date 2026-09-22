// One-off: fetch every block since the fork from an explorer and write data/blocks.json.
//   node scripts/backfill.mjs [explorer-base]      default http://127.0.0.1:13006 (ssh -L to the hub)
import fs from "node:fs";
import { blockRecord, FORK_HEIGHT } from "../src/galaxy.js";

const BASE = (process.argv[2] || "http://127.0.0.1:13006").replace(/\/$/, "");
const get = async (p) => { for (let i = 0; i < 4; i++) { try { const r = await fetch(BASE + p); if (r.ok) return r.json(); } catch {} await new Promise((s) => setTimeout(s, 1500)); } throw new Error("failed " + p); };
fs.mkdirSync("data", { recursive: true });
const tip = await get("/api/blocks/tip/height");
const out = new Map();
let h = tip;
while (h >= FORK_HEIGHT) {
  const page = await get(`/api/v1/blocks/${h}`);
  if (!page.length) break;
  for (const b of page) if (b.height >= FORK_HEIGHT) out.set(b.height, blockRecord(b));
  h = page[page.length - 1].height - 1;
  if (out.size % 1500 < 15) process.stdout.write(`\r${out.size} blocks, at ${h}   `);
}
const recs = [...out.values()].sort((a, b) => a.h - b.h);
fs.writeFileSync("data/blocks.json", JSON.stringify(recs));
console.log(`\nwrote ${recs.length} blocks ${recs[0].h}..${recs[recs.length - 1].h}, ${(fs.statSync("data/blocks.json").size / 1e6).toFixed(2)} MB`);
const missing = []; for (let x = FORK_HEIGHT; x <= tip; x++) if (!out.has(x)) missing.push(x);
console.log("missing heights:", missing.length, missing.slice(0, 10));
