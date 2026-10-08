"""RouteHash BLAKE2b rentals for the pool site.

The marketplace list is one JSON document (a handful of rigs, not a stream), so it
is held as a list. Fee maths is computed here from sats, hashrate, fee and hours.
USD of a BTC amount uses a cached BTC/USD quote. XBT yield uses the caller's
per-TH/day figure, the same one ``/api/hardware`` publishes.
"""

from __future__ import annotations

import json
import re
import threading
import time
from dataclasses import asdict, dataclass
from decimal import Decimal
from html import escape

LISTINGS_URL = "https://app.routehash.com/api/marketplace/listings"
SOURCE_URL = "https://app.routehash.com/"
SOURCE_NAME = "RouteHash"
# Confirmed in the RouteHash app: `?rent=` opens the rent modal for that rig id.
RENT_URL = "https://app.routehash.com/?rent={rig_id}"

# Compared 2026-10-08: mempool.space, Kraken last and Coinbase spot agreed within
# about $30. mempool.space is the one we keep; the other two are only if it fails.
BTC_USD_SOURCES = (
    ("mempool.space", "https://mempool.space/api/v1/prices"),
    ("kraken", "https://api.kraken.com/0/public/Ticker?pair=XBTUSD"),
    ("coinbase", "https://api.coinbase.com/v2/prices/BTC-USD/spot"),
)

NOTE = (
    "Estimates use current network difficulty and the 3.125 XBT base subsidy, "
    "through your own DATUM gateway on Lazarus (bonus included). Rental prices "
    "are RouteHash's, paid in BTC. Luck, transaction fees and electricity are not included."
)

# Hits inside rental copy only. `stratum.` is a hostname (`stratum.example`), not the
# word "stratum" in "stratum listener".
_FORBIDDEN = (
    re.compile(r"stratum\.", re.I),
    re.compile(r":23334\b"),
    re.compile(r":3333\b"),
    re.compile(r":23335\b"),
    re.compile(r"pool\.routehash\.com", re.I),
)

_SATS = Decimal(100_000_000)
_lock = threading.Lock()
_listings = {
    "rows": None,
    "ts": 0.0,
    "fetched_at": None,
    "refreshing": False,
    "error": "",
}
_btc = {"usd": None, "source": "", "ts": 0.0, "refreshing": False, "error": "", "stale": False}
_view: dict[str, list | None] = {"rigs": None}


def stratum_hits(text: str) -> list[str]:
    """Forbidden endpoint spellings found in `text`. Empty means the copy is clean."""
    found = []
    for rx in _FORBIDDEN:
        match = rx.search(text or "")
        if match:
            found.append(match.group(0))
    return found


def reset() -> None:
    """Drop cached listings, the BTC quote and the last rendered rigs. For tests."""
    with _lock:
        _listings.update(rows=None, ts=0.0, fetched_at=None, refreshing=False, error="")
        _btc.update(usd=None, source="", ts=0.0, refreshing=False, error="", stale=False)
        _view["rigs"] = None


def view_rigs() -> list | None:
    """Last normalised rigs, or None if this process has not fetched yet."""
    with _lock:
        rows = _view["rigs"]
        return None if rows is None else list(rows)


def _dec(value) -> Decimal | None:
    if value is None or value == "":
        return None
    try:
        number = Decimal(str(value))
    except Exception:
        return None
    if not number.is_finite():
        return None
    return number


def _pos_float(value) -> float | None:
    number = _dec(value)
    if number is None or number <= 0:
        return None
    return float(number)


def _int(value) -> int | None:
    number = _dec(value)
    if number is None:
        return None
    return int(number)


def _f(number: Decimal | None) -> float | None:
    if number is None:
        return None
    return float(number)


def fee_math(sats_th_day, hashrate_th, fee_bps, min_hours):
    """BTC per day including the platform fee, and the minimum-booking cost.

    ``btc_day_with_fee = hashrate_th * sats_th_day / 1e8 * (1 + fee_bps/10000)``.
    The minimum booking is that day rate times ``min_hours / 24``. A missing fee
    or a missing input is None, not zero: a caller must be able to tell.
    """
    sats = _dec(sats_th_day)
    th = _dec(hashrate_th)
    bps = _dec(fee_bps)
    hours = _dec(min_hours)
    if sats is None or th is None or th <= 0 or sats < 0 or bps is None or bps < 0:
        return None, None
    day = (sats / _SATS) * th * (Decimal(1) + bps / Decimal(10_000))
    booking = None
    if hours is not None and hours > 0:
        booking = day * (hours / Decimal(24))
    return day, booking


