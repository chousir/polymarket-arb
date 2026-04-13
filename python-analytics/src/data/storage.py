# SQLite 讀取 — dry_run_trades / cycle_results / mention_dry_run_trades / weather_dry_run_trades

from __future__ import annotations

import sqlite3
from dataclasses import dataclass
from typing import Optional


@dataclass
class DryRunTrade:
    id: int
    ts: str
    strategy_id: str
    market_slug: str
    leg: int
    side: str
    price: float
    size_usdc: float
    fee_usdc: float
    signal_dump_pct: Optional[float]
    hedge_sum: Optional[float]
    would_profit: Optional[float]


@dataclass
class MentionDryRunTrade:
    id: int
    ts: str
    strategy_id: str
    event_id: str
    market_slug: str
    speaker: str
    keyword: str
    side: str               # "YES" / "NO"
    action: str             # ENTRY / TAKE_PROFIT / STOP_LOSS / TIME_EXIT / NO_TRADE
    price: float
    size_usdc: float
    spread_at_decision: Optional[float]
    depth_usdc_at_decision: Optional[float]
    entry_price: Optional[float]
    exit_price: Optional[float]
    hold_sec: Optional[int]
    taker_fee_bps: Optional[int]
    slippage_buffer_bps: Optional[int]
    execution_risk_bps: Optional[int]
    expected_net_edge_bps: Optional[float]
    realized_pnl_usdc: Optional[float]
    reason_code: str        # EDGE_OK / EDGE_TOO_LOW / SPREAD_TOO_WIDE / DEPTH_TOO_THIN / TIME_EXIT
    note: Optional[str]


@dataclass
class WeatherDryRunTrade:
    id: int
    ts: str
    strategy_id: str
    event_id: str
    market_slug: str
    city: str
    market_type: str          # "temp_range" | "extreme" | "precip"
    side: str                 # "YES" | "NO" | "NONE"
    action: str               # ENTRY | TAKE_PROFIT | STOP_LOSS | FORECAST_SHIFT | TIME_DECAY_EXIT | NO_TRADE
    price: float
    size_usdc: float
    spread_at_decision: Optional[float]
    depth_usdc_at_decision: Optional[float]
    entry_price: Optional[float]
    exit_price: Optional[float]
    hold_sec: Optional[int]
    model: str                # "gfs" | "ecmwf" | "ensemble"
    p_yes_at_entry: Optional[float]
    p_yes_at_exit: Optional[float]
    lead_days: Optional[int]
    taker_fee_bps: Optional[int]
    slippage_buffer_bps: Optional[int]
    expected_net_edge_bps: Optional[float]
    realized_pnl_usdc: Optional[float]
    reason_code: str
    note: Optional[str]


@dataclass
class CycleResult:
    id: int
    strategy_id: str
    market_slug: str
    mode: str
    leg1_side: Optional[str]
    leg1_price: Optional[float]
    leg2_price: Optional[float]
    resolved_winner: Optional[str]
    pnl_usdc: Optional[float]
    created_at: str


