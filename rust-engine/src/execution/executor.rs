use uuid::Uuid;

#[cfg(test)]
use once_cell::sync::Lazy;
#[cfg(test)]
use std::sync::Mutex;

use crate::api::clob::ClobClient;
use crate::config::BotConfig;
use crate::db::writer::{DbWriter, DryRunTrade};
use crate::error::AppError;
use crate::risk::capital::SharedCapital;

// ── Public types ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct OrderIntent {
    pub strategy_id: String,
    pub market_slug: String,
    pub token_id: String,
    pub side: String,
    /// Limit price（0–1 Polymarket binary token）
    pub price: f64,
    /// 下注金額（USDC，由 CapitalTracker 決定，已動態計算）
    pub size_usdc: f64,
    /// 此訂單的預計費用（taker fee + gas），由呼叫方計算後填入
    pub fee_usdc: f64,
    /// 1 = Leg 1, 2 = Leg 2
    pub leg: i32,
    pub signal_dump_pct: Option<f64>,
    pub hedge_sum: Option<f64>,
}

#[derive(Debug)]
pub enum OrderResult {
    Simulated { order_id: String },
    Filled { order_id: String, tx: String },
}

#[cfg(test)]
#[derive(Debug, Clone)]
pub enum MockSubmitOrderOutcome {
    Success,
    Failure(String),
}

#[cfg(test)]
static MOCK_SUBMIT_ORDER_OUTCOME: Lazy<Mutex<Option<MockSubmitOrderOutcome>>> =
    Lazy::new(|| Mutex::new(None));

#[cfg(test)]
static MOCK_LAST_ORDER_INTENT: Lazy<Mutex<Option<OrderIntent>>> =
    Lazy::new(|| Mutex::new(None));

#[cfg(test)]
pub fn set_mock_submit_order_outcome(outcome: Option<MockSubmitOrderOutcome>) {
    *MOCK_SUBMIT_ORDER_OUTCOME
        .lock()
        .expect("mock submit outcome mutex poisoned") = outcome;
    *MOCK_LAST_ORDER_INTENT
        .lock()
        .expect("mock last intent mutex poisoned") = None;
}

#[cfg(test)]
pub fn take_mock_last_order_intent() -> Option<OrderIntent> {
    MOCK_LAST_ORDER_INTENT
        .lock()
        .expect("mock last intent mutex poisoned")
        .take()
}

// ── Executor ──────────────────────────────────────────────────────────────────

pub struct Executor {
    pub config: BotConfig,
    db: DbWriter,
    /// 每策略的資金追蹤器（用於資金紀錄）
    capital: SharedCapital,
}

impl Executor {
    pub fn new(config: BotConfig, db: DbWriter, capital: SharedCapital) -> Self {
        Executor { config, db, capital }
    }

    pub async fn submit(&self, intent: &OrderIntent) -> Result<OrderResult, AppError> {
        submit_order(intent, &self.config, &self.db, &self.capital).await
    }
}

// ── Entry point ───────────────────────────────────────────────────────────────