@dataclass(frozen=True)
class Rig:
    name: str
    rig_id: int
    hashrate_th: float | None
    live_th: float | None
    status: str
    rentable: bool
    is_already_rented: bool
    min_hours: float | None
    max_hours: float | None
    price_sats_th_day: int | None
    price_usd_th_day: float | None
    btc_day_with_fee: float | None
    min_cost_btc: float | None
    min_cost_usd: float | None
    platform_fee_bps: int | None
    xbt_day: float | None
    usd_day: float | None
    url: str

    def to_json(self) -> dict:
        return asdict(self)


def parse_listings(body: str) -> list:
    data = json.loads(body)
    rows = data.get("listings") if isinstance(data, dict) else data
    if not isinstance(rows, list):
        raise ValueError("routehash listings unexpected")
    return rows


def _parse_btc_usd(source: str, body: str) -> float | None:
    data = json.loads(body)
    if source == "mempool.space":
        return _pos_float(data.get("USD")) if isinstance(data, dict) else None
    if source == "kraken":
        result = data.get("result") if isinstance(data, dict) else None
        if not isinstance(result, dict) or not result:
            return None
        pair = result.get("XXBTZUSD") or result.get("XBTUSD") or next(iter(result.values()))
        last = pair.get("c") if isinstance(pair, dict) else None
        if isinstance(last, list) and last:
            return _pos_float(last[0])
        return None
    if source == "coinbase":
        inner = data.get("data") if isinstance(data, dict) else None
        if not isinstance(inner, dict):
            return None
        return _pos_float(inner.get("amount"))
    return None


def quote_btc_usd(get_text) -> tuple[float | None, str]:
    """First source that answers. mempool.space, then Kraken, then Coinbase."""
    errors = []
    for name, url in BTC_USD_SOURCES:
        try:
            usd = _parse_btc_usd(name, get_text(url))
        except Exception as exc:
            errors.append(f"{name}: {exc}")
            continue
        if usd:
            return usd, name
        errors.append(f"{name}: no price")
    raise RuntimeError("; ".join(errors) or "no btc quote")


def normalize(rows, *, ths_btc_day, btc_usd, xbt_usd) -> list[Rig]:
    """Blake2b rigs only. Yield is advertised TH times ``ths_btc_day`` (XBT/TH/day)."""
    ths_rate = _dec(ths_btc_day)
    btc = _dec(btc_usd)
    xbt_px = _dec(xbt_usd)
    if ths_rate is not None and ths_rate <= 0:
        ths_rate = None
    rigs: list[Rig] = []
    for row in rows or []:
        if not isinstance(row, dict):
            continue
        if str(row.get("algo") or "").lower() != "blake2b":
            continue
        rig_id = _int(row.get("rig_id") if row.get("rig_id") is not None else row.get("id"))
        if rig_id is None:
            continue
        name = str(row.get("name") or "").strip() or f"#{rig_id}"
        advertised = _pos_float(row.get("hashrate_th"))
        live = _pos_float(row.get("live_th"))
        sats = _int(row.get("price_sats_th_day"))
        fee_bps = _int(row.get("platform_fee_bps"))
        min_hours = _pos_float(row.get("min_hours"))
        max_hours = _pos_float(row.get("max_hours"))
        day, booking = fee_math(sats, advertised, fee_bps, min_hours)
        usd_th = None
        if sats is not None and sats >= 0 and btc is not None and btc > 0:
            usd_th = _f((Decimal(sats) / _SATS) * btc)
        xbt_day = None
        if advertised is not None and ths_rate is not None:
            xbt_day = _f(Decimal(str(advertised)) * ths_rate)
        usd_day = None
        if xbt_day is not None and xbt_px is not None and xbt_px > 0:
            usd_day = _f(Decimal(str(xbt_day)) * xbt_px)
        rigs.append(
            Rig(
                name=name,
                rig_id=rig_id,
                hashrate_th=advertised,
                live_th=live,
                status=str(row.get("status") or ""),
                rentable=bool(row.get("rentable")),
                is_already_rented=bool(row.get("is_already_rented")),
                min_hours=min_hours,
                max_hours=max_hours,
                price_sats_th_day=sats,
                price_usd_th_day=usd_th,
                btc_day_with_fee=_f(day),
                min_cost_btc=_f(booking),
                min_cost_usd=_f(booking * btc) if booking is not None and btc is not None and btc > 0 else None,
                platform_fee_bps=fee_bps,
                xbt_day=xbt_day,
                usd_day=usd_day,
                url=RENT_URL.format(rig_id=rig_id),
            )
        )
    rigs.sort(key=lambda rig: (-(rig.hashrate_th or 0), rig.name))
    return rigs


def _iso(ts: float) -> str:
    return time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(ts))


def _fetch_listings(get_text):
    rows = parse_listings(get_text(LISTINGS_URL))
    return rows, _iso(time.time())


