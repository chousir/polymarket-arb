# Trump Mention Market — backtest / audit report
#
# Reads from mention_dry_run_trades and computes:
#   - trigger rate / fill rate
#   - avg hold seconds
#   - gross PnL (before cost) and net PnL (after cost)
#   - max drawdown, consecutive losses
#
# Run from repo root:
#   cd python-analytics && python -m src.backtest.mention_report --days 14
#   cd python-analytics && python -m src.backtest.mention_report --days 14 --db ../rust-engine/data/market_snapshots.db

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

from src.data.storage import DbReader, MentionDryRunTrade  # noqa: E402
from src.backtest.metrics import max_drawdown_pct, sharpe_ratio  # noqa: E402

# ── Report dataclass ──────────────────────────────────────────────────────────


@dataclass
class MentionBacktestReport:
    # Window
    days_requested: int
    total_rows: int             # all rows in window

    # Activity
    entry_count: int            # action == ENTRY
    cancel_count: int           # action == NO_TRADE (signal seen but not traded)
    trigger_rate: float         # entry_count / (entry_count + cancel_count), 0 if none
    fill_rate: float            # (take_profit + stop_loss) / entry_count — exits before time-out

    # Hold time
    avg_hold_sec: Optional[float]   # mean hold_sec across exit rows
    max_hold_sec: Optional[int]

    # PnL — net (realized_pnl_usdc already includes fee deduction in the engine)
    net_pnl_usdc: float
    gross_pnl_usdc: float           # net + estimated fees paid

    # Risk
    max_drawdown_pct: float         # 0–1
    sharpe_ratio: float             # annualised
    max_consecutive_losses: int

    # Rejection breakdown (reason_code counts)
    edge_too_low_count: int
    spread_too_wide_count: int
    depth_too_thin_count: int

    def print(self) -> None:
        print("=" * 60)
        print("  Polymarket Mention Market — Backtest Report")
        print("=" * 60)
        print(f"  Window requested   : {self.days_requested} day(s)")
        print(f"  Total DB rows      : {self.total_rows}")
        print("-" * 60)
        print(f"  Entries            : {self.entry_count}")
        print(f"  Cancels            : {self.cancel_count}")
        _tr = f"{self.trigger_rate * 100:.1f}%" if (self.entry_count + self.cancel_count) else "n/a"
        print(f"  Trigger rate       : {_tr}  (entries / signals)")
        _fr = f"{self.fill_rate * 100:.1f}%" if self.entry_count else "n/a"
        print(f"  Fill rate          : {_fr}  (TP+SL / entries)")
        if self.avg_hold_sec is not None:
            print(f"  Avg hold           : {self.avg_hold_sec:.0f}s  (max {self.max_hold_sec}s)")
        print("-" * 60)
        print(f"  Net PnL            : {self.net_pnl_usdc:+.4f} USDC")
        print(f"  Gross PnL          : {self.gross_pnl_usdc:+.4f} USDC  (before fees)")
        print(f"  Max drawdown       : {self.max_drawdown_pct * 100:.2f}%")
        _sharpe = f"{self.sharpe_ratio:.3f}" if self.entry_count >= 2 else "n/a"
        print(f"  Sharpe (ann.)      : {_sharpe}")
        print(f"  Max consec. losses : {self.max_consecutive_losses}")
        print("-" * 60)
        print(f"  Reject — edge low  : {self.edge_too_low_count}")
        print(f"  Reject — spread    : {self.spread_too_wide_count}")
        print(f"  Reject — depth     : {self.depth_too_thin_count}")
        print("=" * 60)


# ── Engine ────────────────────────────────────────────────────────────────────


