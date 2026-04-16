#!/usr/bin/env python3
"""
Polymarket 策略狀態報告

用法:
  python tools/status.py                    # 預設：最近 7 天，所有策略
  python tools/status.py --days 30          # 最近 30 天
  python tools/status.py --strategy dump_hedge_conservative
  python tools/status.py --trades 50        # 顯示最近 50 筆交易
  python tools/status.py --all-trades       # 顯示所有交易（不分頁）
"""

from __future__ import annotations

import argparse
import os
import sys
from collections import defaultdict

# tomllib 在 Python 3.11 內建；舊版需要 `pip install tomli`
try:
    import tomllib
except ModuleNotFoundError:
    try:
        import tomli as tomllib  # type: ignore[no-redef]
    except ModuleNotFoundError:
        sys.exit("錯誤：Python < 3.11 需要安裝 tomli：pip install tomli")
from datetime import datetime, timezone
from typing import Optional

# ── Path bootstrap ────────────────────────────────────────────────────────────
_SCRIPT_DIR = os.path.dirname(os.path.abspath(__file__))
_REPO_ROOT = os.path.normpath(os.path.join(_SCRIPT_DIR, ".."))
_ANALYTICS_ROOT = os.path.join(_REPO_ROOT, "python-analytics")
if _ANALYTICS_ROOT not in sys.path:
    sys.path.insert(0, _ANALYTICS_ROOT)

from src.data.storage import DbReader  # noqa: F401 — path resolved via sys.path at runtime

# ── ANSI colors ───────────────────────────────────────────────────────────────
GREEN  = "\033[0;32m"
RED    = "\033[0;31m"
YELLOW = "\033[0;33m"
BLUE   = "\033[0;34m"
CYAN   = "\033[0;36m"
BOLD   = "\033[1m"
DIM    = "\033[2m"
NC     = "\033[0m"


def _pnl_str(val: Optional[float]) -> str:
    if val is None:
        return f"{DIM}—{NC}"
    if val > 0:
        return f"{GREEN}+{val:.4f}{NC}"
    if val < 0:
        return f"{RED}{val:.4f}{NC}"
    return f"{val:.4f}"


def _pct(val: float) -> str:
    pct = val * 100
    if pct > 0:
        return f"{GREEN}+{pct:.2f}%{NC}"
    if pct < 0:
        return f"{RED}{pct:.2f}%{NC}"
    return f"{DIM}{pct:.2f}%{NC}"


def _wr(val: float) -> str:
    pct = val * 100
    if pct >= 60:
        return f"{GREEN}{pct:.1f}%{NC}"
    if pct >= 40:
        return f"{YELLOW}{pct:.1f}%{NC}"
    return f"{RED}{pct:.1f}%{NC}"


def _db_path() -> str:
    raw = os.environ.get("DB_PATH", "")
    if raw:
        return raw if os.path.isabs(raw) else os.path.join(_REPO_ROOT, raw)
    # Docker volume 掛載在 repo 根目錄 ./data/（優先）
    docker_path = os.path.join(_REPO_ROOT, "data", "market_snapshots.db")
    if os.path.exists(docker_path):
        return docker_path
    # 本機 cargo run 寫入 rust-engine/data/
    return os.path.join(_REPO_ROOT, "rust-engine", "data", "market_snapshots.db")


def _load_capital_config() -> tuple[float, dict[str, dict]]:
    path = os.path.join(_REPO_ROOT, "config", "settings.toml")
    if not os.path.exists(path):
        return 0.0, {}
    with open(path, "rb") as f:
        raw = tomllib.load(f)
    initial = float(raw.get("capital", {}).get("initial_capital_usdc", 0.0) or 0.0)
    result: dict[str, dict] = {}
    for s in raw.get("strategies", []):
        sid = s.get("id")
        if not sid or sid in result:
            continue
        alloc = float(s.get("capital_allocation_pct", 0.0) or 0.0)
        result[sid] = {
            "capital_allocation_pct": alloc,
            "initial_allocated_usdc": initial * alloc,
            "enabled": s.get("enabled", False),
            "type": s.get("type", ""),
        }
    return initial, result


def hr(width: int = 80) -> str:
    return "─" * width