class DbReader:
    """Thin synchronous SQLite reader for analytics / backtest use."""

    def __init__(self, db_path: str) -> None:
        self._path = db_path

    def _connect(self) -> sqlite3.Connection:
        conn = sqlite3.connect(self._path)
        conn.row_factory = sqlite3.Row
        return conn

    # ── Trades ────────────────────────────────────────────────────────────────

    def get_dry_run_trades(
        self,
        days: Optional[int] = None,
        strategy_id: Optional[str] = None,
    ) -> list[DryRunTrade]:
        """Return dry_run_trades, optionally filtered by time window and/or strategy."""
        conditions = []
        params: list = []

        if days is not None:
            conditions.append("ts >= datetime('now', ?)")
            params.append(f"-{days} days")
        if strategy_id is not None:
            conditions.append("strategy_id = ?")
            params.append(strategy_id)

        where = ("WHERE " + " AND ".join(conditions)) if conditions else ""
        sql = f"SELECT * FROM dry_run_trades {where} ORDER BY ts"

        with self._connect() as conn:
            rows = conn.execute(sql, params).fetchall()
        return [_row_to_trade(r) for r in rows]

    # ── Mention trades ────────────────────────────────────────────────────────

    def get_mention_dry_run_trades(
        self,
        days: Optional[int] = None,
        strategy_id: Optional[str] = None,
        action: Optional[str] = None,
    ) -> "list[MentionDryRunTrade]":
        """Return mention_dry_run_trades, optionally filtered by time / strategy / action."""
        conditions = []
        params: list = []

        if days is not None:
            conditions.append("ts >= datetime('now', ?)")
            params.append(f"-{days} days")
        if strategy_id is not None:
            conditions.append("strategy_id = ?")
            params.append(strategy_id)
        if action is not None:
            conditions.append("action = ?")
            params.append(action)

        where = ("WHERE " + " AND ".join(conditions)) if conditions else ""
        sql = f"SELECT * FROM mention_dry_run_trades {where} ORDER BY ts"

        with self._connect() as conn:
            rows = conn.execute(sql, params).fetchall()
        return [_row_to_mention_trade(r) for r in rows]

    # ── Weather trades ────────────────────────────────────────────────────────

    def get_weather_dry_run_trades(
        self,
        days: Optional[int] = None,
        strategy_id: Optional[str] = None,
        action: Optional[str] = None,
        city: Optional[str] = None,
    ) -> "list[WeatherDryRunTrade]":
        """Return weather_dry_run_trades, optionally filtered."""
        conditions = []
        params: list = []

        if days is not None:
            conditions.append("ts >= datetime('now', ?)")
            params.append(f"-{days} days")
        if strategy_id is not None:
            conditions.append("strategy_id = ?")
            params.append(strategy_id)
        if action is not None:
            conditions.append("action = ?")
            params.append(action)
        if city is not None:
            conditions.append("city = ?")
            params.append(city)

        where = ("WHERE " + " AND ".join(conditions)) if conditions else ""
        sql = f"SELECT * FROM weather_dry_run_trades {where} ORDER BY ts"

        with self._connect() as conn:
            rows = conn.execute(sql, params).fetchall()
        return [_row_to_weather_trade(r) for r in rows]

    # ── Cycles ────────────────────────────────────────────────────────────────

    def get_cycle_results(
        self,
        days: Optional[int] = None,
        strategy_id: Optional[str] = None,
    ) -> list[CycleResult]:
        """Return cycle_results, optionally filtered by time window and/or strategy."""
        conditions = []
        params: list = []

        if days is not None:
            conditions.append("created_at >= datetime('now', ?)")
            params.append(f"-{days} days")
        if strategy_id is not None:
            conditions.append("strategy_id = ?")
            params.append(strategy_id)

        where = ("WHERE " + " AND ".join(conditions)) if conditions else ""
        sql = f"SELECT * FROM cycle_results {where} ORDER BY created_at"

        with self._connect() as conn:
            rows = conn.execute(sql, params).fetchall()
        return [_row_to_cycle(r) for r in rows]

    # ── Strategy index ────────────────────────────────────────────────────────

    def get_strategy_ids(self) -> list[str]:
        """Return distinct strategy_id values seen in cycle_results and dry_run_trades."""
        with self._connect() as conn:
            rows = conn.execute(
                "SELECT DISTINCT strategy_id FROM cycle_results "
                "UNION "
                "SELECT DISTINCT strategy_id FROM dry_run_trades "
                "ORDER BY strategy_id"
            ).fetchall()
        return [r[0] for r in rows]

    # ── Meta ──────────────────────────────────────────────────────────────────

    def earliest_trade_ts(self) -> Optional[str]:
        """Timestamp of the oldest dry_run_trade row, or None if table is empty."""
        with self._connect() as conn:
            row = conn.execute("SELECT MIN(ts) FROM dry_run_trades").fetchone()
        return row[0] if row else None

    def data_span_days(self) -> float:
        """Approximate span (in days) between earliest and latest trade timestamps."""
        with self._connect() as conn:
            row = conn.execute(
                "SELECT MIN(ts), MAX(ts) FROM dry_run_trades"
            ).fetchone()
        if not row or row[0] is None:
            return 0.0
        from datetime import datetime

        fmt = "%Y-%m-%d %H:%M:%S"
        try:
            t0 = datetime.strptime(row[0][:19], fmt)
            t1 = datetime.strptime(row[1][:19], fmt)
            return (t1 - t0).total_seconds() / 86_400
        except ValueError:
            return 0.0


# ── Helpers ───────────────────────────────────────────────────────────────────


