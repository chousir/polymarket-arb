"""
診斷腳本：追蹤 "Highest temperature in Paris on April 19?" 市場
透過 weather_ecmwf_global_low 策略的完整 pipeline。

步驟：
  Step 1  — Gamma API 抓取（tag_slug=temperature）
  Step 2  — 找到 Paris April 19 事件
  Step 3  — 解析每個子市場（parse_city / parse_market_type / parse_temp_range）
  Step 4  — Filter Check 1: market_type != Unknown
  Step 5  — Filter Check 2: city 在 ecmwf_global_low city_whitelist
  Step 6  — Filter Check 3: lead_days 在 [min=0, max=5]
  Step 7  — Filter Check 4: depth_usdc >= min_depth_usdc=50.0
  Step 8  — Filter Check 5: secs_to_close >= abort_before_close_sec=3600
"""

import re
import json
import time
import httpx
from datetime import datetime, timezone, date

# ── Strategy config (from settings.toml weather_ecmwf_global_low) ─────────────
STRATEGY_ID = "weather_ecmwf_global_low"
CFG = {
    "min_lead_days":          0,
    "max_lead_days":          5,
    "city_whitelist":         ["NYC", "London", "Miami", "Chicago", "LA", "Tokyo",
                               "Sydney", "Paris", "Toronto", "Dubai", "Seoul",
                               "Boston", "Houston"],
    "min_depth_usdc":         50.0,
    "abort_before_close_sec": 3600,   # default (not overridden in toml)
    "allowed_types":          ["TempRange", "Extreme", "Precip"],  # default
}

GAMMA_BASE = "https://gamma-api.polymarket.com"
TODAY = date(2026, 4, 18)   # 今天，從 CLAUDE.md currentDate

# ── ANSI 顏色 ─────────────────────────────────────────────────────────────────
GREEN  = "\033[92m"
RED    = "\033[91m"
YELLOW = "\033[93m"
CYAN   = "\033[96m"
RESET  = "\033[0m"
BOLD   = "\033[1m"

def ok(msg):    print(f"  {GREEN}✓ PASS{RESET}  {msg}")
def fail(msg):  print(f"  {RED}✗ FAIL{RESET}  {msg}")
def info(msg):  print(f"  {CYAN}ℹ{RESET}      {msg}")
def warn(msg):  print(f"  {YELLOW}⚠{RESET}      {msg}")
def sep(title): print(f"\n{BOLD}{'─'*60}{RESET}\n{BOLD}{title}{RESET}")

# ── Mirrors Rust parse_city() ─────────────────────────────────────────────────
CITY_ALIASES = [
    ("new york city", "NYC"), ("new york", "NYC"), ("nyc", "NYC"),
    ("san francisco", "San Francisco"), ("los angeles", "LA"),
    ("chicago", "Chicago"), ("miami", "Miami"), ("houston", "Houston"),
    ("phoenix", "Phoenix"), ("boston", "Boston"), ("seattle", "Seattle"),
    ("atlanta", "Atlanta"), ("dallas", "Dallas"), ("denver", "Denver"),
    ("toronto", "Toronto"), ("london", "London"), ("paris", "Paris"),
    ("berlin", "Berlin"), ("amsterdam", "Amsterdam"), ("madrid", "Madrid"),
    ("rome", "Rome"), ("tokyo", "Tokyo"), ("seoul", "Seoul"),
    ("dubai", "Dubai"), ("singapore", "Singapore"), ("hong kong", "Hong Kong"),
    ("bangkok", "Bangkok"), ("mumbai", "Mumbai"), ("sydney", "Sydney"),
    ("melbourne", "Melbourne"), ("beijing", "Beijing"), ("moscow", "Moscow"),
    ("sao paulo", "São Paulo"), ("buenos aires", "Buenos Aires"),
    ("ankara", "Ankara"), ("wellington", "Wellington"), ("munich", "Munich"),
    ("tel aviv", "Tel Aviv"), ("austin", "Austin"), ("lucknow", "Lucknow"),
    (" la ", "LA"), ("(la)", "LA"), ("(nyc)", "NYC"),
]

def parse_city(question: str) -> str:
    q = question.lower()
    for alias, canonical in CITY_ALIASES:
        if alias in q:
            return canonical
    return ""

# ── Mirrors Rust parse_temp_token() ──────────────────────────────────────────
def f_to_c(f: float) -> float:
    return (f - 32.0) * 5.0 / 9.0