def section(title: str) -> None:
    pad = (76 - len(title)) // 2
    print(f"\n{BOLD}{BLUE}{'═' * pad} {title} {'═' * pad}{NC}")


def subsection(title: str) -> None:
    print(f"\n  {BOLD}{CYAN}{title}{NC}")
    print("  " + hr(70))


# ── Portfolio summary ─────────────────────────────────────────────────────────

def print_portfolio(reader: DbReader, days: int, initial_capital: float) -> None:
    section("資產組合總覽")

    cycles = reader.get_cycle_results(days=days)
    trades = reader.get_dry_run_trades(days=days)

    resolved_pnl = [c.pnl_usdc for c in cycles if c.pnl_usdc is not None]
    total_pnl    = sum(resolved_pnl)
    wins         = sum(1 for p in resolved_pnl if p >= 0)
    losses       = sum(1 for p in resolved_pnl if p < 0)
    win_rate     = wins / len(resolved_pnl) if resolved_pnl else 0.0
    total_invested = sum(t.size_usdc for t in trades)
    total_fees     = sum(t.fee_usdc   for t in trades)

    try:
        m_trades = reader.get_mention_dry_run_trades(days=days)
        m_exits  = [t for t in m_trades if t.action in ("TAKE_PROFIT", "STOP_LOSS", "TIME_EXIT")]
        total_pnl     += sum(t.realized_pnl_usdc for t in m_exits if t.realized_pnl_usdc is not None)
        total_invested += sum(t.size_usdc for t in m_trades if t.action == "ENTRY")
    except Exception:
        pass

    try:
        w_trades = reader.get_weather_dry_run_trades(days=days)
        w_exits  = [t for t in w_trades if t.action in (
            "TAKE_PROFIT", "STOP_LOSS", "FORECAST_SHIFT", "TIME_DECAY_EXIT")]
        total_pnl     += sum(t.realized_pnl_usdc for t in w_exits if t.realized_pnl_usdc is not None)
        total_invested += sum(t.size_usdc for t in w_trades if t.action == "ENTRY")
    except Exception:
        pass

    current_capital = initial_capital + total_pnl
    pnl_pct  = total_pnl / initial_capital if initial_capital > 0 else 0.0
    roi      = total_pnl / total_invested  if total_invested > 0  else 0.0
    now      = datetime.now(timezone.utc).strftime("%Y-%m-%d %H:%M UTC")

    print(f"\n  {DIM}統計期間：最近 {days} 天  ｜  資料時間：{now}{NC}\n")

    col = 24
    print(f"  {'初始模擬資金':<{col}} ${initial_capital:>12,.2f} USDC")
    pnl_sign = f"{GREEN}+${total_pnl:,.4f}{NC}" if total_pnl >= 0 else f"{RED}-${abs(total_pnl):,.4f}{NC}"
    print(f"  {'累計 PnL':<{col}} {pnl_sign}  ({_pct(pnl_pct)})")
    print(f"  {'模擬現值':<{col}} ${current_capital:>12,.4f} USDC")
    print(f"  {'已投入金額':<{col}} ${total_invested:>12,.2f} USDC")
    print(f"  {'手續費支出':<{col}} ${total_fees:>12,.4f} USDC")
    print()
    print(f"  {'已解算週期':<{col}} {len(resolved_pnl):>8} 筆")
    print(f"  {'勝 / 負':<{col}} {GREEN}{wins}{NC} 勝  /  {RED}{losses}{NC} 負")
    print(f"  {'勝率':<{col}} {_wr(win_rate)}")
    print(f"  {'投入報酬率 (ROI)':<{col}} {_pct(roi)}")


# ── Strategy comparison table ─────────────────────────────────────────────────


