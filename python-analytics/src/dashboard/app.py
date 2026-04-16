# FastAPI 後端
#
# Start:
#   cd python-analytics && uvicorn src.dashboard.app:app --reload --port 8080

from __future__ import annotations

import asyncio
import json
import os
import sys
import tomllib
from collections import defaultdict
from datetime import datetime, timezone
from typing import Optional

# ── Path bootstrap ────────────────────────────────────────────────────────────
_HERE = os.path.dirname(os.path.abspath(__file__))           # …/dashboard/
_ANALYTICS_ROOT = os.path.normpath(os.path.join(_HERE, "../.."))  # …/python-analytics/
_REPO_ROOT = os.path.normpath(os.path.join(_HERE, "../../.."))    # …/polymarket-arb/
if _ANALYTICS_ROOT not in sys.path:
    sys.path.insert(0, _ANALYTICS_ROOT)

from fastapi import FastAPI, WebSocket, WebSocketDisconnect
from fastapi.middleware.cors import CORSMiddleware
from fastapi.responses import JSONResponse
from fastapi.staticfiles import StaticFiles

import dataclasses

from src.data.storage import CycleResult, DbReader, DryRunTrade, MentionDryRunTrade, WeatherDryRunTrade, WeatherLadderTrade
from src.backtest.metrics import win_rate
from src.backtest.mention_report import run_mention_backtest
from src.backtest.weather_report import run_weather_backtest

# ── App setup ─────────────────────────────────────────────────────────────────

app = FastAPI(title="Polymarket Arb Dashboard", version="0.2.0")

app.add_middleware(
    CORSMiddleware,
    allow_origins=["*"],
    allow_methods=["*"],
    allow_headers=["*"],
)

# ── DB path ───────────────────────────────────────────────────────────────────


def _db_path() -> str:
    _this_file = os.path.abspath(__file__)
    _repo = os.path.normpath(os.path.join(os.path.dirname(_this_file), "../../.."))
    default = os.path.join(_repo, "rust-engine", "data", "market_snapshots.db")
    raw = os.environ.get("DB_PATH", "")
    if not raw:
        return default
    p = raw if os.path.isabs(raw) else os.path.join(_repo, raw)
    return os.path.normpath(p)


def _reader() -> DbReader:
    return DbReader(_db_path())


def _settings_path() -> str:
    return os.path.join(_REPO_ROOT, "config", "settings.toml")


def _load_strategy_capital_config() -> dict[str, dict]:
    """Return strategy capital metadata from config/settings.toml.

    Shape:
    {
      strategy_id: {
        "capital_allocation_pct": float,
        "initial_allocated_usdc": float,
      }
    }
    """
    path = _settings_path()
    if not os.path.exists(path):
        return {}

    with open(path, "rb") as f:
        raw = tomllib.load(f)

    initial_capital = float(raw.get("capital", {}).get("initial_capital_usdc", 0.0) or 0.0)
    result: dict[str, dict] = {}
    for s in raw.get("strategies", []):
        sid = s.get("id")
        if not sid:
            continue
        alloc = float(s.get("capital_allocation_pct", 0.0) or 0.0)
        result[sid] = {
            "capital_allocation_pct": alloc,
            "initial_allocated_usdc": initial_capital * alloc,
        }
    return result


def _load_weather_strategy_ids() -> list[str]:
    """Return IDs of all enabled weather strategies from settings.toml."""
    path = _settings_path()
    if not os.path.exists(path):
        return []
    with open(path, "rb") as f:
        raw = tomllib.load(f)
    return [
        s["id"]
        for s in raw.get("strategies", [])
        if s.get("type") == "weather" and s.get("enabled", False) and s.get("id")
    ]


def _load_weather_ladder_strategy_ids() -> list[str]:
    """Return IDs of all enabled weather_ladder strategies from settings.toml."""
    path = _settings_path()
    if not os.path.exists(path):
        return []
    with open(path, "rb") as f:
        raw = tomllib.load(f)
    return [
        s["id"]
        for s in raw.get("strategies", [])
        if s.get("type") == "weather_ladder" and s.get("enabled", False) and s.get("id")
    ]


# ── Stat helpers ──────────────────────────────────────────────────────────────


def _build_stats(days: int = 1, strategy_id: Optional[str] = None) -> dict:
    """Summary for a given strategy (or all strategies if strategy_id is None)."""
    r = _reader()
    cycles = r.get_cycle_results(days=days, strategy_id=strategy_id)
    triggered = [c for c in cycles if c.leg1_price is not None]
    resolved = [c for c in triggered if c.pnl_usdc is not None]
    pnl_series = [c.pnl_usdc for c in resolved]  # type: ignore[misc]
    wr = win_rate(pnl_series) if pnl_series else 0.0

    trades = r.get_dry_run_trades(days=days, strategy_id=strategy_id)
    total_invested = sum(t.size_usdc for t in trades)
    total_fees = sum(t.fee_usdc for t in trades)

    return {
        "strategy_id": strategy_id or "all",
        "total_cycles": len(cycles),
        "triggered_cycles": len(triggered),
        "trigger_rate": len(triggered) / len(cycles) if cycles else 0.0,
        "win_rate": wr,
        "total_pnl_usdc": sum(pnl_series),
        "total_invested_usdc": total_invested,
        "total_fees_usdc": total_fees,
        "as_of": datetime.now(timezone.utc).isoformat(),
    }