def _listings_refresh(get_text) -> None:
    try:
        rows, fetched_at = _fetch_listings(get_text)
        with _lock:
            _listings["rows"] = rows
            _listings["ts"] = time.time()
            _listings["fetched_at"] = fetched_at
            _listings["error"] = ""
    except Exception as exc:
        print("rentals", exc, flush=True)
        with _lock:
            _listings["error"] = str(exc)
            if _listings["rows"] is None:
                _listings["ts"] = time.time()
    finally:
        with _lock:
            _listings["refreshing"] = False


def listings_catalog(get_text, max_age=900.0):
    """Raw marketplace rows. A failed refresh keeps the previous rows and sets ``error``."""
    now = time.time()
    with _lock:
        rows = _listings["rows"]
        age = now - _listings["ts"]
        err = _listings["error"]
        fetched_at = _listings["fetched_at"]
        if rows is not None and age < max_age:
            return rows, fetched_at, err
        if not _listings["refreshing"]:
            _listings["refreshing"] = True
            threading.Thread(target=_listings_refresh, args=(get_text,), daemon=True).start()
        if rows is not None:
            return rows, fetched_at, err
    try:
        rows, fetched_at = _fetch_listings(get_text)
        with _lock:
            _listings["rows"] = rows
            _listings["ts"] = time.time()
            _listings["fetched_at"] = fetched_at
            _listings["error"] = ""
        return rows, fetched_at, ""
    except Exception as exc:
        print("rentals", exc, flush=True)
        with _lock:
            _listings["error"] = str(exc)
            return _listings["rows"] or [], _listings["fetched_at"], str(exc)


def _btc_refresh(get_text) -> None:
    try:
        usd, source = quote_btc_usd(get_text)
        with _lock:
            _btc["usd"] = usd
            _btc["source"] = source
            _btc["ts"] = time.time()
            _btc["error"] = ""
            _btc["stale"] = False
    except Exception as exc:
        print("rentals btc", exc, flush=True)
        with _lock:
            _btc["error"] = str(exc)
            _btc["stale"] = _btc["usd"] is not None
            if _btc["usd"] is None:
                _btc["ts"] = time.time()
    finally:
        with _lock:
            _btc["refreshing"] = False


def btc_usd_catalog(get_text, max_age=900.0):
    """Cached BTC/USD. A miss keeps the last good quote and never invents 0."""
    now = time.time()
    with _lock:
        usd = _btc["usd"]
        age = now - _btc["ts"]
        if usd and age < max_age:
            return usd, _btc["source"], _btc["error"], False
        if not _btc["refreshing"]:
            _btc["refreshing"] = True
            threading.Thread(target=_btc_refresh, args=(get_text,), daemon=True).start()
        if usd:
            return usd, _btc["source"], _btc["error"], True
    try:
        usd, source = quote_btc_usd(get_text)
        with _lock:
            _btc["usd"] = usd
            _btc["source"] = source
            _btc["ts"] = time.time()
            _btc["error"] = ""
            _btc["stale"] = False
        return usd, source, "", False
    except Exception as exc:
        print("rentals btc", exc, flush=True)
        with _lock:
            _btc["error"] = str(exc)
            _btc["stale"] = _btc["usd"] is not None
            return _btc["usd"], _btc["source"], str(exc), _btc["usd"] is not None


def payload(*, ths_btc_day, xbt_usd, get_text, max_age=900.0) -> dict:
    rows, fetched_at, err = listings_catalog(get_text, max_age=max_age)
    btc_usd, btc_source, _btc_err, btc_stale = btc_usd_catalog(get_text, max_age=max_age)
    rigs = normalize(rows, ths_btc_day=ths_btc_day, btc_usd=btc_usd, xbt_usd=xbt_usd)
    encoded = [rig.to_json() for rig in rigs]
    with _lock:
        _view["rigs"] = encoded
    return {
        "source": SOURCE_NAME,
        "source_url": SOURCE_URL,
        "fetched_at": fetched_at,
        "btc_usd": btc_usd,
        "btc_usd_source": btc_source,
        "btc_usd_stale": btc_stale,
        "price_usd": xbt_usd,
        "ths_btc_day": ths_btc_day,
        "rigs": encoded,
        "error": err or "",
        "note": NOTE,
    }


def wait_idle(timeout=2.0) -> None:
    deadline = time.time() + timeout
    while time.time() < deadline:
        with _lock:
            if not _listings["refreshing"] and not _btc["refreshing"]:
                return
        time.sleep(0.01)
    raise TimeoutError("rentals refresh did not finish")


def _th(value) -> str:
    if value is None:
        return "—"
    text = f"{float(value):.3f}".rstrip("0").rstrip(".")
    return text + " TH/s"


