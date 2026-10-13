"""Fail when a page's crawlable text changed but its sitemap <lastmod> did not.

The dates in server.py are kept by hand (_SEO_ARTICLE, _SEO_MODIFIED). Search engines only trust
<lastmod> while it is true, so this check keeps it true: lastmod-hashes.json records, per sitemap
URL, the date and a hash of the text a crawler reads. A page whose hash moved while its date stayed
put fails the build; the fix is to bump that page's date and run this script with --update.

    python3 tests/lastmod.py dist            # check
    python3 tests/lastmod.py dist --update   # record the current dates and hashes
"""
import hashlib
import json
import re
import sys
from pathlib import Path
from urllib.parse import urlsplit
from xml.etree import ElementTree as ET

out = Path(sys.argv[1])
update = "--update" in sys.argv[2:]
store = Path(__file__).resolve().parent.parent / "lastmod-hashes.json"
ns = {"s": "http://www.sitemaps.org/schemas/sitemap/0.9"}


def crawlable(html):
    """The page text minus the parts that change without the words changing: scripts, styles,
    structured data (it carries the date itself), the modified-time meta and the live rental grid."""
    html = re.sub(r"(?s)<script\b.*?</script>|<style\b.*?</style>", "", html)
    html = re.sub(r'<meta property="article:modified_time"[^>]*>', "", html)
    html = re.sub(r'(?s)(<ul class="hw-grid" id="rentals-grid">).*?(</ul>)', r"\1\2", html)
    return hashlib.sha256(re.sub(r"\s+", " ", html).encode("utf-8")).hexdigest()


current = {}
for url in ET.parse(out / "sitemap.xml").findall("s:url", ns):
    loc = url.find("s:loc", ns).text
    path = urlsplit(loc).path
    file = out / (path.lstrip("/") + ("index.html" if path.endswith("/") else ".html"))
    current[path] = {"lastmod": url.find("s:lastmod", ns).text, "sha256": crawlable(file.read_text(encoding="utf-8"))}

if update:
    store.write_text(json.dumps(current, indent=1, sort_keys=True) + "\n", encoding="utf-8")
    print(f"recorded {len(current)} pages in {store.name}")
    sys.exit(0)

recorded = json.loads(store.read_text(encoding="utf-8"))
stale, bumped = [], []
for path, now in current.items():
    was = recorded.get(path)
    if not was:
        stale.append(f"{path}: new page, run --update")
    elif now["sha256"] != was["sha256"] and now["lastmod"] <= was["lastmod"]:
        stale.append(f"{path}: text changed, lastmod still {now['lastmod']}")
    elif now["sha256"] == was["sha256"] and now["lastmod"] != was["lastmod"]:
        bumped.append(f"{path}: date moved to {now['lastmod']} without a text change")
for path in recorded.keys() - current.keys():
    stale.append(f"{path}: gone from the sitemap, run --update")
for line in bumped:
    print("note:", line)
if stale:
    print("\n".join(stale))
    print("Bump the page's date in pool/server.py (_SEO_ARTICLE / _SEO_MODIFIED), rebuild, then run --update.")
    sys.exit(1)
print(f"PASS: {len(current)} sitemap dates match their pages' text")