def _build_strategy_table(days: int = 7) -> list[dict]:
    """Per-strategy summary table for the dashboard overview."""
    r = _reader()
    cycles = r.get_cycle_results(days=days)
    trades = r.get_dry_run_trades(days=days)
    cap_cfg = _load_strategy_capital_config()

    by_strategy_cycles: dict[str, list[CycleResult]] = defaultdict(list)
    by_strategy_trades: dict[str, list[DryRunTrade]] = defaultdict(list)

    for c in cycles:
        by_strategy_cycles[c.strategy_id].append(c)
    for t in trades:
        by_strategy_trades[t.strategy_id].append(t)

    all_ids = sorted(set(by_strategy_cycles) | set(by_strategy_trades))
    result = []
    for sid in all_ids:
        scycles = by_strategy_cycles[sid]
        strades = by_strategy_trades[sid]
        triggered = [c for c in scycles if c.leg1_price is not None]
        resolved = [c for c in triggered if c.pnl_usdc is not None]
        pnl_series = [c.pnl_usdc for c in resolved]  # type: ignore[misc]
        total_invested = sum(t.size_usdc for t in strades)
        total_fees = sum(t.fee_usdc for t in strades)
        total_pnl = sum(pnl_series)
        cfg = cap_cfg.get(sid, {})
        initial_allocated = float(cfg.get("initial_allocated_usdc", 0.0) or 0.0)
        wr = win_rate(pnl_series) if pnl_series else 0.0
        pnl_pct_alloc = (total_pnl / initial_allocated) if initial_allocated > 0 else 0.0
        pnl_pct_invested = (total_pnl / total_invested) if total_invested > 0 else 0.0
        result.append({
            "strategy_id": sid,
            "total_cycles": len(scycles),
            "triggered_cycles": len(triggered),
            "total_trades": len(strades),
            "trigger_rate": len(triggered) / len(scycles) if scycles else 0.0,
            "win_rate": wr,
            "total_pnl_usdc": total_pnl,
            "total_invested_usdc": total_invested,
            "total_fees_usdc": total_fees,
            "capital_allocation_pct": float(cfg.get("capital_allocation_pct", 0.0) or 0.0),
            "initial_allocated_usdc": initial_allocated,
            "pnl_pct_alloc": pnl_pct_alloc,
            "pnl_pct_invested": pnl_pct_invested,
        })
    return sorted(result, key=lambda x: x["total_pnl_usdc"], reverse=True)


def _build_strategy_detail(strategy_id: str, days: int = 30) -> dict:
    """Detailed strategy view used by frontend drill-down panel."""
    r = _reader()
    cap_cfg = _load_strategy_capital_config().get(strategy_id, {})

    cycles = r.get_cycle_results(days=days, strategy_id=strategy_id)
    trades = r.get_dry_run_trades(days=days, strategy_id=strategy_id)

    triggered = [c for c in cycles if c.leg1_price is not None]
    resolved = [c for c in triggered if c.pnl_usdc is not None]
    pnl_series = [c.pnl_usdc for c in resolved]  # type: ignore[misc]

    total_pnl = sum(pnl_series)
    total_invested = sum(t.size_usdc for t in trades)
    total_fees = sum(t.fee_usdc for t in trades)
    wr = win_rate(pnl_series) if pnl_series else 0.0

    initial_allocated = float(cap_cfg.get("initial_allocated_usdc", 0.0) or 0.0)
    pnl_pct_alloc = (total_pnl / initial_allocated) if initial_allocated > 0 else 0.0
    pnl_pct_invested = (total_pnl / total_invested) if total_invested > 0 else 0.0

    leg1_prices = [c.leg1_price for c in triggered if c.leg1_price is not None]
    leg2_prices = [c.leg2_price for c in triggered if c.leg2_price is not None]

    recent_cycles = sorted(cycles, key=lambda c: c.created_at or "", reverse=True)[:5]
    recent_trades = sorted(trades, key=lambda t: t.ts or "", reverse=True)[:20]

    return {
        "strategy_id": strategy_id,
        "days": days,
        "capital_allocation_pct": float(cap_cfg.get("capital_allocation_pct", 0.0) or 0.0),
        "initial_allocated_usdc": initial_allocated,
        "total_cycles": len(cycles),
        "triggered_cycles": len(triggered),
        "total_trades": len(trades),
        "win_rate": wr,
        "total_pnl_usdc": total_pnl,
        "total_invested_usdc": total_invested,
        "total_fees_usdc": total_fees,
        "pnl_pct_alloc": pnl_pct_alloc,
        "pnl_pct_invested": pnl_pct_invested,
        "avg_leg1_price": (sum(leg1_prices) / len(leg1_prices)) if leg1_prices else None,
        "avg_leg2_price": (sum(leg2_prices) / len(leg2_prices)) if leg2_prices else None,
        "recent_cycles": [
            {
                "id": c.id,
                "market_slug": c.market_slug,
                "leg1_price": c.leg1_price,
                "leg2_price": c.leg2_price,
                "pnl_usdc": c.pnl_usdc,
                "created_at": c.created_at,
            }
            for c in recent_cycles
        ],
        "recent_trades": [
            {
                "id": t.id,
                "market_slug": t.market_slug,
                "leg": t.leg,
                "side": t.side,
                "price": t.price,
                "size_usdc": t.size_usdc,
                "fee_usdc": t.fee_usdc,
                "would_profit": t.would_profit,
                "ts": t.ts,
            }
            for t in recent_trades
        ],
    }


def _build_mention_stats(days: int = 7) -> dict:
    """Aggregate stats for all mention strategies (from mention_dry_run_trades)."""
    try:
        report = run_mention_backtest(db_path=_db_path(), days=days)
        return {
            "days": days,
            "total_rows": report.total_rows,
            "entry_count": report.entry_count,
            "cancel_count": report.cancel_count,
            "trigger_rate": report.trigger_rate,
            "fill_rate": report.fill_rate,
            "avg_hold_sec": report.avg_hold_sec,
            "max_hold_sec": report.max_hold_sec,
            "net_pnl_usdc": report.net_pnl_usdc,
            "gross_pnl_usdc": report.gross_pnl_usdc,
            "max_drawdown_pct": report.max_drawdown_pct,
            "sharpe_ratio": report.sharpe_ratio,
            "max_consecutive_losses": report.max_consecutive_losses,
            "edge_too_low_count": report.edge_too_low_count,
            "spread_too_wide_count": report.spread_too_wide_count,
            "depth_too_thin_count": report.depth_too_thin_count,
            "as_of": datetime.now(timezone.utc).isoformat(),
        }
    except Exception:
        return {
            "days": days, "total_rows": 0, "entry_count": 0, "cancel_count": 0,
            "trigger_rate": 0.0, "fill_rate": 0.0, "avg_hold_sec": None,
            "max_hold_sec": None, "net_pnl_usdc": 0.0, "gross_pnl_usdc": 0.0,
            "max_drawdown_pct": 0.0, "sharpe_ratio": 0.0,
            "max_consecutive_losses": 0, "edge_too_low_count": 0,
            "spread_too_wide_count": 0, "depth_too_thin_count": 0,
            "as_of": datetime.now(timezone.utc).isoformat(),
        }


