use std::sync::{Arc, Mutex};

use rusqlite::Connection;

// ── Public record types ───────────────────────────────────────────────────────

/// One event row for the mention-market strategy.
///
/// `action` values : ENTRY | TAKE_PROFIT | STOP_LOSS | TIME_EXIT | NO_TRADE
/// `reason_code`   : EDGE_OK | EDGE_TOO_LOW | SPREAD_TOO_WIDE | DEPTH_TOO_THIN | TIME_EXIT
#[derive(Debug, Clone)]
pub struct MentionDryRunTrade {
    pub strategy_id: String,
    /// Unique id tying entry/exit rows for the same position (nanosecond ts string)
    pub event_id: String,
    pub market_slug: String,
    pub speaker: String,        // "trump"
    pub keyword: String,        // matched keyword phrase
    pub side: String,           // "YES" / "NO"
    pub action: String,         // ENTRY / TAKE_PROFIT / STOP_LOSS / TIME_EXIT / NO_TRADE
    pub price: f64,
    pub size_usdc: f64,
    pub spread_at_decision: Option<f64>,
    pub depth_usdc_at_decision: Option<f64>,
    /// Price at which this position was opened (None for ENTRY rows itself)
    pub entry_price: Option<f64>,
    pub exit_price: Option<f64>,
    pub hold_sec: Option<i64>,
    pub taker_fee_bps: Option<i64>,
    pub slippage_buffer_bps: Option<i64>,
    pub execution_risk_bps: Option<i64>,
    pub expected_net_edge_bps: Option<f64>,
    pub realized_pnl_usdc: Option<f64>,
    pub reason_code: String,
    pub note: Option<String>,
}

/// One event row for the weather-market strategy.
///
/// `action` values : ENTRY | TAKE_PROFIT | STOP_LOSS | FORECAST_SHIFT | TIME_DECAY_EXIT | NO_TRADE
#[derive(Debug, Clone)]
pub struct WeatherDryRunTrade {
    pub strategy_id: String,
    /// Unique id tying entry/exit rows for the same position
    pub event_id: String,
    pub market_slug: String,
    pub city: String,
    /// "temp_range" | "extreme" | "precip"
    pub market_type: String,
    /// "YES" | "NO"
    pub side: String,
    /// ENTRY | TAKE_PROFIT | STOP_LOSS | FORECAST_SHIFT | TIME_DECAY_EXIT | NO_TRADE
    pub action: String,
    pub price: f64,
    pub size_usdc: f64,
    pub spread_at_decision: Option<f64>,
    pub depth_usdc_at_decision: Option<f64>,
    pub entry_price: Option<f64>,
    pub exit_price: Option<f64>,
    pub hold_sec: Option<i64>,
    /// Forecast model used at entry ("gfs" | "ecmwf" | "ensemble")
    pub model: String,
    /// Model probability of YES outcome at entry (0.0–1.0)
    pub p_yes_at_entry: Option<f64>,
    /// Model probability of YES outcome at exit (for FORECAST_SHIFT analysis)
    pub p_yes_at_exit: Option<f64>,
    /// Days from today to the market's resolution date at entry
    pub lead_days: Option<i64>,
    pub taker_fee_bps: Option<i64>,
    pub slippage_buffer_bps: Option<i64>,
    pub expected_net_edge_bps: Option<f64>,
    pub realized_pnl_usdc: Option<f64>,
    pub reason_code: String,
    pub note: Option<String>,
}

#[derive(Debug, Clone)]
pub struct DryRunTrade {
    pub strategy_id: String,
    pub market_slug: String,
    pub leg: i32,
    pub side: String,
    pub price: f64,
    pub size_usdc: f64,
    /// Taker fee + gas fee（USDC）
    pub fee_usdc: f64,
    pub signal_dump_pct: Option<f64>,
    pub hedge_sum: Option<f64>,
    pub would_profit: Option<f64>,
}