def print_strategy_table(reader: DbReader, days: int, cap_cfg: dict) -> None:
    section("各策略績效比較")

    # ── Dump-Hedge / Pure-Arb ─────────────────────────────────────────────────
    cycles = reader.get_cycle_results(days=days)
    trades = reader.get_dry_run_trades(days=days)
    by_c: dict[str, list] = defaultdict(list)
    by_t: dict[str, list] = defaultdict(list)
    for c in cycles: by_c[c.strategy_id].append(c)
    for t in trades: by_t[t.strategy_id].append(t)

    dh_ids = sorted(set(by_c) | set(by_t))

    if dh_ids:
        subsection("Dump-Hedge / Pure-Arb")
        _table_header()
        for sid in dh_ids:
            scyc  = by_c[sid]
            trig  = [c for c in scyc if c.leg1_price is not None]
            res   = [c for c in trig if c.pnl_usdc is not None]
            pnls  = [c.pnl_usdc for c in res]  # type: ignore[misc]
            alloc = float(cap_cfg.get(sid, {}).get("initial_allocated_usdc", 0.0) or 0.0)
            enab  = cap_cfg.get(sid, {}).get("enabled", True)
            _print_row(sid, len(scyc), len(trig)/len(scyc) if scyc else 0,
                       pnls, alloc, enab)

    # ── Mention ───────────────────────────────────────────────────────────────
    try:
        m_rows = reader.get_mention_dry_run_trades(days=days)
    except Exception:
        m_rows = []
    if m_rows:
        by_m: dict[str, list] = defaultdict(list)
        for r in m_rows: by_m[r.strategy_id].append(r)
        subsection("Mention Market")
        _table_header()
        for sid, rows in sorted(by_m.items()):
            entries = [t for t in rows if t.action == "ENTRY"]
            cancels = [t for t in rows if t.action in ("NO_TRADE", "CANCEL")]
            exits   = [t for t in rows if t.action in ("TAKE_PROFIT", "STOP_LOSS", "TIME_EXIT")]
            pnls    = [t.realized_pnl_usdc for t in exits if t.realized_pnl_usdc is not None]
            signals = len(entries) + len(cancels)
            alloc   = float(cap_cfg.get(sid, {}).get("initial_allocated_usdc", 0.0) or 0.0)
            enab    = cap_cfg.get(sid, {}).get("enabled", True)
            _print_row(sid, signals, len(entries)/signals if signals else 0,
                       pnls, alloc, enab)

    # ── Weather ───────────────────────────────────────────────────────────────
    try:
        w_rows = reader.get_weather_dry_run_trades(days=days)
    except Exception:
        w_rows = []
    if w_rows:
        by_w: dict[str, list] = defaultdict(list)
        for r in w_rows: by_w[r.strategy_id].append(r)
        subsection("Weather Market")
        _table_header()
        for sid, rows in sorted(by_w.items()):
            entries = [t for t in rows if t.action == "ENTRY"]
            cancels = [t for t in rows if t.action in ("NO_TRADE", "CANCEL")]
            exits   = [t for t in rows if t.action in (
                "TAKE_PROFIT", "STOP_LOSS", "FORECAST_SHIFT", "TIME_DECAY_EXIT")]
            pnls    = [t.realized_pnl_usdc for t in exits if t.realized_pnl_usdc is not None]
            signals = len(entries) + len(cancels)
            alloc   = float(cap_cfg.get(sid, {}).get("initial_allocated_usdc", 0.0) or 0.0)
            enab    = cap_cfg.get(sid, {}).get("enabled", True)
            _print_row(sid, signals, len(entries)/signals if signals else 0,
                       pnls, alloc, enab)

    # ── Weather Ladder ────────────────────────────────────────────────────────
    try:
        l_rows = reader.get_weather_ladder_trades(days=days)
    except Exception:
        l_rows = []
    if l_rows:
        by_l: dict[str, list] = defaultdict(list)
        for r in l_rows: by_l[r.strategy_id].append(r)
        subsection("Weather Ladder")
        _table_header()
        for sid, rows in sorted(by_l.items()):
            entries = [t for t in rows if t.action == "LADDER_ENTRY"]
            exits   = [t for t in rows if t.action in ("HOLD_TO_RESOLUTION", "CATASTROPHIC_SHIFT_EXIT")]
            pnls    = [t.realized_pnl_usdc for t in exits if t.realized_pnl_usdc is not None]
            # 每個 ladder_id 算一個「訊號」
            ladders = len({t.ladder_id for t in rows})
            alloc   = float(cap_cfg.get(sid, {}).get("initial_allocated_usdc", 0.0) or 0.0)
            enab    = cap_cfg.get(sid, {}).get("enabled", True)
            _print_row(sid, ladders, len(entries) / max(len(rows), 1),
                       pnls, alloc, enab)

    if not dh_ids and not m_rows and not w_rows and not l_rows:
        print(f"\n  {DIM}（本期無任何策略資料）{NC}")


