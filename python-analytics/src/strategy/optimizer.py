# 參數網格搜索（27 組）
#
# Run from repo root:
#   cd python-analytics && python -m src.strategy.optimizer
#   cd python-analytics && python -m src.strategy.optimizer --db ../rust-engine/data/market_snapshots.db --days 7

from __future__ import annotations

import argparse
import os
import sys
from dataclasses import dataclass
from typing import Optional

# ── Path bootstrap ────────────────────────────────────────────────────────────
_HERE = os.path.dirname(__file__)
_ANALYTICS_ROOT = os.path.normpath(os.path.join(_HERE, "../.."))
if _ANALYTICS_ROOT not in sys.path:
    sys.path.insert(0, _ANALYTICS_ROOT)

from src.data.storage import DbReader, DryRunTrade  # noqa: E402
from src.backtest.metrics import max_drawdown_pct, sharpe_ratio, win_rate  # noqa: E402

# ── Parameter grid ────────────────────────────────────────────────────────────

# dump_threshold: 0.050, 0.075, 0.100, … 0.250  (9 values, step 0.025)
DUMP_THRESHOLDS: list[float] = [round(0.05 + i * 0.025, 3) for i in range(9)]
# bet_size: 10, 25, 50 USDC  (3 values)
BET_SIZES: list[float] = [10.0, 25.0, 50.0]

# ── Result dataclass ──────────────────────────────────────────────────────────


@dataclass
class OptResult:
    dump_threshold: float
    bet_size_usdc: float
    triggered: int          # trades that would have fired at this threshold
    win_rate: float
    total_pnl_usdc: float
    max_drawdown_pct: float
    sharpe: float

    def _win_pct(self) -> str:
        return f"{self.win_rate * 100:.1f}%" if self.triggered else "  n/a"

    def _sharpe_str(self) -> str:
        return f"{self.sharpe:+.3f}" if self.triggered >= 2 else "   n/a"


# ── Core optimiser ────────────────────────────────────────────────────────────


def run_optimizer(
    trades: list[DryRunTrade],
    sample_note: str = "",
) -> list[OptResult]:
    """Evaluate all 27 parameter combinations on the supplied trade list.

    Strategy logic re-simulation
    ─────────────────────────────
    Each DryRunTrade row recorded the *actual* dump_pct that triggered Leg 1
    (field: signal_dump_pct).  To re-evaluate a candidate threshold we ask:
    "would this trade have been triggered under the candidate threshold?"
    → yes  if signal_dump_pct >= candidate_threshold
    → no   otherwise (trade is excluded from this config's PnL)

    PnL per triggered trade
    ───────────────────────
    If would_profit is populated we scale it proportionally to the candidate
    bet_size relative to the original bet_size implied by size_usdc.
    If would_profit is NULL (market not yet resolved) we skip that trade from
    the PnL/win-rate calculation but still count it as triggered.
    """
    results: list[OptResult] = []

    for threshold in DUMP_THRESHOLDS:
        for bet in BET_SIZES:
            # Only consider Leg-1 entries (leg==1) — these carry signal_dump_pct
            triggered_trades = [
                t for t in trades
                if t.leg == 1
                and t.signal_dump_pct is not None
                and t.signal_dump_pct >= threshold
            ]

            # Build scaled PnL series for trades that have resolution data
            pnl_series: list[float] = []
            for t in triggered_trades:
                if t.would_profit is None:
                    continue
                # Scale PnL proportionally to new bet size vs original
                scale = bet / t.size_usdc if t.size_usdc > 0 else 1.0
                pnl_series.append(t.would_profit * scale)

            wr = win_rate(pnl_series)
            total_pnl = sum(pnl_series)
            mdd = max_drawdown_pct(pnl_series)
            sharpe = sharpe_ratio(pnl_series)

            results.append(OptResult(
                dump_threshold=threshold,
                bet_size_usdc=bet,
                triggered=len(triggered_trades),
                win_rate=wr,
                total_pnl_usdc=total_pnl,
                max_drawdown_pct=mdd,
                sharpe=sharpe,
            ))

    # Sort by win_rate descending, then total_pnl descending as tiebreaker
    results.sort(key=lambda r: (r.win_rate, r.total_pnl_usdc), reverse=True)
    return results


# ── Pretty-print ──────────────────────────────────────────────────────────────

_HDR = (
    f"{'dump_thr':>8}  {'bet_usdc':>8}  {'n':>5}  "
    f"{'win_rate':>8}  {'total_pnl':>10}  {'max_dd':>7}  {'sharpe':>8}"
)
_SEP = "-" * len(_HDR)


def print_results(
    results: list[OptResult],
    days: int,
    sample_note: str,
    top_n: Optional[int] = None,
) -> None:
    print("=" * len(_HDR))
    print("  Polymarket Dump-Hedge — Parameter Optimiser")
    print("=" * len(_HDR))
    if sample_note:
        print(f"  ⚠  {sample_note}")
    print(f"  Grid: {len(DUMP_THRESHOLDS)} thresholds × {len(BET_SIZES)} bet sizes = "
          f"{len(results)} configurations  |  window: {days}d")
    print(_SEP)
    print(_HDR)
    print(_SEP)

    display = results if top_n is None else results[:top_n]
    for r in display:
        print(
            f"{r.dump_threshold:>8.3f}  {r.bet_size_usdc:>8.0f}  {r.triggered:>5}  "
            f"{r._win_pct():>8}  {r.total_pnl_usdc:>+10.4f}  "
            f"{r.max_drawdown_pct * 100:>6.2f}%  {r._sharpe_str():>8}"
        )

    print("=" * len(_HDR))

    if results:
        best = results[0]
        print(
            f"\n  Best config: dump_threshold={best.dump_threshold:.3f}  "
            f"bet_size={best.bet_size_usdc:.0f} USDC  "
            f"win_rate={best._win_pct().strip()}  "
            f"pnl={best.total_pnl_usdc:+.4f} USDC"
        )


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
        description="Polymarket Dump-Hedge parameter grid search"
    )
    parser.add_argument(
        "--days", type=int, default=7,
        help="Days of history to use (default: 7)",
    )
    parser.add_argument(
        "--db", type=str, default=None,
        help="Path to SQLite DB (default: $DB_PATH or ./data/market_snapshots.db)",
    )
    parser.add_argument(
        "--top", type=int, default=None,
        help="Show only the top N results (default: all 27)",
    )
    args = parser.parse_args()

    db_path = args.db or _default_db_path()

    if not os.path.exists(db_path):
        print(f"[Error] DB not found: {db_path}", file=sys.stderr)
        print("  Run the Rust engine in dry_run mode first.", file=sys.stderr)
        sys.exit(1)

    reader = DbReader(db_path)
    span = reader.data_span_days()
    trades = reader.get_dry_run_trades(days=args.days)

    sample_note = ""
    if span == 0.0:
        sample_note = f"No data in DB yet — grid shown with zero trades (requested {args.days}d)"
    elif span < args.days:
        sample_note = (
            f"Only {span:.2f}d of data available "
            f"(requested {args.days}d) — results based on available sample"
        )

    results = run_optimizer(trades, sample_note=sample_note)
    print_results(results, days=args.days, sample_note=sample_note, top_n=args.top)


if __name__ == "__main__":
    main()
