# 從 dry_run_trades 回測
#
# Run from repo root:
#   cd python-analytics && python -m src.backtest.engine --days 3
#   cd python-analytics && python -m src.backtest.engine --days 7 --db ../data/market_snapshots.db

from __future__ import annotations

import argparse
import os
import sys
from dataclasses import dataclass
from typing import Optional

# ── Path bootstrap (for direct execution outside the package) ─────────────────
_HERE = os.path.dirname(__file__)
_PKG_ROOT = os.path.normpath(os.path.join(_HERE, ".."))  # python-analytics/src/
_ANALYTICS_ROOT = os.path.normpath(os.path.join(_HERE, "../.."))  # python-analytics/
if _ANALYTICS_ROOT not in sys.path:
    sys.path.insert(0, _ANALYTICS_ROOT)

from src.data.storage import CycleResult, DbReader, DryRunTrade  # noqa: E402
from src.backtest.metrics import max_drawdown_pct, sharpe_ratio, win_rate  # noqa: E402

# ── Report dataclass ──────────────────────────────────────────────────────────


@dataclass
class BacktestReport:
    # Window
    days_requested: int
    days_available: float
    sample_note: str          # "" when full window is available; warning otherwise

    # Cycle-level stats
    total_cycles: int         # all cycle_results rows in window
    triggered_cycles: int     # cycles where Leg 1 fired (leg1_price is not None)
    win_rate: float           # fraction of triggered cycles with pnl > 0
    total_pnl_usdc: float

    # Risk
    max_drawdown_pct: float   # 0–1
    sharpe_ratio: float       # annualised, assuming one 15-min cycle per observation

    # Per-trade averages
    avg_leg1_price: Optional[float]
    avg_leg2_price: Optional[float]
    avg_profit_per_win: Optional[float]

    def print(self) -> None:
        """Pretty-print the report to stdout."""
        print("=" * 56)
        print("  Polymarket Dump-Hedge — Backtest Report")
        print("=" * 56)
        if self.sample_note:
            print(f"  ⚠  {self.sample_note}")
        print(f"  Window requested : {self.days_requested} day(s)")
        print(f"  Data available   : {self.days_available:.2f} day(s)")
        print("-" * 56)
        print(f"  Total cycles     : {self.total_cycles}")
        print(f"  Triggered (Leg1) : {self.triggered_cycles}")
        _pct = f"{self.win_rate * 100:.1f}%" if self.triggered_cycles else "n/a"
        print(f"  Win rate         : {_pct}")
        print(f"  Total PnL        : {self.total_pnl_usdc:+.4f} USDC")
        print(f"  Max drawdown     : {self.max_drawdown_pct * 100:.2f}%")
        _sharpe = f"{self.sharpe_ratio:.3f}" if self.triggered_cycles >= 2 else "n/a"
        print(f"  Sharpe (ann.)    : {_sharpe}")
        print("-" * 56)
        if self.avg_leg1_price is not None:
            print(f"  Avg Leg1 price   : {self.avg_leg1_price:.4f}")
        if self.avg_leg2_price is not None:
            print(f"  Avg Leg2 price   : {self.avg_leg2_price:.4f}")
        if self.avg_profit_per_win is not None:
            print(f"  Avg profit/win   : {self.avg_profit_per_win:+.4f} USDC")
        print("=" * 56)


# ── Engine ────────────────────────────────────────────────────────────────────


def run_backtest(db_path: str, days: int) -> BacktestReport:
    """Load data from SQLite and compute BacktestReport."""
    reader = DbReader(db_path)

    span = reader.data_span_days()
    cycles: list[CycleResult] = reader.get_cycle_results(days=days)
    trades: list[DryRunTrade] = reader.get_dry_run_trades(days=days)

    # Sample note when the DB holds less data than requested
    sample_note = ""
    if span < days:
        if span == 0.0:
            sample_note = f"No data in DB yet — report is empty (requested {days}d)"
        else:
            sample_note = (
                f"Only {span:.2f}d of data available "
                f"(requested {days}d) — metrics based on available sample"
            )

    # ── Triggered cycles (Leg 1 fired) ───────────────────────────────────────
    triggered = [c for c in cycles if c.leg1_price is not None]

    # ── PnL series: prefer cycle_results.pnl_usdc, fall back to would_profit ─
    resolved = [c for c in triggered if c.pnl_usdc is not None]
    pnl_series: list[float] = [c.pnl_usdc for c in resolved]  # type: ignore[misc]

    if not pnl_series:
        pnl_series = _pnl_from_trades(trades)

    total_pnl = sum(pnl_series)
    wr = win_rate(pnl_series)
    sharpe = sharpe_ratio(pnl_series)
    mdd = max_drawdown_pct(pnl_series)

    # ── Price averages ────────────────────────────────────────────────────────
    leg1_prices = [c.leg1_price for c in triggered if c.leg1_price is not None]
    leg2_prices = [c.leg2_price for c in triggered if c.leg2_price is not None]
    wins = [p for p in pnl_series if p > 0]

    avg_leg1 = sum(leg1_prices) / len(leg1_prices) if leg1_prices else None
    avg_leg2 = sum(leg2_prices) / len(leg2_prices) if leg2_prices else None
    avg_profit_per_win = sum(wins) / len(wins) if wins else None

    return BacktestReport(
        days_requested=days,
        days_available=round(span, 4),
        sample_note=sample_note,
        total_cycles=len(cycles),
        triggered_cycles=len(triggered),
        win_rate=wr,
        total_pnl_usdc=total_pnl,
        max_drawdown_pct=mdd,
        sharpe_ratio=sharpe,
        avg_leg1_price=avg_leg1,
        avg_leg2_price=avg_leg2,
        avg_profit_per_win=avg_profit_per_win,
    )


# ── Helpers ───────────────────────────────────────────────────────────────────


def _pnl_from_trades(trades: list[DryRunTrade]) -> list[float]:
    """Derive a PnL series from would_profit on Leg-1 trades when
    cycle_results doesn't have pnl_usdc populated yet (pre-resolution)."""
    from collections import defaultdict

    by_slug: dict[str, list[DryRunTrade]] = defaultdict(list)
    for t in trades:
        by_slug[t.market_slug].append(t)

    pnl: list[float] = []
    for slug_trades in by_slug.values():
        leg1 = [t for t in slug_trades if t.leg == 1]
        if leg1 and leg1[0].would_profit is not None:
            pnl.append(leg1[0].would_profit)
    return pnl


# ── CLI ───────────────────────────────────────────────────────────────────────


def _default_db_path() -> str:
    try:
        from dotenv import load_dotenv
        load_dotenv()
    except ImportError:
        pass
    return os.environ.get("DB_PATH", "./data/market_snapshots.db")


def main() -> None:
    parser = argparse.ArgumentParser(
        description="Polymarket Dump-Hedge backtest engine"
    )
    parser.add_argument(
        "--days",
        type=int,
        default=3,
        help="Number of past days to include (default: 3)",
    )
    parser.add_argument(
        "--db",
        type=str,
        default=None,
        help="Path to SQLite DB (default: $DB_PATH or ./data/market_snapshots.db)",
    )
    args = parser.parse_args()

    db_path = args.db or _default_db_path()

    if not os.path.exists(db_path):
        print(f"[Error] DB not found: {db_path}", file=sys.stderr)
        print(
            "  Run the Rust engine in dry_run mode first to populate data.",
            file=sys.stderr,
        )
        sys.exit(1)

    report = run_backtest(db_path=db_path, days=args.days)
    report.print()


if __name__ == "__main__":
    main()