#[derive(Debug, Clone)]
pub struct LiveTrade {
    pub strategy_id: String,
    pub market_slug: String,
    pub leg: i32,
    pub side: String,
    pub order_id: String,
    pub price: f64,
    pub size_usdc: f64,
    pub fee_usdc: f64,
    pub filled_usdc: Option<f64>,
    pub status: String,
    pub tx_hash: Option<String>,
}

#[derive(Debug, Clone)]
pub struct CycleResult {
    pub strategy_id: String,
    pub market_slug: String,
    pub mode: String,
    pub leg1_side: Option<String>,
    pub leg1_price: Option<f64>,
    pub leg2_price: Option<f64>,
    pub resolved_winner: Option<String>,
    pub pnl_usdc: Option<f64>,
}

// ── DbWriter ──────────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct DbWriter {
    conn: Arc<Mutex<Connection>>,
}

impl DbWriter {
    pub fn open(path: &str) -> Result<Self, crate::error::AppError> {
        let conn = Connection::open(path)?;
        init_schema(&conn)?;
        migrate_add_columns(&conn)?;
        Ok(DbWriter {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    // ── Async write helpers ───────────────────────────────────────────────────

    pub async fn write_mention_dry_run_trade(
        &self,
        trade: &MentionDryRunTrade,
    ) -> Result<(), crate::error::AppError> {
        let conn = Arc::clone(&self.conn);
        let trade = trade.clone();
        tokio::task::spawn_blocking(move || -> Result<(), rusqlite::Error> {
            let conn = conn.lock().expect("SQLite mutex poisoned");
            conn.execute(
                "INSERT INTO mention_dry_run_trades \
                 (strategy_id, event_id, market_slug, speaker, keyword, side, action, \
                  price, size_usdc, spread_at_decision, depth_usdc_at_decision, \
                  entry_price, exit_price, hold_sec, \
                  taker_fee_bps, slippage_buffer_bps, execution_risk_bps, \
                  expected_net_edge_bps, realized_pnl_usdc, reason_code, note) \
                 VALUES \
                 (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21)",
                rusqlite::params![
                    trade.strategy_id,
                    trade.event_id,
                    trade.market_slug,
                    trade.speaker,
                    trade.keyword,
                    trade.side,
                    trade.action,
                    trade.price,
                    trade.size_usdc,
                    trade.spread_at_decision,
                    trade.depth_usdc_at_decision,
                    trade.entry_price,
                    trade.exit_price,
                    trade.hold_sec,
                    trade.taker_fee_bps,
                    trade.slippage_buffer_bps,
                    trade.execution_risk_bps,
                    trade.expected_net_edge_bps,
                    trade.realized_pnl_usdc,
                    trade.reason_code,
                    trade.note,
                ],
            )?;
            Ok(())
        })
        .await
        .map_err(|e| crate::error::AppError::Other(e.to_string()))?
        .map_err(crate::error::AppError::DbError)
    }

    pub async fn write_weather_dry_run_trade(
        &self,
        trade: &WeatherDryRunTrade,
    ) -> Result<(), crate::error::AppError> {
        let conn = Arc::clone(&self.conn);
        let trade = trade.clone();
        tokio::task::spawn_blocking(move || -> Result<(), rusqlite::Error> {
            let conn = conn.lock().expect("SQLite mutex poisoned");
            conn.execute(
                "INSERT INTO weather_dry_run_trades \
                 (strategy_id, event_id, market_slug, city, market_type, side, action, \
                  price, size_usdc, spread_at_decision, depth_usdc_at_decision, \
                  entry_price, exit_price, hold_sec, model, p_yes_at_entry, p_yes_at_exit, \
                  lead_days, taker_fee_bps, slippage_buffer_bps, \
                  expected_net_edge_bps, realized_pnl_usdc, reason_code, note) \
                 VALUES \
                 (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,\
                  ?18,?19,?20,?21,?22,?23,?24)",
                rusqlite::params![
                    trade.strategy_id,
                    trade.event_id,
                    trade.market_slug,
                    trade.city,
                    trade.market_type,
                    trade.side,
                    trade.action,
                    trade.price,
                    trade.size_usdc,
                    trade.spread_at_decision,
                    trade.depth_usdc_at_decision,
                    trade.entry_price,
                    trade.exit_price,
                    trade.hold_sec,
                    trade.model,
                    trade.p_yes_at_entry,
                    trade.p_yes_at_exit,
                    trade.lead_days,
                    trade.taker_fee_bps,
                    trade.slippage_buffer_bps,
                    trade.expected_net_edge_bps,
                    trade.realized_pnl_usdc,
                    trade.reason_code,
                    trade.note,
                ],
            )?;
            Ok(())
        })
        .await
        .map_err(|e| crate::error::AppError::Other(e.to_string()))?
        .map_err(crate::error::AppError::DbError)
    }

    pub async fn write_dry_run_trade(
        &self,
        trade: &DryRunTrade,
    ) -> Result<(), crate::error::AppError> {
        let conn = Arc::clone(&self.conn);
        let trade = trade.clone();
        tokio::task::spawn_blocking(move || -> Result<(), rusqlite::Error> {
            let conn = conn.lock().expect("SQLite mutex poisoned");
            conn.execute(
                "INSERT INTO dry_run_trades \
                 (strategy_id, market_slug, leg, side, price, size_usdc, fee_usdc, \
                  signal_dump_pct, hedge_sum, would_profit) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                rusqlite::params![
                    trade.strategy_id,
                    trade.market_slug,
                    trade.leg,
                    trade.side,
                    trade.price,
                    trade.size_usdc,
                    trade.fee_usdc,
                    trade.signal_dump_pct,
                    trade.hedge_sum,
                    trade.would_profit,
                ],
            )?;
            Ok(())
        })
        .await
        .map_err(|e| crate::error::AppError::Other(e.to_string()))?
        .map_err(crate::error::AppError::DbError)
    }

    pub async fn write_cycle_result(
        &self,
        result: &CycleResult,
    ) -> Result<(), crate::error::AppError> {
        let conn = Arc::clone(&self.conn);
        let result = result.clone();
        tokio::task::spawn_blocking(move || -> Result<(), rusqlite::Error> {
            let conn = conn.lock().expect("SQLite mutex poisoned");
            conn.execute(
                "INSERT INTO cycle_results \
                 (strategy_id, market_slug, mode, leg1_side, leg1_price, \
                  leg2_price, resolved_winner, pnl_usdc) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                rusqlite::params![
                    result.strategy_id,
                    result.market_slug,
                    result.mode,
                    result.leg1_side,
                    result.leg1_price,
                    result.leg2_price,
                    result.resolved_winner,
                    result.pnl_usdc,
                ],
            )?;
            Ok(())
        })
        .await
        .map_err(|e| crate::error::AppError::Other(e.to_string()))?
        .map_err(crate::error::AppError::DbError)
    }

    pub async fn write_live_trade(
        &self,
        trade: &LiveTrade,
    ) -> Result<(), crate::error::AppError> {
        let conn = Arc::clone(&self.conn);
        let trade = trade.clone();
        tokio::task::spawn_blocking(move || -> Result<(), rusqlite::Error> {
            let conn = conn.lock().expect("SQLite mutex poisoned");
            conn.execute(
                "INSERT INTO live_trades \
                 (strategy_id, market_slug, leg, side, order_id, price, \
                  size_usdc, fee_usdc, filled_usdc, status, tx_hash) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                rusqlite::params![
                    trade.strategy_id,
                    trade.market_slug,
                    trade.leg,
                    trade.side,
                    trade.order_id,
                    trade.price,
                    trade.size_usdc,
                    trade.fee_usdc,
                    trade.filled_usdc,
                    trade.status,
                    trade.tx_hash,
                ],
            )?;
            Ok(())
        })
        .await
        .map_err(|e| crate::error::AppError::Other(e.to_string()))?
        .map_err(crate::error::AppError::DbError)
    }