def _row_to_weather_trade(r: sqlite3.Row) -> WeatherDryRunTrade:
    def _f(key: str) -> Optional[float]:
        v = r[key]
        return float(v) if v is not None else None

    def _i(key: str) -> Optional[int]:
        v = r[key]
        return int(v) if v is not None else None

    return WeatherDryRunTrade(
        id=r["id"],
        ts=r["ts"] or "",
        strategy_id=r["strategy_id"],
        event_id=r["event_id"],
        market_slug=r["market_slug"],
        city=r["city"] or "",
        market_type=r["market_type"] or "",
        side=r["side"],
        action=r["action"],
        price=float(r["price"]),
        size_usdc=float(r["size_usdc"]),
        spread_at_decision=_f("spread_at_decision"),
        depth_usdc_at_decision=_f("depth_usdc_at_decision"),
        entry_price=_f("entry_price"),
        exit_price=_f("exit_price"),
        hold_sec=_i("hold_sec"),
        model=r["model"] or "gfs",
        p_yes_at_entry=_f("p_yes_at_entry"),
        p_yes_at_exit=_f("p_yes_at_exit"),
        lead_days=_i("lead_days"),
        taker_fee_bps=_i("taker_fee_bps"),
        slippage_buffer_bps=_i("slippage_buffer_bps"),
        expected_net_edge_bps=_f("expected_net_edge_bps"),
        realized_pnl_usdc=_f("realized_pnl_usdc"),
        reason_code=r["reason_code"] or "",
        note=r["note"],
    )


def _row_to_mention_trade(r: sqlite3.Row) -> MentionDryRunTrade:
    def _f(key: str) -> Optional[float]:
        v = r[key]
        return float(v) if v is not None else None

    def _i(key: str) -> Optional[int]:
        v = r[key]
        return int(v) if v is not None else None

    return MentionDryRunTrade(
        id=r["id"],
        ts=r["ts"] or "",
        strategy_id=r["strategy_id"],
        event_id=r["event_id"],
        market_slug=r["market_slug"],
        speaker=r["speaker"] or "trump",
        keyword=r["keyword"],
        side=r["side"],
        action=r["action"],
        price=float(r["price"]),
        size_usdc=float(r["size_usdc"]),
        spread_at_decision=_f("spread_at_decision"),
        depth_usdc_at_decision=_f("depth_usdc_at_decision"),
        entry_price=_f("entry_price"),
        exit_price=_f("exit_price"),
        hold_sec=_i("hold_sec"),
        taker_fee_bps=_i("taker_fee_bps"),
        slippage_buffer_bps=_i("slippage_buffer_bps"),
        execution_risk_bps=_i("execution_risk_bps"),
        expected_net_edge_bps=_f("expected_net_edge_bps"),
        realized_pnl_usdc=_f("realized_pnl_usdc"),
        reason_code=r["reason_code"] or "",
        note=r["note"],
    )


def _row_to_trade(r: sqlite3.Row) -> DryRunTrade:
    return DryRunTrade(
        id=r["id"],
        ts=r["ts"] or "",
        strategy_id=r["strategy_id"] or "default",
        market_slug=r["market_slug"],
        leg=r["leg"],
        side=r["side"],
        price=float(r["price"]),
        size_usdc=float(r["size_usdc"]),
        fee_usdc=float(r["fee_usdc"]) if r["fee_usdc"] is not None else 0.0,
        signal_dump_pct=float(r["signal_dump_pct"]) if r["signal_dump_pct"] is not None else None,
        hedge_sum=float(r["hedge_sum"]) if r["hedge_sum"] is not None else None,
        would_profit=float(r["would_profit"]) if r["would_profit"] is not None else None,
    )


def _row_to_cycle(r: sqlite3.Row) -> CycleResult:
    return CycleResult(
        id=r["id"],
        strategy_id=r["strategy_id"] or "default",
        market_slug=r["market_slug"],
        mode=r["mode"],
        leg1_side=r["leg1_side"],
        leg1_price=float(r["leg1_price"]) if r["leg1_price"] is not None else None,
        leg2_price=float(r["leg2_price"]) if r["leg2_price"] is not None else None,
        resolved_winner=r["resolved_winner"],
        pnl_usdc=float(r["pnl_usdc"]) if r["pnl_usdc"] is not None else None,
        created_at=r["created_at"] or "",
    )
