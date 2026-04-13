# Sharpe, WinRate, MaxDD — pure functions, no external deps

from __future__ import annotations

import math
from typing import Sequence


def win_rate(pnl_series: Sequence[float]) -> float:
    """Fraction of profitable trades (pnl > 0).  Returns 0.0 for empty input."""
    if not pnl_series:
        return 0.0
    wins = sum(1 for p in pnl_series if p > 0)
    return wins / len(pnl_series)


def sharpe_ratio(pnl_series: Sequence[float], risk_free: float = 0.0) -> float:
    """Annualised Sharpe ratio assuming each observation is one 15-minute cycle.

    There are 4 × 24 × 365 = 35 040 cycles per year.
    Returns 0.0 when std-dev is zero or input is empty.
    """
    if len(pnl_series) < 2:
        return 0.0
    n = len(pnl_series)
    mean = sum(pnl_series) / n
    variance = sum((x - mean) ** 2 for x in pnl_series) / (n - 1)
    std = math.sqrt(variance)
    if std == 0.0:
        return 0.0
    cycles_per_year = 4 * 24 * 365  # 35 040
    return (mean - risk_free) / std * math.sqrt(cycles_per_year)


def max_drawdown_pct(pnl_series: Sequence[float]) -> float:
    """Maximum peak-to-trough drawdown as a positive fraction (0–1).

    Computed on the cumulative equity curve starting at 0.
    Returns 0.0 for empty or monotonically increasing input.
    """
    if not pnl_series:
        return 0.0
    peak = 0.0
    equity = 0.0
    max_dd = 0.0
    for pnl in pnl_series:
        equity += pnl
        if equity > peak:
            peak = equity
        dd = (peak - equity) / peak if peak > 0 else 0.0
        if dd > max_dd:
            max_dd = dd
    return max_dd
