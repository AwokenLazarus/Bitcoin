#!/usr/bin/env python3
"""RouteHash rental normaliser, stale cache, and the no-stratum copy rule.

    python3 -m pytest -q pool/test_rentals.py
"""
import json
import os
import re
import sys
import tempfile
import unittest
from pathlib import Path

import pytest

POOL = Path(__file__).resolve().parent
FIXTURE = POOL / "fixtures" / "routehash_listings_2026-10-08.json"
_cfg = POOL / "config.json"
_made_cfg = not _cfg.exists()
if _made_cfg:
    _cfg.write_text(json.dumps({"public_url": "https://pool.lazarus-xbt.xyz"}))
os.environ.setdefault("POOL_DB", str(Path(tempfile.mkdtemp(prefix="rentals-")) / "pool.sqlite"))
os.environ["POOL_UI_NO_WRITE"] = "1"
sys.path.insert(0, str(POOL))
try:
    import rentals  # noqa: E402
    import server  # noqa: E402
finally:
    if _made_cfg:
        _cfg.unlink()

LABELS = {
    "rentable": "Rentable",
    "rented": "Rented",
    "unavailable": "Unavailable",
    "live": "live",
    "sats": "sats / TH / day",
    "usdTh": "USD / TH / day",
    "btcDay": "Cost / day incl. fee",
    "xbtCost": "Cost / day in XBT",
    "minCost": "Minimum booking",
    "payXbt": "Pay in XBT",
    "payXbtDirect": "Direct, no exchange",
    "payXbtConvert": "Converted on NeoxEX",
    "fee": "Platform fee",
    "xbtDay": "Est. XBT / day",
    "usdDay": "Est. $ / day",
    "rent": "Rent on RouteHash",
}


def _section(html, section_id):
    match = re.search(rf'<section id="{section_id}".*?</section>', html, re.S)
    if not match:
        raise AssertionError(f"no #{section_id} section")
    return match.group(0)


def _intro(html):
    match = re.search(r'<section class="seo-intro".*?</section>', html, re.S)
    return match.group(0) if match else ""


def rental_copy():
    """The rental surfaces the no-stratum rule covers. Not the shared site chrome."""
    index = (POOL / "static" / "index.html").read_text(encoding="utf-8")
    parts = [_section(index, "rentals")]
    for locale in ("en", "zh-CN"):
        pack = server._i18n_dict(locale)
        parts.append(json.dumps(pack.get("rentals") or {}, ensure_ascii=False))
        for key in ("footer", "nav"):
            block = pack.get(key) or {}
            for name in ("rentals", "rentPage"):
                if name in block:
                    parts.append(str(block[name]))
        hardware = pack.get("hardware") or {}
        if "rent" in hardware:
            parts.append(str(hardware["rent"]))
    for path in ("/rentals", "/zh/rentals"):
        page = server.render_pool_index(path)
        parts.append(_intro(page))
        parts.append(_section(page, "rentals"))
        title = re.search(r"<title[^>]*>(.*?)</title>", page, re.S)
        desc = re.search(r'<meta name="description"[^>]*content="([^"]*)"', page)
        parts.append(title.group(1) if title else "")
        parts.append(desc.group(1) if desc else "")
    llms = server.llms_txt()
    parts.extend(line for line in llms.splitlines() if "/rentals" in line)
    block = re.search(r"## Renting hashrate\n.*?(?=\n## )", llms, re.S)
    parts.append(block.group(0) if block else "")
    intro = server._SEO_INTRO["/rentals"]
    parts.append(json.dumps(intro, ensure_ascii=False))
    return "\n".join(parts)


