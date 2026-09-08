#!/usr/bin/env python3
"""Scale mempool-hotfix /api/v1/historical-price from SHA prints to BTCB2.

Block pages, tx fiat, and subsidy+fees call this endpoint with the block
timestamp. The DB still stores Coinbase/Kraken SHA prices (~$80k). Multiply
the series so the newest point matches getLatestPrices() (already BTCB2).

Idempotent. Usage:
  patch-btcb2-historical.py /home/umbrel/mempool-hotfix/backend/package/api/mining/mining-routes.js
"""
from __future__ import annotations

import sys
from pathlib import Path

MARKER = "function scaleHistoricalToBlake2b"

IMPORT_OLD = """const PricesRepository_1 = __importDefault(require("../../repositories/PricesRepository"));
"""

IMPORT_NEW = """const PricesRepository_1 = __importDefault(require("../../repositories/PricesRepository"));
const price_updater_1 = __importDefault(require("../../tasks/price-updater"));
"""

SEND_OLD = """            res.status(200).send(response);
        }
        catch (e) {
            (0, api_1.handleError)(req, res, 500, 'Failed to get historical prices');
"""

SEND_NEW = """            scaleHistoricalToBlake2b(response);
            res.status(200).send(response);
        }
        catch (e) {
            (0, api_1.handleError)(req, res, 500, 'Failed to get historical prices');
"""

HELPER = r'''
function scaleHistoricalToBlake2b(response) {
    const live = price_updater_1.default.getLatestPrices();
    if (!response || !response.prices || !response.prices.length || !live || !(live.USD > 0)) {
        return;
    }
    let latest = response.prices[0];
    for (const p of response.prices) {
        if ((p.time || 0) >= (latest.time || 0)) {
            latest = p;
        }
    }
    const sha = latest.USD;
    if (!(sha > 20000)) {
        return;
    }
    const f = live.USD / sha;
    for (const p of response.prices) {
        for (const k of Object.keys(p)) {
            if (k === "time") {
                continue;
            }
            if (typeof p[k] === "number" && p[k] > 0) {
                p[k] = Math.round(p[k] * f * 100) / 100;
            }
        }
    }
}
'''


def patch(text: str) -> str:
    if "function scaleHistoricalToBlake2b" in text:
        return text
    if IMPORT_OLD not in text:
        raise SystemExit("PricesRepository import not found")
    if SEND_OLD not in text:
        raise SystemExit("historical-price send site not found")
    if IMPORT_NEW.strip() not in text:
        text = text.replace(IMPORT_OLD, IMPORT_NEW, 1)
    # Standalone helper after imports (route handler is not bound, so no `this`).
    needle = "const api_1 = require(\"../../utils/api\");\n"
    alt = "const api_1 = require('../../utils/api');\n"
    if needle in text:
        text = text.replace(needle, needle + HELPER, 1)
    elif alt in text:
        text = text.replace(alt, alt + HELPER, 1)
    else:
        # compiled import style
        compiled = "const api_1 = require(\"../../utils/api\");"
        if compiled not in text:
            raise SystemExit("api import not found for helper insert")
        text = text.replace(compiled, compiled + "\n" + HELPER, 1)
    text = text.replace(SEND_OLD, SEND_NEW, 1)
    if "scaleHistoricalToBlake2b" not in text:
        raise SystemExit("patch did not land")
    return text


def main() -> None:
    path = Path(sys.argv[1] if len(sys.argv) > 1 else
                "/home/umbrel/mempool-hotfix/backend/package/api/mining/mining-routes.js")
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