def _build_mention_strategy_breakdown(days: int = 7) -> list[dict]:
    """Per-mention-strategy summary for the dashboard."""
    try:
        r = _reader()
        rows = r.get_mention_dry_run_trades(days=days)
    except Exception:
        return []

    cap_cfg = _load_strategy_capital_config()
    by_sid: dict[str, list[MentionDryRunTrade]] = defaultdict(list)
    for row in rows:
        by_sid[row.strategy_id].append(row)

    result = []
    for sid, trades in by_sid.items():
        entries  = [t for t in trades if t.action == "ENTRY"]
        cancels  = [t for t in trades if t.action in ("NO_TRADE", "CANCEL")]
        tp_rows  = [t for t in trades if t.action == "TAKE_PROFIT"]
        sl_rows  = [t for t in trades if t.action == "STOP_LOSS"]
        te_rows  = [t for t in trades if t.action == "TIME_EXIT"]
        exit_rows = tp_rows + sl_rows + te_rows

        signals = len(entries) + len(cancels)
        trigger_rate = len(entries) / signals if signals else 0.0
        fill_rate = (len(tp_rows) + len(sl_rows)) / len(entries) if entries else 0.0

        pnl_series = [
            t.realized_pnl_usdc for t in exit_rows if t.realized_pnl_usdc is not None
        ]
        net_pnl = sum(pnl_series)

        active_keywords = sorted({t.keyword for t in entries})
        active_markets  = sorted({t.market_slug for t in entries})

        cfg = cap_cfg.get(sid, {})
        initial_allocated = float(cfg.get("initial_allocated_usdc", 0.0) or 0.0)
        pnl_pct = (net_pnl / initial_allocated) if initial_allocated > 0 else 0.0

        result.append({
            "strategy_id": sid,
            "entry_count": len(entries),
            "cancel_count": len(cancels),
            "trigger_rate": trigger_rate,
            "fill_rate": fill_rate,
            "net_pnl_usdc": net_pnl,
            "capital_allocation_pct": float(cfg.get("capital_allocation_pct", 0.0) or 0.0),
            "initial_allocated_usdc": initial_allocated,
            "pnl_pct_alloc": pnl_pct,
            "active_keywords": active_keywords,
            "active_markets": active_markets,
            "total_markets": len(active_markets),
        })

    return sorted(result, key=lambda x: x["net_pnl_usdc"], reverse=True)


def _build_mention_strategy_detail(strategy_id: str, days: int = 30) -> dict:
    """Single mention-strategy drill-down, parallel structure to _build_strategy_detail."""
    try:
        r = _reader()
        trades = r.get_mention_dry_run_trades(days=days, strategy_id=strategy_id)
    except Exception:
        trades = []

    cap_cfg = _load_strategy_capital_config().get(strategy_id, {})

    entries   = [t for t in trades if t.action == "ENTRY"]
    cancels   = [t for t in trades if t.action == "CANCEL"]
    tp_rows   = [t for t in trades if t.action == "TAKE_PROFIT"]
    sl_rows   = [t for t in trades if t.action == "STOP_LOSS"]
    te_rows   = [t for t in trades if t.action == "TIME_EXIT"]
    exit_rows = tp_rows + sl_rows + te_rows

    signals      = len(entries) + len(cancels)
    trigger_rate = len(entries) / signals if signals else 0.0
    fill_rate    = (len(tp_rows) + len(sl_rows)) / len(entries) if entries else 0.0
    # win = resolved exit with positive PnL
    pnl_exits    = [t.realized_pnl_usdc for t in exit_rows if t.realized_pnl_usdc is not None]
    win_rate_val = sum(1 for p in pnl_exits if p >= 0) / len(pnl_exits) if pnl_exits else 0.0
    net_pnl      = sum(pnl_exits)
    total_invested = sum(t.size_usdc for t in entries)

    initial_allocated = float(cap_cfg.get("initial_allocated_usdc", 0.0) or 0.0)
    pnl_pct_alloc    = net_pnl / initial_allocated if initial_allocated > 0 else 0.0
    pnl_pct_invested = net_pnl / total_invested if total_invested > 0 else 0.0

    avg_entry_price = sum(t.price for t in entries) / len(entries) if entries else None
    avg_exit_price  = sum(t.price for t in exit_rows) / len(exit_rows) if exit_rows else None

    hold_secs   = [t.hold_sec for t in exit_rows if t.hold_sec is not None]
    avg_hold_sec = sum(hold_secs) / len(hold_secs) if hold_secs else None

    active_keywords = sorted({t.keyword for t in entries})
    active_markets  = sorted({t.market_slug for t in entries})

    recent_trades = sorted(trades, key=lambda t: t.ts or "", reverse=True)[:20]

    return {
        "strategy_id": strategy_id,
        "days": days,
        "capital_allocation_pct": float(cap_cfg.get("capital_allocation_pct", 0.0) or 0.0),
        "initial_allocated_usdc": initial_allocated,
        "total_signals": signals,
        "entry_count": len(entries),
        "cancel_count": len(cancels),
        "total_trades": len(trades),
        "trigger_rate": trigger_rate,
        "fill_rate": fill_rate,
        "win_rate": win_rate_val,
        "net_pnl_usdc": net_pnl,
        "total_invested_usdc": total_invested,
        "pnl_pct_alloc": pnl_pct_alloc,
        "pnl_pct_invested": pnl_pct_invested,
        "avg_entry_price": avg_entry_price,
        "avg_exit_price": avg_exit_price,
        "avg_hold_sec": avg_hold_sec,
        "tp_count": len(tp_rows),
        "sl_count": len(sl_rows),
        "te_count": len(te_rows),
        "active_keywords": active_keywords,
        "active_markets": active_markets,
        "recent_trades": [
            {
                "id": t.id,
                "ts": t.ts,
                "market_slug": t.market_slug,
                "keyword": t.keyword,
                "side": t.side,
                "action": t.action,
                "price": t.price,
                "size_usdc": t.size_usdc,
                "hold_sec": t.hold_sec,
                "expected_net_edge_bps": t.expected_net_edge_bps,
                "realized_pnl_usdc": t.realized_pnl_usdc,
                "reason_code": t.reason_code,
            }
            for t in recent_trades
        ],
    }


