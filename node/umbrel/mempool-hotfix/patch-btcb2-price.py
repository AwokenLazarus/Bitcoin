#!/usr/bin/env python3
"""Overlay BLAKE2b BTC (BTCB2) on the mempool-hotfix price updater.

The Umbrel mempool API is /home/umbrel/mempool-hotfix (not the Docker api
container). Stock feeds are Coinbase/Kraken/… SHA Bitcoin. This patch makes
getLatestPrices() — REST /api/v1/prices and the websocket `conversions`
field — return the pool's BTCB2 volume-weighted price (Neoxa + NonKYC).

Idempotent. Usage: patch-btcb2-price.py /home/umbrel/mempool-hotfix/backend/package/tasks/price-updater.js
"""
from __future__ import annotations

import sys
from pathlib import Path

MARKER = "$applyBlake2bOverlay"

METHODS = r'''
    getLatestPrices() {
        return this.latestGoodPrices;
    }
    async $fetchBlake2bPrices() {
        const http = require("http");
        return new Promise((resolve) => {
            const req = http.get("http://127.0.0.1:8888/api/v1/prices", { timeout: 8000 }, (res) => {
                let buf = "";
                res.on("data", (c) => { buf += c; });
                res.on("end", () => {
                    try { resolve(JSON.parse(buf)); }
                    catch (e) { resolve(null); }
                });
            });
            req.on("error", () => resolve(null));
            req.on("timeout", () => { req.destroy(); resolve(null); });
        });
    }
    async $applyBlake2bOverlay() {
        const doc = await this.$fetchBlake2bPrices();
        if (!doc || !(doc.USD > 0)) {
            return false;
        }
        const prev = this.latestGoodPrices && this.latestGoodPrices.USD;
        this.latestGoodPrices = Object.assign({}, this.latestGoodPrices || this.getEmptyPricesObj(), doc);
        this.latestGoodPrices.time = doc.time || Math.round(Date.now() / 1000);
        this.latestPrices = Object.assign({}, this.latestGoodPrices);
        return this.latestGoodPrices.USD !== prev;
    }
    startBlake2bLoop() {
        if (this._blakeLoop) {
            return;
        }
        const tick = async () => {
            const changed = await this.$applyBlake2bOverlay();
            if (changed && this.ratesChangedCallback && this.latestGoodPrices.USD > 0) {
                this.ratesChangedCallback(this.latestGoodPrices);
            }
        };
        this._blakeLoop = setInterval(tick, 45000);
        tick();
    }
'''

GET_LATEST = """    getLatestPrices() {
        return this.latestGoodPrices;
    }
"""

INIT_OLD = """        this.latestGoodPrices = JSON.parse(JSON.stringify(this.latestPrices));
    }
"""

INIT_NEW = """        this.latestGoodPrices = JSON.parse(JSON.stringify(this.latestPrices));
        await this.$applyBlake2bOverlay();
        this.startBlake2bLoop();
    }
"""

CB_OLD = """        if (this.ratesChangedCallback && this.latestGoodPrices.USD > 0) {
            this.ratesChangedCallback(this.latestGoodPrices);
        }
"""

CB_NEW = """        await this.$applyBlake2bOverlay();
        if (this.ratesChangedCallback && this.latestGoodPrices.USD > 0) {
            this.ratesChangedCallback(this.latestGoodPrices);
        }
"""


def patch(text: str) -> str:
    if MARKER in text:
        return text
    if GET_LATEST not in text:
        raise SystemExit("getLatestPrices() not found")
    if INIT_OLD not in text:
        raise SystemExit("$initializeLatestPriceWithDb tail not found")
    if text.count(CB_OLD) < 1:
        raise SystemExit("ratesChangedCallback site not found")
    text = text.replace(GET_LATEST, METHODS, 1)
    text = text.replace(INIT_OLD, INIT_NEW, 1)
    text = text.replace(CB_OLD, CB_NEW)
    if MARKER not in text:
        raise SystemExit("patch did not land")
    return text


def main() -> None:
    path = Path(sys.argv[1] if len(sys.argv) > 1 else
                "/home/umbrel/mempool-hotfix/backend/package/tasks/price-updater.js")
    orig = path.read_text()
    new = patch(orig)
    if new == orig:
        print(f"already patched: {path}")
        return
    bak = path.with_suffix(path.suffix + ".bak-pre-btcb2")
    if not bak.exists():
        bak.write_text(orig)
        print(f"backup {bak}")
    path.write_text(new)
    print(f"patched {path}")


if __name__ == "__main__":
    main()
