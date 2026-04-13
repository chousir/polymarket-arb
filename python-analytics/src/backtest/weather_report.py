# Weather Market Strategy — backtest / audit report
#
# Reads from weather_dry_run_trades and computes:
#   - Per-strategy: entries, trigger rate, fill rate, net PnL, win rate, Sharpe, MDD
#   - Per-city: entry count, net PnL, win rate, avg edge at entry
#   - Per-market-type: temp_range / extreme / precip performance
#   - Per-model: gfs / ecmwf / ensemble performance + avg p_yes confidence
#   - Per-lead-days bucket: short (1–3d) / medium (4–7d) / long (8–14d)
#   - Exit-trigger breakdown: TP / SL / FORECAST_SHIFT / TIME_DECAY_EXIT
#   - FORECAST_SHIFT analysis: avg |Δp_yes|, fraction where direction flipped
#
# Run from repo root:
#   cd python-analytics && python -m src.backtest.weather_report --days 14
#   cd python-analytics && python -m src.backtest.weather_report --days 14 --json
#   cd python-analytics && python -m src.backtest.weather_report \
#       --days 30 --strategy weather_nyc_gfs_v1 \
#       --db ../rust-engine/data/market_snapshots.db

from __future__ import annotations

import argparse
import dataclasses
import json
import os
import sys
from dataclasses import dataclass
from typing import Optional

# ── Path bootstrap ────────────────────────────────────────────────────────────
_HERE = os.path.dirname(__file__)
_ANALYTICS_ROOT = os.path.normpath(os.path.join(_HERE, "../.."))
if _ANALYTICS_ROOT not in sys.path:
    sys.path.insert(0, _ANALYTICS_ROOT)

from src.data.storage import DbReader, WeatherDryRunTrade  # noqa: E402
from src.backtest.metrics import max_drawdown_pct, sharpe_ratio, win_rate  # noqa: E402

# ── Report dataclasses ────────────────────────────────────────────────────────


@dataclass
class ExitBreakdown:
    take_profit: int
    stop_loss: int
    forecast_shift: int
    time_decay_exit: int

    @property
    def total(self) -> int:
        return self.take_profit + self.stop_loss + self.forecast_shift + self.time_decay_exit

    def pct(self, n: int) -> str:
        return f"{n / self.total * 100:.1f}%" if self.total else "n/a"


@dataclass
class ForecastShiftAnalysis:
    count: int
    avg_p_yes_delta: Optional[float]   # mean |Δp_yes| across FORECAST_SHIFT exits
    pct_direction_flipped: Optional[float]  # fraction where YES→BuyNo or NO→BuyYes


@dataclass
class CityBreakdown:
    city: str
    entry_count: int
    net_pnl_usdc: float
    win_rate: float
    avg_edge_bps: float


@dataclass
class MarketTypeBreakdown:
    market_type: str     # "temp_range" | "extreme" | "precip"
    entry_count: int
    net_pnl_usdc: float
    win_rate: float


@dataclass
class ModelBreakdown:
    model: str           # "gfs" | "ecmwf" | "ensemble"
    entry_count: int
    net_pnl_usdc: float
    win_rate: float
    avg_p_yes_at_entry: Optional[float]


@dataclass
class LeadDaysBreakdown:
    bucket: str          # "short (1-3d)" | "medium (4-7d)" | "long (8-14d)"
    entry_count: int
    net_pnl_usdc: float
    win_rate: float


@dataclass
class StrategyBreakdown:
    strategy_id: str
    entry_count: int
    cancel_count: int
    trigger_rate: float
    net_pnl_usdc: float
    sharpe: float
    max_drawdown_pct: float
    win_rate: float


