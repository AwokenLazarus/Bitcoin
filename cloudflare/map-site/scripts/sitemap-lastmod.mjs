// Set each <lastmod> in public/sitemap.xml to the date of the last commit that changed that
// page's HTML. Run it in the commit that changes a page (npm run sitemap), so the sitemap never
// says "today" for pages that did not change: search engines only use lastmod while it stays true.
//
//   node scripts/sitemap-lastmod.mjs          rewrite public/sitemap.xml
//   node scripts/sitemap-lastmod.mjs --check  exit 1 and list the URLs whose date is stale
import { execFileSync } from "node:child_process";
import { readFileSync, writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const here = dirname(dirname(fileURLToPath(import.meta.url)));
const sitemapPath = join(here, "public", "sitemap.xml");
const check = process.argv.includes("--check");

function pageFile(loc) {
  const path = new URL(loc).pathname.replace(/^\/|\/$/g, "");
  return join("public", path, "index.html");
}

function lastChanged(file) {
  const dirty = execFileSync("git", ["status", "--porcelain", "--", file], { cwd: here, encoding: "utf8" }).trim();
  if (dirty) return new Date().toISOString().slice(0, 10);
  const date = execFileSync("git", ["log", "-1", "--format=%cs", "--", file], { cwd: here, encoding: "utf8" }).trim();
  return date || new Date().toISOString().slice(0, 10);
}

const xml = readFileSync(sitemapPath, "utf8");
const stale = [];
const out = xml.replace(/<url><loc>([^<]+)<\/loc><lastmod>([^<]*)<\/lastmod>/g, (whole, loc, old) => {
  const date = lastChanged(pageFile(loc));
  if (date !== old) stale.push(`${loc}  ${old} -> ${date}`);
  return `<url><loc>${loc}</loc><lastmod>${date}</lastmod>`;
});

if (check) {
  if (stale.length) {
    console.error(`sitemap.xml lastmod is out of date (run: npm run sitemap):\n  ${stale.join("\n  ")}`);
    process.exit(1);
  }
  console.log("sitemap.xml lastmod matches git");
} else {
  writeFileSync(sitemapPath, out);
  console.log(stale.length ? `updated:\n  ${stale.join("\n  ")}` : "sitemap.xml already current");
}