def _build_daily_winrates(strategy_id: Optional[str] = None) -> list[dict]:
    """Win-rate per calendar day for the past 14 days (line chart)."""
    r = _reader()
    cycles = r.get_cycle_results(days=14, strategy_id=strategy_id)
    by_day: dict[str, list[float]] = {}
    for c in cycles:
        if not c.created_at or c.pnl_usdc is None:
            continue
        day = c.created_at[:10]
        by_day.setdefault(day, []).append(c.pnl_usdc)
    result = []
    for day in sorted(by_day):
        pnl = by_day[day]
        result.append({"date": day, "win_rate": win_rate(pnl), "n": len(pnl)})
    return result


# ── REST endpoints ────────────────────────────────────────────────────────────


@app.get("/health")
def health_check():
    """Docker healthcheck endpoint。"""
    return {"status": "ok"}


@app.get("/api/stats")
def get_stats(strategy_id: Optional[str] = None):
    """今日總覽。可加 ?strategy_id=xxx 篩選單一策略。"""
    return _build_stats(days=1, strategy_id=strategy_id)


@app.get("/api/strategies")
def get_strategies(days: int = 7):
    """各策略績效比較表（最近 N 天，預設 7 天）。

    回傳欄位：
    - strategy_id       策略識別碼
    - total_cycles      本期總視窗數
    - triggered_cycles  至少進場一腿的視窗數
    - trigger_rate      觸發率
    - win_rate          有 PnL 記錄的視窗中，獲利的比例
    - total_pnl_usdc    累計 PnL（USDC）
    - total_invested_usdc  已投入金額（兩腿合計，USDC）
    """
    return _build_strategy_table(days=days)


@app.get("/api/strategy-detail")
def get_strategy_detail(strategy_id: str, days: int = 30):
    """單一策略詳情（初始分配資金、交易、PnL、勝率、近期紀錄）。"""
    return _build_strategy_detail(strategy_id=strategy_id, days=days)


@app.get("/api/cycles")
def get_cycles(strategy_id: Optional[str] = None):
    """最近 15 筆 cycle_results。可加 ?strategy_id=xxx 篩選。"""
    r = _reader()
    cycles = r.get_cycle_results(days=30, strategy_id=strategy_id)[-15:]
    return [
        {
            "id": c.id,
            "strategy_id": c.strategy_id,
            "market_slug": c.market_slug,
            "mode": c.mode,
            "leg1_triggered": c.leg1_price is not None,
            "leg1_price": c.leg1_price,
            "leg2_price": c.leg2_price,
            "pnl_usdc": c.pnl_usdc,
            "resolved_winner": c.resolved_winner,
            "created_at": c.created_at,
        }
        for c in reversed(cycles)
    ]


@app.get("/api/dry-runs")
def get_dry_runs(strategy_id: Optional[str] = None):
    """最近 100 筆 dry_run_trades。可加 ?strategy_id=xxx 篩選。"""
    r = _reader()
    trades = r.get_dry_run_trades(days=30, strategy_id=strategy_id)[-100:]
    return [
        {
            "id": t.id,
            "ts": t.ts,
            "strategy_id": t.strategy_id,
            "market_slug": t.market_slug,
            "leg": t.leg,
            "side": t.side,
            "price": t.price,
            "size_usdc": t.size_usdc,
            "fee_usdc": t.fee_usdc,
            "signal_dump_pct": t.signal_dump_pct,
            "hedge_sum": t.hedge_sum,
            "would_profit": t.would_profit,
        }
        for t in reversed(trades)
    ]


@app.get("/api/winrate-history")
def get_winrate_history(strategy_id: Optional[str] = None):
    """過去 14 天每日勝率（折線圖資料）。可加 ?strategy_id=xxx 篩選。"""
    return _build_daily_winrates(strategy_id=strategy_id)


# ── Weather helpers ───────────────────────────────────────────────────────────