@dataclass
class WeatherBacktestReport:
    # ── Window ────────────────────────────────────────────────────────────────
    days_requested: int
    strategy_filter: Optional[str]
    total_rows: int

    # ── Activity ──────────────────────────────────────────────────────────────
    entry_count: int
    cancel_count: int
    trigger_rate: float     # entries / (entries + cancels)
    fill_rate: float        # (TP + SL + FS) / entries — market-resolved exits

    # ── Hold time ─────────────────────────────────────────────────────────────
    avg_hold_sec: Optional[float]
    max_hold_sec: Optional[int]

    # ── PnL ───────────────────────────────────────────────────────────────────
    net_pnl_usdc: float
    gross_pnl_usdc: float   # net + estimated fees paid

    # ── Risk ──────────────────────────────────────────────────────────────────
    win_rate_overall: float
    max_drawdown_pct: float
    sharpe_ratio: float
    max_consecutive_losses: int

    # ── Rejection breakdown ───────────────────────────────────────────────────
    low_edge_count: int
    low_confidence_count: int
    spread_wide_count: int
    low_depth_count: int
    unknown_type_count: int

    # ── Exit trigger breakdown ────────────────────────────────────────────────
    exits: ExitBreakdown

    # ── FORECAST_SHIFT analysis ───────────────────────────────────────────────
    forecast_shift_analysis: ForecastShiftAnalysis

    # ── Dimensional breakdowns ────────────────────────────────────────────────
    by_strategy: list[StrategyBreakdown]
    by_city: list[CityBreakdown]
    by_market_type: list[MarketTypeBreakdown]
    by_model: list[ModelBreakdown]
    by_lead_days: list[LeadDaysBreakdown]

    # ── Human-readable display ────────────────────────────────────────────────

    def print(self) -> None:
        sep = "=" * 66
        dash = "-" * 66
        print(sep)
        print("  Polymarket Weather Market — Backtest Report")
        print(sep)
        _filt = f"  strategy={self.strategy_filter}" if self.strategy_filter else "  (all strategies)"
        print(f"  Window : last {self.days_requested} day(s){_filt}")
        print(f"  DB rows: {self.total_rows}")
        print(dash)
        print(f"  Entries          : {self.entry_count}")
        print(f"  Cancels          : {self.cancel_count}")
        _tr = f"{self.trigger_rate * 100:.1f}%" if (self.entry_count + self.cancel_count) else "n/a"
        print(f"  Trigger rate     : {_tr}  (entries / signals)")
        _fr = f"{self.fill_rate * 100:.1f}%" if self.entry_count else "n/a"
        print(f"  Fill rate        : {_fr}  (TP+SL+FS / entries)")
        if self.avg_hold_sec is not None:
            h = self.avg_hold_sec
            m = int(h // 60)
            s = int(h % 60)
            print(f"  Avg hold         : {m}m {s}s  (max {self.max_hold_sec}s)")
        print(dash)
        print(f"  Net PnL          : {self.net_pnl_usdc:+.4f} USDC")
        print(f"  Gross PnL        : {self.gross_pnl_usdc:+.4f} USDC  (before fees)")
        _wr = f"{self.win_rate_overall * 100:.1f}%" if self.entry_count else "n/a"
        print(f"  Win rate         : {_wr}")
        print(f"  Max drawdown     : {self.max_drawdown_pct * 100:.2f}%")
        _sharpe = f"{self.sharpe_ratio:.3f}" if self.entry_count >= 2 else "n/a"
        print(f"  Sharpe (ann.)    : {_sharpe}")
        print(f"  Max consec. loss : {self.max_consecutive_losses}")
        print(dash)

        # Exit trigger breakdown
        e = self.exits
        print("  Exit triggers:")
        print(f"    TAKE_PROFIT     : {e.take_profit}  ({e.pct(e.take_profit)})")
        print(f"    STOP_LOSS       : {e.stop_loss}  ({e.pct(e.stop_loss)})")
        print(f"    FORECAST_SHIFT  : {e.forecast_shift}  ({e.pct(e.forecast_shift)})")
        print(f"    TIME_DECAY_EXIT : {e.time_decay_exit}  ({e.pct(e.time_decay_exit)})")

        # FORECAST_SHIFT analysis
        fsa = self.forecast_shift_analysis
        if fsa.count:
            _delta = f"{fsa.avg_p_yes_delta:.3f}" if fsa.avg_p_yes_delta is not None else "n/a"
            _flip  = f"{fsa.pct_direction_flipped * 100:.1f}%" if fsa.pct_direction_flipped is not None else "n/a"
            print(f"  Forecast shift   : avg |Δp_yes|={_delta}  direction_flipped={_flip}")
        print(dash)

        # Rejection codes
        print(f"  Reject — low edge        : {self.low_edge_count}")
        print(f"  Reject — low confidence  : {self.low_confidence_count}")
        print(f"  Reject — spread wide     : {self.spread_wide_count}")
        print(f"  Reject — low depth       : {self.low_depth_count}")
        print(f"  Reject — unknown type    : {self.unknown_type_count}")
        print(dash)

        # Per-strategy
        if len(self.by_strategy) > 1:
            print("  By strategy:")
            for s in sorted(self.by_strategy, key=lambda x: -x.net_pnl_usdc):
                _wr2 = f"{s.win_rate * 100:.0f}%"
                print(f"    {s.strategy_id:<35} entries={s.entry_count:>4}"
                      f"  pnl={s.net_pnl_usdc:+.2f}  wr={_wr2}  sharpe={s.sharpe:.2f}")
            print(dash)

        # Per-city
        if self.by_city:
            print("  By city:")
            for c in sorted(self.by_city, key=lambda x: -x.net_pnl_usdc):
                _wr2 = f"{c.win_rate * 100:.0f}%"
                print(f"    {c.city:<12} entries={c.entry_count:>4}"
                      f"  pnl={c.net_pnl_usdc:+.2f}  wr={_wr2}"
                      f"  avg_edge={c.avg_edge_bps:.0f}bps")
            print(dash)

        # Per-market-type
        if self.by_market_type:
            print("  By market type:")
            for mt in sorted(self.by_market_type, key=lambda x: -x.net_pnl_usdc):
                _wr2 = f"{mt.win_rate * 100:.0f}%"
                print(f"    {mt.market_type:<12} entries={mt.entry_count:>4}"
                      f"  pnl={mt.net_pnl_usdc:+.2f}  wr={_wr2}")
            print(dash)

        # Per-model
        if self.by_model:
            print("  By model:")
            for m in sorted(self.by_model, key=lambda x: -x.net_pnl_usdc):
                _wr2 = f"{m.win_rate * 100:.0f}%"
                _conf = f"{m.avg_p_yes_at_entry:.3f}" if m.avg_p_yes_at_entry is not None else "n/a"
                print(f"    {m.model:<10} entries={m.entry_count:>4}"
                      f"  pnl={m.net_pnl_usdc:+.2f}  wr={_wr2}"
                      f"  avg_p_yes={_conf}")
            print(dash)

        # Per-lead-days bucket
        if self.by_lead_days:
            print("  By lead days:")
            bucket_order = {"short (1-3d)": 0, "medium (4-7d)": 1, "long (8-14d)": 2}
            for ld in sorted(self.by_lead_days, key=lambda x: bucket_order.get(x.bucket, 99)):
                _wr2 = f"{ld.win_rate * 100:.0f}%"
                print(f"    {ld.bucket:<15} entries={ld.entry_count:>4}"
                      f"  pnl={ld.net_pnl_usdc:+.2f}  wr={_wr2}")
            print(dash)

        print(sep)

    def to_json(self, indent: int = 2) -> str:
        return json.dumps(dataclasses.asdict(self), indent=indent)


# ── Engine ────────────────────────────────────────────────────────────────────


def run_weather_backtest(
    db_path: str,
    days: int,
    strategy_id: Optional[str] = None,
) -> WeatherBacktestReport:
    reader = DbReader(db_path)
    rows: list[WeatherDryRunTrade] = reader.get_weather_dry_run_trades(
        days=days,
        strategy_id=strategy_id,
    )

    total_rows = len(rows)

    # ── Partition by action ───────────────────────────────────────────────────
    entries  = [r for r in rows if r.action == "ENTRY"]
    cancels  = [r for r in rows if r.action in ("NO_TRADE", "CANCEL")]
    tp_rows  = [r for r in rows if r.action == "TAKE_PROFIT"]
    sl_rows  = [r for r in rows if r.action == "STOP_LOSS"]
    fs_rows  = [r for r in rows if r.action == "FORECAST_SHIFT"]
    td_rows  = [r for r in rows if r.action == "TIME_DECAY_EXIT"]

    entry_count  = len(entries)
    cancel_count = len(cancels)

    signals      = entry_count + cancel_count
    trigger_rate = entry_count / signals if signals else 0.0

    market_resolved = len(tp_rows) + len(sl_rows) + len(fs_rows)
    fill_rate = market_resolved / entry_count if entry_count else 0.0

    # ── Hold time ─────────────────────────────────────────────────────────────
    exit_rows = tp_rows + sl_rows + fs_rows + td_rows
    hold_secs = [r.hold_sec for r in exit_rows if r.hold_sec is not None]
    avg_hold_sec = sum(hold_secs) / len(hold_secs) if hold_secs else None
    max_hold_sec = max(hold_secs) if hold_secs else None

    # ── PnL series (exit rows only) ───────────────────────────────────────────
    pnl_series = [
        r.realized_pnl_usdc for r in exit_rows if r.realized_pnl_usdc is not None
    ]
    net_pnl = sum(pnl_series)

    total_fees = _estimate_fees(entries)
    gross_pnl  = net_pnl + total_fees

    # ── Risk metrics ──────────────────────────────────────────────────────────
    mdd       = max_drawdown_pct(pnl_series)
    sharpe    = sharpe_ratio(pnl_series)
    wr        = win_rate(pnl_series)
    max_consec = _max_consecutive_losses(pnl_series)

    # ── Rejection breakdown ───────────────────────────────────────────────────
    low_edge        = sum(1 for r in rows if r.reason_code in ("LOW_EDGE", "EDGE_TOO_LOW"))
    low_confidence  = sum(1 for r in rows if r.reason_code == "LOW_CONFIDENCE")
    spread_wide     = sum(1 for r in rows if r.reason_code in ("SPREAD_WIDE", "SPREAD_TOO_WIDE"))
    low_depth       = sum(1 for r in rows if r.reason_code == "LOW_DEPTH")
    unknown_type    = sum(1 for r in rows if r.reason_code == "UNKNOWN_TYPE")

    # ── Exit trigger breakdown ────────────────────────────────────────────────
    exits = ExitBreakdown(
        take_profit=len(tp_rows),
        stop_loss=len(sl_rows),
        forecast_shift=len(fs_rows),
        time_decay_exit=len(td_rows),
    )

    # ── FORECAST_SHIFT analysis ───────────────────────────────────────────────
    fs_analysis = _forecast_shift_analysis(fs_rows)

    # ── Per-strategy breakdown ────────────────────────────────────────────────
    strategy_ids = sorted({r.strategy_id for r in rows})
    by_strategy  = [_strategy_breakdown(sid, entries, cancels, exit_rows) for sid in strategy_ids]

    # ── Per-city breakdown ────────────────────────────────────────────────────
    cities   = sorted({r.city for r in entries if r.city})
    by_city  = [_city_breakdown(city, entries, exit_rows) for city in cities]

    # ── Per-market-type breakdown ─────────────────────────────────────────────
    mkt_types      = sorted({r.market_type for r in entries if r.market_type})
    by_market_type = [_market_type_breakdown(mt, entries, exit_rows) for mt in mkt_types]

    # ── Per-model breakdown ───────────────────────────────────────────────────
    models    = sorted({r.model for r in entries if r.model})
    by_model  = [_model_breakdown(m, entries, exit_rows) for m in models]

    # ── Per-lead-days bucket breakdown ────────────────────────────────────────
    by_lead_days = _lead_days_breakdown(entries, exit_rows)

    return WeatherBacktestReport(
        days_requested=days,
        strategy_filter=strategy_id,
        total_rows=total_rows,
        entry_count=entry_count,
        cancel_count=cancel_count,
        trigger_rate=trigger_rate,
        fill_rate=fill_rate,
        avg_hold_sec=avg_hold_sec,
        max_hold_sec=max_hold_sec,
        net_pnl_usdc=net_pnl,
        gross_pnl_usdc=gross_pnl,
        win_rate_overall=wr,
        max_drawdown_pct=mdd,
        sharpe_ratio=sharpe,
        max_consecutive_losses=max_consec,
        low_edge_count=low_edge,
        low_confidence_count=low_confidence,
        spread_wide_count=spread_wide,
        low_depth_count=low_depth,
        unknown_type_count=unknown_type,
        exits=exits,
        forecast_shift_analysis=fs_analysis,
        by_strategy=by_strategy,
        by_city=by_city,
        by_market_type=by_market_type,
        by_model=by_model,
        by_lead_days=by_lead_days,
    )


# ── Dimension-breakdown helpers ───────────────────────────────────────────────


def _strategy_breakdown(
    sid: str,
    entries: list[WeatherDryRunTrade],
    cancels: list[WeatherDryRunTrade],
    exit_rows: list[WeatherDryRunTrade],
) -> StrategyBreakdown:
    s_entries = [r for r in entries if r.strategy_id == sid]
    s_cancels = [r for r in cancels if r.strategy_id == sid]
    s_pnl     = [r.realized_pnl_usdc for r in exit_rows
                 if r.strategy_id == sid and r.realized_pnl_usdc is not None]
    sigs = len(s_entries) + len(s_cancels)
    return StrategyBreakdown(
        strategy_id=sid,
        entry_count=len(s_entries),
        cancel_count=len(s_cancels),
        trigger_rate=len(s_entries) / sigs if sigs else 0.0,
        net_pnl_usdc=sum(s_pnl),
        sharpe=sharpe_ratio(s_pnl),
        max_drawdown_pct=max_drawdown_pct(s_pnl),
        win_rate=win_rate(s_pnl),
    )


def _city_breakdown(
    city: str,
    entries: list[WeatherDryRunTrade],
    exit_rows: list[WeatherDryRunTrade],
) -> CityBreakdown:
    c_entries = [r for r in entries if r.city == city]
    c_pnl     = [r.realized_pnl_usdc for r in exit_rows
                 if r.city == city and r.realized_pnl_usdc is not None]
    edges = [r.expected_net_edge_bps for r in c_entries if r.expected_net_edge_bps is not None]
    avg_edge = sum(edges) / len(edges) if edges else 0.0
    return CityBreakdown(
        city=city,
        entry_count=len(c_entries),
        net_pnl_usdc=sum(c_pnl),
        win_rate=win_rate(c_pnl),
        avg_edge_bps=avg_edge,
    )


def _market_type_breakdown(
    market_type: str,
    entries: list[WeatherDryRunTrade],
    exit_rows: list[WeatherDryRunTrade],
) -> MarketTypeBreakdown:
    m_entries = [r for r in entries if r.market_type == market_type]
    m_pnl     = [r.realized_pnl_usdc for r in exit_rows
                 if r.market_type == market_type and r.realized_pnl_usdc is not None]
    return MarketTypeBreakdown(
        market_type=market_type,
        entry_count=len(m_entries),
        net_pnl_usdc=sum(m_pnl),
        win_rate=win_rate(m_pnl),
    )


def _model_breakdown(
    model: str,
    entries: list[WeatherDryRunTrade],
    exit_rows: list[WeatherDryRunTrade],
) -> ModelBreakdown:
    m_entries = [r for r in entries if r.model == model]
    m_pnl     = [r.realized_pnl_usdc for r in exit_rows
                 if r.model == model and r.realized_pnl_usdc is not None]
    confs = [r.p_yes_at_entry for r in m_entries if r.p_yes_at_entry is not None]
    avg_conf = sum(confs) / len(confs) if confs else None
    return ModelBreakdown(
        model=model,
        entry_count=len(m_entries),
        net_pnl_usdc=sum(m_pnl),
        win_rate=win_rate(m_pnl),
        avg_p_yes_at_entry=avg_conf,
    )


def _lead_days_breakdown(
    entries: list[WeatherDryRunTrade],
    exit_rows: list[WeatherDryRunTrade],
) -> list[LeadDaysBreakdown]:
    def bucket(lead: Optional[int]) -> str:
        if lead is None:
            return "unknown"
        if lead <= 3:
            return "short (1-3d)"
        if lead <= 7:
            return "medium (4-7d)"
        return "long (8-14d)"

    buckets: dict[str, list[float]] = {}
    bucket_entry_count: dict[str, int] = {}

    for r in entries:
        b = bucket(r.lead_days)
        bucket_entry_count[b] = bucket_entry_count.get(b, 0) + 1

    for r in exit_rows:
        if r.realized_pnl_usdc is None:
            continue
        b = bucket(r.lead_days)
        buckets.setdefault(b, []).append(r.realized_pnl_usdc)

    result = []
    for b in sorted(bucket_entry_count.keys()):
        pnl = buckets.get(b, [])
        result.append(LeadDaysBreakdown(
            bucket=b,
            entry_count=bucket_entry_count[b],
            net_pnl_usdc=sum(pnl),
            win_rate=win_rate(pnl),
        ))
    return result


def _forecast_shift_analysis(
    fs_rows: list[WeatherDryRunTrade],
) -> ForecastShiftAnalysis:
    if not fs_rows:
        return ForecastShiftAnalysis(count=0, avg_p_yes_delta=None, pct_direction_flipped=None)

    deltas = [
        abs(r.p_yes_at_exit - r.p_yes_at_entry)
        for r in fs_rows
        if r.p_yes_at_entry is not None and r.p_yes_at_exit is not None
    ]
    avg_delta = sum(deltas) / len(deltas) if deltas else None

    # A direction flip means: we held YES (p_yes_at_entry > 0.5) but new p_yes < 0.5,
    # or we held NO (p_yes_at_entry < 0.5) but new p_yes > 0.5.
    flips = sum(
        1 for r in fs_rows
        if r.p_yes_at_entry is not None and r.p_yes_at_exit is not None
        and (
            (r.p_yes_at_entry > 0.5 and r.p_yes_at_exit < 0.5)
            or (r.p_yes_at_entry < 0.5 and r.p_yes_at_exit > 0.5)
        )
    )
    countable = sum(
        1 for r in fs_rows
        if r.p_yes_at_entry is not None and r.p_yes_at_exit is not None
    )
    pct_flipped = flips / countable if countable else None

    return ForecastShiftAnalysis(
        count=len(fs_rows),
        avg_p_yes_delta=avg_delta,
        pct_direction_flipped=pct_flipped,
    )


# ── PnL helpers ───────────────────────────────────────────────────────────────


def _estimate_fees(entries: list[WeatherDryRunTrade]) -> float:
    """Reconstruct total fees paid from bps fields on ENTRY rows."""
    total = 0.0
    for r in entries:
        taker = (r.taker_fee_bps or 0) / 10_000
        slip  = (r.slippage_buffer_bps or 0) / 10_000
        total += r.size_usdc * (taker + slip)
    return total


def _max_consecutive_losses(pnl_series: list[float]) -> int:
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
    _repo_root = os.path.normpath(os.path.join(_HERE, "../../.."))
    return os.path.join(_repo_root, "rust-engine", "data", "market_snapshots.db")


def main() -> None:
    parser = argparse.ArgumentParser(
        description="Weather Market Strategy backtest report"
    )
    parser.add_argument(
        "--days", type=int, default=14,
        help="Number of past days to include (default: 14)",
    )
    parser.add_argument(
        "--strategy", type=str, default=None,
        help="Filter to a single strategy_id (default: all)",
    )
    parser.add_argument(
        "--db", type=str, default=None,
        help="Path to SQLite DB (default: rust-engine/data/market_snapshots.db)",
    )
    parser.add_argument(
        "--json", action="store_true",
        help="Output machine-readable JSON (for dashboard consumption)",
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

    report = run_weather_backtest(
        db_path=db_path,
        days=args.days,
        strategy_id=args.strategy,
    )

    if args.json:
        print(report.to_json())
    else:
        report.print()


if __name__ == "__main__":
    main()
