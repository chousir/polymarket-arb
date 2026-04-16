"""
測試：驗證 tag_slug=daily-temperature 能正確抓取所有溫度市場。

問題：
  原程式碼用 tag_slug=temperature，但 Polymarket 並非所有溫度事件都掛這個 tag。
  例如 "highest-temperature-in-paris-on-april-19-2026" 只有 'daily-temperature' tag，
  沒有 'temperature' tag，導致 fetch_weather_markets() 完全看不到這個市場。

測試通過條件（全部 assert 為 True）才能修改 weather_market.rs。
"""

import httpx

BASE = "https://gamma-api.polymarket.com"
PARIS_APR19 = "highest-temperature-in-paris-on-april-19-2026"

def fetch_all_events(tag_slug: str) -> list[dict]:
    slugs_seen: set[str] = set()
    all_events: list[dict] = []
    for page in range(10):
        url = (f"{BASE}/events?tag_slug={tag_slug}"
               f"&active=true&closed=false&limit=100&offset={page * 100}")
        r = httpx.get(url, timeout=60)
        r.raise_for_status()
        events: list[dict] = r.json()
        for ev in events:
            slug = ev.get("slug", "")
            if slug not in slugs_seen:
                slugs_seen.add(slug)
                all_events.append(ev)
        if len(events) < 100:
            break
    return all_events


def test_old_tag_misses_paris_apr19():
    """舊的 tag_slug=temperature 不應包含 April 19 Paris 事件。"""
    events = fetch_all_events("temperature")
    slugs = {e.get("slug") for e in events}
    assert PARIS_APR19 not in slugs, (
        f"tag_slug=temperature 意外包含 {PARIS_APR19}，問題已由 Polymarket 修復？"
    )
    print(f"✓ test_old_tag_misses_paris_apr19: temperature tag 回傳 {len(slugs)} 事件，確認不含 April 19 Paris")


def test_new_tag_finds_paris_apr19():
    """新的 tag_slug=daily-temperature 必須包含 April 19 Paris 事件。"""
    events = fetch_all_events("daily-temperature")
    slugs = {e.get("slug") for e in events}
    assert PARIS_APR19 in slugs, (
        f"tag_slug=daily-temperature 未找到 {PARIS_APR19}"
    )
    print(f"✓ test_new_tag_finds_paris_apr19: daily-temperature tag 回傳 {len(slugs)} 事件，確認含 April 19 Paris")


def test_new_tag_is_superset_of_old_tag():
    """daily-temperature 應涵蓋所有 temperature tag 的事件（超集）。"""
    old_events = fetch_all_events("temperature")
    new_events = fetch_all_events("daily-temperature")
    old_slugs = {e.get("slug") for e in old_events}
    new_slugs = {e.get("slug") for e in new_events}
    missing = old_slugs - new_slugs
    assert not missing, (
        f"daily-temperature 缺少 temperature tag 內的 {len(missing)} 個事件: {missing}"
    )
    print(f"✓ test_new_tag_is_superset_of_old_tag: daily-temperature({len(new_slugs)}) ⊇ temperature({len(old_slugs)})")


def test_paris_apr19_sub_markets_parseable():
    """April 19 Paris 事件的所有子市場都應有 question / endDate / YES+NO token。"""
    r = httpx.get(f"{BASE}/events?slug={PARIS_APR19}", timeout=30)
    r.raise_for_status()
    events = r.json()
    assert events, f"找不到事件 slug={PARIS_APR19}"
    ev = events[0]
    markets = ev.get("markets", [])
    assert markets, "事件內沒有 markets"

    import json as _json
    issues: list[str] = []
    for m in markets:
        q   = m.get("question") or ""
        ed  = m.get("endDate") or ""
        try:
            outcomes = _json.loads(m.get("outcomes") or "[]")
            tids     = _json.loads(m.get("clobTokenIds") or "[]")
        except Exception as e:
            issues.append(f"JSON parse error ({q!r}): {e}")
            continue
        if not q:
            issues.append("缺 question")
        if not ed:
            issues.append(f"缺 endDate ({q!r})")
        if len(outcomes) < 2 or len(tids) < 2:
            issues.append(f"outcomes/tokens 不足 ({q!r})")
        if "yes" not in [o.lower() for o in outcomes]:
            issues.append(f"缺 YES outcome ({q!r})")
        if "no" not in [o.lower() for o in outcomes]:
            issues.append(f"缺 NO outcome ({q!r})")

    assert not issues, f"子市場解析問題: {issues}"
    print(f"✓ test_paris_apr19_sub_markets_parseable: {len(markets)} 個子市場全部可解析")