def _table_header() -> None:
    print(
        f"  {'策略 ID':<38}  {'訊號':>6}  {'觸發率':>7}  "
        f"{'勝率':>5}  {'PnL(USDC)':>10}  {'分配資金':>9}  {'報酬%':>8}"
    )
    print("  " + hr(95))


def _print_row(sid: str, total: int, trig_rate: float,
               pnl_vals: list[float], alloc: float, enabled: bool) -> None:
    total_pnl = sum(pnl_vals)
    has_exits = len(pnl_vals) > 0
    wr        = sum(1 for p in pnl_vals if p >= 0) / len(pnl_vals) if has_exits else None
    pnl_pct   = total_pnl / alloc if alloc > 0 else 0.0

    sid_s = sid if enabled else f"{DIM}{sid}{NC}"
    pnl_s = f"{GREEN}+{total_pnl:.4f}{NC}" if total_pnl > 0 else (
            f"{RED}{total_pnl:.4f}{NC}" if total_pnl < 0 else f"{DIM}+0.0000{NC}")
    # 沒有退場紀錄時顯示 — 而非誤導性的 0.0%
    if wr is None:
        wr_plain = f"{DIM}{'—':>5}{NC}"
    elif wr * 100 >= 60:
        wr_plain = f"{GREEN}{wr*100:>5.1f}%{NC}"
    elif wr * 100 >= 40:
        wr_plain = f"{YELLOW}{wr*100:>5.1f}%{NC}"
    else:
        wr_plain = f"{RED}{wr*100:>5.1f}%{NC}"
    pct_plain = _pct(pnl_pct)

    print(
        f"  {sid_s:<38}  {total:>6}  {trig_rate*100:>6.1f}%  "
        f"{wr_plain}  {pnl_s}  ${alloc:>8,.0f}  {pct_plain}"
    )


# ── Cycle log ─────────────────────────────────────────────────────────────────

def print_cycle_log(reader: DbReader, days: int, limit: int,
                    strategy_filter: Optional[str]) -> None:
    cycles = reader.get_cycle_results(days=days, strategy_id=strategy_filter)
    if not cycles:
        return

    section("週期結算紀錄")
    recent = sorted(cycles, key=lambda c: c.created_at or "", reverse=True)[:limit]

    print(
        f"\n  {'時間':<20}  {'策略':<35}  {'市場':<28}  "
        f"{'腿方向':>6}  {'L1':>7}  {'L2':>7}  {'結算':>10}  {'PnL':>10}"
    )
    print("  " + hr(120))

    for c in recent:
        ts   = (c.created_at or "")[:19]
        l1   = f"{c.leg1_price:.4f}" if c.leg1_price is not None else "—"
        l2   = f"{c.leg2_price:.4f}" if c.leg2_price is not None else "—"
        res  = c.resolved_winner or f"{DIM}pending{NC}"
        pnl  = _pnl_str(c.pnl_usdc)
        side = c.leg1_side or "—"
        slug = c.market_slug[:26]
        print(
            f"  {ts:<20}  {c.strategy_id:<35}  {slug:<28}  "
            f"{side:>6}  {l1:>7}  {l2:>7}  {res:>10}  {pnl:>19}"
        )


# ── Trade log ─────────────────────────────────────────────────────────────────