class RentalsMath(unittest.TestCase):
    def setUp(self):
        rentals.reset()
        self.raw = json.loads(FIXTURE.read_text(encoding="utf-8"))

    def test_fixture_keeps_only_blake2b_and_matches_routehash_fee(self):
        rigs = rentals.normalize(self.raw["listings"], ths_btc_day="0.01", btc_usd="82707", xbt_usd="2", xbt_btc="0.00966688")
        self.assertEqual(len(rigs), 1)
        rig = rigs[0]
        upstream = next(row for row in self.raw["listings"] if row["algo"] == "blake2b")
        self.assertEqual(rig.rig_id, upstream["rig_id"])
        self.assertEqual(rig.name, upstream["name"])
        self.assertAlmostEqual(rig.hashrate_th, 0.55, places=6)
        self.assertAlmostEqual(rig.live_th, upstream["live_th"], places=4)
        self.assertEqual(rig.price_sats_th_day, 5500)
        self.assertEqual(rig.platform_fee_bps, 200)
        self.assertTrue(rig.rentable)
        self.assertFalse(rig.is_already_rented)
        # RouteHash's own price_btc_day_with_fee and est_min_cost_btc, recomputed here.
        self.assertAlmostEqual(rig.btc_day_with_fee, upstream["price_btc_day_with_fee"], places=12)
        self.assertAlmostEqual(rig.min_cost_btc, upstream["est_min_cost_btc"], places=12)
        self.assertAlmostEqual(rig.min_cost_usd, rig.min_cost_btc * 82707, places=8)
        self.assertAlmostEqual(rig.usd_cost_day, rig.btc_day_with_fee * 82707, places=8)
        self.assertAlmostEqual(rig.price_usd_th_day, 5500 / 1e8 * 82707, places=6)
        self.assertAlmostEqual(rig.xbt_cost_day, rig.btc_day_with_fee / 0.00966688, places=10)
        self.assertAlmostEqual(rig.min_cost_xbt, rig.min_cost_btc / 0.00966688, places=10)
        self.assertTrue(rig.takes_xbt)
        self.assertAlmostEqual(rig.xbt_day, 0.55 * 0.01, places=8)
        self.assertAlmostEqual(rig.usd_day, rig.xbt_day * 2, places=8)
        self.assertEqual(rig.url, "https://app.routehash.com/?rent=2")
        self.assertNotIn("idle_destination_label", rig.to_json())

    def test_missing_fields_do_not_become_zero(self):
        rows = [
            {"algo": "sha256", "rig_id": 1, "name": "SHA", "hashrate_th": 100, "price_sats_th_day": 1, "platform_fee_bps": 200},
            "not-a-row",
            {"algo": "blake2b", "name": "no id"},
            {"algo": "BLAKE2b", "rig_id": 7, "hashrate_th": 1, "price_sats_th_day": 1000, "min_hours": 4},
            {"algo": "blake2b", "id": 8, "hashrate_th": 2, "price_sats_th_day": 1000, "platform_fee_bps": 0, "min_hours": 24},
        ]
        rigs = rentals.normalize(rows, ths_btc_day=None, btc_usd=None, xbt_usd=None)
        self.assertEqual([rig.rig_id for rig in rigs], [8, 7])
        missing = next(rig for rig in rigs if rig.rig_id == 7)
        self.assertEqual(missing.name, "#7")
        self.assertIsNone(missing.btc_day_with_fee)
        self.assertIsNone(missing.min_cost_btc)
        self.assertIsNone(missing.min_cost_usd)
        self.assertIsNone(missing.xbt_day)
        self.assertIsNone(missing.xbt_cost_day)
        self.assertIsNone(missing.min_cost_xbt)
        self.assertFalse(missing.takes_xbt)
        free = next(rig for rig in rigs if rig.rig_id == 8)
        day, booking = rentals.fee_math(1000, 2, 0, 24)
        self.assertAlmostEqual(free.btc_day_with_fee, float(day), places=12)
        self.assertAlmostEqual(free.min_cost_btc, float(booking), places=12)

    def test_price_parsers(self):
        self.assertEqual(rentals._parse_btc_usd("mempool.space", '{"USD": 82707}'), 82707)
        kraken = '{"result": {"XXBTZUSD": {"c": ["82679.80000", "0.1"]}}}'
        self.assertEqual(rentals._parse_btc_usd("kraken", kraken), 82679.8)
        coinbase = '{"data": {"amount": "82683.285", "base": "BTC", "currency": "USD"}}'
        self.assertEqual(rentals._parse_btc_usd("coinbase", coinbase), 82683.285)
        self.assertIsNone(rentals._parse_btc_usd("mempool.space", '{"USD": 0}'))
        neoxex = '{"success": true, "pair": "BTCB2_BTC", "ticker": {"lastPrice": 0.0097, "bestBid": 0.00966688, "bestAsk": 0.0097}}'
        self.assertEqual(rentals._parse_xbt_btc(neoxex), 0.00966688)
        self.assertIsNone(rentals._parse_xbt_btc('{"ticker": {"bestBid": 0}}'))
        self.assertIsNone(rentals._parse_xbt_btc('{"success": false}'))

    def test_upstream_error_keeps_the_stale_copy(self):
        kept = [{"algo": "blake2b", "rig_id": 9, "name": "Kept", "hashrate_th": 1,
                 "price_sats_th_day": 1000, "platform_fee_bps": 200, "min_hours": 3}]
        with rentals._lock:
            rentals._listings.update(rows=kept, ts=0, fetched_at="2026-10-08T00:00:00Z", refreshing=False, error="")

        def down(_url):
            raise RuntimeError("down")

        rows, _fetched, _err = rentals.listings_catalog(down, max_age=900)
        rentals.wait_idle()
        rows, fetched, err = rentals.listings_catalog(down, max_age=900)
        rentals.wait_idle()
        self.assertEqual(rows, kept)
        self.assertEqual(fetched, "2026-10-08T00:00:00Z")
        self.assertIn("down", err)
        doc = rentals.payload(ths_btc_day=1, xbt_usd=None, get_text=down, max_age=900)
        rentals.wait_idle()
        self.assertEqual(doc["rigs"][0]["name"], "Kept")
        self.assertIn("down", doc["error"])
        self.assertIsNone(doc["btc_usd"])
        self.assertIsNone(doc["xbt_btc"])

    def test_cold_failure_is_an_empty_list_not_a_fake_zero_price(self):
        def down(_url):
            raise RuntimeError("down")

        rows, _fetched, err = rentals.listings_catalog(down)
        rentals.wait_idle()
        self.assertEqual(rows, [])
        self.assertIn("down", err)

    def test_cards_for_zero_and_one_listing(self):
        rigs = rentals.normalize(self.raw["listings"], ths_btc_day="0.01", btc_usd="82707", xbt_usd="2", xbt_btc="0.00966688")
        one = rentals.cards_html([rig.to_json() for rig in rigs], LABELS)
        self.assertIn("Direct, no exchange", one)
        self.assertIn("0.00319183 XBT", one)
        converted = dict(rigs[0].to_json(), takes_xbt=False)
        self.assertIn("Converted on NeoxEX", rentals.card_html(converted, LABELS))
        zero = rentals.cards_html([], LABELS)
        self.assertIn("data-rig=\"2\"", one)
        self.assertIn("Rent on RouteHash", one)
        self.assertIn("https://app.routehash.com/?rent=2", one)
        self.assertNotIn(":23334", one)
        self.assertEqual(zero, "")
        shell = '<ul class="hw-grid" id="rentals-grid"></ul><p class="note" id="rentals-empty" hidden>'
        filled = rentals.apply_section(shell, [rig.to_json() for rig in rigs], LABELS)
        empty = rentals.apply_section(shell, [], LABELS)
        self.assertIn("data-rig=\"2\"", filled)
        self.assertIn('id="rentals-empty" hidden', filled)
        self.assertNotIn("data-rig=", empty)
        self.assertIn('id="rentals-empty"', empty)
        self.assertNotIn('id="rentals-empty" hidden', empty)


