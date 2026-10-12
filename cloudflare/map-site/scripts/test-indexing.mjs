// Static sitemap contracts plus the dynamic route's canonical redirect, without live APIs/KV.
import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import worker from '../src/worker.js';
const publicDir = new URL('../public/', import.meta.url);
const xml = readFileSync(new URL('sitemap.xml', publicDir), 'utf8');
const urls = [...xml.matchAll(/<loc>([^<]+)<\/loc>/g)].map(m=>m[1]);
assert.equal(urls.length, 10);
assert.equal(new Set(urls).size, 10);
for (const url of urls) {
  const path = new URL(url).pathname;
  if (['/naughtylist/', '/map/table/'].includes(path)) continue; // Worker-rendered, tested below/elsewhere.
  const html = readFileSync(new URL(path.slice(1)+'index.html', publicDir),'utf8');
  assert.ok(html.includes(`rel="canonical" href="${url}"`), url);
  assert.ok(!/<meta[^>]+name="robots"[^>]+noindex/i.test(html), url);
  assert.ok(/<h1\b/.test(html), url);
}
for (const path of ['/naughtylist', '/naughtylist/index.html']) {
  const r = await worker.fetch(new Request('https://lazarus-xbt.xyz'+path+'?pool=Lazarus'), {}, {});
  assert.equal(r.status, 301);
  assert.equal(r.headers.get('Location'),'https://lazarus-xbt.xyz/naughtylist/?pool=Lazarus');
}
assert.ok(!readFileSync(new URL('robots.txt',publicDir),'utf8').includes('Disallow: /\n'));
console.log('PASS: main sitemap/static canonical pages and dynamic canonical redirects');