def parse_temp_token(s: str) -> float | None:
    s = s.strip().rstrip("?!.,;:'\"")
    normalized = s.replace("°", "").lower()
    s2 = normalized.strip()

    if s2.endswith("fahrenheit"):
        num_str, is_f = s2[:-len("fahrenheit")].strip(), True
    elif s2.endswith("celsius"):
        num_str, is_f = s2[:-len("celsius")].strip(), False
    elif s2.endswith("f"):
        n = s2[:-1].strip()
        if re.fullmatch(r"-?\d+(\.\d+)?", n):
            num_str, is_f = n, True
        else:
            return None
    elif s2.endswith("c"):
        n = s2[:-1].strip()
        if re.fullmatch(r"-?\d+(\.\d+)?", n):
            num_str, is_f = n, False
        else:
            return None
    else:
        return None

    try:
        val = float(num_str)
    except ValueError:
        return None

    if is_f:
        if not (-60 <= val <= 140):
            return None
        return f_to_c(val)
    else:
        if not (-50 <= val <= 55):
            return None
        return val

def extract_temperatures(question: str) -> list[float]:
    tokens = [t for t in re.split(r"[ ,()\[\]]", question) if t]
    results: list[float] = []
    for i, tok in enumerate(tokens):
        v = parse_temp_token(tok)
        if v is not None:
            if not any(abs(x - v) < 0.1 for x in results):
                results.append(v)
            continue
        if i + 1 < len(tokens):
            combined = tok + tokens[i + 1]
            v2 = parse_temp_token(combined)
            if v2 is not None and not any(abs(x - v2) < 0.1 for x in results):
                results.append(v2)
    results.sort()
    return results

PRECIP_KWS = [
    "rain","rainfall","precipitation","snow","snowfall","hurricane","typhoon",
    "cyclone","tornado","flood","flooding","thunderstorm","hail","sleet","drizzle",
]
EXTREME_KWS = [
    "exceed","exceeds","above","below","over","under","higher than","lower than",
    "or higher","or above","at least","heat wave","record","freeze","freezing",
]
EXTREME_ONLY_KWS = ["heat wave","cold snap","freeze","blizzard","polar vortex"]

def parse_market_type(question: str) -> str:
    q = question.lower()
    if any(kw in q for kw in PRECIP_KWS):
        return "Precip"
    temps = extract_temperatures(question)
    if len(temps) >= 2 and (temps[-1] - temps[0]) > 0.5:
        return "TempRange"
    if temps and any(kw in q for kw in EXTREME_KWS):
        return "Extreme"
    if any(kw in q for kw in EXTREME_ONLY_KWS):
        return "Extreme"
    if temps:
        return "TempRange"
    return "Unknown"

def parse_temp_range(question: str) -> tuple[float, float] | None:
    temps = extract_temperatures(question)
    if not temps:
        return None
    if len(temps) == 1:
        return (temps[0], temps[0])
    return (temps[0], temps[-1])

# ── Fetch helpers ─────────────────────────────────────────────────────────────
def fetch_events_page(offset: int, limit: int = 100) -> list[dict]:
    url = (f"{GAMMA_BASE}/events"
           f"?tag_slug=temperature&active=true&closed=false"
           f"&limit={limit}&offset={offset}")
    info(f"GET {url}")
    r = httpx.get(url, timeout=60)
    r.raise_for_status()
    return r.json()

# ── Filter checks (mirrors weather_filter.rs) ─────────────────────────────────
def run_filter_checks(market: dict, depth_usdc: float, now_ts: int) -> str:
    """Return 'ACCEPT' or rejection reason_code."""
    mtype = market["market_type"]
    city  = market["city"]
    close_ts = market["close_ts"]
    target_date = market["target_date"]

    lead_days = (target_date - TODAY).days

    # Check 1
    if mtype == "Unknown":
        return "UNKNOWN_TYPE"
    if mtype not in CFG["allowed_types"]:
        return "TYPE_NOT_ALLOWED"

    # Check 2
    if not city:
        return "UNKNOWN_CITY"
    wl = CFG["city_whitelist"]
    if wl and city.lower() not in [c.lower() for c in wl]:
        return "CITY_NOT_WHITELISTED"

    # Check 3
    if lead_days < CFG["min_lead_days"]:
        return f"LEAD_TOO_SHORT (lead={lead_days})"
    if lead_days > CFG["max_lead_days"]:
        return f"LEAD_TOO_LONG (lead={lead_days})"

    # Check 4
    if depth_usdc < CFG["min_depth_usdc"]:
        return f"LOW_DEPTH (depth={depth_usdc:.2f})"

    # Check 5
    secs = close_ts - now_ts
    if secs < CFG["abort_before_close_sec"]:
        return f"TOO_CLOSE_TO_EXPIRY (secs_to_close={secs})"

    return "ACCEPT"

