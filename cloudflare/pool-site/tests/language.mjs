import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import vm from 'node:vm';
const source = readFileSync(new URL('../../../pool/static/i18n.js', import.meta.url), 'utf8');
function boot(path, cookie, browser, query = '') {
  const navigations = [];
  const sandbox = {
    URLSearchParams,
    location: {pathname:path, search:query, hash:'#connect', assign:url=>navigations.push(url)},
    navigator: {language:browser, languages:[browser]},
    document: {cookie, documentElement:{classList:{toggle(){}}}, querySelectorAll:()=>[], dispatchEvent(){}},
    CustomEvent: class {}, window:{}
  };
  // apply() also reads document.head; no translated nodes are needed to exercise detection.
  sandbox.document.querySelector = () => null;
  sandbox.document.head = {querySelector: () => null};
  vm.runInNewContext(source, sandbox);
  sandbox.window.LZ_I18N.boot();
  return {lang:sandbox.window.LZ_I18N.lang(), navigations};
}
for (const path of ['/', '/hardware', '/connect']) {
  assert.equal(boot(path,'lz_lang=zh-CN','zh-CN').lang,'en');
  assert.equal(boot('/zh'+path,'lz_lang=en','en-US').lang,'zh-CN');
}
assert.deepEqual(boot('/hardware','','en','?lang=zh-CN&source=guide').navigations,['/zh/hardware?source=guide#connect']);
assert.deepEqual(boot('/zh/hardware','','zh-CN','?lang=en').navigations,['/hardware#connect']);
assert.equal(boot('/miner/address','lz_lang=zh-CN','en').lang,'zh-CN');
console.log('PASS: canonical language is stable; legacy language links navigate; miner preferences preserved');