/// 所有訂單的唯一入口。
///
/// **DRY_RUN**：模擬下單 → 寫 DB → 更新資金追蹤器 → 不碰 CLOB
/// **Live**：Phase 3 前返回 NotImplemented
pub async fn submit_order(
    intent: &OrderIntent,
    config: &BotConfig,
    db: &DbWriter,
    capital: &SharedCapital,
) -> Result<OrderResult, AppError> {
    #[cfg(test)]
    {
        if let Some(outcome) = MOCK_SUBMIT_ORDER_OUTCOME
            .lock()
            .expect("mock submit outcome mutex poisoned")
            .clone()
        {
            *MOCK_LAST_ORDER_INTENT
                .lock()
                .expect("mock last intent mutex poisoned") = Some(intent.clone());
            return match outcome {
                MockSubmitOrderOutcome::Success => Ok(OrderResult::Filled {
                    order_id: "mock-order-id".to_string(),
                    tx: "mock-filled".to_string(),
                }),
                MockSubmitOrderOutcome::Failure(msg) => {
                    Err(AppError::ApiError(format!("mock submit failure: {msg}")))
                }
            };
        }
    }

    if config.is_dry_run() {
        let order_id = Uuid::new_v4().to_string();

        tracing::info!(
            "[DRY_RUN] 模擬提交: order_id={order_id}  strategy={}  slug={}  leg={}  \
             side={}  price={:.4}  size={:.4} USDC  fee={:.4} USDC",
            intent.strategy_id,
            intent.market_slug,
            intent.leg,
            intent.side,
            intent.price,
            intent.size_usdc,
            intent.fee_usdc,
        );

        let trade = DryRunTrade {
            strategy_id: intent.strategy_id.clone(),
            market_slug: intent.market_slug.clone(),
            leg: intent.leg,
            side: intent.side.clone(),
            price: intent.price,
            size_usdc: intent.size_usdc,
            fee_usdc: intent.fee_usdc,
            signal_dump_pct: intent.signal_dump_pct,
            hedge_sum: intent.hedge_sum,
            would_profit: None,
        };

        db.write_dry_run_trade(&trade).await?;

        // 更新資金追蹤器：鎖定下注金額 + 扣除費用
        capital
            .lock()
            .expect("capital mutex poisoned")
            .on_order_submit(intent.size_usdc, intent.fee_usdc);

        return Ok(OrderResult::Simulated { order_id });
    }

    // ── Live path ─────────────────────────────────────────────────────────────
    let client = ClobClient::new(config.clone());
    let result = client.submit_live_order(intent, db).await?;
    Ok(OrderResult::Filled {
        order_id: result.order_id,
        tx: result.status,
    })
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{BotConfig, StrategyConfig, StrategyType, TradingMode};
    use crate::db::writer::DbWriter;
    use crate::risk::capital;

    fn dry_run_config() -> BotConfig {
        BotConfig {
            mode: TradingMode::DryRun,
            dry_run: true,
            monitor_window_sec: 120,
            abort_before_close_sec: 30,
            gamma_base: String::new(),
            clob_base: String::new(),
            ws_market_url: String::new(),
            binance_ws_url: String::new(),
            chain_id: 137,
            polygon_rpc_url: String::new(),
            orders_per_second: 60,
            books_per_10s: 5,
            max_ws_connections: 5,
            retry_base_ms: 1000,
            retry_max_attempts: 3,
            max_daily_loss_usdc: 50.0,
            circuit_breaker_loss_pct: 0.20,
            initial_capital_usdc: 500.0,
            taker_fee_pct: 0.018,
            gas_fee_usdc: 0.005,
            polygon_private_key: String::new(),
            clob_api_key: String::new(),
            clob_api_secret: String::new(),
            clob_api_passphrase: String::new(),
            db_path: ":memory:".to_string(),
            telegram_bot_token: String::new(),
            strategies: vec![],
        }
    }

    fn test_capital() -> SharedCapital {
        capital::new_shared(&StrategyConfig {
            id: "test".to_string(),
            strategy_type: StrategyType::DumpHedge,
            enabled: true,
            dump_threshold_pct: 0.15,
            hedge_wait_limit_sec: 180,
            hedge_threshold_sum: 0.93,
            min_token_price: 0.05,
            max_token_price: 0.95,
            capital_allocation_pct: 0.20,
            trade_size_pct: 0.10,
            max_drawdown_pct: 0.30,
            initial_allocated_usdc: 100.0,
            direction_mode: String::new(),
            active_keyword_count: 0,
            entry_no_min_price: 0.0,
            entry_yes_max_price: 0.0,
            take_profit_no_price: 0.0,
            take_profit_yes_price: 0.0,
            stop_loss_delta: 0.0,
            taker_fee_bps: 0.0,
            slippage_buffer_bps: 0.0,
            execution_risk_bps: 0.0,
            min_net_edge_bps: 0.0,
            max_spread: 0.0,
            min_model_confidence: 0.60,
            weather_min_depth_usdc: 50.0,
            weather_min_lead_days: 1,
            weather_max_lead_days: 14,
            city_whitelist: vec![],
            weather_forecast_model: "gfs".to_string(),
            forecast_shift_threshold: 0.15,
            consensus_max_divergence: 0.10,
            loop_interval_sec: 60,
        })
    }

    #[tokio::test]
    async fn dry_run_intercept_writes_to_db() {
        let db = DbWriter::open(":memory:").expect("open in-memory DB");
        let config = dry_run_config();
        let cap = test_capital();

        let intent = OrderIntent {
            strategy_id: "test_strategy".to_string(),
            market_slug: "btc-updown-15m-test".to_string(),
            token_id: "token_up_123".to_string(),
            side: "BUY".to_string(),
            price: 0.40,
            size_usdc: 10.0,
            fee_usdc: 0.185,
            leg: 1,
            signal_dump_pct: Some(0.18),
            hedge_sum: None,
        };

        let result = submit_order(&intent, &config, &db, &cap)
            .await
            .expect("submit_order must not fail in dry_run mode");

        assert!(matches!(result, OrderResult::Simulated { .. }));

        let count = db.count_records("dry_run_trades").await.expect("count");
        assert_eq!(count, 1);

        // 確認資金追蹤器已扣除費用
        let tracker = cap.lock().unwrap();
        assert!((tracker.total_fees_paid - 0.185).abs() < 1e-9);
    }

    #[tokio::test]
    async fn live_path_returns_error_without_credentials() {
        // Live path is implemented (Phase 3); without real credentials it
        // will fail with an auth/network error rather than NotImplemented.
        let db = DbWriter::open(":memory:").expect("open in-memory DB");
        let mut config = dry_run_config();
        config.mode = TradingMode::Live;
        config.dry_run = false;
        let cap = test_capital();

        let intent = OrderIntent {
            strategy_id: "test".to_string(),
            market_slug: "test".to_string(),
            token_id: "token".to_string(),
            side: "BUY".to_string(),
            price: 0.40,
            size_usdc: 10.0,
            fee_usdc: 0.185,
            leg: 1,
            signal_dump_pct: None,
            hedge_sum: None,
        };

        // Without a running CLOB endpoint this must fail with some error
        let result = submit_order(&intent, &config, &db, &cap).await;
        assert!(result.is_err(), "live path without credentials must return Err");
    }
}
