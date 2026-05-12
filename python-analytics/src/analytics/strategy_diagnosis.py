# Strategy diagnosis — grouped analysis of resolved weather trades to surface
# loss-concentration patterns across multiple dimensions: strategy_id, model,
# city, lead_days bucket, p_yes_at_entry bucket, market_type, side.
#
# Output: tables (CSV-style) + summary Markdown for docs/improvement-log/.
#
# Usage:
#   cd python-analytics
#   python -m src.analytics.strategy_diagnosis --days 90
#   python -m src.analytics.strategy_diagnosis --days 90 --json
#   python -m src.analytics.strategy_diagnosis --days 90 --markdown > ../docs/improvement-log/phase4-diagnosis.md

from __future__ import annotations

import argparse
import csv
import io
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
class GroupRow:
    dimension: str        # e.g. "city"
    bucket: str           # e.g. "NYC"
    n_resolved: int
    n_wins: int
    win_rate: float
    total_pnl_usdc: float
    avg_pnl_per_usd: float
    avg_size_usdc: float


@dataclass
class DiagnosisReport:
    days: int
    total_resolved: int
    total_pnl_usdc: float
    overall_win_rate: float
    overall_avg_pnl_per_usd: float
    groups: dict[str, list[GroupRow]]


EXIT_ACTIONS = ("SETTLEMENT", "TAKE_PROFIT", "STOP_LOSS",
                "FORECAST_SHIFT", "TIME_DECAY_EXIT")


def _connect(db: str) -> sqlite3.Connection:
    c = sqlite3.connect(db)
    c.row_factory = sqlite3.Row
    return c


def _fetch_resolved(conn: sqlite3.Connection, days: int) -> list[sqlite3.Row]:
    placeholders = ",".join(["?"] * len(EXIT_ACTIONS))
    sql = (
        "SELECT strategy_id, market_slug, city, market_type, side, action, "
        "       price, size_usdc, entry_price, exit_price, hold_sec, "
        "       model, p_yes_at_entry, lead_days, expected_net_edge_bps, "
        "       realized_pnl_usdc "
        "FROM weather_dry_run_trades "
        f"WHERE action IN ({placeholders}) "
        "AND realized_pnl_usdc IS NOT NULL "
        "AND ts >= datetime('now', ?)"
    )
    return conn.execute(sql, list(EXIT_ACTIONS) + [f"-{days} days"]).fetchall()


def _bucket_lead(d: Optional[int]) -> str:
    if d is None:
        return "unknown"
    if d <= 0:
        return "same-day"
    if d <= 2:
        return "1-2d"
    if d <= 4:
        return "3-4d"
    if d <= 7:
        return "5-7d"
    return "8+d"


def _bucket_p_yes(p: Optional[float]) -> str:
    if p is None:
        return "unknown"
    if p < 0.20:
        return "0.00-0.20"
    if p < 0.40:
        return "0.20-0.40"
    if p < 0.60:
        return "0.40-0.60"
    if p < 0.80:
        return "0.60-0.80"
    return "0.80-1.00"


def _group_stats(rows: list[sqlite3.Row], key_fn) -> list[GroupRow]:
    groups: dict[str, list[sqlite3.Row]] = {}
    for r in rows:
        groups.setdefault(key_fn(r), []).append(r)

    out = []
    for bucket, members in groups.items():
        pnls = [m["realized_pnl_usdc"] for m in members]
        sizes = [m["size_usdc"] for m in members if m["size_usdc"]]
        wins = sum(1 for p in pnls if p > 0)
        total_pnl = sum(pnls)
        avg_size = sum(sizes) / len(sizes) if sizes else 0.0
        pnl_per_usd = [m["realized_pnl_usdc"] / m["size_usdc"]
                       for m in members
                       if m["size_usdc"] and m["size_usdc"] > 0]
        avg_pnl_per_usd = sum(pnl_per_usd) / len(pnl_per_usd) if pnl_per_usd else 0.0
        out.append(GroupRow(
            dimension="",
            bucket=str(bucket),
            n_resolved=len(members),
            n_wins=wins,
            win_rate=wins / len(members) if members else 0.0,
            total_pnl_usdc=total_pnl,
            avg_pnl_per_usd=avg_pnl_per_usd,
            avg_size_usdc=avg_size,
        ))
    out.sort(key=lambda x: x.total_pnl_usdc)
    return out