def _btc_text(value) -> str:
    if value is None:
        return "—"
    text = f"{float(value):.8f}".rstrip("0").rstrip(".")
    return text + " BTC"


def _usd_text(value) -> str:
    if value is None:
        return "—"
    number = float(value)
    if number >= 100:
        return f"${number:,.0f}"
    return f"${number:,.2f}"


def _sats_text(value) -> str:
    if value is None:
        return "—"
    return f"{int(value):,}"


def _fee_text(bps) -> str:
    if bps is None:
        return "—"
    pct = bps / 100
    if pct == int(pct):
        return f"{int(pct)}%"
    return f"{pct:.2f}%"


def _hours_text(rig: dict) -> str:
    lo, hi = rig.get("min_hours"), rig.get("max_hours")
    if lo and hi:
        return f"{_trim(lo)}–{_trim(hi)} h"
    if lo:
        return f"{_trim(lo)} h min"
    if hi:
        return f"up to {_trim(hi)} h"
    return "—"


def _trim(value) -> str:
    return f"{float(value):.3f}".rstrip("0").rstrip(".")


def card_html(rig: dict, labels: dict) -> str:
    """One rental card. Same facts the browser paints; safe to drop into the page."""
    name = escape(rig.get("name") or "")
    url = escape(rig.get("url") or SOURCE_URL, quote=True)
    rented = bool(rig.get("is_already_rented"))
    rentable = bool(rig.get("rentable")) and not rented
    if rented:
        pill, cls = labels.get("rented", "Rented"), "warn"
    elif rentable:
        pill, cls = labels.get("rentable", "Rentable"), "ok"
    else:
        pill, cls = rig.get("status") or labels.get("unavailable", "Unavailable"), ""
    live = rig.get("live_th")
    spec = _th(rig.get("hashrate_th"))
    if live:
        spec += " · " + labels.get("live", "live") + " " + _th(live)
    spec += " · " + _hours_text(rig)
    facts = (
        (labels.get("sats", "sats / TH / day"), _sats_text(rig.get("price_sats_th_day"))),
        (labels.get("usdTh", "USD / TH / day"), _usd_text(rig.get("price_usd_th_day"))),
        (labels.get("btcDay", "BTC / day incl. fee"), _btc_text(rig.get("btc_day_with_fee"))),
        (labels.get("minCost", "Minimum booking"), _btc_text(rig.get("min_cost_btc")) + " · " + _usd_text(rig.get("min_cost_usd"))),
        (labels.get("fee", "Platform fee"), _fee_text(rig.get("platform_fee_bps"))),
        (labels.get("xbtDay", "Est. XBT / day"), _xbt_text(rig.get("xbt_day"))),
        (labels.get("usdDay", "Est. $ / day"), _usd_text(rig.get("usd_day"))),
    )
    rows = "".join(f"<div><dt>{escape(dt)}</dt><dd>{escape(dd)}</dd></div>" for dt, dd in facts)
    oos = "" if rentable else " oos"
    rent = escape(labels.get("rent", "Rent on RouteHash"))
    return (
        "<li>"
        f'<a class="hw-card{oos}" href="{url}" target="_blank" rel="noreferrer" data-rig="{rig.get("rig_id")}">'
        '<span class="hw-img hw-img-empty" aria-hidden="true"></span>'
        '<div class="hw-body"><div class="hw-head">'
        f"<h3>{name}</h3><span class=\"pill {cls}\">{escape(pill)}</span></div>"
        f'<p class="hw-spec">{escape(spec)}</p><dl class="hw-facts">{rows}</dl>'
        f'<p class="hw-spec">{rent}</p></div></a></li>'
    )


def _xbt_text(value) -> str:
    if value is None:
        return "—"
    return f"{float(value):.8f}".rstrip("0").rstrip(".") + " XBT"


def cards_html(rigs: list, labels: dict) -> str:
    return "".join(card_html(rig, labels) for rig in rigs)


def apply_section(raw: str, rigs: list | None, labels: dict) -> str:
    """Fill ``#rentals-grid``. None leaves the shell for the browser to fill."""
    if rigs is None:
        return raw
    cards = cards_html(rigs, labels)
    raw = re.sub(
        r'(<ul class="hw-grid" id="rentals-grid">).*?(</ul>)',
        lambda match: match.group(1) + cards + match.group(2),
        raw,
        count=1,
        flags=re.S,
    )
    if not rigs:
        raw = raw.replace('id="rentals-empty" hidden', 'id="rentals-empty"', 1)
    elif 'id="rentals-empty" hidden' not in raw:
        raw = raw.replace('id="rentals-empty"', 'id="rentals-empty" hidden', 1)
    return raw