def _build_weather_stats(days: int = 7) -> dict:
    """Aggregate stats for all weather strategies (from weather_dry_run_trades)."""
    try:
        report = run_weather_backtest(db_path=_db_path(), days=days)
        return {
            "days": days,
            "total_rows": report.total_rows,
            "entry_count": report.entry_count,
            "cancel_count": report.cancel_count,
            "trigger_rate": report.trigger_rate,
            "fill_rate": report.fill_rate,
            "avg_hold_sec": report.avg_hold_sec,
            "max_hold_sec": report.max_hold_sec,
            "net_pnl_usdc": report.net_pnl_usdc,
            "gross_pnl_usdc": report.gross_pnl_usdc,
            "win_rate_overall": report.win_rate_overall,
            "max_drawdown_pct": report.max_drawdown_pct,
            "sharpe_ratio": report.sharpe_ratio,
            "max_consecutive_losses": report.max_consecutive_losses,
            "low_edge_count": report.low_edge_count,
            "low_confidence_count": report.low_confidence_count,
            "spread_wide_count": report.spread_wide_count,
            "low_depth_count": report.low_depth_count,
            "exits": {
                "take_profit": report.exits.take_profit,
                "stop_loss": report.exits.stop_loss,
                "forecast_shift": report.exits.forecast_shift,
                "time_decay_exit": report.exits.time_decay_exit,
            },
            "forecast_shift_analysis": {
                "count": report.forecast_shift_analysis.count,
                "avg_p_yes_delta": report.forecast_shift_analysis.avg_p_yes_delta,
                "pct_direction_flipped": report.forecast_shift_analysis.pct_direction_flipped,
            },
            "as_of": datetime.now(timezone.utc).isoformat(),
        }
    except Exception:
        return {
            "days": days, "total_rows": 0, "entry_count": 0, "cancel_count": 0,
            "trigger_rate": 0.0, "fill_rate": 0.0, "avg_hold_sec": None,
            "max_hold_sec": None, "net_pnl_usdc": 0.0, "gross_pnl_usdc": 0.0,
            "win_rate_overall": 0.0, "max_drawdown_pct": 0.0, "sharpe_ratio": 0.0,
            "max_consecutive_losses": 0, "low_edge_count": 0, "low_confidence_count": 0,
            "spread_wide_count": 0, "low_depth_count": 0,
            "exits": {"take_profit": 0, "stop_loss": 0, "forecast_shift": 0, "time_decay_exit": 0},
            "forecast_shift_analysis": {"count": 0, "avg_p_yes_delta": None, "pct_direction_flipped": None},
            "as_of": datetime.now(timezone.utc).isoformat(),
        }


def _build_weather_strategy_breakdown(days: int = 7) -> list[dict]:
    """Per-weather-strategy summary for the dashboard."""
    try:
        r = _reader()
        rows = r.get_weather_dry_run_trades(days=days)
    except Exception:
        return []

    cap_cfg = _load_strategy_capital_config()
    # Seed with all enabled weather strategies so zero-trade ones still appear
    by_sid: dict[str, list[WeatherDryRunTrade]] = defaultdict(list)
    for sid in _load_weather_strategy_ids():
        by_sid[sid]  # ensure key exists (defaultdict creates empty list)
    for row in rows:
        by_sid[row.strategy_id].append(row)

    result = []
    for sid, trades in by_sid.items():
        entries   = [t for t in trades if t.action == "ENTRY"]
        cancels   = [t for t in trades if t.action in ("NO_TRADE", "CANCEL")]
        exit_rows = [t for t in trades if t.action in (
            "TAKE_PROFIT", "STOP_LOSS", "FORECAST_SHIFT", "TIME_DECAY_EXIT"
        )]
        market_resolved = [t for t in exit_rows if t.action in (
            "TAKE_PROFIT", "STOP_LOSS", "FORECAST_SHIFT"
        )]

        signals      = len(entries) + len(cancels)
        trigger_rate = len(entries) / signals if signals else 0.0
        fill_rate    = len(market_resolved) / len(entries) if entries else 0.0

        pnl_series = [t.realized_pnl_usdc for t in exit_rows if t.realized_pnl_usdc is not None]
        net_pnl    = sum(pnl_series)

        active_cities = sorted({t.city for t in entries if t.city})
        cfg           = cap_cfg.get(sid, {})
        initial_allocated = float(cfg.get("initial_allocated_usdc", 0.0) or 0.0)
        pnl_pct = (net_pnl / initial_allocated) if initial_allocated > 0 else 0.0

        result.append({
            "strategy_id": sid,
            "entry_count": len(entries),
            "cancel_count": len(cancels),
            "trigger_rate": trigger_rate,
            "fill_rate": fill_rate,
            "net_pnl_usdc": net_pnl,
            "capital_allocation_pct": float(cfg.get("capital_allocation_pct", 0.0) or 0.0),
            "initial_allocated_usdc": initial_allocated,
            "pnl_pct_alloc": pnl_pct,
            "active_cities": active_cities,
            "total_cities": len(active_cities),
        })

    return sorted(result, key=lambda x: x["net_pnl_usdc"], reverse=True)