def run(db: str, days: int) -> DiagnosisReport:
    conn = _connect(db)
    rows = _fetch_resolved(conn, days)
    conn.close()

    if not rows:
        return DiagnosisReport(
            days=days, total_resolved=0, total_pnl_usdc=0.0,
            overall_win_rate=0.0, overall_avg_pnl_per_usd=0.0, groups={},
        )

    total_pnl = sum(r["realized_pnl_usdc"] for r in rows)
    wins = sum(1 for r in rows if r["realized_pnl_usdc"] > 0)
    pnl_per_usd = [r["realized_pnl_usdc"] / r["size_usdc"]
                   for r in rows if r["size_usdc"] and r["size_usdc"] > 0]
    avg_pnl_per_usd = sum(pnl_per_usd) / len(pnl_per_usd) if pnl_per_usd else 0.0

    dim_specs = [
        ("strategy_id", lambda r: r["strategy_id"] or "unknown"),
        ("model", lambda r: r["model"] or "unknown"),
        ("city", lambda r: r["city"] or "unknown"),
        ("market_type", lambda r: r["market_type"] or "unknown"),
        ("side", lambda r: r["side"] or "unknown"),
        ("action", lambda r: r["action"] or "unknown"),
        ("lead_days_bucket", lambda r: _bucket_lead(r["lead_days"])),
        ("p_yes_at_entry_bucket", lambda r: _bucket_p_yes(r["p_yes_at_entry"])),
    ]

    groups: dict[str, list[GroupRow]] = {}
    for dim_name, key_fn in dim_specs:
        gs = _group_stats(rows, key_fn)
        for g in gs:
            g.dimension = dim_name
        groups[dim_name] = gs

    return DiagnosisReport(
        days=days,
        total_resolved=len(rows),
        total_pnl_usdc=total_pnl,
        overall_win_rate=wins / len(rows),
        overall_avg_pnl_per_usd=avg_pnl_per_usd,
        groups=groups,
    )


def _print_table(report: DiagnosisReport) -> None:
    print("=" * 90)
    print("  Strategy Diagnosis — Loss-Concentration Analysis")
    print("=" * 90)
    print(f"  Window: last {report.days} day(s)")
    print(f"  Resolved trades: {report.total_resolved}")
    print(f"  Total PnL: {report.total_pnl_usdc:+.4f} USDC")
    print(f"  Win rate: {report.overall_win_rate * 100:.1f}%")
    print(f"  Avg PnL per USD invested: {report.overall_avg_pnl_per_usd:+.5f}")
    print("=" * 90)

    for dim, rows in report.groups.items():
        print()
        print(f"  [{dim}]")
        print(f"  {'bucket':<28} {'n':>4} {'wins':>4} {'win%':>6} "
              f"{'pnl':>10} {'pnl/USD':>10} {'avg_size':>10}")
        print("  " + "-" * 86)
        for r in rows:
            print(f"  {r.bucket:<28} {r.n_resolved:>4} {r.n_wins:>4} "
                  f"{r.win_rate * 100:>5.1f}% {r.total_pnl_usdc:>+10.3f} "
                  f"{r.avg_pnl_per_usd:>+10.5f} {r.avg_size_usdc:>10.2f}")

    print("=" * 90)


def _to_markdown(report: DiagnosisReport) -> str:
    out = io.StringIO()
    out.write(f"# Phase 4：策略診斷報告（自動產出）\n\n")
    out.write(f"- 資料視窗：最近 **{report.days}** 天\n")
    out.write(f"- 已結算交易：**{report.total_resolved}** 筆\n")
    out.write(f"- 累計 PnL：**{report.total_pnl_usdc:+.4f} USDC**\n")
    out.write(f"- 整體勝率：**{report.overall_win_rate * 100:.1f}%**\n")
    out.write(f"- 平均 PnL/USDC：**{report.overall_avg_pnl_per_usd:+.5f}**\n\n")

    for dim, rows in report.groups.items():
        out.write(f"## by {dim}\n\n")
        out.write("| bucket | n | wins | win% | PnL | PnL/USD | avg size |\n")
        out.write("|---|---:|---:|---:|---:|---:|---:|\n")
        for r in rows:
            out.write(
                f"| {r.bucket} | {r.n_resolved} | {r.n_wins} | "
                f"{r.win_rate * 100:.1f}% | {r.total_pnl_usdc:+.3f} | "
                f"{r.avg_pnl_per_usd:+.5f} | {r.avg_size_usdc:.2f} |\n"
            )
        out.write("\n")
    return out.getvalue()


def main() -> None:
    ap = argparse.ArgumentParser(description="Strategy loss-concentration diagnosis")
    ap.add_argument("--db", default=None)
    ap.add_argument("--days", type=int, default=90)
    ap.add_argument("--json", action="store_true")
    ap.add_argument("--markdown", action="store_true")
    args = ap.parse_args()

    db = args.db
    if db is None:
        repo_root = os.path.normpath(os.path.join(_HERE, "../../.."))
        db = os.path.join(repo_root, "rust-engine", "data", "market_snapshots.db")
    if not os.path.exists(db):
        print(f"[ERROR] DB not found: {db}", file=sys.stderr)
        sys.exit(1)

    report = run(db, args.days)
    if args.json:
        print(json.dumps(asdict(report), indent=2))
    elif args.markdown:
        print(_to_markdown(report))
    else:
        _print_table(report)


if __name__ == "__main__":
    main()
