use serde::Deserialize;
use std::env;

// ── Private TOML-shaped structs (serde deserialize only) ──────────────────────

#[derive(Debug, Clone, Deserialize)]
struct GlobalSection {
    mode: String,
    monitor_window_sec: u64,
    abort_before_close_sec: u64,
}

#[derive(Debug, Clone, Deserialize)]
struct ApiSection {
    gamma_base: String,
    clob_base: String,
    ws_market_url: String,
    binance_ws_url: String,
    chain_id: u64,
    polygon_rpc_url: String,
}

#[derive(Debug, Clone, Deserialize)]
struct RateLimitsSection {
    orders_per_second: u64,
    books_per_10s: u64,
    max_ws_connections: u64,
    retry_base_ms: u64,
    retry_max_attempts: u64,
}

#[derive(Debug, Clone, Deserialize)]
struct RiskSection {
    max_daily_loss_usdc: f64,
    circuit_breaker_loss_pct: f64,
}

#[derive(Debug, Clone, Deserialize)]
struct CapitalSection {
    /// 模擬總資金（dry_run）；live 啟動時從鏈上查詢後覆蓋
    initial_capital_usdc: f64,
    /// Polymarket taker fee，最高 1.80%
    taker_fee_pct: f64,
    /// 每筆交易 Polygon gas 費（USDC）
    gas_fee_usdc: f64,
}

#[derive(Debug, Clone, Deserialize)]
struct RawStrategyConfig {
    id: String,
    #[serde(rename = "type")]
    strategy_type: String,
    enabled: bool,
    dump_threshold_pct: Option<f64>,
    hedge_wait_limit_sec: Option<u64>,
    hedge_threshold_sum: Option<f64>,  // Optional for mention
    /// 使用 initial_capital 的幾%
    capital_allocation_pct: f64,
    /// 每次下注使用可用資金的幾%
    trade_size_pct: f64,
    /// 虧損達此比例停止入場
    max_drawdown_pct: f64,
    min_token_price: Option<f64>,
    max_token_price: Option<f64>,
    // ── Mention-specific fields ────────────────────────────────────────────
    direction_mode: Option<String>,           // "no_only", "yes_only", etc.
    active_keyword_count: Option<usize>,
    entry_no_min_price: Option<f64>,
    entry_yes_max_price: Option<f64>,
    take_profit_no_price: Option<f64>,
    take_profit_yes_price: Option<f64>,
    stop_loss_delta: Option<f64>,
    taker_fee_bps: Option<f64>,
    slippage_buffer_bps: Option<f64>,
    execution_risk_bps: Option<f64>,
    min_net_edge_bps: Option<f64>,
    max_spread: Option<f64>,
    // ── Weather-specific fields ────────────────────────────────────────────
    /// Minimum model probability required to trade either side (default 0.60)
    min_model_confidence: Option<f64>,
    /// Minimum order-book depth in USDC for weather markets (default 50.0)
    weather_min_depth_usdc: Option<f64>,
    /// Minimum forecast lead time in days (default 1)
    weather_min_lead_days: Option<u32>,
    /// Maximum forecast lead time in days (default 14)
    weather_max_lead_days: Option<u32>,
    /// Cities to trade; empty = all supported cities (default [])
    city_whitelist: Option<Vec<String>>,
    /// Forecast model used by this weather strategy (default "gfs")
    weather_forecast_model: Option<String>,
    /// Exit position when model p_yes shifts by this much (default 0.15)
    forecast_shift_threshold: Option<f64>,
    /// Max allowed probability divergence across consensus models (default 0.10)
    consensus_max_divergence: Option<f64>,
    /// Per-strategy loop interval in seconds (strategy-specific defaults)
    loop_interval_sec: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
struct RawSettings {
    global: GlobalSection,
    api: ApiSection,
    rate_limits: RateLimitsSection,
    risk: RiskSection,
    capital: CapitalSection,
    strategies: Vec<RawStrategyConfig>,
}

// ── Public types ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub enum TradingMode {
    DryRun,
    Live,
}