def _build_weather_strategy_detail(strategy_id: str, days: int = 30) -> dict:
    """Single weather-strategy drill-down, parallel structure to _build_mention_strategy_detail."""
    try:
        r = _reader()
        trades = r.get_weather_dry_run_trades(days=days, strategy_id=strategy_id)
    except Exception:
        trades = []

    cap_cfg = _load_strategy_capital_config().get(strategy_id, {})

    entries   = [t for t in trades if t.action == "ENTRY"]
    cancels   = [t for t in trades if t.action == "CANCEL"]
    tp_rows   = [t for t in trades if t.action == "TAKE_PROFIT"]
    sl_rows   = [t for t in trades if t.action == "STOP_LOSS"]
    fs_rows   = [t for t in trades if t.action == "FORECAST_SHIFT"]
    td_rows   = [t for t in trades if t.action == "TIME_DECAY_EXIT"]
    exit_rows = tp_rows + sl_rows + fs_rows + td_rows

    signals      = len(entries) + len(cancels)
    trigger_rate = len(entries) / signals if signals else 0.0
    fill_rate    = (len(tp_rows) + len(sl_rows) + len(fs_rows)) / len(entries) if entries else 0.0

    pnl_exits    = [t.realized_pnl_usdc for t in exit_rows if t.realized_pnl_usdc is not None]
    win_rate_val = sum(1 for p in pnl_exits if p >= 0) / len(pnl_exits) if pnl_exits else 0.0
    net_pnl      = sum(pnl_exits)
    total_invested = sum(t.size_usdc for t in entries)

    initial_allocated = float(cap_cfg.get("initial_allocated_usdc", 0.0) or 0.0)
    pnl_pct_alloc    = net_pnl / initial_allocated if initial_allocated > 0 else 0.0
    pnl_pct_invested = net_pnl / total_invested if total_invested > 0 else 0.0

    avg_entry_price = sum(t.price for t in entries) / len(entries) if entries else None
    avg_exit_price  = sum(t.price for t in exit_rows) / len(exit_rows) if exit_rows else None

    hold_secs    = [t.hold_sec for t in exit_rows if t.hold_sec is not None]
    avg_hold_sec = sum(hold_secs) / len(hold_secs) if hold_secs else None

    active_cities = sorted({t.city for t in entries if t.city})

    # Open positions = entries whose event_id has no corresponding exit record.
    exited_event_ids = {t.event_id for t in exit_rows}
    open_entries = [t for t in entries if t.event_id not in exited_event_ids]

    # Always surface all ENTRY + EXIT rows (they are few and important).
    # Pad the remaining slots with the most-recent NO_TRADE rows so the log
    # stays readable even when entries/exits are older than recent NO_TRADEs.
    _key = lambda t: t.ts or ""
    _entry_exit = sorted([t for t in trades if t.action != "NO_TRADE"], key=_key, reverse=True)
    _no_trade   = sorted([t for t in trades if t.action == "NO_TRADE"],  key=_key, reverse=True)[:15]
    recent_trades = sorted(_entry_exit + _no_trade, key=_key, reverse=True)

    return {
        "strategy_id": strategy_id,
        "days": days,
        "capital_allocation_pct": float(cap_cfg.get("capital_allocation_pct", 0.0) or 0.0),
        "initial_allocated_usdc": initial_allocated,
        "total_signals": signals,
        "entry_count": len(entries),
        "cancel_count": len(cancels),
        "total_trades": len(trades),
        "trigger_rate": trigger_rate,
        "fill_rate": fill_rate,
        "win_rate": win_rate_val,
        "net_pnl_usdc": net_pnl,
        "total_invested_usdc": total_invested,
        "pnl_pct_alloc": pnl_pct_alloc,
        "pnl_pct_invested": pnl_pct_invested,
        "avg_entry_price": avg_entry_price,
        "avg_exit_price": avg_exit_price,
        "avg_hold_sec": avg_hold_sec,
        "tp_count": len(tp_rows),
        "sl_count": len(sl_rows),
        "fs_count": len(fs_rows),
        "td_count": len(td_rows),
        "active_cities": active_cities,
        "open_positions": [
            {
                "id": t.id,
                "ts": t.ts,
                "market_slug": t.market_slug,
                "city": t.city,
                "side": t.side,
                "price": t.price,
                "p_yes_at_entry": t.p_yes_at_entry,
                "lead_days": t.lead_days,
                "model": t.model,
            }
            for t in sorted(open_entries, key=lambda t: t.ts or "", reverse=True)
        ],
        "recent_trades": [
            {
                "id": t.id,
                "ts": t.ts,
                "market_slug": t.market_slug,
                "city": t.city,
                "market_type": t.market_type,
                "side": t.side,
                "action": t.action,
                "price": t.price,
                "size_usdc": t.size_usdc,
                "hold_sec": t.hold_sec,
                "model": t.model,
                "p_yes_at_entry": t.p_yes_at_entry,
                "p_yes_at_exit": t.p_yes_at_exit,
                "lead_days": t.lead_days,
                "expected_net_edge_bps": t.expected_net_edge_bps,
                "realized_pnl_usdc": t.realized_pnl_usdc,
                "reason_code": t.reason_code,
            }
            for t in recent_trades
        ],
    }


# ── Mention Market endpoints ───────────────────────────────────────────────────


@app.get("/api/mention/stats")
def get_mention_stats(days: int = 7):
    """Mention Market 總覽（aggregate across all mention strategies）。"""
    return _build_mention_stats(days=days)


@app.get("/api/mention/strategies")
def get_mention_strategies(days: int = 7):
    """每個 mention strategy 的績效拆解。"""
    return _build_mention_strategy_breakdown(days=days)


@app.get("/api/mention/strategy-detail")
def get_mention_strategy_detail(strategy_id: str, days: int = 30):
    """單一 mention 策略詳情（與 /api/strategy-detail 結構對稱）。"""
    return _build_mention_strategy_detail(strategy_id=strategy_id, days=days)


@app.get("/api/mention/trades")
def get_mention_trades(days: int = 30, strategy_id: Optional[str] = None):
    """最近 100 筆 mention_dry_run_trades。可加 ?strategy_id=xxx 篩選。"""
    try:
        r = _reader()
        trades = r.get_mention_dry_run_trades(days=days, strategy_id=strategy_id)[-100:]
    except Exception:
        return []
    return [
        {
            "id": t.id,
            "ts": t.ts,
            "strategy_id": t.strategy_id,
            "market_slug": t.market_slug,
            "keyword": t.keyword,
            "side": t.side,
            "action": t.action,
            "price": t.price,
            "size_usdc": t.size_usdc,
            "spread_at_decision": t.spread_at_decision,
            "depth_usdc_at_decision": t.depth_usdc_at_decision,
            "hold_sec": t.hold_sec,
            "expected_net_edge_bps": t.expected_net_edge_bps,
            "realized_pnl_usdc": t.realized_pnl_usdc,
            "reason_code": t.reason_code,
            "note": t.note,
        }
        for t in reversed(trades)
    ]


# ── Weather Market endpoints ──────────────────────────────────────────────────


@app.get("/api/weather/stats")
def get_weather_stats(days: int = 7):
    """Weather Market 總覽（aggregate across all weather strategies）。"""
    return _build_weather_stats(days=days)


@app.get("/api/weather/strategies")
def get_weather_strategies(days: int = 7):
    """每個 weather strategy 的績效拆解。"""
    return _build_weather_strategy_breakdown(days=days)