class RentalsPage(unittest.TestCase):
    def setUp(self):
        rentals.reset()
        server._I18N_CACHE.clear()

    def test_page_links_sitemap_and_llms(self):
        page = server.render_pool_index("/rentals")
        zh = server.render_pool_index("/zh/rentals")
        self.assertIn("https://pool.lazarus-xbt.xyz/rentals", page)
        self.assertIn("https://pool.lazarus-xbt.xyz/zh/rentals", page)
        self.assertIn("Rent BLAKE2b hashrate, pointed at your own node", page)
        self.assertIn("租 BLAKE2b 算力，指向你自己的节点", zh)
        self.assertIn('href="/hardware"', page)
        self.assertIn('href="/rentals"', server.render_pool_index("/hardware"))
        sitemap = server.sitemap_xml()
        self.assertIn("https://pool.lazarus-xbt.xyz/rentals", sitemap)
        self.assertIn("https://pool.lazarus-xbt.xyz/zh/rentals", sitemap)
        llms = server.llms_txt()
        self.assertIn("## Renting hashrate", llms)
        self.assertIn("/rentals", llms)
        self.assertIn("footer.rentals", (POOL / "static" / "index.html").read_text(encoding="utf-8"))

    def test_no_stratum_in_rental_copy(self):
        copy = rental_copy()
        self.assertEqual(rentals.stratum_hits(copy), [])
        planted = copy + "\npoint the rental at :23334"
        self.assertIn(":23334", rentals.stratum_hits(planted))

    def test_fixture_renders_into_the_page(self):
        body = FIXTURE.read_text(encoding="utf-8")

        def get_text(url):
            if url == rentals.LISTINGS_URL:
                return body
            if "mempool.space" in url:
                return '{"USD": 82707}'
            if url == rentals.XBT_BTC_URL:
                return '{"success": true, "ticker": {"bestBid": 0.00966688}}'
            raise RuntimeError(url)

        doc = rentals.payload(ths_btc_day="0.01", xbt_usd="2", get_text=get_text)
        self.assertEqual(doc["source"], "RouteHash")
        self.assertEqual(doc["source_url"], "https://app.routehash.com/")
        self.assertEqual(doc["btc_usd_source"], "mempool.space")
        self.assertEqual(doc["xbt_btc"], 0.00966688)
        self.assertEqual(doc["xbt_btc_source"], "NeoxEX")
        self.assertEqual(doc["error"], "")
        self.assertTrue(doc["fetched_at"])
        page = server.render_pool_index("/rentals")
        self.assertIn("data-rig=\"2\"", page)
        self.assertIn("HSBOX", page)


@pytest.mark.live
class RentalsLive(unittest.TestCase):
    def test_live_listings_and_btc_quote(self):
        rentals.reset()
        doc = server.rentals_payload()
        rentals.wait_idle()
        self.assertEqual(doc["source_url"], "https://app.routehash.com/")
        self.assertEqual(doc["error"], "")
        self.assertIsInstance(doc["rigs"], list)
        self.assertGreater(doc["btc_usd"], 1000)
        self.assertIn(doc["btc_usd_source"], {"mempool.space", "kraken", "coinbase"})
        self.assertGreater(doc["xbt_btc"], 0)
        self.assertEqual(doc["xbt_btc_source"], "NeoxEX")
        for rig in doc["rigs"]:
            self.assertTrue(rig["url"].startswith("https://app.routehash.com/?rent="))
            self.assertNotIn("stratum.", json.dumps(rig))
