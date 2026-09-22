#!/usr/bin/env python3
"""Turn the captured mempool.space-branded files in dist/ into Lazarus Mempool ones.

The capture source is the node at home and is left exactly as it is; everything here happens to
the copy in dist/, after build.sh has fetched it. Every rule is idempotent, so running this on a
dist/ that was already processed changes nothing. verify-mempool.py imports shell(), config(),
manifest() and theme_version() to apply the same rules to the node's bytes before comparing.

Usage: postprocess.py <dist dir>"""
import hashlib, re, shutil, sys
from pathlib import Path

SITE = "https://mempool.lazarus-xbt.xyz"
NAME = "Lazarus Mempool"
TITLE = "Lazarus Mempool - BLAKE2b BTC Explorer"
DESCRIPTION = "Block explorer and mempool visualiser for BLAKE2b BTC, run by Lazarus Pool."
COLOUR = "#100d08"
CREST = "chi-rho-crest.svg"
CONFIG = {"ACCELERATOR_BUTTON": "false", "MEMPOOL_WEBSITE_URL": f"'{SITE}'", "SERVICES_API": f"'{SITE}/api/v1/services'"}
# mempool.space marketing media nothing on this site shows: the promo video is only in the
# official-instance part of the About page, the release screenshots are referenced nowhere.
MEDIA = ("resources/promo-video", "resources/screenshots")


def theme_version(css, js):
    """Cache-busting value for /lazarus/theme.css and theme.js: changes whenever either does."""
    return hashlib.sha256(css + js).hexdigest()[:10]


def shell_rules(version, crest):
    """(name, pattern, replacement) for an index.html. `crest` says the theme ships CREST."""
    meta = r'(<meta (?:name|property)="%s" content=")%s(")'
    rules = [
        ("title", r"<title>mempool - Bitcoin Explorer</title>", f"<title>{TITLE}</title>"),
        ("canonical", r'(rel="canonical" href=")https://mempool\.space/?(")', rf"\g<1>{SITE}\g<2>"),
        ("og:url", meta % ("og:url", r"https://mempool\.space[^\"]*"), rf"\g<1>{SITE}\g<2>"),
        ("titles", meta % ("(?:og:title|twitter:title|og:site_name)", r"[^\"]*[Mm]empool[^\"]*"), rf"\g<1>{NAME}\g<2>"),
        ("description", meta % ("(?:description|og:description|twitter:description)", r"[^\"]*"), rf"\g<1>{DESCRIPTION}\g<2>"),
        ("theme-color", r'content="#1d1f31"', f'content="{COLOUR}"'),
        ("theme-version", r"(/lazarus/theme\.(?:css|js)\?v=)[^\"']*", rf"\g<1>{version}"),
    ]
    if crest:
        rules.append(("image", meta % ("(?:og:image|twitter:image)", r"https://mempool\.space/[^\"]*"), rf"\g<1>{SITE}/lazarus/{CREST}\g<2>"))
    return rules


def shell(html, version, crest, changed=None):
    """Apply the shell rules to one index.html (str). Names of rules that changed it go in `changed`."""
    for name, pattern, replacement in shell_rules(version, crest):
        out = re.sub(pattern, replacement, html)
        if out != html and changed is not None:
            changed.add(name)
        html = out
    return html


def config(js):
    """Set the CONFIG keys in a resources/config.js (`window.__env.KEY = value;` lines)."""
    for key, value in CONFIG.items():
        line = re.compile(rf"^(\s*window\.__env\.{key}\s*=\s*)[^;\n]*;", re.M)
        if line.search(js):
            js = line.sub(lambda m: f"{m.group(1)}{value};", js)
            continue
        # Absent: add it inside the closure, or after it if the file is not shaped as expected.
        new = f"    window.__env.{key} = {value};\n"
        end = js.rfind("}(")
        start = js.rfind("\n", 0, end) + 1 if end >= 0 else -1
        js = js[:start] + new + js[start:] if end >= 0 else js.rstrip("\n") + f"\nwindow.__env = window.__env || {{}};\n{new.lstrip()}"
    return js