@app.get("/api/weather/strategy-detail")
def get_weather_strategy_detail(strategy_id: str, days: int = 30):
    """單一 weather 策略詳情（與 /api/mention/strategy-detail 結構對稱）。"""
    return _build_weather_strategy_detail(strategy_id=strategy_id, days=days)


@app.get("/api/weather/trades")
def get_weather_trades(days: int = 30, strategy_id: Optional[str] = None):
    """最近 100 筆 weather_dry_run_trades。可加 ?strategy_id=xxx 篩選。"""
    try:
        r = _reader()
        trades = r.get_weather_dry_run_trades(days=days, strategy_id=strategy_id)[-100:]
    except Exception:
        return []
    return [
        {
            "id": t.id,
            "ts": t.ts,
            "strategy_id": t.strategy_id,
            "market_slug": t.market_slug,
            "city": t.city,
            "market_type": t.market_type,
            "side": t.side,
            "action": t.action,
            "price": t.price,
            "size_usdc": t.size_usdc,
            "spread_at_decision": t.spread_at_decision,
            "depth_usdc_at_decision": t.depth_usdc_at_decision,
            "hold_sec": t.hold_sec,
            "model": t.model,
            "p_yes_at_entry": t.p_yes_at_entry,
            "p_yes_at_exit": t.p_yes_at_exit,
            "lead_days": t.lead_days,
            "expected_net_edge_bps": t.expected_net_edge_bps,
            "realized_pnl_usdc": t.realized_pnl_usdc,
            "reason_code": t.reason_code,
            "note": t.note,
        }
        for t in reversed(trades)
    ]


# ── Weather Ladder helpers ────────────────────────────────────────────────────


def _build_ladder_strategy_breakdown(days: int = 7) -> list[dict]:
    """Per-weather_ladder-strategy summary for the dashboard."""
    r = _reader()
    rows = r.get_weather_ladder_trades(days=days)

    cap_cfg = _load_strategy_capital_config()
    by_sid: dict[str, list[WeatherLadderTrade]] = defaultdict(list)
    for sid in _load_weather_ladder_strategy_ids():
        by_sid[sid]
    for row in rows:
        by_sid[row.strategy_id].append(row)

    result = []
    for sid, trades in by_sid.items():
        entries   = [t for t in trades if t.action == "LADDER_ENTRY"]
        exits     = [t for t in trades if t.action in ("HOLD_TO_RESOLUTION", "CATASTROPHIC_SHIFT_EXIT")]
        cat_exits = [t for t in trades if t.action == "CATASTROPHIC_SHIFT_EXIT"]

        # Unique ladders
        entered_ladder_ids = {t.ladder_id for t in entries}
        exited_ladder_ids  = {t.ladder_id for t in exits}
        open_ladder_ids    = entered_ladder_ids - exited_ladder_ids
        total_ladders = len(entered_ladder_ids)
        open_ladders  = len(open_ladder_ids)

        pnl_series = [t.realized_pnl_usdc for t in exits if t.realized_pnl_usdc is not None]
        net_pnl    = sum(pnl_series)

        # Avg payout ratio from entries (one entry per leg, all same ratio per ladder)
        payout_ratios = []
        by_lid: dict[str, list] = defaultdict(list)
        for t in entries:
            by_lid[t.ladder_id].append(t)
        for legs in by_lid.values():
            if legs and legs[0].ladder_payout_ratio > 0:
                payout_ratios.append(legs[0].ladder_payout_ratio)
        avg_payout = sum(payout_ratios) / len(payout_ratios) if payout_ratios else 0.0

        total_invested = sum(t.size_usdc for t in entries)
        active_cities  = sorted({t.city for t in entries if t.city})
        cfg = cap_cfg.get(sid, {})
        initial_allocated = float(cfg.get("initial_allocated_usdc", 0.0) or 0.0)
        pnl_pct = net_pnl / initial_allocated if initial_allocated > 0 else 0.0

        result.append({
            "strategy_id": sid,
            "total_ladders": total_ladders,
            "open_ladders": open_ladders,
            "cat_exit_count": len({t.ladder_id for t in cat_exits}),
            "total_invested_usdc": total_invested,
            "avg_payout_ratio": avg_payout,
            "net_pnl_usdc": net_pnl,
            "capital_allocation_pct": float(cfg.get("capital_allocation_pct", 0.0) or 0.0),
            "initial_allocated_usdc": initial_allocated,
            "pnl_pct_alloc": pnl_pct,
            "active_cities": active_cities,
            "total_cities": len(active_cities),
        })

    return sorted(result, key=lambda x: x["net_pnl_usdc"], reverse=True)