def run_mention_backtest(db_path: str, days: int) -> MentionBacktestReport:
    reader = DbReader(db_path)
    rows: list[MentionDryRunTrade] = reader.get_mention_dry_run_trades(days=days)

    total_rows = len(rows)

    entries  = [r for r in rows if r.action == "ENTRY"]
    cancels  = [r for r in rows if r.action in ("NO_TRADE", "CANCEL")]
    tp_rows  = [r for r in rows if r.action == "TAKE_PROFIT"]
    sl_rows  = [r for r in rows if r.action == "STOP_LOSS"]
    te_rows  = [r for r in rows if r.action == "TIME_EXIT"]

    entry_count  = len(entries)
    cancel_count = len(cancels)

    signals = entry_count + cancel_count
    trigger_rate = entry_count / signals if signals else 0.0

    resolved_exits = len(tp_rows) + len(sl_rows)
    fill_rate = resolved_exits / entry_count if entry_count else 0.0

    # ── Hold time ─────────────────────────────────────────────────────────────
    exit_rows = tp_rows + sl_rows + te_rows
    hold_secs = [r.hold_sec for r in exit_rows if r.hold_sec is not None]
    avg_hold_sec = sum(hold_secs) / len(hold_secs) if hold_secs else None
    max_hold_sec = max(hold_secs) if hold_secs else None

    # ── PnL series from exit rows ─────────────────────────────────────────────
    pnl_series = [
        r.realized_pnl_usdc
        for r in exit_rows
        if r.realized_pnl_usdc is not None
    ]
    net_pnl = sum(pnl_series)

    # Gross = net + fees paid (reconstruct from bps fields on entry rows)
    total_fees = _estimate_fees(entries)
    gross_pnl = net_pnl + total_fees

    # ── Risk metrics ──────────────────────────────────────────────────────────
    mdd = max_drawdown_pct(pnl_series)
    sharpe = sharpe_ratio(pnl_series)
    max_consec = _max_consecutive_losses(pnl_series)

    # ── Rejection breakdown ───────────────────────────────────────────────────
    edge_low    = sum(1 for r in rows if r.reason_code == "EDGE_TOO_LOW")
    spread_wide = sum(1 for r in rows if r.reason_code == "SPREAD_TOO_WIDE")
    depth_thin  = sum(1 for r in rows if r.reason_code == "DEPTH_TOO_THIN")

    return MentionBacktestReport(
        days_requested=days,
        total_rows=total_rows,
        entry_count=entry_count,
        cancel_count=cancel_count,
        trigger_rate=trigger_rate,
        fill_rate=fill_rate,
        avg_hold_sec=avg_hold_sec,
        max_hold_sec=max_hold_sec,
        net_pnl_usdc=net_pnl,
        gross_pnl_usdc=gross_pnl,
        max_drawdown_pct=mdd,
        sharpe_ratio=sharpe,
        max_consecutive_losses=max_consec,
        edge_too_low_count=edge_low,
        spread_too_wide_count=spread_wide,
        depth_too_thin_count=depth_thin,
    )


# ── Helpers ───────────────────────────────────────────────────────────────────


def _estimate_fees(entries: list[MentionDryRunTrade]) -> float:
    """Reconstruct total fees paid from bps fields on ENTRY rows."""
    total = 0.0
    for r in entries:
        taker = (r.taker_fee_bps or 0) / 10_000
        slip  = (r.slippage_buffer_bps or 0) / 10_000
        exec_ = (r.execution_risk_bps or 0) / 10_000
        total += r.size_usdc * (taker + slip + exec_)
    return total


def _max_consecutive_losses(pnl_series: list[float]) -> int:
    """Return the length of the longest consecutive run of negative PnL values."""
    max_run = 0
    current = 0
    for p in pnl_series:
        if p < 0:
            current += 1
            max_run = max(max_run, current)
        else:
            current = 0
    return max_run


# ── CLI ───────────────────────────────────────────────────────────────────────


def _default_db_path() -> str:
    # Anchor to the rust-engine data directory
    _repo_root = os.path.normpath(os.path.join(_HERE, "../../.."))
    return os.path.join(_repo_root, "rust-engine", "data", "market_snapshots.db")


def main() -> None:
    parser = argparse.ArgumentParser(
        description="Trump Mention Market backtest report"
    )
    parser.add_argument(
        "--days", type=int, default=14,
        help="Number of past days to include (default: 14)",
    )
    parser.add_argument(
        "--db", type=str, default=None,
        help="Path to SQLite DB (default: rust-engine/data/market_snapshots.db)",
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

    report = run_mention_backtest(db_path=db_path, days=args.days)
    report.print()


if __name__ == "__main__":
    main()
