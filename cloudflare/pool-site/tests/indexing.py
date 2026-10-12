"""Validate the shipped HTML bundle, not just its source templates. No network access."""
import json
import sys
from html.parser import HTMLParser
from pathlib import Path
from urllib.parse import urlsplit
from xml.etree import ElementTree as ET

class Page(HTMLParser):
    def __init__(self, text):
        super().__init__()
        self.canonical, self.lang, self.nav, self.alts = [], None, {}, {}
        self.noindex = False
        self.h1 = 0
        self.feed(text)
    def handle_starttag(self, tag, attrs):
        a = dict(attrs)
        if tag == 'html': self.lang = a.get('lang')
        if tag == 'h1': self.h1 += 1
        if tag == 'meta' and a.get('name') == 'robots': self.noindex |= 'noindex' in a.get('content', '')
        if tag == 'link' and a.get('rel') == 'canonical': self.canonical.append(a['href'])
        if tag == 'link' and 'hreflang' in a: self.alts[a['hreflang']] = a['href']
        if tag == 'a' and 'data-view-link' in a: self.nav[a['data-view-link']] = a['href']

out = Path(sys.argv[1])
ns = {'s': 'http://www.sitemaps.org/schemas/sitemap/0.9'}
urls = [e.text for e in ET.parse(out/'sitemap.xml').findall('s:url/s:loc', ns)]
assert len(urls) == len(set(urls)), 'duplicate sitemap URLs'
count = 0
for url in urls:
    path = urlsplit(url).path
    if path.endswith('.md'): continue
    file = out / (path.lstrip('/') + ('index.html' if path.endswith('/') else '.html'))
    text = file.read_text()
    page = Page(text)
    assert page.canonical == [url], (url, page.canonical)
    assert not page.noindex and page.h1 == 1, (url, page.h1)
    prefix = '/zh' if path.startswith('/zh/') else ''
    assert page.lang == ('zh-CN' if prefix else 'en'), url
    for key, target in {'home':'/', 'mine':'/connect', 'payouts':'/blocks', 'learn':'/how'}.items():
        assert page.nav[key] == prefix + target, (url, key, page.nav)
    assert set(page.alts) == {'en', 'zh-CN', 'x-default'}, url
    for alt in page.alts.values(): assert alt in urls, (url, alt)
    count += 1
assert count == 36, count
assert not (out/'static/index.html').exists(), 'raw template is publicly exported'
assert 'Disallow: /\n' not in (out/'robots.txt').read_text()
for route in ['/static/', '/static/index.html', '/static/index']:
    assert f'{route} / 301' in (out/'_redirects').read_text()
assert (out/'404.html').exists(), 'Pages would enable SPA fallback without 404.html'
print(f'PASS: {count} canonical HTML pages, languages, document navigation, sitemap and template exclusion')