def manifest(text):
    """Name and colours in site.webmanifest, edited in place so the rest stays byte-identical."""
    for key, value in (("name", NAME), ("short_name", NAME), ("theme_color", COLOUR), ("background_color", COLOUR)):
        text = re.sub(rf'("{key}"\s*:\s*")[^"]*(")', rf"\g<1>{value}\g<2>", text)
    return text


MINING_UNITS_STOCK = "getMiningUnits(){let P=18;"
MINING_UNITS_PH = "getMiningUnits(){let P=15;"


def mining_units(js):
    """Stock v3.3.1 hard-codes EH/s (10^18) for pool hashrates: the pool pie tooltip, the pools
    table and the dashboard. This network is ~35 PH/s, so it read 0.03 EH/s. Use PH/s (10^15)."""
    return js.replace(MINING_UNITS_STOCK, MINING_UNITS_PH)


BUNDLE_TAG = ".ph1"


def rename_patched_bundle(main_js):
    """The patched bundle keeps its content hash in the name, and it is served `immutable` for 30
    days, so a browser that already has it would never ask again. Give it a new name and point
    that locale's index.html at it. Bump BUNDLE_TAG whenever the bundle patch changes."""
    if main_js.name.endswith(BUNDLE_TAG + ".js"):
        return False
    new = main_js.with_name(main_js.name[: -len(".js")] + BUNDLE_TAG + ".js")
    index = main_js.parent / "index.html"
    html = index.read_text(encoding="utf-8")
    if html.count(main_js.name) != 1:
        raise SystemExit(f"{index}: expected one reference to {main_js.name}, found {html.count(main_js.name)}")
    main_js.rename(new)
    index.write_text(html.replace(main_js.name, new.name), encoding="utf-8")
    return True


def rewrite(path, fn):
    old = path.read_text(encoding="utf-8")
    new = fn(old)
    if new != old:
        path.write_text(new, encoding="utf-8")
    return new != old


def main(dist):
    dist = Path(dist)
    version = theme_version((dist / "lazarus/theme.css").read_bytes(), (dist / "lazarus/theme.js").read_bytes())
    crest = (dist / "lazarus" / CREST).is_file()
    counts = {name: 0 for name, _, _ in shell_rules(version, True)}
    for page in sorted(dist.rglob("index.html")):
        changed = set()
        rewrite(page, lambda html: shell(html, version, crest, changed))
        for name in changed:
            counts[name] += 1
    counts["config.js"] = sum(rewrite(p, config) for p in sorted(dist.rglob("config.js")))
    mains = sorted(dist.glob("*/main.*.js"))
    counts["main.js (mining units in PH/s)"] = sum(rewrite(p, mining_units) for p in mains)
    left = [str(p) for p in mains if MINING_UNITS_STOCK in p.read_text(encoding="utf-8")]
    if not mains or left:
        raise SystemExit(f"mining units patch did not apply: {len(mains)} bundles, still EH/s in {left[:3]}")
    counts["main.js renamed for browser caches"] = sum(rename_patched_bundle(p) for p in mains if not p.name.endswith(BUNDLE_TAG + ".js"))
    counts["webmanifest"] = int(rewrite(dist / "resources/favicons/site.webmanifest", manifest))
    counts["media"] = 0
    for rel in MEDIA:
        target = dist / rel
        if target.exists():
            files = [f for f in target.rglob("*") if f.is_file()]
            print(f"postprocess: removed {rel} ({len(files)} files, {sum(f.stat().st_size for f in files) / 1e6:.1f} MB)")
            shutil.rmtree(target)
            counts["media"] += 1
    print(f"postprocess: theme v={version}; files changed per rule: " + " ".join(f"{k}={v}" for k, v in counts.items()))


if __name__ == "__main__":
    main(sys.argv[1] if len(sys.argv) > 1 else Path(__file__).parent / "dist")
