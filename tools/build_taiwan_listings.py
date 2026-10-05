#!/usr/bin/env python3
"""Build the Taiwan half of the generated symbol inventory.

Everything listed on the Taiwan Stock Exchange (TWSE, Yahoo suffix `.TW`) and
the Taipei Exchange (TPEx, `.TWO`) that anyone charts: common stocks, the
Innovation Board, ETFs, ETNs, depositary receipts and REITs. Warrants — forty
thousand of them, most alive for a few months — preferred shares and
asset-backed certificates are the paper around a listing rather than the
listing, and are left out the way build_listings.py leaves out the US ones.

Every row carries two names, because people in Taiwan search in Chinese and
everybody else searches in English: `name` is the English one the exchange
publishes and `local_name` is the Chinese short name everyone actually says —
台積電, not 臺灣積體電路製造股份有限公司.

The universe comes from the ISIN registry, which is the one place that lists
every security on both exchanges with what kind of security it is. Two more
feeds rank it: market cap for stocks, the day's turnover for funds. Like the
US script, a ranking feed that is down costs its rows their popularity and
nothing else.

Run: tools/build_taiwan_listings.py > crates/omacharts-engine/src/listings_tw.tsv
"""

import html
import json
import re
import sys
import urllib.error

# The same bands, the same ranking and the same fetch as the US half, so a 9
# means the same thing in both files.
from build_listings import banded, fetch

ISIN = "https://isin.twse.com.tw/isin/C_public.jsp?strMode={mode}"
ISIN_EN = "https://isin.twse.com.tw/isin/e_C_public.jsp?strMode={mode}"

# strMode 2 is the TWSE's listed board, 4 is the TPEx's.
MARKETS = (
    # mode, Yahoo suffix, venue as people say it
    (2, "TW", "TWSE"),
    (4, "TWO", "TPEx"),
)

# Full English names for TWSE funds. The ISIN registry spells them in capitals
# and cuts them short; the fund register has them the way the issuer wrote them.
TWSE_FUNDS = "https://openapi.twse.com.tw/v1/opendata/t187ap47_L"

# Shares outstanding, for market cap.
TWSE_COMPANIES = "https://openapi.twse.com.tw/v1/opendata/t187ap03_L"
TPEX_COMPANIES = "https://www.tpex.org.tw/openapi/v1/mopsfin_t187ap03_O"

# The day's close and turnover for every security on each board.
TWSE_DAY = "https://openapi.twse.com.tw/v1/exchangeReport/STOCK_DAY_ALL"
TPEX_DAY = "https://www.tpex.org.tw/openapi/v1/tpex_mainboard_daily_close_quotes"

# What the ISIN registry calls each section it lists, in both languages, to
# the kind it becomes here. A section not in here is not charted. The Chinese
# names are what decide; the English page is only read for names.
SECTIONS = {
    "股票": "equity",
    "創新板": "equity",
    "臺灣存託憑證(TDR)": "equity",
    "受益證券-不動產投資信託": "equity",
    "ETF": "etf",
    "ETN": "etf",
}

# Taipei opens at 09:00, which is 01:00 UTC with no daylight saving to move
# it. This is what makes a four-hour bar start at the open.
SESSION_ORIGIN = 3600


def fetch_json(url: str):
    return json.loads(fetch(url))


def registry(url: str):
    """(section, code, name) for every row of one ISIN page.

    The page is Big5 — cp950, strictly, since a few company names use the
    Microsoft extensions plain Big5 cannot decode — and an HTML table whose
    section headings are rows with a single cell.
    """
    text = fetch(url).decode("cp950", "replace")
    section = None
    for row in re.findall(r"<tr>(.*?)</tr>", text, re.S):
        cells = [
            html.unescape(re.sub(r"<[^>]+>", "", cell)).strip()
            for cell in re.findall(r"<td[^>]*>(.*?)</td>", row, re.S)
        ]
        if not cells:
            continue
        if len(cells) == 1 or not any(cells[1:]):
            section = cells[0]
            continue
        # "2330　台積電": the code and the name, joined by an ideographic space.
        code, _, name = cells[0].partition("　")
        code, name = code.strip(), name.strip()
        if code and name:
            yield section, code, name


def number(text) -> float:
    try:
        return float(str(text).replace(",", "").strip())
    except ValueError:
        return 0.0


def warn(what: str, err: Exception) -> None:
    print(f"# warning: no {what} ({err})", file=sys.stderr)


