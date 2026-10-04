#!/usr/bin/env python3
"""pool/gateways.json drives the gateway pick list, llms.txt and /api/gateway-builds.

    python3 pool/test_gateway_builds.py

Imports server.py read-only the way dev/serve.py does (a throwaway DB, and a config.json only if
none exists), so it runs anywhere the tree is checked out.
"""
import json
import os
import re
import sys
import tempfile
import unittest
from pathlib import Path

POOL = Path(__file__).resolve().parent
_cfg = POOL / "config.json"
_made_cfg = not _cfg.exists()
if _made_cfg:
    _cfg.write_text(json.dumps({"public_url": "https://pool.lazarus-xbt.xyz"}))
os.environ.setdefault("POOL_DB", str(Path(tempfile.mkdtemp(prefix="gwb-")) / "pool.sqlite"))
os.environ["POOL_UI_NO_WRITE"] = "1"
sys.path.insert(0, str(POOL))
try:
    import server  # noqa: E402
finally:
    if _made_cfg:
        _cfg.unlink()


def pick_order(page):
    ol = re.search(r'<ol class="gw-rank".*?</ol>', page, re.S).group(0)
    return re.findall(r'<li(?: class="reco")? data-gw="([a-z0-9-]+)">', ol), ol


class GatewayBuilds(unittest.TestCase):
    def setUp(self):
        self.doc = server.gateway_builds()
        self.ids = [g["id"] for g in self.doc["gateways"]]

    def test_file_is_complete(self):
        self.assertTrue(self.ids, "gateways.json lists no gateways")
        self.assertEqual(len(self.ids), len(set(self.ids)), "duplicate id")
        html = (POOL / "static" / "index.html").read_text(encoding="utf-8")
        en = (POOL / "static" / "i18n" / "en.js").read_text(encoding="utf-8")
        zh = (POOL / "static" / "i18n" / "zh-CN.js").read_text(encoding="utf-8")
        for g in self.doc["gateways"]:
            for k in ("name", "repo", "i18n", "summary"):
                self.assertTrue(g.get(k), f"{g['id']} has no {k}")
            self.assertNotIn("PLACEHOLDER", g["summary"])
            self.assertIn(f'data-gw="{g["id"]}"', html, f"index.html has no item for {g['id']}")
            key = g["i18n"].split(".", 1)[1]
            for name, d in (("en", en), ("zh-CN", zh)):
                self.assertRegex(d, rf'"{re.escape(key)}":', f"{name} has no {g['i18n']}")
        self.assertLessEqual(sum(1 for g in self.doc["gateways"] if g.get("reco")), 1)
        if any(g.get("reco") for g in self.doc["gateways"]):
            self.assertTrue(self.doc["gateways"][0].get("reco"), "only the first entry can be recommended")

    def test_page_follows_the_file(self):
        page = server.render_pool_index("/")
        order, ol = pick_order(page)
        self.assertEqual(order[: len(self.ids)], self.ids)
        nums = re.findall(r'<span class="n" aria-hidden="true">(\d+)</span>', ol)
        self.assertEqual(nums, [str(i) for i in range(1, len(order) + 1)])
        tags = ol.count('data-i18n="connect.reco"')
        self.assertEqual(tags, 1 if self.doc["gateways"][0].get("reco") else 0)
        if tags:
            first = ol.split("</li>", 1)[0]
            self.assertIn('class="reco"', first)
            self.assertIn('data-i18n="connect.reco"', first)

    def test_reordering_is_an_edit_to_the_file_alone(self):
        saved = server.gateway_builds
        rev = dict(self.doc, gateways=list(reversed(self.doc["gateways"])))
        rev["gateways"] = [dict(g, reco=(i == 0)) for i, g in enumerate(rev["gateways"])]
        server.gateway_builds = lambda: rev
        try:
            order, ol = pick_order(server.render_pool_index("/"))
            self.assertEqual(order[: len(self.ids)], list(reversed(self.ids)))
            self.assertEqual(ol.count('class="reco"'), 1)
            self.assertIn(f'<li class="reco" data-gw="{self.ids[-1]}">', ol)
            llms = server.llms_txt()
            self.assertLess(llms.index(rev["gateways"][0]["repo"]), llms.index(rev["gateways"][-1]["repo"]))
        finally:
            server.gateway_builds = saved

    def test_llms_txt_lists_every_gateway_in_order(self):
        llms = server.llms_txt()
        self.assertIn("## Which DATUM gateway", llms)
        self.assertNotIn("GATEWAYSPLACEHOLDER", llms)
        self.assertNotIn("SITEPLACEHOLDER", llms)
        pos = [llms.index(g["repo"]) for g in self.doc["gateways"]]
        self.assertEqual(pos, sorted(pos))

    def test_zh_page_keeps_the_same_order(self):
        order, _ = pick_order(server.render_pool_index("/zh/"))
        self.assertEqual(order[: len(self.ids)], self.ids)



class KitDownloads(unittest.TestCase):
    """/kit/install.sh and the kit tarball redirect to R2; nothing else under /kit/ does."""

    @classmethod
    def setUpClass(cls):
        import http.client
        import threading

        cls.http = http.client
        cls.srv = server.ThreadingHTTPServer(("127.0.0.1", 0), server.Handler)
        threading.Thread(target=cls.srv.serve_forever, daemon=True).start()

    @classmethod
    def tearDownClass(cls):
        cls.srv.shutdown()

    def get(self, path):
        c = self.http.HTTPConnection("127.0.0.1", self.srv.server_address[1], timeout=10)
        c.request("GET", path)
        r = c.getresponse()
        r.read()
        return r.status, r.getheader("Location")

    def test_kit_files_redirect_to_r2(self):
        for name in ("install.sh", "lazarus-gateway-kit-v1.tar.gz"):
            self.assertEqual(self.get("/kit/" + name), (302, "https://snapshots.lazarus-xbt.xyz/kit/" + name))

    def test_other_kit_paths_do_not_redirect(self):
        for path in ("/kit/../config.json", "/kit/evil.sh", "/kit/install.sh.bak"):
            self.assertNotEqual(self.get(path)[0], 302, path)


if __name__ == "__main__":
    unittest.main(verbosity=2)
