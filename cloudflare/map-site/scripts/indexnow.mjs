// Tell IndexNow engines (Bing, Yandex, Seznam, Naver, and through Bing, ChatGPT search and
// Copilot) which hub URLs changed, so they recrawl within hours. Run it after a deploy.
// Google does not use IndexNow; it reads the sitemap.
//
//   node scripts/indexnow.mjs                 dry run: print what would be sent
//   node scripts/indexnow.mjs --live          submit every URL in public/sitemap.xml
//   node scripts/indexnow.mjs --live /blake2b/ /learn/   submit only these paths
//
// The key is the public/<key>.txt file served at the site root; it is public by design.
import { readFileSync, readdirSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const HOST = "lazarus-xbt.xyz";
const here = dirname(dirname(fileURLToPath(import.meta.url)));
const pub = join(here, "public");
const keyFile = readdirSync(pub).find((f) => /^[0-9a-f]{32}\.txt$/.test(f));
if (!keyFile) throw new Error("no IndexNow key file (public/<32 hex>.txt)");
const key = readFileSync(join(pub, keyFile), "utf8").trim();

const args = process.argv.slice(2);
const live = args.includes("--live");
const paths = args.filter((a) => a.startsWith("/"));
const fromSitemap = [...readFileSync(join(pub, "sitemap.xml"), "utf8").matchAll(/<loc>([^<]+)<\/loc>/g)].map((m) => m[1]);
const urlList = paths.length ? paths.map((p) => `https://${HOST}${p}`) : fromSitemap;

const body = { host: HOST, key, keyLocation: `https://${HOST}/${keyFile}`, urlList };
if (!live) {
  console.log("dry run (add --live to submit):\n" + JSON.stringify(body, null, 2));
} else {
  const res = await fetch("https://api.indexnow.org/indexnow", {
    method: "POST",
    headers: { "Content-Type": "application/json; charset=utf-8" },
    body: JSON.stringify(body),
  });
  console.log(`IndexNow: HTTP ${res.status} for ${urlList.length} URL(s)`);
  if (res.status >= 300) process.exit(1);
}