FAILURES = (urllib.error.URLError, OSError, ValueError, KeyError, json.JSONDecodeError)


def fund_names() -> dict:
    try:
        rows = fetch_json(TWSE_FUNDS)
    except FAILURES as err:
        warn("fund names", err)
        return {}
    names = {}
    for row in rows:
        code, name = row.get("基金代號", "").strip(), row.get("基金英文名稱", "").strip()
        if code and name:
            # "...Securities Investment Trust Fund" is on every one of them.
            name = re.sub(r"\s+Securities Investment Trust Fund$", "", name, flags=re.I)
            names[code] = re.sub(r"\s+", " ", name)
    return names


def shares() -> dict:
    out = {}
    for what, url, code_at, shares_at in (
        ("TWSE share counts", TWSE_COMPANIES, "公司代號", "已發行普通股數或TDR原股發行股數"),
        ("TPEx share counts", TPEX_COMPANIES, "SecuritiesCompanyCode", "IssueShares"),
    ):
        try:
            for row in fetch_json(url):
                count = number(row.get(shares_at, 0))
                if count > 0:
                    out[row[code_at].strip()] = count
        except FAILURES as err:
            warn(what, err)
    return out


def sessions() -> dict:
    """code -> (close, turnover in TWD) for the last session on both boards."""
    out = {}
    for what, url, code_at, close_at, value_at in (
        ("TWSE quotes", TWSE_DAY, "Code", "ClosingPrice", "TradeValue"),
        ("TPEx quotes", TPEX_DAY, "SecuritiesCompanyCode", "Close", "TransactionAmount"),
    ):
        try:
            for row in fetch_json(url):
                out[row[code_at].strip()] = (number(row.get(close_at)), number(row.get(value_at)))
        except FAILURES as err:
            warn(what, err)
    return out


def popularity(rows: dict) -> dict:
    """Band per (code, suffix), stocks by market cap and funds by turnover.

    Each market is banded on its own: a TPEx household name is a household
    name, whatever it would rank among the TWSE's giants.
    """
    counts, quotes = shares(), sessions()
    bands = {}
    for _, suffix, _ in MARKETS:
        caps, turnover = {}, {}
        for (code, sfx), (kind, _, _, _) in rows.items():
            if sfx != suffix:
                continue
            close, value = quotes.get(code, (0.0, 0.0))
            if kind == "equity" and close > 0 and counts.get(code, 0) > 0:
                caps[code] = close * counts[code]
            elif kind == "etf" and value > 0:
                turnover[code] = value
        for measured in (caps, turnover):
            for code, band in banded(measured).items():
                bands[(code, suffix)] = band
    return bands


def tidy(text: str) -> str:
    """No tabs or line breaks can reach a TSV cell."""
    return re.sub(r"\s+", " ", text).strip()


def main() -> int:
    funds = fund_names()
    rows = {}
    for mode, suffix, venue in MARKETS:
        try:
            english = {code: name for _, code, name in registry(ISIN_EN.format(mode=mode))}
        except FAILURES as err:
            warn(f"English names for {venue}", err)
            english = {}
        for section, code, local in registry(ISIN.format(mode=mode)):
            kind = SECTIONS.get(section)
            if kind is None:
                continue
            name = (funds.get(code) if suffix == "TW" else None) or english.get(code) or local
            rows[(code, suffix)] = (kind, tidy(name), tidy(local), venue)

    if len(rows) < 1500:
        print(f"# refusing: only {len(rows)} listings, the registry looks broken", file=sys.stderr)
        return 1

    bands = popularity(rows)

    print(
        "# kind\tsymbol\tname\tsuffix\tcurrency\ttier\tsession_origin"
        "\tyahoo_override\texchange\tpopularity\tlocal_name"
    )
    print("# Generated by tools/build_taiwan_listings.py from the TWSE ISIN")
    print("# registry and the TWSE and TPEx open data. Do not edit: the curated")
    print("# inventory is seed.tsv. Everything here is tier 2.")
    ranked = 0
    for (code, suffix), (kind, name, local, venue) in sorted(rows.items()):
        band = bands.get((code, suffix))
        if band is not None:
            ranked += 1
        print(
            f"{kind}\t{code}\t{name}\t{suffix}\tTWD\t2\t{SESSION_ORIGIN}\t-"
            f"\t{venue}\t{band if band else '-'}\t{local}"
        )
    print(f"# {len(rows)} listings, {ranked} ranked", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
