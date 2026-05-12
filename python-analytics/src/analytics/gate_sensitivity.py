# Gate sensitivity analysis — quantify how many extra ENTRIES each gate would
# unlock if its threshold were relaxed, and (when sample is large enough)
# estimate the expected PnL of those extra entries from analogous resolved trades.
#
# Usage:
#   cd python-analytics
#   python -m src.analytics.gate_sensitivity --days 30
#   python -m src.analytics.gate_sensitivity --days 30 --strategy weather_custom_verify
#   python -m src.analytics.gate_sensitivity --days 30 --json
#
# Output: per-gate table with current threshold, candidate thresholds (-10%, -20%, -30%),
# extra entries unlocked, and (if resolved-trade history sufficient) expected PnL/USD.

from __future__ import annotations

import argparse
import json
import os
import sqlite3
import sys
from dataclasses import dataclass, asdict
from typing import Optional

_HERE = os.path.dirname(__file__)
_ANALYTICS_ROOT = os.path.normpath(os.path.join(_HERE, "../.."))
if _ANALYTICS_ROOT not in sys.path:
    sys.path.insert(0, _ANALYTICS_ROOT)


@dataclass
class GateScenario:
    gate: str                # reason_code
    candidate: str           # human description of the relaxation
    rule: str                # SQL-style description
    extra_entries: int       # NO_TRADE rows that would have passed
    sample_metric: Optional[float]  # mean of relevant metric on unlocked rows
    expected_pnl_per_usd: Optional[float]  # rough expected PnL per 1 USDC bet


@dataclass
class GateReport:
    days: int
    strategy_filter: Optional[str]
    total_no_trade: int
    total_entry: int
    total_resolved_with_pnl: int
    base_win_rate: Optional[float]
    base_avg_pnl: Optional[float]
    scenarios: list[GateScenario]


def _connect(db: str) -> sqlite3.Connection:
    c = sqlite3.connect(db)
    c.row_factory = sqlite3.Row
    return c


def _base_filter(days: int, strategy_id: Optional[str]) -> tuple[str, list]:
    conds = ["ts >= datetime('now', ?)"]
    params: list = [f"-{days} days"]
    if strategy_id:
        conds.append("strategy_id = ?")
        params.append(strategy_id)
    return " AND ".join(conds), params


def _count(conn, where: str, params: list) -> int:
    sql = f"SELECT COUNT(*) FROM weather_dry_run_trades WHERE {where}"
    return int(conn.execute(sql, params).fetchone()[0])


def _scalar(conn, sql: str, params: list) -> Optional[float]:
    row = conn.execute(sql, params).fetchone()
    return float(row[0]) if row and row[0] is not None else None


def _resolved_pnl_stats(conn, where: str, params: list) -> tuple[Optional[float], Optional[float], int]:
    """Return (win_rate, avg_pnl_per_usd, sample_size) for resolved trades matching where."""
    sql = (
        "SELECT realized_pnl_usdc, size_usdc FROM weather_dry_run_trades "
        f"WHERE {where} AND action IN ('SETTLEMENT','TAKE_PROFIT','STOP_LOSS','FORECAST_SHIFT','TIME_DECAY_EXIT') "
        "AND realized_pnl_usdc IS NOT NULL"
    )
    rows = conn.execute(sql, params).fetchall()
    if not rows:
        return None, None, 0
    pnls = [r[0] for r in rows]
    pnl_per_usd = [r[0] / r[1] for r in rows if r[1] and r[1] > 0]
    wr = sum(1 for p in pnls if p > 0) / len(pnls)
    avg_pnl_per_usd = sum(pnl_per_usd) / len(pnl_per_usd) if pnl_per_usd else None
    return wr, avg_pnl_per_usd, len(rows)