def test_paris_apr19_all_pass_filter():
    """April 19 Paris 所有子市場，用 ecmwf_global_low 參數，5 項 filter 全通過。"""
    import json as _json, re, time
    from datetime import datetime, timezone, date

    CFG = {
        "min_lead_days": 0, "max_lead_days": 5,
        "city_whitelist": ["NYC","London","Miami","Chicago","LA","Tokyo",
                           "Sydney","Paris","Toronto","Dubai","Seoul","Boston","Houston"],
        "min_depth_usdc": 50.0,
        "abort_before_close_sec": 3600,
        "allowed_types": ["TempRange","Extreme","Precip"],
    }
    TODAY = date(2026, 4, 18)
    now_ts = int(time.time())

    CITY_ALIASES = [
        ("new york city","NYC"),("new york","NYC"),("nyc","NYC"),
        ("los angeles","LA"),("chicago","Chicago"),("miami","Miami"),
        ("houston","Houston"),("boston","Boston"),("toronto","Toronto"),
        ("london","London"),("paris","Paris"),("tokyo","Tokyo"),
        ("seoul","Seoul"),("dubai","Dubai"),("sydney","Sydney"),
    ]
    PRECIP_KWS = ["rain","rainfall","precipitation","snow","snowfall","hurricane",
                  "typhoon","cyclone","tornado","flood","flooding","thunderstorm",
                  "hail","sleet","drizzle"]
    EXTREME_KWS = ["exceed","exceeds","above","below","over","under","higher than",
                   "lower than","or higher","or above","at least","heat wave",
                   "record","freeze","freezing"]

    def f_to_c(f): return (f-32.0)*5.0/9.0
    def parse_city(q):
        ql = q.lower()
        for a, c in CITY_ALIASES:
            if a in ql: return c
        return ""
    def parse_temp_token(s):
        s = s.strip().rstrip("?!.,;:")
        n = s.replace("°","").lower().strip()
        if n.endswith("fahrenheit"): num, isf = n[:-10].strip(), True
        elif n.endswith("celsius"):  num, isf = n[:-6].strip(), False
        elif n.endswith("f"):
            p = n[:-1].strip()
            if re.fullmatch(r"-?\d+(\.\d+)?", p): num, isf = p, True
            else: return None
        elif n.endswith("c"):
            p = n[:-1].strip()
            if re.fullmatch(r"-?\d+(\.\d+)?", p): num, isf = p, False
            else: return None
        else: return None
        try: v = float(num)
        except: return None
        if isf:
            if not (-60 <= v <= 140): return None
            return f_to_c(v)
        if not (-50 <= v <= 55): return None
        return v
    def extract_temps(q):
        tokens = [t for t in re.split(r"[ ,()\[\]]", q) if t]
        res: list[float] = []
        for i, tok in enumerate(tokens):
            v = parse_temp_token(tok)
            if v is not None:
                if not any(abs(x-v)<0.1 for x in res): res.append(v)
                continue
            if i+1 < len(tokens):
                v2 = parse_temp_token(tok+tokens[i+1])
                if v2 is not None and not any(abs(x-v2)<0.1 for x in res): res.append(v2)
        res.sort(); return res
    def parse_mtype(q):
        ql = q.lower()
        if any(k in ql for k in PRECIP_KWS): return "Precip"
        temps = extract_temps(q)
        if len(temps) >= 2 and temps[-1]-temps[0] > 0.5: return "TempRange"
        if temps and any(k in ql for k in EXTREME_KWS): return "Extreme"
        if temps: return "TempRange"
        return "Unknown"

    r = httpx.get(f"{BASE}/events?slug={PARIS_APR19}", timeout=30)
    ev = r.json()[0]
    markets = ev["markets"]
    failures: list[str] = []

    for m in markets:
        q = m.get("question","")
        ed = m.get("endDate","")
        liquidity = float(m.get("liquidityClob") or 0.0)
        dt = datetime.fromisoformat(ed.replace("Z","+00:00"))
        close_ts = int(dt.timestamp())
        target_date = dt.date()

        city   = parse_city(q)
        mtype  = parse_mtype(q)
        lead   = (target_date - TODAY).days
        secs   = close_ts - now_ts

        if mtype == "Unknown":           failures.append(f"UNKNOWN_TYPE: {q}")
        elif mtype not in CFG["allowed_types"]: failures.append(f"TYPE_NOT_ALLOWED: {q}")
        elif not city:                   failures.append(f"UNKNOWN_CITY: {q}")
        elif city.lower() not in [c.lower() for c in CFG["city_whitelist"]]:
                                         failures.append(f"CITY_NOT_WHITELISTED({city}): {q}")
        elif lead < CFG["min_lead_days"]: failures.append(f"LEAD_TOO_SHORT({lead}d): {q}")
        elif lead > CFG["max_lead_days"]: failures.append(f"LEAD_TOO_LONG({lead}d): {q}")
        elif liquidity < CFG["min_depth_usdc"]: failures.append(f"LOW_DEPTH({liquidity:.1f}): {q}")
        elif secs < CFG["abort_before_close_sec"]: failures.append(f"TOO_CLOSE({secs}s): {q}")

    assert not failures, f"{len(failures)} 個子市場未通過 filter:\n" + "\n".join(failures)
    print(f"✓ test_paris_apr19_all_pass_filter: {len(markets)}/{len(markets)} 子市場通過 weather_ecmwf_global_low filter")


if __name__ == "__main__":
    tests = [
        test_old_tag_misses_paris_apr19,
        test_new_tag_finds_paris_apr19,
        test_new_tag_is_superset_of_old_tag,
        test_paris_apr19_sub_markets_parseable,
        test_paris_apr19_all_pass_filter,
    ]
    passed = 0
    for t in tests:
        try:
            t()
            passed += 1
        except AssertionError as e:
            print(f"✗ {t.__name__}: {e}")
        except Exception as e:
            print(f"✗ {t.__name__} [ERROR]: {e}")

    print(f"\n{'='*50}")
    print(f"結果: {passed}/{len(tests)} 通過")
    if passed == len(tests):
        print("✓ 全部通過 — 可以安全修改 weather_market.rs 的 TAG_SLUG")
    else:
        print("✗ 有測試失敗 — 不要修改主程式碼")
