# One-shot tool: backfill SETTLEMENT rows for historical ENTRY rows whose market
# has already closed but never got an exit record written.
#
# Use case: ran the engine before SettlementReconciler existed (or had a long
# downtime), and now `weather_dry_run_trades` has many ENTRY rows with no
# SETTLEMENT/TIME_DECAY_EXIT pair. Dashboard shows 0% settlement_coverage.
#
# This script reads Polymarket's Gamma API for each unresolved entry,
# computes PnL, and inserts a SETTLEMENT (or TIME_DECAY_EXIT fallback) row.
#
# Usage:
#   python tools/backfill_settlements.py --db rust-engine/data/market_snapshots.db --days 90 --dry-run
#   python tools/backfill_settlements.py --db rust-engine/data/market_snapshots.db --days 90 --apply
#
# DRY_RUN safety: writes only to weather_dry_run_trades (the dry_run table).
# Live trades are NEVER touched.

from __future__ import annotations

import argparse
import json
import os
import sqlite3
import sys
import time
from dataclasses import dataclass
from typing import Optional

try:
    import httpx
except ImportError:
    print("Missing dependency: pip install httpx", file=sys.stderr)
    sys.exit(1)

GAMMA_BASE = "https://gamma-api.polymarket.com"
# Match the Rust constant in settlement_reconciler.rs
GRACE_PERIOD_SEC = 7 * 24 * 3600
TAKER_FEE_BPS_DEFAULT = 180  # 1.8% — matches config/settings.toml
GAS_FEE_USDC_DEFAULT = 0.005


@dataclass
class UnresolvedEntry:
    id: int
    strategy_id: str
    event_id: str
    market_slug: str
    city: str
    market_type: str
    side: str
    entry_price: float
    size_usdc: float
    token_id: str
    close_ts: int
    p_yes_at_entry: float
    lead_days: int
    expected_net_edge_bps: float
    model: str
    entry_ts: int
    taker_fee_bps: Optional[int]
    slippage_buffer_bps: Optional[int]


def _table_columns(conn: sqlite3.Connection, table: str) -> set[str]:
    return {r[1] for r in conn.execute(f"PRAGMA table_info({table})").fetchall()}


def load_unresolved(db: str, days: int) -> list[UnresolvedEntry]:
    """ENTRY rows whose market has closed but no SETTLEMENT/exit recorded."""
    conn = sqlite3.connect(db)
    conn.row_factory = sqlite3.Row
    cols = _table_columns(conn, "weather_dry_run_trades")
    has_token = "token_id" in cols
    has_close_ts = "close_ts" in cols
    if not (has_token and has_close_ts):
        conn.close()
        print(
            "[ERROR] DB schema 太舊：缺欄位 "
            f"{'token_id ' if not has_token else ''}"
            f"{'close_ts' if not has_close_ts else ''}\n"
            "  Backfill 需要 token_id 才能呼叫 Polymarket Gamma API。\n"
            "  請先讓引擎跑一段時間自動 migration，或在 VPS 上的新 DB 執行此腳本。",
            file=sys.stderr,
        )
        sys.exit(2)

    now_ts = int(time.time())
    sql = """
    SELECT e.id, e.strategy_id, e.event_id, e.market_slug, e.city, e.market_type,
           e.side, e.price AS entry_price, e.size_usdc, e.token_id, e.close_ts,
           COALESCE(e.p_yes_at_entry, 0.5) AS p_yes_at_entry,
           COALESCE(e.lead_days, 0) AS lead_days,
           COALESCE(e.expected_net_edge_bps, 0.0) AS expected_net_edge_bps,
           e.model,
           CAST(strftime('%s', e.ts) AS INTEGER) AS entry_ts,
           e.taker_fee_bps, e.slippage_buffer_bps
    FROM weather_dry_run_trades e
    WHERE e.action = 'ENTRY'
      AND e.close_ts > 0
      AND e.close_ts <= ?
      AND e.ts >= datetime('now', ?)
      AND NOT EXISTS (
          SELECT 1 FROM weather_dry_run_trades x
          WHERE x.event_id = e.event_id
            AND x.action IN ('SETTLEMENT','TAKE_PROFIT','STOP_LOSS',
                             'FORECAST_SHIFT','TIME_DECAY_EXIT')
      )
    ORDER BY e.close_ts
    """
    rows = conn.execute(sql, (now_ts, f"-{days} days")).fetchall()
    conn.close()
    return [UnresolvedEntry(**dict(r)) for r in rows]


def fetch_settlement_price(client: httpx.Client, slug: str, token_id: str) -> Optional[float]:
    """Mirror of rust-engine/src/api/gamma.rs::fetch_token_settlement_price."""
    url = f"{GAMMA_BASE}/markets?slug={slug}"
    try:
        resp = client.get(url, timeout=10.0)
        resp.raise_for_status()
        markets = resp.json()
    except Exception as e:
        print(f"  [WARN] gamma fetch failed {slug}: {e}", file=sys.stderr)
        return None

    if not markets:
        return None
    m = markets[0]

    winner = m.get("winner")
    if winner:
        return 1.0 if winner == token_id else 0.0

    try:
        token_ids = json.loads(m.get("clobTokenIds", "[]"))
        outcome_prices_raw = m.get("outcomePrices")
        if not outcome_prices_raw:
            return None
        prices = json.loads(outcome_prices_raw)
    except (json.JSONDecodeError, TypeError):
        return None

    for i, tid in enumerate(token_ids):
        if tid != token_id:
            continue
        try:
            p = float(prices[i])
        except (ValueError, IndexError, TypeError):
            return None
        if p >= 0.999:
            return 1.0
        if p <= 0.001:
            return 0.0
        return None  # still mid-range — not settled yet
    return None