# ── Main ──────────────────────────────────────────────────────────────────────
def main():
    print(f"\n{BOLD}{'='*60}{RESET}")
    print(f"{BOLD}  診斷：Highest temperature in Paris on April 19?{RESET}")
    print(f"{BOLD}  策略：{STRATEGY_ID}{RESET}")
    print(f"{BOLD}  今天：{TODAY}{RESET}")
    print(f"{BOLD}{'='*60}{RESET}")

    # ── Step 1: Gamma API fetch ───────────────────────────────────────────────
    sep("Step 1 ── Gamma API 抓取（tag_slug=temperature）")
    now_ts = int(time.time())

    all_events: list[dict] = []
    for page in range(10):
        offset = page * 100
        events = fetch_events_page(offset)
        all_events.extend(events)
        info(f"Page {page}: got {len(events)} events (total so far: {len(all_events)})")
        if len(events) < 100:
            break

    ok(f"共抓取 {len(all_events)} 個 temperature events")

    # ── Step 2: 找 Paris April 19 事件 ───────────────────────────────────────
    sep("Step 2 ── 找 Paris April 19 事件")
    paris_apr19_events = []
    for ev in all_events:
        slug = ev.get("slug", "")
        if "paris" in slug.lower() and ("april-19" in slug.lower() or "apr-19" in slug.lower()):
            paris_apr19_events.append(ev)
            info(f"找到事件 slug: {slug}")

    if not paris_apr19_events:
        # 廣義搜尋
        warn("沒找到 paris + april-19 slug，改搜尋 'paris' 所有事件…")
        for ev in all_events:
            slug = ev.get("slug", "")
            if "paris" in slug.lower():
                info(f"Paris 事件: {slug}  (markets: {len(ev.get('markets', []))})")
        print(f"\n  {RED}✗ 找不到 Paris April 19 事件！市場可能已關閉或尚未出現在 API。{RESET}")
        return
    else:
        ok(f"找到 {len(paris_apr19_events)} 個 Paris April 19 事件")

    # ── Step 3: 解析子市場 ────────────────────────────────────────────────────
    sep("Step 3 ── 解析子市場（parse_city / parse_market_type / parse_temp_range）")
    parsed_markets: list[dict] = []

    for ev in paris_apr19_events:
        raw_markets = ev.get("markets", [])
        info(f"事件 '{ev.get('slug')}' 有 {len(raw_markets)} 個子市場")
        for rm in raw_markets:
            question = rm.get("question") or ""
            slug     = rm.get("slug", "")
            end_date = rm.get("endDate", "")
            outcomes_raw   = rm.get("outcomes", "[]")
            clob_token_raw = rm.get("clobTokenIds", "[]")
            liquidity      = float(rm.get("liquidityClob") or 0.0)

            # Parse timestamp
            try:
                dt = datetime.fromisoformat(end_date.replace("Z", "+00:00"))
                close_ts   = int(dt.timestamp())
                target_date = dt.date()
            except Exception:
                warn(f"  endDate 解析失敗: {end_date} (slug={slug})")
                continue

            # Parse tokens
            try:
                outcomes   = json.loads(outcomes_raw)
                token_ids  = json.loads(clob_token_raw)
            except Exception:
                warn(f"  outcomes/clobTokenIds 解析失敗 (slug={slug})")
                continue

            if len(outcomes) < 2 or len(token_ids) < 2:
                warn(f"  outcomes 或 token_ids 長度不足 (slug={slug})")
                continue

            token_yes = token_no = None
            for i, out in enumerate(outcomes):
                if out.lower() == "yes":
                    token_yes = token_ids[i]
                elif out.lower() == "no":
                    token_no = token_ids[i]

            if not token_yes or not token_no:
                warn(f"  找不到 YES/NO token (slug={slug}, outcomes={outcomes})")
                continue

            city       = parse_city(question)
            mtype      = parse_market_type(question)
            temp_range = parse_temp_range(question)
            temps      = extract_temperatures(question)

            parsed_markets.append({
                "slug":        slug,
                "question":    question,
                "city":        city,
                "market_type": mtype,
                "target_date": target_date,
                "temp_range":  temp_range,
                "temps":       temps,
                "token_yes":   token_yes,
                "token_no":    token_no,
                "close_ts":    close_ts,
                "liquidity":   liquidity,
            })

            print(f"\n  ─ slug: {slug}")
            print(f"    question:    {question}")
            print(f"    city:        {city!r:15s}  {'✓' if city else '✗ 空字串'}")
            print(f"    market_type: {mtype:12s}")
            print(f"    temps found: {temps}")
            print(f"    temp_range:  {temp_range}")
            print(f"    target_date: {target_date}")
            print(f"    close_ts:    {close_ts}  ({datetime.fromtimestamp(close_ts, tz=timezone.utc).isoformat()})")
            print(f"    liquidity:   {liquidity:.2f} USDC")

    ok(f"成功解析 {len(parsed_markets)} 個子市場")

    # ── Step 4-8: 逐項 Filter Check ───────────────────────────────────────────
    sep("Steps 4-8 ── weather_ecmwf_global_low Filter Checks")
    print(f"\n  策略參數：")
    for k, v in CFG.items():
        print(f"    {k}: {v}")
    print()

    accepted = []
    for m in parsed_markets:
        print(f"\n  {BOLD}[市場] {m['slug']}{RESET}")
        print(f"  Question: {m['question']}")

        lead_days = (m["target_date"] - TODAY).days
        secs_to_close = m["close_ts"] - now_ts
        depth = m["liquidity"]   # 以 liquidityClob 作為深度代理

        # Check 1: market_type
        mtype = m["market_type"]
        if mtype == "Unknown":
            fail(f"Check 1 (market_type): {mtype} → UNKNOWN_TYPE ✗")
            continue
        elif mtype not in CFG["allowed_types"]:
            fail(f"Check 1 (market_type): {mtype} 不在 allowed_types → TYPE_NOT_ALLOWED ✗")
            continue
        else:
            ok(f"Check 1 (market_type): {mtype} ∈ {CFG['allowed_types']}")

        # Check 2: city whitelist
        city = m["city"]
        if not city:
            fail(f"Check 2 (city): 空字串 → UNKNOWN_CITY ✗")
            continue
        if city.lower() not in [c.lower() for c in CFG["city_whitelist"]]:
            fail(f"Check 2 (city): {city!r} 不在 whitelist → CITY_NOT_WHITELISTED ✗")
            continue
        else:
            ok(f"Check 2 (city): {city!r} ∈ city_whitelist")

        # Check 3: lead_days
        print(f"         lead_days = ({m['target_date']} - {TODAY}).days = {lead_days}")
        if lead_days < CFG["min_lead_days"]:
            fail(f"Check 3 (lead_days): {lead_days} < min={CFG['min_lead_days']} → LEAD_TOO_SHORT ✗")
            continue
        elif lead_days > CFG["max_lead_days"]:
            fail(f"Check 3 (lead_days): {lead_days} > max={CFG['max_lead_days']} → LEAD_TOO_LONG ✗")
            continue
        else:
            ok(f"Check 3 (lead_days): {lead_days} ∈ [{CFG['min_lead_days']}, {CFG['max_lead_days']}]")

        # Check 4: depth
        print(f"         depth (liquidityClob) = {depth:.2f} USDC")
        if depth < CFG["min_depth_usdc"]:
            fail(f"Check 4 (depth): {depth:.2f} < min={CFG['min_depth_usdc']} → LOW_DEPTH ✗")
            continue
        else:
            ok(f"Check 4 (depth): {depth:.2f} >= {CFG['min_depth_usdc']} USDC")

        # Check 5: secs_to_close
        print(f"         secs_to_close = {secs_to_close}s  ({secs_to_close/3600:.1f}h)")
        if secs_to_close < CFG["abort_before_close_sec"]:
            fail(f"Check 5 (secs_to_close): {secs_to_close}s < {CFG['abort_before_close_sec']}s → TOO_CLOSE_TO_EXPIRY ✗")
            continue
        else:
            ok(f"Check 5 (secs_to_close): {secs_to_close}s >= {CFG['abort_before_close_sec']}s")

        print(f"  {GREEN}{BOLD}→ ACCEPT ✓{RESET}  該子市場通過全部 5 項篩選！")
        accepted.append(m)

    # ── 總結 ──────────────────────────────────────────────────────────────────
    sep("總結")
    total = len(parsed_markets)
    print(f"  解析子市場總數: {total}")
    print(f"  通過 filter:   {len(accepted)}")
    print(f"  被拒絕:        {total - len(accepted)}")
    if accepted:
        print(f"\n  {GREEN}以下子市場會進入決策層（weather_ecmwf_global_low）：{RESET}")
        for m in accepted:
            tr = m["temp_range"]
            tr_str = f"{tr[0]:.1f}~{tr[1]:.1f}°C" if tr else "N/A"
            print(f"    • {m['question']}  [{tr_str}]")
    else:
        print(f"\n  {RED}沒有任何子市場通過 filter。{RESET}")
        print(f"  請查看上面各子市場的 FAIL 原因。")

if __name__ == "__main__":
    main()