    // ── Stats helpers ─────────────────────────────────────────────────────────

    pub async fn count_dry_run_trades(&self) -> Result<i64, crate::error::AppError> {
        let conn = Arc::clone(&self.conn);
        tokio::task::spawn_blocking(move || -> Result<i64, rusqlite::Error> {
            let conn = conn.lock().expect("SQLite mutex poisoned");
            conn.query_row("SELECT COUNT(*) FROM dry_run_trades", [], |row| row.get(0))
        })
        .await
        .map_err(|e| crate::error::AppError::Other(e.to_string()))?
        .map_err(crate::error::AppError::DbError)
    }

    pub async fn count_cycle_results(&self) -> Result<i64, crate::error::AppError> {
        let conn = Arc::clone(&self.conn);
        tokio::task::spawn_blocking(move || -> Result<i64, rusqlite::Error> {
            let conn = conn.lock().expect("SQLite mutex poisoned");
            conn.query_row("SELECT COUNT(*) FROM cycle_results", [], |row| row.get(0))
        })
        .await
        .map_err(|e| crate::error::AppError::Other(e.to_string()))?
        .map_err(crate::error::AppError::DbError)
    }

    #[cfg(test)]
    pub(crate) async fn count_records(
        &self,
        table: &str,
    ) -> Result<i64, crate::error::AppError> {
        let conn = Arc::clone(&self.conn);
        let sql = format!("SELECT COUNT(*) FROM {table}");
        tokio::task::spawn_blocking(move || -> Result<i64, rusqlite::Error> {
            let conn = conn.lock().expect("SQLite mutex poisoned");
            conn.query_row(&sql, [], |row| row.get(0))
        })
        .await
        .map_err(|e| crate::error::AppError::Other(e.to_string()))?
        .map_err(crate::error::AppError::DbError)
    }
}

