# Polymarket Data API client — fetch own-wallet trade and position history
# directly from polymarket.com.
#
# Use case: in live mode this lets us cross-check local dry_run_trades against
# the authoritative on-chain record. In dry_run mode the wallet has no real
# trades, so this script just returns [].
#
# Wallet address comes from .env::POLYGON_PUBLIC_KEY.
#
# Usage:
#   cd python-analytics
#   python -m src.data.polymarket_history --wallet 0x...           # ad-hoc
#   python -m src.data.polymarket_history --persist                # write to DB
#   python -m src.data.polymarket_history --days 30 --json
#
# Notes on endpoints (verified 2025/2026):
#   GET https://data-api.polymarket.com/trades?user=<wallet>&limit=500
#   GET https://data-api.polymarket.com/positions?user=<wallet>&limit=500
# These are public, no auth required. Be polite with sleep between calls.

from __future__ import annotations

import argparse
import json
import os
import sqlite3
import sys
import time
from dataclasses import dataclass, asdict
from typing import Optional

try:
    import httpx
except ImportError:
    print("Missing dependency: pip install httpx", file=sys.stderr)
    sys.exit(1)

try:
    from dotenv import load_dotenv
    load_dotenv()
except ImportError:
    pass

DATA_API_BASE = "https://data-api.polymarket.com"


@dataclass
class ExternalTrade:
    tx_hash: str
    market_slug: str
    condition_id: str
    token_id: str
    side: str            # "BUY" / "SELL"
    outcome: str         # "Yes" / "No" / etc
    price: float
    size_usdc: float     # USDC amount
    ts: int              # unix seconds
    raw: dict


def fetch_trades(client: httpx.Client, wallet: str, limit: int = 500) -> list[ExternalTrade]:
    url = f"{DATA_API_BASE}/trades"
    params = {"user": wallet, "limit": limit}
    try:
        resp = client.get(url, params=params, timeout=15.0)
        resp.raise_for_status()
        data = resp.json()
    except Exception as e:
        print(f"[WARN] data-api /trades failed: {e}", file=sys.stderr)
        return []

    if not isinstance(data, list):
        # Sometimes wrapped in {"data": [...]}
        data = data.get("data", []) if isinstance(data, dict) else []

    out = []
    for d in data:
        try:
            out.append(ExternalTrade(
                tx_hash=str(d.get("transactionHash") or d.get("tx_hash") or ""),
                market_slug=str(d.get("slug") or d.get("market_slug") or ""),
                condition_id=str(d.get("conditionId") or ""),
                token_id=str(d.get("asset") or d.get("tokenId") or ""),
                side=str(d.get("side") or "").upper(),
                outcome=str(d.get("outcome") or ""),
                price=float(d.get("price") or 0.0),
                size_usdc=float(d.get("size") or d.get("usdcSize") or 0.0),
                ts=int(d.get("timestamp") or d.get("ts") or 0),
                raw=d,
            ))
        except (TypeError, ValueError) as e:
            print(f"[WARN] skipping malformed trade row: {e}", file=sys.stderr)
    return out


def ensure_schema(db_path: str) -> None:
    """Create polymarket_trades_external table if not exists."""
    conn = sqlite3.connect(db_path)
    conn.execute("""
        CREATE TABLE IF NOT EXISTS polymarket_trades_external (
            tx_hash       TEXT PRIMARY KEY,
            ts            INTEGER NOT NULL,
            market_slug   TEXT NOT NULL,
            condition_id  TEXT,
            token_id      TEXT,
            side          TEXT,
            outcome       TEXT,
            price         REAL,
            size_usdc     REAL,
            raw_json      TEXT,
            fetched_at    DATETIME DEFAULT CURRENT_TIMESTAMP
        )
    """)
    conn.commit()
    conn.close()


def persist_trades(db_path: str, trades: list[ExternalTrade]) -> int:
    ensure_schema(db_path)
    conn = sqlite3.connect(db_path)
    inserted = 0
    for t in trades:
        try:
            conn.execute(
                """INSERT OR IGNORE INTO polymarket_trades_external
                   (tx_hash, ts, market_slug, condition_id, token_id, side, outcome,
                    price, size_usdc, raw_json)
                   VALUES (?,?,?,?,?,?,?,?,?,?)""",
                (t.tx_hash, t.ts, t.market_slug, t.condition_id, t.token_id,
                 t.side, t.outcome, t.price, t.size_usdc, json.dumps(t.raw)),
            )
            if conn.total_changes > inserted:
                inserted += 1
        except sqlite3.Error as e:
            print(f"[WARN] insert failed for {t.tx_hash}: {e}", file=sys.stderr)
    conn.commit()
    conn.close()
    return inserted


def main() -> None:
    ap = argparse.ArgumentParser(description="Fetch own-wallet Polymarket history")
    ap.add_argument("--wallet", default=os.environ.get("POLYGON_PUBLIC_KEY", ""),
                    help="Wallet address (default: $POLYGON_PUBLIC_KEY)")
    ap.add_argument("--db", default=None, help="SQLite DB path for --persist")
    ap.add_argument("--limit", type=int, default=500)
    ap.add_argument("--persist", action="store_true",
                    help="Insert into polymarket_trades_external table")
    ap.add_argument("--json", action="store_true", help="Print JSON instead of table")
    args = ap.parse_args()

    if not args.wallet:
        print("[ERROR] No wallet provided. Set POLYGON_PUBLIC_KEY in .env or pass --wallet.",
              file=sys.stderr)
        sys.exit(1)

    db = args.db
    if db is None:
        repo_root = os.path.normpath(os.path.join(os.path.dirname(__file__), "../../.."))
        db = os.path.join(repo_root, "rust-engine", "data", "market_snapshots.db")

    with httpx.Client() as client:
        trades = fetch_trades(client, args.wallet, limit=args.limit)

    if not trades:
        print("(no trades returned — dry_run wallet won't have any real Polymarket trades)")
        return

    if args.json:
        print(json.dumps([asdict(t) for t in trades], indent=2, default=str))
    else:
        print(f"Wallet {args.wallet}  trades fetched: {len(trades)}")
        print(f"{'ts':<11} {'slug':<40} {'side':<5} {'outcome':<8} {'price':>7} {'usdc':>10}")
        for t in trades[:40]:
            print(f"{t.ts:<11} {t.market_slug[:40]:<40} {t.side:<5} {t.outcome:<8} "
                  f"{t.price:>7.4f} {t.size_usdc:>10.2f}")
        if len(trades) > 40:
            print(f"... ({len(trades) - 40} more)")

    if args.persist:
        inserted = persist_trades(db, trades)
        print(f"\nPersisted {inserted} new rows to polymarket_trades_external (db={db})")


if __name__ == "__main__":
    main()