impl TradingMode {
    fn from_str(s: &str) -> Self {
        if s.eq_ignore_ascii_case("live") {
            TradingMode::Live
        } else {
            TradingMode::DryRun
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum StrategyType {
    DumpHedge,
    PureArb,
    Mention,
    Weather,
}

impl StrategyType {
    fn from_str(s: &str) -> Self {
        if s.eq_ignore_ascii_case("pure_arb") {
            StrategyType::PureArb
        } else if s.eq_ignore_ascii_case("mention") {
            StrategyType::Mention
        } else if s.eq_ignore_ascii_case("weather") {
            StrategyType::Weather
        } else {
            StrategyType::DumpHedge
        }
    }
}

/// 單一策略實例的可調控參數。
#[derive(Debug, Clone)]
pub struct StrategyConfig {
    pub id: String,
    pub strategy_type: StrategyType,
    pub enabled: bool,

    // ── DumpHedge 專用 ─────────────────────────────────────────────────────────
    pub dump_threshold_pct: f64,
    pub hedge_wait_limit_sec: u64,

    // ── 共用策略參數 ───────────────────────────────────────────────────────────
    pub hedge_threshold_sum: f64,
    pub min_token_price: f64,
    pub max_token_price: f64,

    // ── 資金管理 ───────────────────────────────────────────────────────────────
    /// 使用 initial_capital 的幾%
    pub capital_allocation_pct: f64,
    /// 每次下注使用可用資金的幾%（動態 bet size）
    pub trade_size_pct: f64,
    /// 虧損達此比例後停止入場
    pub max_drawdown_pct: f64,
    /// 預先計算：initial_capital × allocation_pct（由 BotConfig::load 填入）
    pub initial_allocated_usdc: f64,

    // ── Mention 專用 ──────────────────────────────────────────────────────────
    pub direction_mode: String,
    pub active_keyword_count: usize,
    pub entry_no_min_price: f64,
    pub entry_yes_max_price: f64,
    pub take_profit_no_price: f64,
    pub take_profit_yes_price: f64,
    pub stop_loss_delta: f64,
    pub taker_fee_bps: f64,
    pub slippage_buffer_bps: f64,
    pub execution_risk_bps: f64,
    pub min_net_edge_bps: f64,
    pub max_spread: f64,
    // ── Weather 專用 ──────────────────────────────────────────────────────────
    /// Minimum model probability required to trade either side
    pub min_model_confidence: f64,
    /// Minimum order-book depth in USDC for weather markets
    pub weather_min_depth_usdc: f64,
    /// Minimum forecast lead time in days
    pub weather_min_lead_days: u32,
    /// Maximum forecast lead time in days
    pub weather_max_lead_days: u32,
    /// Cities to trade (empty = all supported cities)
    pub city_whitelist: Vec<String>,
    /// Forecast model used by this strategy: "gfs" | "ecmwf" | "ensemble" | "nws" | "consensus" | "metar_short"
    pub weather_forecast_model: String,
    /// Exit position when |Δp_yes| exceeds this threshold
    pub forecast_shift_threshold: f64,
    /// Consensus mode only: max allowed probability divergence among models
    pub consensus_max_divergence: f64,
    /// Background loop interval in seconds for strategies with polling loops
    pub loop_interval_sec: u64,
}

#[derive(Debug, Clone)]
pub struct BotConfig {
    pub mode: TradingMode,
    pub dry_run: bool,

    // ── 全域交易參數 ───────────────────────────────────────────────────────────
    pub monitor_window_sec: u64,
    pub abort_before_close_sec: u64,

    // ── API 端點 ───────────────────────────────────────────────────────────────
    pub gamma_base: String,
    pub clob_base: String,
    pub ws_market_url: String,
    pub binance_ws_url: String,
    pub chain_id: u64,
    pub polygon_rpc_url: String,

    // ── 速率限制 ───────────────────────────────────────────────────────────────
    pub orders_per_second: u64,
    pub books_per_10s: u64,
    pub max_ws_connections: u64,
    pub retry_base_ms: u64,
    pub retry_max_attempts: u64,

    // ── 風控 ───────────────────────────────────────────────────────────────────
    pub max_daily_loss_usdc: f64,
    pub circuit_breaker_loss_pct: f64,

    // ── 資金管理（全域） ───────────────────────────────────────────────────────
    /// 總模擬資金（dry_run）；live 由鏈上查詢覆蓋
    pub initial_capital_usdc: f64,
    /// Taker fee 比例
    pub taker_fee_pct: f64,
    /// 每筆交易 gas 費（USDC）
    pub gas_fee_usdc: f64,

    // ── 憑證（來自 .env，不記 log）────────────────────────────────────────────
    pub polygon_private_key: String,
    pub clob_api_key: String,
    pub clob_api_secret: String,
    pub clob_api_passphrase: String,
    pub db_path: String,
    pub telegram_bot_token: String,

    // ── 策略清單 ───────────────────────────────────────────────────────────────
    pub strategies: Vec<StrategyConfig>,
}

impl BotConfig {
    pub fn load() -> Result<Self, crate::error::AppError> {
        let _ = dotenvy::dotenv();

        let config_path = format!("{}/../config/settings", env!("CARGO_MANIFEST_DIR"));

        let raw: RawSettings = config::Config::builder()
            .add_source(config::File::with_name(&config_path))
            .build()
            .map_err(|e| crate::error::AppError::ConfigError(e.to_string()))?
            .try_deserialize()
            .map_err(|e| crate::error::AppError::ConfigError(e.to_string()))?;

        let mode_str = env::var("TRADING_MODE").unwrap_or(raw.global.mode.clone());
        let mode = TradingMode::from_str(&mode_str);
        let initial_capital = raw.capital.initial_capital_usdc;

        let strategies = raw
            .strategies
            .into_iter()
            .map(|s| {
                let strat_type = StrategyType::from_str(&s.strategy_type);
                let default_loop_interval_sec = match strat_type {
                    StrategyType::Mention => 60,
                    StrategyType::Weather => 15 * 60,
                    StrategyType::DumpHedge | StrategyType::PureArb => 60,
                };
                StrategyConfig {
                    id: s.id,
                    strategy_type: strat_type.clone(),
                    enabled: s.enabled,
                    dump_threshold_pct: s.dump_threshold_pct.unwrap_or(0.0),
                    hedge_wait_limit_sec: s.hedge_wait_limit_sec.unwrap_or(180),
                    hedge_threshold_sum: s.hedge_threshold_sum.unwrap_or(0.93),
                    min_token_price: s.min_token_price.unwrap_or(0.05),
                    max_token_price: s.max_token_price.unwrap_or(0.95),
                    capital_allocation_pct: s.capital_allocation_pct,
                    trade_size_pct: s.trade_size_pct,
                    max_drawdown_pct: s.max_drawdown_pct,
                    initial_allocated_usdc: initial_capital * s.capital_allocation_pct,
                    // Mention-specific (use defaults if not specified)
                    direction_mode: s.direction_mode.unwrap_or_else(|| "no_first".to_string()),
                    active_keyword_count: s.active_keyword_count.unwrap_or(1),
                    entry_no_min_price: s.entry_no_min_price.unwrap_or(0.05),
                    entry_yes_max_price: s.entry_yes_max_price.unwrap_or(0.40),
                    take_profit_no_price: s.take_profit_no_price.unwrap_or(0.80),
                    take_profit_yes_price: s.take_profit_yes_price.unwrap_or(0.80),
                    stop_loss_delta: s.stop_loss_delta.unwrap_or(0.10),
                    taker_fee_bps: s.taker_fee_bps.unwrap_or(180.0),
                    slippage_buffer_bps: s.slippage_buffer_bps.unwrap_or(50.0),
                    execution_risk_bps: s.execution_risk_bps.unwrap_or(20.0),
                    min_net_edge_bps: s.min_net_edge_bps.unwrap_or(100.0),
                    max_spread: s.max_spread.unwrap_or(0.05),
                    // Weather-specific
                    min_model_confidence:     s.min_model_confidence.unwrap_or(0.60),
                    weather_min_depth_usdc:   s.weather_min_depth_usdc.unwrap_or(50.0),
                    weather_min_lead_days:    s.weather_min_lead_days.unwrap_or(1),
                    weather_max_lead_days:    s.weather_max_lead_days.unwrap_or(14),
                    city_whitelist:           s.city_whitelist.unwrap_or_default(),
                    weather_forecast_model:   s.weather_forecast_model.unwrap_or_else(|| "gfs".to_string()),
                    forecast_shift_threshold: s.forecast_shift_threshold.unwrap_or(0.15),
                    consensus_max_divergence: s.consensus_max_divergence.unwrap_or(0.10),
                    loop_interval_sec:        s.loop_interval_sec.unwrap_or(default_loop_interval_sec),
                }
            })
            .collect();

        Ok(BotConfig {
            dry_run: mode == TradingMode::DryRun,
            mode,

            monitor_window_sec: raw.global.monitor_window_sec,
            abort_before_close_sec: raw.global.abort_before_close_sec,

            gamma_base: raw.api.gamma_base,
            clob_base: raw.api.clob_base,
            ws_market_url: raw.api.ws_market_url,
            binance_ws_url: raw.api.binance_ws_url,
            chain_id: raw.api.chain_id,
            polygon_rpc_url: raw.api.polygon_rpc_url,

            orders_per_second: raw.rate_limits.orders_per_second,
            books_per_10s: raw.rate_limits.books_per_10s,
            max_ws_connections: raw.rate_limits.max_ws_connections,
            retry_base_ms: raw.rate_limits.retry_base_ms,
            retry_max_attempts: raw.rate_limits.retry_max_attempts,

            max_daily_loss_usdc: raw.risk.max_daily_loss_usdc,
            circuit_breaker_loss_pct: raw.risk.circuit_breaker_loss_pct,

            initial_capital_usdc: raw.capital.initial_capital_usdc,
            taker_fee_pct: raw.capital.taker_fee_pct,
            gas_fee_usdc: raw.capital.gas_fee_usdc,

            polygon_private_key: env::var("POLYGON_PRIVATE_KEY").unwrap_or_default(),
            clob_api_key: env::var("CLOB_API_KEY").unwrap_or_default(),
            clob_api_secret: env::var("CLOB_API_SECRET").unwrap_or_default(),
            clob_api_passphrase: env::var("CLOB_API_PASSPHRASE").unwrap_or_default(),
            db_path: env::var("DB_PATH")
                .unwrap_or_else(|_| "./data/market_snapshots.db".to_string()),
            telegram_bot_token: env::var("TELEGRAM_BOT_TOKEN").unwrap_or_default(),

            strategies,
        })
    }

    pub fn is_dry_run(&self) -> bool {
        self.mode == TradingMode::DryRun
    }

    pub fn is_live_confirmed(args: &[String]) -> bool {
        args.contains(&"--confirm-live".to_string())
    }

    /// 計算單筆訂單的費用（taker fee + gas）
    pub fn compute_fee(&self, size_usdc: f64) -> f64 {
        size_usdc * self.taker_fee_pct + self.gas_fee_usdc
    }
}
