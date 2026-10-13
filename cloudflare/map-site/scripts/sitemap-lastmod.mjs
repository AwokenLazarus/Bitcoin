// Set each <lastmod> in public/sitemap.xml to the date of the last commit that changed that
// page's HTML (or, for Worker-rendered pages, its source files). Run it in the commit that changes a page (npm run sitemap), so the sitemap never
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

// Pages the Worker renders have no HTML file; their words live in these sources.
const RENDERED = {
  "/naughtylist/": ["src/naughtypage.js", "src/naughtylist.js"],
  "/map/table/": ["src/worker.js", "src/galaxy.js"],
};

function pageFiles(loc) {
  const pathname = new URL(loc).pathname;
  if (RENDERED[pathname]) return RENDERED[pathname];
  return [join("public", pathname.replace(/^\/|\/$/g, ""), "index.html")];
}

// Local calendar date, the same one git's %cs prints for a commit made here.
const today = () => new Date().toLocaleDateString("en-CA");

function lastChanged(files) {
  const dirty = execFileSync("git", ["status", "--porcelain", "--", ...files], { cwd: here, encoding: "utf8" }).trim();
  if (dirty) return today();
  const date = execFileSync("git", ["log", "-1", "--format=%cs", "--", ...files], { cwd: here, encoding: "utf8" }).trim();
  if (!date) throw new Error(`no git history for ${files.join(", ")}; add the page to RENDERED or commit it`);
  return date;
}

const xml = readFileSync(sitemapPath, "utf8");
const stale = [];
const out = xml.replace(/<url><loc>([^<]+)<\/loc><lastmod>([^<]*)<\/lastmod>/g, (whole, loc, old) => {
  const date = lastChanged(pageFiles(loc));
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