def _build_ladder_strategy_detail(strategy_id: str, days: int = 30) -> dict:
    """Single ladder-strategy drill-down."""
    r = _reader()
    trades = r.get_weather_ladder_trades(days=days, strategy_id=strategy_id)
    cap_cfg = _load_strategy_capital_config().get(strategy_id, {})

    entries   = [t for t in trades if t.action == "LADDER_ENTRY"]
    htr_rows  = [t for t in trades if t.action == "HOLD_TO_RESOLUTION"]
    cat_rows  = [t for t in trades if t.action == "CATASTROPHIC_SHIFT_EXIT"]
    exit_rows = htr_rows + cat_rows

    entered_ladder_ids = {t.ladder_id for t in entries}
    exited_ladder_ids  = {t.ladder_id for t in exit_rows}
    open_ladder_ids    = entered_ladder_ids - exited_ladder_ids

    # Build open-ladder groups
    by_lid: dict[str, list[WeatherLadderTrade]] = defaultdict(list)
    for t in entries:
        by_lid[t.ladder_id].append(t)

    open_ladders_list = []
    for lid in open_ladder_ids:
        legs = sorted(by_lid.get(lid, []), key=lambda t: t.leg_index)
        if not legs:
            continue
        first = legs[0]
        open_ladders_list.append({
            "ladder_id": lid,
            "city": first.city,
            "target_date": first.target_date,
            "legs": len(legs),
            "sum_price": first.ladder_sum_price,
            "payout_ratio": first.ladder_payout_ratio,
            "combined_p": first.ladder_combined_p,
            "total_usdc": sum(t.size_usdc for t in legs),
            "model": first.model,
            "lead_days": first.lead_days,
            "entry_ts": first.ts,
        })
    open_ladders_list.sort(key=lambda x: x["entry_ts"], reverse=True)

    pnl_series = [t.realized_pnl_usdc for t in exit_rows if t.realized_pnl_usdc is not None]
    net_pnl    = sum(pnl_series)
    total_invested = sum(t.size_usdc for t in entries)

    payout_ratios = [by_lid[lid][0].ladder_payout_ratio
                     for lid in entered_ladder_ids if by_lid.get(lid)]
    avg_payout = sum(payout_ratios) / len(payout_ratios) if payout_ratios else 0.0

    initial_allocated = float(cap_cfg.get("initial_allocated_usdc", 0.0) or 0.0)
    pnl_pct_alloc     = net_pnl / initial_allocated if initial_allocated > 0 else 0.0

    # Recent trade log: all entry/exit rows, most recent first
    _key = lambda t: t.ts or ""
    recent_trades = sorted(trades, key=_key, reverse=True)

    return {
        "strategy_id": strategy_id,
        "days": days,
        "capital_allocation_pct": float(cap_cfg.get("capital_allocation_pct", 0.0) or 0.0),
        "initial_allocated_usdc": initial_allocated,
        "total_ladders": len(entered_ladder_ids),
        "open_ladders": len(open_ladder_ids),
        "htr_count": len({t.ladder_id for t in htr_rows}),
        "cat_count": len({t.ladder_id for t in cat_rows}),
        "avg_payout_ratio": avg_payout,
        "net_pnl_usdc": net_pnl,
        "total_invested_usdc": total_invested,
        "pnl_pct_alloc": pnl_pct_alloc,
        "open_ladders_list": open_ladders_list,
        "recent_trades": [
            {
                "id": t.id,
                "ts": t.ts,
                "ladder_id": t.ladder_id[:8],  # truncated for display
                "city": t.city,
                "target_date": t.target_date,
                "action": t.action,
                "leg_index": t.leg_index,
                "price": t.price,
                "size_usdc": t.size_usdc,
                "p_yes": t.p_yes,
                "lead_days": t.lead_days,
                "ladder_legs": t.ladder_legs,
                "ladder_sum_price": t.ladder_sum_price,
                "payout_ratio": t.ladder_payout_ratio,
                "combined_p": t.ladder_combined_p,
                "realized_pnl_usdc": t.realized_pnl_usdc,
                "model": t.model,
                "reason_code": t.reason_code,
            }
            for t in recent_trades
        ],
    }


# ── Weather Ladder endpoints ──────────────────────────────────────────────────


@app.get("/api/ladder/strategies")
def get_ladder_strategies(days: int = 7):
    """每個 weather_ladder strategy 的績效拆解。"""
    return _build_ladder_strategy_breakdown(days=days)


@app.get("/api/ladder/strategy-detail")
def get_ladder_strategy_detail(strategy_id: str, days: int = 30):
    """單一 weather_ladder 策略詳情。"""
    return _build_ladder_strategy_detail(strategy_id=strategy_id, days=days)


# ── WebSocket: broadcast stats every 5 s ─────────────────────────────────────


class _ConnectionManager:
    def __init__(self) -> None:
        self._clients: list[WebSocket] = []

    async def connect(self, ws: WebSocket) -> None:
        await ws.accept()
        self._clients.append(ws)

    def disconnect(self, ws: WebSocket) -> None:
        self._clients.remove(ws)

    async def broadcast(self, data: str) -> None:
        dead = []
        for ws in self._clients:
            try:
                await ws.send_text(data)
            except Exception:
                dead.append(ws)
        for ws in dead:
            self._clients.remove(ws)


_manager = _ConnectionManager()


@app.on_event("startup")
async def _start_broadcaster() -> None:
    asyncio.create_task(_broadcast_loop())


async def _broadcast_loop() -> None:
    """每 5 秒推送一次：全體統計 + 各策略比較表。"""
    while True:
        await asyncio.sleep(5)
        if not _manager._clients:
            continue
        try:
            payload = json.dumps({
                "overall": _build_stats(days=1),
                "strategies": _build_strategy_table(days=7),
                "mention": _build_mention_stats(days=7),
                "mention_strategies": _build_mention_strategy_breakdown(days=7),
                "weather": _build_weather_stats(days=7),
                "weather_strategies": _build_weather_strategy_breakdown(days=7),
                "ladder_strategies": _build_ladder_strategy_breakdown(days=7),
            })
            await _manager.broadcast(payload)
        except Exception:
            pass


@app.websocket("/ws/live")
async def ws_live(ws: WebSocket) -> None:
    await _manager.connect(ws)
    try:
        await ws.send_text(json.dumps({
            "overall": _build_stats(days=1),
            "strategies": _build_strategy_table(days=7),
            "mention": _build_mention_stats(days=7),
            "mention_strategies": _build_mention_strategy_breakdown(days=7),
            "weather": _build_weather_stats(days=7),
            "weather_strategies": _build_weather_strategy_breakdown(days=7),
            "ladder_strategies": _build_ladder_strategy_breakdown(days=7),
        }))
        while True:
            await ws.receive_text()
    except WebSocketDisconnect:
        _manager.disconnect(ws)


# ── Serve React build (if present) ───────────────────────────────────────────

_STATIC = os.path.join(_HERE, "frontend", "dist")
if os.path.isdir(_STATIC):
    app.mount("/", StaticFiles(directory=_STATIC, html=True), name="static")