// ── Schema ────────────────────────────────────────────────────────────────────

fn init_schema(conn: &Connection) -> Result<(), rusqlite::Error> {
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS markets (
            slug        TEXT PRIMARY KEY,
            window_ts   INTEGER NOT NULL,
            close_ts    INTEGER NOT NULL,
            open_up     REAL,
            open_down   REAL,
            created_at  DATETIME DEFAULT CURRENT_TIMESTAMP
        );

        CREATE TABLE IF NOT EXISTS dry_run_trades (
            id              INTEGER PRIMARY KEY,
            ts              DATETIME DEFAULT CURRENT_TIMESTAMP,
            strategy_id     TEXT NOT NULL DEFAULT 'default',
            market_slug     TEXT NOT NULL,
            leg             INTEGER NOT NULL,
            side            TEXT NOT NULL,
            price           REAL NOT NULL,
            size_usdc       REAL NOT NULL,
            fee_usdc        REAL NOT NULL DEFAULT 0,
            signal_dump_pct REAL,
            hedge_sum       REAL,
            would_profit    REAL
        );

        CREATE TABLE IF NOT EXISTS live_trades (
            id          INTEGER PRIMARY KEY,
            ts          DATETIME DEFAULT CURRENT_TIMESTAMP,
            strategy_id TEXT NOT NULL DEFAULT 'default',
            market_slug TEXT NOT NULL,
            leg         INTEGER NOT NULL,
            side        TEXT NOT NULL,
            order_id    TEXT,
            price       REAL NOT NULL,
            size_usdc   REAL NOT NULL,
            fee_usdc    REAL NOT NULL DEFAULT 0,
            filled_usdc REAL,
            status      TEXT NOT NULL,
            tx_hash     TEXT
        );

        CREATE TABLE IF NOT EXISTS cycle_results (
            id              INTEGER PRIMARY KEY,
            strategy_id     TEXT NOT NULL DEFAULT 'default',
            market_slug     TEXT NOT NULL,
            mode            TEXT NOT NULL,
            leg1_side       TEXT,
            leg1_price      REAL,
            leg2_price      REAL,
            resolved_winner TEXT,
            pnl_usdc        REAL,
            created_at      DATETIME DEFAULT CURRENT_TIMESTAMP
        );

        CREATE TABLE IF NOT EXISTS mention_dry_run_trades (
            id                      INTEGER PRIMARY KEY,
            ts                      DATETIME DEFAULT CURRENT_TIMESTAMP,
            strategy_id             TEXT NOT NULL,
            event_id                TEXT NOT NULL,
            market_slug             TEXT NOT NULL,
            speaker                 TEXT NOT NULL DEFAULT 'trump',
            keyword                 TEXT NOT NULL,
            side                    TEXT NOT NULL,
            action                  TEXT NOT NULL,
            price                   REAL NOT NULL,
            size_usdc               REAL NOT NULL,
            spread_at_decision      REAL,
            depth_usdc_at_decision  REAL,
            entry_price             REAL,
            exit_price              REAL,
            hold_sec                INTEGER,
            taker_fee_bps           INTEGER,
            slippage_buffer_bps     INTEGER,
            execution_risk_bps      INTEGER,
            expected_net_edge_bps   REAL,
            realized_pnl_usdc       REAL,
            reason_code             TEXT NOT NULL,
            note                    TEXT
        );

        CREATE TABLE IF NOT EXISTS weather_dry_run_trades (
            id                      INTEGER PRIMARY KEY,
            ts                      DATETIME DEFAULT CURRENT_TIMESTAMP,
            strategy_id             TEXT NOT NULL,
            event_id                TEXT NOT NULL,
            market_slug             TEXT NOT NULL,
            city                    TEXT NOT NULL,
            market_type             TEXT NOT NULL,
            side                    TEXT NOT NULL,
            action                  TEXT NOT NULL,
            price                   REAL NOT NULL,
            size_usdc               REAL NOT NULL,
            spread_at_decision      REAL,
            depth_usdc_at_decision  REAL,
            entry_price             REAL,
            exit_price              REAL,
            hold_sec                INTEGER,
            model                   TEXT NOT NULL DEFAULT 'gfs',
            p_yes_at_entry          REAL,
            p_yes_at_exit           REAL,
            lead_days               INTEGER,
            taker_fee_bps           INTEGER,
            slippage_buffer_bps     INTEGER,
            expected_net_edge_bps   REAL,
            realized_pnl_usdc       REAL,
            reason_code             TEXT NOT NULL,
            note                    TEXT
        );

        CREATE TABLE IF NOT EXISTS strategy_versions (
            id               INTEGER PRIMARY KEY,
            version          TEXT NOT NULL,
            params_json      TEXT NOT NULL,
            backtest_sharpe  REAL,
            backtest_winrate REAL,
            deployed_at      DATETIME,
            retired_at       DATETIME
        );
        ",
    )
}

/// 舊資料庫遷移：為既有資料表補上新欄位。
fn migrate_add_columns(conn: &Connection) -> Result<(), rusqlite::Error> {
    let migrations: &[(&str, &str)] = &[
        ("dry_run_trades",  "strategy_id TEXT NOT NULL DEFAULT 'default'"),
        ("dry_run_trades",  "fee_usdc REAL NOT NULL DEFAULT 0"),
        ("live_trades",     "strategy_id TEXT NOT NULL DEFAULT 'default'"),
        ("live_trades",     "fee_usdc REAL NOT NULL DEFAULT 0"),
        ("cycle_results",   "strategy_id TEXT NOT NULL DEFAULT 'default'"),
    ];
    for (table, col_def) in migrations {
        let col_name = col_def.split_whitespace().next().unwrap_or("");
        let sql = format!("ALTER TABLE {table} ADD COLUMN {col_def}");
        match conn.execute_batch(&sql) {
            Ok(_) => {}
            Err(e)
                if e.to_string().contains("duplicate column name")
                    || e.to_string().contains(col_name) =>
            {
                // 欄位已存在，忽略
            }
            Err(e) => return Err(e),
        }
    }
    Ok(())
}