def compute_pnl(entry: UnresolvedEntry, exit_price: float) -> float:
    taker = (entry.taker_fee_bps or TAKER_FEE_BPS_DEFAULT) / 10_000
    slip = (entry.slippage_buffer_bps or 0) / 10_000
    # Two-sided fee: entry + exit
    fee_per_side = entry.size_usdc * taker + GAS_FEE_USDC_DEFAULT
    slip_cost = entry.size_usdc * slip
    return (exit_price - entry.entry_price) * entry.size_usdc - 2 * fee_per_side - slip_cost


def insert_exit_row(
    db: str,
    entry: UnresolvedEntry,
    action: str,
    exit_price: float,
    realized_pnl: float,
    note: str,
) -> None:
    now_ts = int(time.time())
    hold_sec = now_ts - entry.entry_ts
    conn = sqlite3.connect(db)
    conn.execute(
        """
        INSERT INTO weather_dry_run_trades
        (strategy_id, event_id, market_slug, city, market_type, side, action,
         price, size_usdc, spread_at_decision, depth_usdc_at_decision,
         entry_price, exit_price, hold_sec, model, p_yes_at_entry, p_yes_at_exit,
         lead_days, taker_fee_bps, slippage_buffer_bps,
         expected_net_edge_bps, realized_pnl_usdc, reason_code, note,
         token_id, close_ts)
        VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)
        """,
        (
            entry.strategy_id, entry.event_id, entry.market_slug, entry.city,
            entry.market_type, entry.side, action,
            exit_price, entry.size_usdc, None, None,
            entry.entry_price, exit_price, hold_sec, entry.model,
            entry.p_yes_at_entry, None, entry.lead_days,
            entry.taker_fee_bps, entry.slippage_buffer_bps,
            entry.expected_net_edge_bps, realized_pnl, action, note,
            entry.token_id, entry.close_ts,
        ),
    )
    conn.commit()
    conn.close()


def main() -> None:
    ap = argparse.ArgumentParser(description="Backfill SETTLEMENT rows from Polymarket Gamma API")
    ap.add_argument("--db", default="rust-engine/data/market_snapshots.db",
                    help="Path to SQLite DB")
    ap.add_argument("--days", type=int, default=90,
                    help="Look-back window (default: 90 days)")
    ap.add_argument("--apply", action="store_true",
                    help="Actually write rows (default: dry-run preview)")
    ap.add_argument("--force-close-grace-days", type=int, default=7,
                    help="Past this many days post-close, write TIME_DECAY_EXIT estimate")
    ap.add_argument("--sleep-ms", type=int, default=200,
                    help="Sleep between Gamma calls to avoid rate limits")
    args = ap.parse_args()

    if not os.path.exists(args.db):
        print(f"[ERROR] DB not found: {args.db}", file=sys.stderr)
        sys.exit(1)

    mode = "APPLY" if args.apply else "DRY-RUN (preview)"
    print(f"=== backfill_settlements.py  mode={mode}  db={args.db}  days={args.days} ===")

    entries = load_unresolved(args.db, args.days)
    if not entries:
        print("No unresolved closed entries found. Nothing to do.")
        return
    print(f"Found {len(entries)} unresolved ENTRY rows whose market has closed.\n")

    settled = 0
    force_closed = 0
    still_pending = 0
    now_ts = int(time.time())
    grace_sec = args.force_close_grace_days * 86400

    with httpx.Client() as client:
        for e in entries:
            secs_since_close = now_ts - e.close_ts
            price = fetch_settlement_price(client, e.market_slug, e.token_id)
            time.sleep(args.sleep_ms / 1000.0)

            if price is not None:
                pnl = compute_pnl(e, price)
                print(f"  [SETTLE] {e.market_slug:<50} {e.side:<3} "
                      f"entry={e.entry_price:.3f} settle={price:.3f} "
                      f"pnl={pnl:+.4f}  ({e.strategy_id})")
                if args.apply:
                    insert_exit_row(args.db, e, "SETTLEMENT", price, pnl,
                                    "backfilled from gamma-api")
                settled += 1
            elif secs_since_close > grace_sec:
                model_settle = e.p_yes_at_entry if e.side == "YES" else 1.0 - e.p_yes_at_entry
                pnl = compute_pnl(e, model_settle)
                print(f"  [FORCE]  {e.market_slug:<50} {e.side:<3} "
                      f"closed {secs_since_close // 86400}d ago, "
                      f"model_settle={model_settle:.3f} pnl={pnl:+.4f}  ({e.strategy_id})")
                if args.apply:
                    insert_exit_row(args.db, e, "TIME_DECAY_EXIT", e.entry_price, pnl,
                                    "backfilled force-close past grace")
                force_closed += 1
            else:
                hours_since = secs_since_close // 3600
                print(f"  [WAIT]   {e.market_slug:<50} {e.side:<3} "
                      f"closed {hours_since}h ago, UMA still pending  ({e.strategy_id})")
                still_pending += 1

    print()
    print(f"Summary: settled={settled}  force-closed={force_closed}  pending={still_pending}")
    if not args.apply:
        print("This was a preview. Re-run with --apply to write rows.")


if __name__ == "__main__":
    main()