def print_trade_log(reader: DbReader, days: int, limit: int,
                    strategy_filter: Optional[str]) -> None:
    section("交易明細紀錄")
    any_data = False

    # ── Dump-Hedge / Pure-Arb ─────────────────────────────────────────────────
    trades = reader.get_dry_run_trades(days=days, strategy_id=strategy_filter)
    if trades:
        any_data = True
        subsection(f"Dump-Hedge / Pure-Arb（最近 {min(limit, len(trades))} 筆）")
        recent = sorted(trades, key=lambda t: t.ts or "", reverse=True)[:limit]
        print(
            f"\n  {'時間':<20}  {'策略':<30}  {'市場':<26}  "
            f"{'腿':>3}  {'方向':>4}  {'價格':>7}  {'金額':>9}  "
            f"{'手續費':>8}  {'dump%':>6}  {'hedge_sum':>9}  {'預期利潤':>10}"
        )
        print("  " + hr(135))
        for t in recent:
            ts    = (t.ts or "")[:19]
            dump  = f"{t.signal_dump_pct*100:.2f}%" if t.signal_dump_pct is not None else "—"
            hs    = f"{t.hedge_sum:.4f}" if t.hedge_sum is not None else "—"
            wp    = _pnl_str(t.would_profit)
            print(
                f"  {ts:<20}  {t.strategy_id:<30}  {t.market_slug[:24]:<26}  "
                f"{t.leg:>3}  {t.side:>4}  {t.price:>7.4f}  {t.size_usdc:>9.2f}  "
                f"{t.fee_usdc:>8.4f}  {dump:>6}  {hs:>9}  {wp:>19}"
            )

    # ── Mention ───────────────────────────────────────────────────────────────
    try:
        m_rows = reader.get_mention_dry_run_trades(days=days, strategy_id=strategy_filter)
    except Exception:
        m_rows = []
    if m_rows:
        any_data = True
        m_rows_filtered = [t for t in m_rows if t.action != "NO_TRADE"]
        subsection(f"Mention Market（最近 {min(limit, len(m_rows_filtered))} 筆）")
        recent = sorted(m_rows_filtered, key=lambda t: t.ts or "", reverse=True)[:limit]
        print(
            f"\n  {'時間':<20}  {'策略':<30}  {'市場':<24}  "
            f"{'動作':<16}  {'方向':>4}  {'價格':>7}  {'金額':>8}  "
            f"{'持倉(s)':>7}  {'邊際bps':>7}  {'實現PnL':>11}  {'原因'}"
        )
        print("  " + hr(145))
        for t in recent:
            ts     = (t.ts or "")[:19]
            hold   = str(t.hold_sec) if t.hold_sec is not None else "—"
            edge   = f"{t.expected_net_edge_bps:.0f}" if t.expected_net_edge_bps is not None else "—"
            pnl    = _pnl_str(t.realized_pnl_usdc)
            act_c  = (GREEN  if t.action == "TAKE_PROFIT" else
                      RED    if t.action == "STOP_LOSS"   else
                      YELLOW if t.action == "TIME_EXIT"   else DIM)
            act    = f"{act_c}{t.action:<16}{NC}"
            print(
                f"  {ts:<20}  {t.strategy_id:<30}  {t.market_slug[:22]:<24}  "
                f"{act}  {t.side:>4}  {t.price:>7.4f}  {t.size_usdc:>8.2f}  "
                f"{hold:>7}  {edge:>7}  {pnl:>20}  {t.reason_code}"
            )

    # ── Weather ───────────────────────────────────────────────────────────────
    try:
        w_rows = reader.get_weather_dry_run_trades(days=days, strategy_id=strategy_filter)
    except Exception:
        w_rows = []
    if w_rows:
        any_data = True
        w_rows_filtered = [t for t in w_rows if t.action != "NO_TRADE"]
        subsection(f"Weather Market（最近 {min(limit, len(w_rows_filtered))} 筆）")
        recent = sorted(w_rows_filtered, key=lambda t: t.ts or "", reverse=True)[:limit]
        print(
            f"\n  {'時間':<20}  {'策略':<32}  {'動作':<18}  {'方向':>4}  "
            f"{'價格':>7}  {'金額':>8}  {'持倉(s)':>7}  {'p_yes入':>7}  {'p_yes出':>7}  "
            f"{'實現PnL':>11}  {'原因':<20}  市場（日期 / 條件）"
        )
        print("  " + hr(170))
        for t in recent:
            ts    = (t.ts or "")[:19]
            hold  = str(t.hold_sec) if t.hold_sec is not None else "—"
            py_in = f"{t.p_yes_at_entry:.3f}" if t.p_yes_at_entry is not None else "—"
            py_out= f"{t.p_yes_at_exit:.3f}"  if t.p_yes_at_exit  is not None else "—"
            pnl   = _pnl_str(t.realized_pnl_usdc)
            act_c = (GREEN  if t.action == "TAKE_PROFIT"    else
                     RED    if t.action == "STOP_LOSS"       else
                     CYAN   if t.action == "FORECAST_SHIFT"  else
                     YELLOW if t.action == "TIME_DECAY_EXIT" else DIM)
            act   = f"{act_c}{t.action:<18}{NC}"
            print(
                f"  {ts:<20}  {t.strategy_id:<32}  {act}  {t.side:>4}  "
                f"{t.price:>7.4f}  {t.size_usdc:>8.2f}  {hold:>7}  {py_in:>7}  {py_out:>7}  "
                f"{pnl:>20}  {t.reason_code:<20}  {t.market_slug}"
            )

    # ── Weather Ladder ────────────────────────────────────────────────────────
    try:
        l_rows = reader.get_weather_ladder_trades(days=days, strategy_id=strategy_filter)
    except Exception:
        l_rows = []
    if l_rows:
        any_data = True
        l_rows_filtered = [t for t in l_rows if t.action != "NO_TRADE"]
        subsection(f"Weather Ladder（最近 {min(limit, len(l_rows_filtered))} 筆）")
        recent = sorted(l_rows_filtered, key=lambda t: t.ts or "", reverse=True)[:limit]
        print(
            f"\n  {'時間':<20}  {'策略':<32}  {'Ladder ID':<20}  "
            f"{'腿#':>4}  {'動作':<26}  {'價格':>7}  {'金額':>8}  "
            f"{'p_yes':>6}  {'賠率':>7}  {'sum_p':>6}  {'實現PnL':>11}  {'原因':<20}  市場（日期 / 條件）"
        )
        print("  " + hr(180))
        for t in recent:
            ts     = (t.ts or "")[:19]
            p_yes  = f"{t.p_yes:.3f}" if t.p_yes is not None else "—"
            pnl    = _pnl_str(t.realized_pnl_usdc)
            act_c  = (GREEN  if t.action == "HOLD_TO_RESOLUTION"      else
                      RED    if t.action == "CATASTROPHIC_SHIFT_EXIT" else DIM)
            act    = f"{act_c}{t.action:<26}{NC}"
            print(
                f"  {ts:<20}  {t.strategy_id:<32}  {t.ladder_id[:18]:<20}  "
                f"{t.leg_index:>4}  {act}  {t.price:>7.4f}  {t.size_usdc:>8.2f}  "
                f"{p_yes:>6}  {t.ladder_payout_ratio:>7.1f}x  {t.ladder_combined_p:>6.3f}  "
                f"{pnl:>20}  {t.reason_code:<20}  {t.market_slug}"
            )

    if not any_data:
        print(f"\n  {DIM}（本期無任何交易紀錄）{NC}")