def run(db: str, days: int, strategy_id: Optional[str] = None) -> GateReport:
    conn = _connect(db)

    base_where, base_params = _base_filter(days, strategy_id)
    total_no_trade = _count(conn, f"{base_where} AND action='NO_TRADE'", base_params)
    total_entry = _count(conn, f"{base_where} AND action='ENTRY'", base_params)
    base_wr, base_avg_pnl, base_resolved = _resolved_pnl_stats(conn, base_where, base_params)

    scenarios: list[GateScenario] = []

    # ── Gate: LOW_DEPTH (depth_usdc_at_decision below min) ────────────────────
    # min_depth_usdc default 50; try 25, 10, 5
    for cand in [25.0, 10.0, 5.0]:
        sql = (
            "SELECT COUNT(*), AVG(depth_usdc_at_decision) "
            f"FROM weather_dry_run_trades WHERE {base_where} "
            "AND action='NO_TRADE' AND reason_code='LOW_DEPTH' "
            "AND depth_usdc_at_decision >= ?"
        )
        row = conn.execute(sql, base_params + [cand]).fetchone()
        extra = int(row[0] or 0)
        avg_d = float(row[1]) if row[1] is not None else None
        # Use analogous resolved-trade PnL — same strategy_id, same lead bucket if possible
        scenarios.append(GateScenario(
            gate="LOW_DEPTH",
            candidate=f"min_depth_usdc → {cand:g}",
            rule=f"depth_usdc_at_decision >= {cand:g}",
            extra_entries=extra,
            sample_metric=avg_d,
            expected_pnl_per_usd=base_avg_pnl,  # rough proxy
        ))

    # ── Gate: LOW_EDGE ────────────────────────────────────────────────────────
    # Approximate "would have passed" by counting LOW_EDGE rows; we can't recompute
    # edge from stored fields alone, but the count itself is a useful upper bound.
    for relax_pct in [0.1, 0.2, 0.3]:
        sql = (
            "SELECT COUNT(*) FROM weather_dry_run_trades "
            f"WHERE {base_where} AND action='NO_TRADE' AND reason_code='LOW_EDGE'"
        )
        c = int(conn.execute(sql, base_params).fetchone()[0])
        # Assume relaxation unlocks a proportional fraction (upper bound = all of them)
        scenarios.append(GateScenario(
            gate="LOW_EDGE",
            candidate=f"min_net_edge_bps -{int(relax_pct * 100)}%",
            rule=f"would unlock ≤ {c} rows (proportional estimate)",
            extra_entries=int(c * relax_pct / 0.3),  # crude linear scale
            sample_metric=None,
            expected_pnl_per_usd=base_avg_pnl,
        ))

    # ── Gate: LOW_CONFIDENCE ──────────────────────────────────────────────────
    for cand in [0.55, 0.50, 0.45]:
        sql = (
            "SELECT COUNT(*), AVG(p_yes_at_entry) "
            f"FROM weather_dry_run_trades WHERE {base_where} "
            "AND action='NO_TRADE' AND reason_code='LOW_CONFIDENCE' "
            "AND (p_yes_at_entry >= ? OR (1 - p_yes_at_entry) >= ?)"
        )
        row = conn.execute(sql, base_params + [cand, cand]).fetchone()
        extra = int(row[0] or 0)
        avg_p = float(row[1]) if row[1] is not None else None
        scenarios.append(GateScenario(
            gate="LOW_CONFIDENCE",
            candidate=f"min_model_confidence → {cand:g}",
            rule=f"max(p_yes, 1-p_yes) >= {cand:g}",
            extra_entries=extra,
            sample_metric=avg_p,
            expected_pnl_per_usd=base_avg_pnl,
        ))

    # ── Gate: SPREAD_WIDE ─────────────────────────────────────────────────────
    for cand in [0.05, 0.10, 0.15]:
        sql = (
            "SELECT COUNT(*), AVG(spread_at_decision) "
            f"FROM weather_dry_run_trades WHERE {base_where} "
            "AND action='NO_TRADE' AND reason_code='SPREAD_WIDE' "
            "AND spread_at_decision <= ?"
        )
        row = conn.execute(sql, base_params + [cand]).fetchone()
        extra = int(row[0] or 0)
        avg_s = float(row[1]) if row[1] is not None else None
        scenarios.append(GateScenario(
            gate="SPREAD_WIDE",
            candidate=f"max_spread → {cand:g}",
            rule=f"spread_at_decision <= {cand:g}",
            extra_entries=extra,
            sample_metric=avg_s,
            expected_pnl_per_usd=base_avg_pnl,
        ))

    conn.close()
    return GateReport(
        days=days,
        strategy_filter=strategy_id,
        total_no_trade=total_no_trade,
        total_entry=total_entry,
        total_resolved_with_pnl=base_resolved,
        base_win_rate=base_wr,
        base_avg_pnl=base_avg_pnl,
        scenarios=scenarios,
    )


def _print(report: GateReport) -> None:
    print("=" * 78)
    print("  Gate Sensitivity Analysis")
    print("=" * 78)
    filt = report.strategy_filter or "(all strategies)"
    print(f"  Window: last {report.days} day(s)  filter: {filt}")
    print(f"  Baseline NO_TRADE: {report.total_no_trade:>6}  ENTRY: {report.total_entry:>6}  "
          f"Resolved-with-PnL: {report.total_resolved_with_pnl}")
    if report.base_win_rate is not None:
        print(f"  Baseline win rate: {report.base_win_rate * 100:.1f}%  "
              f"avg PnL/USD: {report.base_avg_pnl:+.5f}")
    print("-" * 78)
    print(f"  {'Gate':<18} {'Candidate':<35} {'Δ entries':>10} {'metric':>12}")
    print("-" * 78)
    current_gate = None
    for s in report.scenarios:
        if s.gate != current_gate:
            current_gate = s.gate
            print()
        metric = f"{s.sample_metric:.3f}" if s.sample_metric is not None else "n/a"
        print(f"  {s.gate:<18} {s.candidate:<35} {s.extra_entries:>10} {metric:>12}")
    print("=" * 78)
    print()
    print("  指引：'Δ entries' 越高表示放寬此閘門能多解鎖的訊號數。")
    print("  若 LOW_DEPTH 即使閾值降到 5 也無法解鎖多少，代表市場深度真的不足，")
    print("  不是策略過嚴 — 應改為轉換策略目標市場（換城市/換 lead_days）。")


def main() -> None:
    ap = argparse.ArgumentParser(description="Weather gate sensitivity analysis")
    ap.add_argument("--db", default=None, help="SQLite DB path")
    ap.add_argument("--days", type=int, default=30, help="Look-back window")
    ap.add_argument("--strategy", type=str, default=None, help="Filter to one strategy_id")
    ap.add_argument("--json", action="store_true", help="Output JSON instead of table")
    args = ap.parse_args()

    db = args.db
    if db is None:
        repo_root = os.path.normpath(os.path.join(_HERE, "../../.."))
        db = os.path.join(repo_root, "rust-engine", "data", "market_snapshots.db")

    if not os.path.exists(db):
        print(f"[ERROR] DB not found: {db}", file=sys.stderr)
        sys.exit(1)

    report = run(db, args.days, args.strategy)

    if args.json:
        print(json.dumps(asdict(report), indent=2))
    else:
        _print(report)


if __name__ == "__main__":
    main()