# ── Main ──────────────────────────────────────────────────────────────────────

def main() -> None:
    parser = argparse.ArgumentParser(
        description="Polymarket 策略狀態報告",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog=__doc__,
    )
    parser.add_argument("--days",       type=int,  default=7,    help="統計天數（預設 7）")
    parser.add_argument("--trades",     type=int,  default=30,   help="顯示最近 N 筆交易（預設 30）")
    parser.add_argument("--all-trades", action="store_true",     help="顯示全部交易")
    parser.add_argument("--strategy",   type=str,  default=None, help="只顯示指定策略")
    parser.add_argument("--no-trades",  action="store_true",     help="不顯示交易明細")
    parser.add_argument("--no-cycles",  action="store_true",     help="不顯示週期結算")
    args = parser.parse_args()

    db = _db_path()
    if not os.path.exists(db):
        print(f"\n{RED}✗ 找不到資料庫：{db}{NC}")
        print(f"  請先啟動引擎：{GREEN}make docker-run{NC}\n")
        sys.exit(1)

    reader = DbReader(db)
    initial_capital, cap_cfg = _load_capital_config()
    limit = 999_999 if args.all_trades else args.trades

    print(f"\n{BOLD}{'═'*80}")
    print(f"  Polymarket 自動交易系統 — 狀態報告")
    print(f"{'═'*80}{NC}")

    print_portfolio(reader, args.days, initial_capital)

    if not args.strategy:
        print_strategy_table(reader, args.days, cap_cfg)

    if not args.no_cycles:
        print_cycle_log(reader, args.days, limit, args.strategy)

    if not args.no_trades:
        print_trade_log(reader, args.days, limit, args.strategy)

    print(f"\n{DIM}{'─'*80}{NC}\n")


if __name__ == "__main__":
    main()
