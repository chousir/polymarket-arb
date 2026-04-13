// Weather market strategy — 15-minute scan + monitor loop.
//
// Architecture
// ────────────
// Each enabled Weather strategy runs as a background tokio task (spawned in
// main.rs), independent of the BTC 15m market cycle.
//
// Every 15 minutes the task:
//   1. Monitors existing open positions → writes exit records for:
//        TAKE_PROFIT, STOP_LOSS, TIME_DECAY_EXIT, FORECAST_SHIFT
//   2. Fetches all active weather markets (60 s TTL cache).
//   3. Applies weather_filter checks (city whitelist, lead time, depth, expiry).
//   4. Groups qualifying markets by city; fetches one forecast per city using
//      the strategy's configured model (GFS / ECMWF / Ensemble / NWS).
//      (Open-Meteo free API, no rate-limit concerns at 15-min intervals).
//   5. For each market: fetches YES/NO order books, evaluates edge via
//      weather_decision::evaluate().
//   6. Writes ENTRY / NO_TRADE records to weather_dry_run_trades (dry_run) or
//      submits a live order to the CLOB (live mode).
//
// Dynamic closing triggers
// ────────────────────────
//   TAKE_PROFIT      — held token bid ≥ take_profit_price
//   STOP_LOSS        — held token ask ≤ stop_loss_price (price moved against us)
//   TIME_DECAY_EXIT  — market closes within abort_before_close_sec
//   FORECAST_SHIFT   — fresh forecast from the configured model flips our signal
//
// DRY_RUN compliance
// ──────────────────
// Every order path checks is_dry_run() before touching the CLOB.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use chrono::{NaiveDate, Utc};
use once_cell::sync::Lazy;

use crate::api::weather::{city_info, metar, nws, openmeteo, WeatherForecast, WeatherModel};
use crate::api::weather_market::{fetch_weather_markets, WeatherMarket};
use crate::api::clob;
use crate::config::{BotConfig, StrategyConfig};
use crate::db::writer::{DbWriter, WeatherDryRunTrade};
use crate::error::AppError;
use crate::execution::executor;
use crate::risk::capital::SharedCapital;
use crate::strategy::weather_decision::{
    self, WeatherBookSnapshot, WeatherDecisionConfig, WeatherDirection, WeatherSignal,
};
use crate::strategy::weather_filter::{filter_market, WeatherFilterConfig};

// ── Constants ─────────────────────────────────────────────────────────────────

/// Forecast horizon to request from Open-Meteo (days).
/// Always fetch a bit beyond max_lead_days so we cover the full window.
const FORECAST_DAYS: u32 = 16;
const METAR_SHORT_MAX_TTE_SEC: u64 = 24 * 3600;
const MIN_LOOP_INTERVAL_SEC: u64 = 30;

static FORECAST_CACHE: Lazy<Mutex<HashMap<String, (Instant, Vec<WeatherForecast>)>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

// ── Open position ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct WeatherPosition {
    event_id: String,
    market_slug: String,
    /// CLOB token ID being held (YES or NO token)
    token_id: String,
    /// Full market record (needed for FORECAST_SHIFT re-evaluation)
    market: WeatherMarket,
    /// "YES" | "NO"
    side: String,
    entry_price: f64,
    entry_ts: i64,
    size_usdc: f64,
    /// Exit when held token bid ≥ this value
    take_profit_price: f64,
    /// Exit when held token ask ≤ this value
    stop_loss_price: f64,
    /// Model probability of YES at the time of entry (for FORECAST_SHIFT detection)
    p_yes_at_entry: f64,
    /// GFS / ECMWF / Ensemble / Consensus
    model: WeatherModel,
    /// Days from today to target date at the time of entry
    lead_days: i64,
    expected_net_edge_bps: f64,
}

#[derive(Debug, Clone)]
struct ConsensusCityForecasts {
    /// GFS is required; the consensus is skipped if it fails.
    gfs: Vec<WeatherForecast>,
    /// ECMWF is optional; consensus degrades gracefully to GFS+Ensemble if absent.
    ecmwf: Option<Vec<WeatherForecast>>,
    /// Ensemble is optional; consensus degrades gracefully to GFS+ECMWF if absent.
    ensemble: Option<Vec<WeatherForecast>>,
}

// ── Public strategy struct ────────────────────────────────────────────────────

pub struct WeatherStrategy {
    global: BotConfig,
    sc: StrategyConfig,
    capital: SharedCapital,
    forecast_model: WeatherModel,
}

impl WeatherStrategy {
    pub fn new(global: BotConfig, sc: StrategyConfig, capital: SharedCapital) -> Self {
        let forecast_model = parse_weather_model(&sc.weather_forecast_model);
        WeatherStrategy {
            global,
            sc,
            capital,
            forecast_model,
        }
    }

    /// Continuous 15-minute loop.  Runs forever inside `tokio::spawn`.
    pub async fn run_loop(&self, db: &DbWriter) {
        let loop_interval_sec = self.sc.loop_interval_sec.max(MIN_LOOP_INTERVAL_SEC);
        tracing::info!(
            "[Weather:{}] 啟動掃描循環  model={}  min_edge={:.0}bps  max_spread={:.4}  \
             confidence={:.2}  lead={}–{}d  cities=[{}]  shift_thr={:.2}  interval={}s",
            self.sc.id,
            self.forecast_model,
            self.sc.min_net_edge_bps,
            self.sc.max_spread,
            self.sc.min_model_confidence,
            self.sc.weather_min_lead_days,
            self.sc.weather_max_lead_days,
            if self.sc.city_whitelist.is_empty() {
                "all".to_string()
            } else {
                self.sc.city_whitelist.join(",")
            },
            self.sc.forecast_shift_threshold,
            loop_interval_sec,
        );

        let mut open_positions: Vec<WeatherPosition> = Vec::new();
        let mut interval = tokio::time::interval(Duration::from_secs(loop_interval_sec));

        loop {
            interval.tick().await;

            // ── Circuit-breaker gate ───────────────────────────────────────────
            {
                let cap = self.capital.lock().expect("capital mutex poisoned");
                if cap.is_stopped() {
                    tracing::warn!(
                        "[Weather:{}] ⛔ 停損觸發（capital={:.4}），暫停本輪掃描",
                        self.sc.id,
                        cap.current_capital,
                    );
                    continue;
                }
            }

            // ── 1. Monitor existing positions ──────────────────────────────────
            self.monitor_positions(&mut open_positions, db).await;

            // ── 2. Scan for new entries ────────────────────────────────────────
            match self.scan_for_entries(&open_positions, db).await {
                Ok(new_pos) => {
                    if !new_pos.is_empty() {
                        tracing::info!(
                            "[Weather:{}] 本輪新增 {} 個持倉  總持倉={}",
                            self.sc.id,
                            new_pos.len(),
                            open_positions.len() + new_pos.len(),
                        );
                    }
                    open_positions.extend(new_pos);
                }
                Err(e) => {
                    tracing::warn!("[Weather:{}] scan_for_entries 失敗: {e}", self.sc.id);
                }
            }
        }
    }

    // ── Monitor open positions ────────────────────────────────────────────────

    async fn monitor_positions(&self, positions: &mut Vec<WeatherPosition>, db: &DbWriter) {
        let now_ts = Utc::now().timestamp() as u64;
        let mut to_remove: Vec<usize> = Vec::new();

        // Prefetch fresh forecasts for each unique city in open positions
        // (one HTTP call per city, then re-used for all positions in that city)
        let cities_needed: HashSet<String> =
            positions.iter().map(|p| p.market.city.clone()).collect();
        let fresh_forecasts = if self.forecast_model == WeatherModel::Consensus {
            HashMap::new()
        } else {
            fetch_forecasts_for_cities(&cities_needed, self.forecast_model).await
        };
        let consensus_forecasts = if self.forecast_model == WeatherModel::Consensus {
            Some(fetch_consensus_forecasts_for_cities(&cities_needed).await)
        } else {
            None
        };

        for (idx, pos) in positions.iter().enumerate() {
            // ── TIME_DECAY_EXIT: market is about to close ─────────────────────
            let secs_to_close = pos.market.close_ts.saturating_sub(now_ts);
            if secs_to_close < self.global.abort_before_close_sec {
                let hold_sec = now_ts as i64 - pos.entry_ts;
                let realized_pnl = -self.global.compute_fee(pos.size_usdc);
                tracing::info!(
                    "[Weather:{}] TIME_DECAY_EXIT {} side={} secs_left={} hold={}s  pnl={:.4}",
                    self.sc.id, pos.market_slug, pos.side, secs_to_close, hold_sec, realized_pnl
                );
                if self.global.is_dry_run() {
                    self.write_exit(
                        db, pos, "TIME_DECAY_EXIT", pos.entry_price, hold_sec,
                        pos.p_yes_at_entry, realized_pnl, None,
                    ).await;
                    self.capital.lock().expect("capital mutex poisoned")
                        .on_cycle_end(Some(pos.entry_price), None, pos.size_usdc,
                                      self.global.compute_fee(pos.size_usdc));
                    to_remove.push(idx);
                    continue;
                }

                // Live: submit an actual SELL order before removing the position.
                let book = match clob::fetch_order_book(&self.global.clob_base, &pos.token_id).await {
                    Ok(b) => b,
                    Err(e) => {
                        tracing::warn!(
                            "[Weather:{}] TIME_DECAY_EXIT {} 取 book 失敗，延後平倉: {e}",
                            self.sc.id, pos.market_slug
                        );
                        continue;
                    }
                };
                let exit_price = book.best_bid;
                match self.submit_live_exit_order(pos, exit_price, db, "TIME_DECAY_EXIT").await {
                    Ok(()) => {
                        self.capital.lock().expect("capital mutex poisoned")
                            .on_cycle_end(Some(pos.entry_price), Some(exit_price), pos.size_usdc,
                                          self.global.compute_fee(pos.size_usdc));
                        to_remove.push(idx);
                    }
                    Err(e) => {
                        tracing::warn!(
                            "[Weather:{}] TIME_DECAY_EXIT {} live 平倉失敗，保留持倉: {e}",
                            self.sc.id, pos.market_slug
                        );
                    }
                }
                continue;
            }

            // ── Fetch current order book for this token ────────────────────────
            let book = match clob::fetch_order_book(&self.global.clob_base, &pos.token_id).await {
                Ok(b) => b,
                Err(e) => {
                    tracing::debug!(
                        "[Weather:{}] 監控 {}: 訂單簿獲取失敗: {e}",
                        self.sc.id, pos.market_slug
                    );
                    continue;
                }
            };

            // ── TAKE_PROFIT: bid reached the target ────────────────────────────
            if book.best_bid >= pos.take_profit_price {
                let hold_sec = now_ts as i64 - pos.entry_ts;
                let exit_price = book.best_bid;
                let realized_pnl = (exit_price - pos.entry_price) * pos.size_usdc
                    - self.global.compute_fee(pos.size_usdc) * 2.0;
                tracing::info!(
                    "[Weather:{}] TAKE_PROFIT {} side={} bid={:.4} tp={:.4}  \
                     hold={}s  pnl={:.4}",
                    self.sc.id, pos.market_slug, pos.side,
                    book.best_bid, pos.take_profit_price, hold_sec, realized_pnl
                );
                if self.global.is_dry_run() {
                    self.write_exit(
                        db, pos, "TAKE_PROFIT", exit_price, hold_sec,
                        pos.p_yes_at_entry, realized_pnl, None,
                    ).await;
                } else if let Err(e) = self.submit_live_exit_order(pos, exit_price, db, "TAKE_PROFIT").await {
                    tracing::warn!(
                        "[Weather:{}] TAKE_PROFIT {} live 平倉失敗，保留持倉: {e}",
                        self.sc.id, pos.market_slug
                    );
                    continue;
                }
                self.capital.lock().expect("capital mutex poisoned")
                    .on_cycle_end(Some(pos.entry_price), Some(exit_price), pos.size_usdc,
                                  self.global.compute_fee(pos.size_usdc));
                to_remove.push(idx);
                continue;
            }

            // ── STOP_LOSS: ask fell below the floor ────────────────────────────
            if book.best_ask <= pos.stop_loss_price {
                let hold_sec = now_ts as i64 - pos.entry_ts;
                let exit_price = book.best_bid;
                let realized_pnl = (exit_price - pos.entry_price) * pos.size_usdc
                    - self.global.compute_fee(pos.size_usdc) * 2.0;
                tracing::info!(
                    "[Weather:{}] STOP_LOSS {} side={} ask={:.4} floor={:.4}  \
                     hold={}s  pnl={:.4}",
                    self.sc.id, pos.market_slug, pos.side,
                    book.best_ask, pos.stop_loss_price, hold_sec, realized_pnl
                );
                if self.global.is_dry_run() {
                    self.write_exit(
                        db, pos, "STOP_LOSS", exit_price, hold_sec,
                        pos.p_yes_at_entry, realized_pnl, None,
                    ).await;
                } else if let Err(e) = self.submit_live_exit_order(pos, exit_price, db, "STOP_LOSS").await {
                    tracing::warn!(
                        "[Weather:{}] STOP_LOSS {} live 平倉失敗，保留持倉: {e}",
                        self.sc.id, pos.market_slug
                    );
                    continue;
                }
                self.capital.lock().expect("capital mutex poisoned")
                    .on_cycle_end(Some(pos.entry_price), Some(exit_price), pos.size_usdc,
                                  self.global.compute_fee(pos.size_usdc));
                to_remove.push(idx);
                continue;
            }

            // ── FORECAST_SHIFT: fresh model disagrees with our position ─────────
            let decision_cfg = self.make_decision_cfg(0.0); // depth=0 disables depth gate for re-eval
            let snapshot = WeatherBookSnapshot {
                yes_best_ask: book.best_ask,
                yes_best_bid: book.best_bid,
                no_best_ask:  1.0 - book.best_bid, // approximate
                no_best_bid:  1.0 - book.best_ask,
                depth_usdc:   book.depth_usdc,
            };

            let signal_opt = if self.forecast_model == WeatherModel::Consensus {
                consensus_forecasts
                    .as_ref()
                    .and_then(|all| all.get(&pos.market.city))
                    .and_then(|cf| evaluate_consensus_signal_for_market(
                        cf,
                        &pos.market,
                        &snapshot,
                        &decision_cfg,
                        self.sc.consensus_max_divergence,
                    ))
            } else {
                fresh_forecasts
                    .get(&pos.market.city)
                    .and_then(|city_forecasts| {
                        find_forecast_for_date(city_forecasts, pos.market.target_date)
                    })
                    .map(|forecast| weather_decision::evaluate(
                        forecast, &pos.market, &snapshot, &decision_cfg,
                    ))
            };

            if let Some(signal) = signal_opt {
                let direction_flipped = match pos.side.as_str() {
                    "YES" => signal.direction == WeatherDirection::BuyNo,
                    "NO"  => signal.direction == WeatherDirection::BuyYes,
                    _     => false,
                };
                let p_yes_shifted = (signal.p_yes - pos.p_yes_at_entry).abs()
                    >= self.sc.forecast_shift_threshold;

                if direction_flipped || p_yes_shifted {
                    let hold_sec = now_ts as i64 - pos.entry_ts;
                    let exit_price = book.best_bid; // sell at bid
                    let realized_pnl = (exit_price - pos.entry_price) * pos.size_usdc
                        - self.global.compute_fee(pos.size_usdc) * 2.0;
                    tracing::info!(
                        "[Weather:{}] FORECAST_SHIFT {} side={}  \
                         p_yes: {:.3}→{:.3} (delta={:.3})  direction_flip={}  \
                         hold={}s  pnl={:.4}",
                        self.sc.id, pos.market_slug, pos.side,
                        pos.p_yes_at_entry, signal.p_yes,
                        (signal.p_yes - pos.p_yes_at_entry).abs(),
                        direction_flipped,
                        hold_sec, realized_pnl
                    );
                    if self.global.is_dry_run() {
                        self.write_exit(
                            db, pos, "FORECAST_SHIFT", exit_price, hold_sec,
                            pos.p_yes_at_entry, realized_pnl, Some(signal.p_yes),
                        ).await;
                    } else if let Err(e) = self.submit_live_exit_order(pos, exit_price, db, "FORECAST_SHIFT").await {
                        tracing::warn!(
                            "[Weather:{}] FORECAST_SHIFT {} live 平倉失敗，保留持倉: {e}",
                            self.sc.id, pos.market_slug
                        );
                        continue;
                    }
                    self.capital.lock().expect("capital mutex poisoned")
                        .on_cycle_end(Some(pos.entry_price), Some(exit_price), pos.size_usdc,
                                      self.global.compute_fee(pos.size_usdc));
                    to_remove.push(idx);
                    continue;
                }
            }
        }

        // Remove closed positions in reverse order to preserve indices
        for idx in to_remove.into_iter().rev() {
            positions.swap_remove(idx);
        }
    }

    // ── Scan for new entries ──────────────────────────────────────────────────

    async fn scan_for_entries(
        &self,
        existing: &[WeatherPosition],
        db: &DbWriter,
    ) -> Result<Vec<WeatherPosition>, AppError> {
        let now_ts = Utc::now().timestamp() as u64;

        // 1. Fetch weather markets (60 s cache)
        let markets = match fetch_weather_markets().await {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!("[Weather:{}] 獲取市場失敗: {e}", self.sc.id);
                return Ok(Vec::new());
            }
        };
        tracing::debug!(
            "[Weather:{}] 獲取 {} 個天氣市場",
            self.sc.id, markets.len()
        );

        // 2. Build filter config from strategy settings
        let filter_cfg = WeatherFilterConfig {
            city_whitelist:          self.sc.city_whitelist.clone(),
            min_lead_days:           self.sc.weather_min_lead_days,
            max_lead_days:           self.sc.weather_max_lead_days,
            min_depth_usdc:          self.sc.weather_min_depth_usdc,
            abort_before_close_sec:  self.global.abort_before_close_sec,
            ..WeatherFilterConfig::default()
        };

        // Already-open slugs
        let open_slugs: HashSet<&str> =
            existing.iter().map(|p| p.market_slug.as_str()).collect();

        // 3. Apply filter and group qualifying markets by city
        let mut by_city: HashMap<String, Vec<WeatherMarket>> = HashMap::new();
        for market in &markets {
            if open_slugs.contains(market.slug.as_str()) {
                continue;
            }
            if self.forecast_model == WeatherModel::MetarShort {
                let secs_to_close = market.close_ts.saturating_sub(now_ts);
                if secs_to_close > METAR_SHORT_MAX_TTE_SEC {
                    tracing::debug!(
                        "[Weather:{}] {} metar_short 僅交易 24h 內市場，跳過 tte={}s",
                        self.sc.id, market.slug, secs_to_close
                    );
                    continue;
                }
            }
            // Pass depth=0 — we check depth properly after fetching the book below
            let result = filter_market(market, &filter_cfg, 0.0, now_ts);
            if result.is_accepted() || result.reason_code == "LOW_DEPTH" {
                // Accept LOW_DEPTH at this stage; real depth gate fires at book-fetch
                by_city
                    .entry(market.city.clone())
                    .or_default()
                    .push(market.clone());
            } else {
                tracing::debug!(
                    "[Weather:{}] {} 篩選拒絕: {} ({})",
                    self.sc.id, market.slug, result.reason, result.reason_code
                );
            }
        }

        if by_city.is_empty() {
            tracing::debug!("[Weather:{}] 本輪無符合條件的市場", self.sc.id);
            return Ok(Vec::new());
        }

        // 4. Fetch one forecast per city using the configured model
        let cities: HashSet<String> = by_city.keys().cloned().collect();
        let forecasts = if self.forecast_model == WeatherModel::Consensus {
            HashMap::new()
        } else {
            fetch_forecasts_for_cities(&cities, self.forecast_model).await
        };
        let consensus_forecasts = if self.forecast_model == WeatherModel::Consensus {
            Some(fetch_consensus_forecasts_for_cities(&cities).await)
        } else {
            None
        };

        let decision_cfg = self.make_decision_cfg(filter_cfg.min_depth_usdc);
        let mut new_positions = Vec::new();

        for (city, city_markets) in &by_city {
            let city_forecasts = if self.forecast_model == WeatherModel::Consensus {
                None
            } else {
                match forecasts.get(city) {
                    Some(f) => Some(f),
                    None => {
                        tracing::warn!("[Weather:{}] 城市 {city} 無法獲取預測", self.sc.id);
                        // Write NO_TRADE records so the dashboard can show this strategy is active
                        if self.global.is_dry_run() {
                            for market in city_markets {
                                let lead = (market.target_date - Utc::now().date_naive()).num_days();
                                let _ = db.write_weather_dry_run_trade(&WeatherDryRunTrade {
                                    strategy_id:            self.sc.id.clone(),
                                    event_id:               format!("{}-{}-{}",
                                        self.sc.id, market.slug,
                                        Utc::now().timestamp_nanos_opt().unwrap_or_default()),
                                    market_slug:            market.slug.clone(),
                                    city:                   city.clone(),
                                    market_type:            market.market_type.to_string(),
                                    side:                   "NONE".into(),
                                    action:                 "NO_TRADE".into(),
                                    price:                  0.0,
                                    size_usdc:              0.0,
                                    spread_at_decision:     None,
                                    depth_usdc_at_decision: None,
                                    entry_price:            None,
                                    exit_price:             None,
                                    hold_sec:               None,
                                    model:                  format!("{}", self.forecast_model),
                                    p_yes_at_entry:         None,
                                    p_yes_at_exit:          None,
                                    lead_days:              Some(lead),
                                    taker_fee_bps:          None,
                                    slippage_buffer_bps:    None,
                                    expected_net_edge_bps:  None,
                                    realized_pnl_usdc:      None,
                                    reason_code:            "FORECAST_UNAVAILABLE".into(),
                                    note:                   Some(format!(
                                        "model={} 預測獲取失敗，無法評估市場",
                                        self.forecast_model
                                    )),
                                }).await;
                            }
                        }
                        continue;
                    }
                }
            };
            let city_consensus = if self.forecast_model == WeatherModel::Consensus {
                match consensus_forecasts.as_ref().and_then(|m| m.get(city)) {
                    Some(c) => Some(c),
                    None => {
                        tracing::warn!("[Weather:{}] 城市 {city} 共識預測獲取失敗", self.sc.id);
                        // Write NO_TRADE records for consensus failure too
                        if self.global.is_dry_run() {
                            for market in city_markets {
                                let lead = (market.target_date - Utc::now().date_naive()).num_days();
                                let _ = db.write_weather_dry_run_trade(&WeatherDryRunTrade {
                                    strategy_id:            self.sc.id.clone(),
                                    event_id:               format!("{}-{}-{}",
                                        self.sc.id, market.slug,
                                        Utc::now().timestamp_nanos_opt().unwrap_or_default()),
                                    market_slug:            market.slug.clone(),
                                    city:                   city.clone(),
                                    market_type:            market.market_type.to_string(),
                                    side:                   "NONE".into(),
                                    action:                 "NO_TRADE".into(),
                                    price:                  0.0,
                                    size_usdc:              0.0,
                                    spread_at_decision:     None,
                                    depth_usdc_at_decision: None,
                                    entry_price:            None,
                                    exit_price:             None,
                                    hold_sec:               None,
                                    model:                  "consensus".into(),
                                    p_yes_at_entry:         None,
                                    p_yes_at_exit:          None,
                                    lead_days:              Some(lead),
                                    taker_fee_bps:          None,
                                    slippage_buffer_bps:    None,
                                    expected_net_edge_bps:  None,
                                    realized_pnl_usdc:      None,
                                    reason_code:            "FORECAST_UNAVAILABLE".into(),
                                    note:                   Some(
                                        "consensus 需要 GFS+ECMWF+Ensemble 三者齊全，至少一個獲取失敗".into()
                                    ),
                                }).await;
                            }
                        }
                        continue;
                    }
                }
            } else {
                None
            };

            for market in city_markets {
                // 5. Fetch YES/NO order books (small delay between requests)
                let yes_book = clob::fetch_order_book(
                    &self.global.clob_base, &market.token_id_yes,
                ).await;
                tokio::time::sleep(Duration::from_millis(400)).await;
                let no_book = clob::fetch_order_book(
                    &self.global.clob_base, &market.token_id_no,
                ).await;

                let (yes_book, no_book) = match (yes_book, no_book) {
                    (Ok(y), Ok(n)) => (y, n),
                    (Err(e), _) | (_, Err(e)) => {
                        let msg = e.to_string();
                        if msg.contains("404") {
                            // Market already resolved/delisted — expected near expiry
                            tracing::debug!(
                                "[Weather:{}] {} 訂單簿已下架 (404)，跳過",
                                self.sc.id, market.slug
                            );
                        } else {
                            tracing::warn!(
                                "[Weather:{}] {} 訂單簿獲取失敗: {e}",
                                self.sc.id, market.slug
                            );
                        }
                        continue;
                    }
                };

                let snapshot = WeatherBookSnapshot {
                    yes_best_ask:  yes_book.best_ask,
                    yes_best_bid:  yes_book.best_bid,
                    no_best_ask:   no_book.best_ask,
                    no_best_bid:   no_book.best_bid,
                    depth_usdc:    yes_book.depth_usdc.min(no_book.depth_usdc),
                };

                let spread = yes_book.best_ask - yes_book.best_bid;
                let depth  = snapshot.depth_usdc;

                // 6. Evaluate edge
                let (signal, model_used) = if self.forecast_model == WeatherModel::Consensus {
                    let Some(cf) = city_consensus else {
                        continue;
                    };
                    match evaluate_consensus_signal_for_market(
                        cf,
                        market,
                        &snapshot,
                        &decision_cfg,
                        self.sc.consensus_max_divergence,
                    ) {
                        Some(sig) => (sig, WeatherModel::Consensus),
                        None => {
                            tracing::debug!(
                                "[Weather:{}] {} 共識模型缺少足夠預測資料",
                                self.sc.id, market.slug
                            );
                            continue;
                        }
                    }
                } else {
                    let Some(city_fc) = city_forecasts else {
                        continue;
                    };
                    let forecast = match find_forecast_for_date(city_fc, market.target_date) {
                        Some(f) => f,
                        None => {
                            tracing::debug!(
                                "[Weather:{}] {} 找不到 {} 的預測",
                                self.sc.id, market.slug, market.target_date
                            );
                            continue;
                        }
                    };
                    (
                        weather_decision::evaluate(forecast, market, &snapshot, &decision_cfg),
                        forecast.model,
                    )
                };

                let event_id = format!(
                    "{}-{}-{}",
                    self.sc.id, market.slug,
                    Utc::now().timestamp_nanos_opt().unwrap_or_default()
                );
                let lead_days = (market.target_date - Utc::now().date_naive()).num_days();

                tracing::info!(
                    "[Weather:{}] {} | city={} type={} lead={}d | \
                     signal={:?} p_yes={:.3} edge={:.0}bps | {}",
                    self.sc.id, market.slug, city, market.market_type,
                    lead_days, signal.direction, signal.p_yes,
                    signal.edge_bps, signal.reason
                );

                // 7. Act on signal
                if signal.direction == WeatherDirection::Hold {
                    if self.global.is_dry_run() {
                        let _ = db.write_weather_dry_run_trade(&WeatherDryRunTrade {
                            strategy_id:            self.sc.id.clone(),
                            event_id:               event_id.clone(),
                            market_slug:            market.slug.clone(),
                            city:                   city.clone(),
                            market_type:            market.market_type.to_string(),
                            side:                   "NONE".into(),
                            action:                 "NO_TRADE".into(),
                            price:                  0.0,
                            size_usdc:              0.0,
                            spread_at_decision:     Some(spread),
                            depth_usdc_at_decision: Some(depth),
                            entry_price:            None,
                            exit_price:             None,
                            hold_sec:               None,
                            model:                  format!("{}", model_used),
                            p_yes_at_entry:         Some(signal.p_yes),
                            p_yes_at_exit:          None,
                            lead_days:              Some(lead_days),
                            taker_fee_bps:          Some(self.sc.taker_fee_bps as i64),
                            slippage_buffer_bps:    Some(self.sc.slippage_buffer_bps as i64),
                            expected_net_edge_bps:  Some(signal.edge_bps),
                            realized_pnl_usdc:      None,
                            reason_code:            signal.reason_code.clone(),
                            note:                   Some(signal.reason.clone()),
                        }).await;
                    }
                    continue;
                }

                // ── ENTRY ─────────────────────────────────────────────────────
                let (side, token_id, entry_best_ask) = match signal.direction {
                    WeatherDirection::BuyYes => (
                        "YES", market.token_id_yes.clone(), yes_book.best_ask,
                    ),
                    WeatherDirection::BuyNo  => (
                        "NO",  market.token_id_no.clone(),  no_book.best_ask,
                    ),
                    WeatherDirection::Hold => unreachable!(),
                };

                let bet_size = {
                    let cap = self.capital.lock().expect("capital mutex poisoned");
                    if cap.is_stopped() {
                        tracing::warn!(
                            "[Weather:{}] {} — 停損觸發，跳過入場",
                            self.sc.id, market.slug
                        );
                        continue;
                    }
                    cap.current_bet_size()
                };
                let fee_usdc = self.global.compute_fee(bet_size);

                // TP: recover fees + achieve half the stated edge
                let profit_target = (self.sc.taker_fee_bps * 2.0
                    + self.sc.slippage_buffer_bps * 2.0
                    + self.sc.min_net_edge_bps * 0.5)
                    / 10_000.0;
                let take_profit_price = entry_best_ask + profit_target;
                let stop_loss_price   = entry_best_ask - self.sc.stop_loss_delta;

                if self.global.is_dry_run() {
                    let _ = db.write_weather_dry_run_trade(&WeatherDryRunTrade {
                        strategy_id:            self.sc.id.clone(),
                        event_id:               event_id.clone(),
                        market_slug:            market.slug.clone(),
                        city:                   city.clone(),
                        market_type:            market.market_type.to_string(),
                        side:                   side.to_string(),
                        action:                 "ENTRY".into(),
                        price:                  entry_best_ask,
                        size_usdc:              bet_size,
                        spread_at_decision:     Some(spread),
                        depth_usdc_at_decision: Some(depth),
                        entry_price:            Some(entry_best_ask),
                        exit_price:             None,
                        hold_sec:               None,
                        model:                  format!("{}", model_used),
                        p_yes_at_entry:         Some(signal.p_yes),
                        p_yes_at_exit:          None,
                        lead_days:              Some(lead_days),
                        taker_fee_bps:          Some(self.sc.taker_fee_bps as i64),
                        slippage_buffer_bps:    Some(self.sc.slippage_buffer_bps as i64),
                        expected_net_edge_bps:  Some(signal.edge_bps),
                        realized_pnl_usdc:      None,
                        reason_code:            signal.reason_code.clone(),
                        note:                   None,
                    }).await;

                    self.capital
                        .lock()
                        .expect("capital mutex poisoned")
                        .on_order_submit(bet_size, fee_usdc);

                    tracing::info!(
                        "[Weather:{}] [DRY_RUN] ENTRY {} {} @ {:.4}  \
                         edge={:.0}bps  size={:.2} USDC  TP={:.4}  SL={:.4}  \
                         p_yes={:.3}  model={}",
                        self.sc.id, side, market.slug, entry_best_ask,
                        signal.edge_bps, bet_size, take_profit_price, stop_loss_price,
                        signal.p_yes, model_used,
                    );
                } else {
                    // ── Live order ─────────────────────────────────────────────
                    let intent = executor::OrderIntent {
                        strategy_id: self.sc.id.clone(),
                        market_slug: market.slug.clone(),
                        token_id:    token_id.clone(),
                        side:        "BUY".to_string(),
                        price:       entry_best_ask,
                        size_usdc:   bet_size,
                        fee_usdc,
                        leg:         1,
                        signal_dump_pct: None,
                        hedge_sum:       None,
                    };
                    match executor::submit_order(&intent, &self.global, db, &self.capital).await {
                        Ok(result) => {
                            tracing::info!(
                                "[Weather:{}] LIVE 訂單已提交 slug={} side={} result={:?}",
                                self.sc.id, market.slug, side, result
                            );
                        }
                        Err(e) => {
                            tracing::error!(
                                "[Weather:{}] LIVE 訂單失敗 slug={} side={} err={e}",
                                self.sc.id, market.slug, side
                            );
                            continue;
                        }
                    }
                }

                new_positions.push(WeatherPosition {
                    event_id,
                    market_slug:          market.slug.clone(),
                    token_id,
                    market:               market.clone(),
                    side:                 side.to_string(),
                    entry_price:          entry_best_ask,
                    entry_ts:             Utc::now().timestamp(),
                    size_usdc:            bet_size,
                    take_profit_price,
                    stop_loss_price,
                    p_yes_at_entry:       signal.p_yes,
                    model:                model_used,
                    lead_days,
                    expected_net_edge_bps: signal.edge_bps,
                });
            }
        }

        Ok(new_positions)
    }

    // ── Shared exit helper ────────────────────────────────────────────────────

    async fn write_exit(
        &self,
        db: &DbWriter,
        pos: &WeatherPosition,
        action: &str,
        exit_price: f64,
        hold_sec: i64,
        p_yes_at_entry: f64,
        realized_pnl: f64,
        p_yes_at_exit: Option<f64>,
    ) {
        if !self.global.is_dry_run() {
            return; // live exits handled by CLOB fill callbacks (future phase)
        }
        let _ = db.write_weather_dry_run_trade(&WeatherDryRunTrade {
            strategy_id:            self.sc.id.clone(),
            event_id:               pos.event_id.clone(),
            market_slug:            pos.market_slug.clone(),
            city:                   pos.market.city.clone(),
            market_type:            pos.market.market_type.to_string(),
            side:                   pos.side.clone(),
            action:                 action.to_string(),
            price:                  exit_price,
            size_usdc:              pos.size_usdc,
            spread_at_decision:     None,
            depth_usdc_at_decision: None,
            entry_price:            Some(pos.entry_price),
            exit_price:             Some(exit_price),
            hold_sec:               Some(hold_sec),
            model:                  format!("{}", pos.model),
            p_yes_at_entry:         Some(p_yes_at_entry),
            p_yes_at_exit,
            lead_days:              Some(pos.lead_days),
            taker_fee_bps:          Some(self.sc.taker_fee_bps as i64),
            slippage_buffer_bps:    Some(self.sc.slippage_buffer_bps as i64),
            expected_net_edge_bps:  Some(pos.expected_net_edge_bps),
            realized_pnl_usdc:      Some(realized_pnl),
            reason_code:            action.to_string(),
            note:                   None,
        }).await;
    }

    async fn submit_live_exit_order(
        &self,
        pos: &WeatherPosition,
        exit_price: f64,
        db: &DbWriter,
        action: &str,
    ) -> Result<(), AppError> {
        let fee_usdc = self.global.compute_fee(pos.size_usdc);
        let intent = executor::OrderIntent {
            strategy_id: self.sc.id.clone(),
            market_slug: pos.market_slug.clone(),
            token_id: pos.token_id.clone(),
            side: "SELL".to_string(),
            price: exit_price,
            size_usdc: pos.size_usdc,
            fee_usdc,
            leg: 2,
            signal_dump_pct: None,
            hedge_sum: None,
        };

        let result = executor::submit_order(&intent, &self.global, db, &self.capital).await?;
        tracing::info!(
            "[Weather:{}] LIVE {} 已送出平倉單 slug={} side={} price={:.4} result={:?}",
            self.sc.id,
            action,
            pos.market_slug,
            pos.side,
            exit_price,
            result
        );
        Ok(())
    }

    // ── Helpers ───────────────────────────────────────────────────────────────

    fn make_decision_cfg(&self, min_depth_usdc: f64) -> WeatherDecisionConfig {
        let bet_size = {
            let cap = self.capital.lock().expect("capital mutex poisoned");
            cap.current_bet_size()
        };
        WeatherDecisionConfig {
            taker_fee_bps:        self.sc.taker_fee_bps,
            slippage_buffer_bps:  self.sc.slippage_buffer_bps,
            min_net_edge_bps:     self.sc.min_net_edge_bps,
            min_model_confidence: self.sc.min_model_confidence,
            max_spread:           self.sc.max_spread,
            min_depth_usdc,
            bet_size_usdc:        bet_size,
        }
    }
}

// ── Module-level helpers ──────────────────────────────────────────────────────

/// Fetch forecasts for a set of city names using the configured model,
/// returning a map of city → Vec<WeatherForecast>.  Cities not found in
/// `city_info()` are skipped.
async fn fetch_forecasts_for_cities(
    cities: &HashSet<String>,
    model: WeatherModel,
) -> HashMap<String, Vec<WeatherForecast>> {
    let mut result = HashMap::new();
    for city_name in cities {
        if let Some(forecasts) = fetch_city_forecast_cached(city_name, model).await {
            result.insert(city_name.clone(), forecasts);
        }
    }
    result
}

async fn fetch_consensus_forecasts_for_cities(
    cities: &HashSet<String>,
) -> HashMap<String, ConsensusCityForecasts> {
    let mut result = HashMap::new();
    for city_name in cities {
        let gfs      = fetch_city_forecast_cached(city_name, WeatherModel::Gfs).await;
        let ecmwf    = fetch_city_forecast_cached(city_name, WeatherModel::Ecmwf).await;
        let ensemble = fetch_city_forecast_cached(city_name, WeatherModel::Ensemble).await;

        match gfs {
            None => {
                // GFS is the primary model; without it we have no consensus baseline
                tracing::warn!(
                    "[Weather] {} GFS (主要模型) 預測失敗，跳過 consensus",
                    city_name
                );
            }
            Some(gfs) => {
                let available = 1 + ecmwf.is_some() as usize + ensemble.is_some() as usize;
                if available < 3 {
                    tracing::debug!(
                        "[Weather] {} consensus {}/3 模型可用 \
                         ecmwf_ok={} ensemble_ok={}，使用可用模型繼續",
                        city_name, available, ecmwf.is_some(), ensemble.is_some(),
                    );
                }
                result.insert(city_name.clone(), ConsensusCityForecasts { gfs, ecmwf, ensemble });
            }
        }
    }
    result
}

async fn fetch_city_forecast_cached(
    city_name: &str,
    model: WeatherModel,
) -> Option<Vec<WeatherForecast>> {
    let cache_key = format!("{}:{}", model, city_name.to_lowercase());
    let cache_ttl = forecast_cache_ttl(model);

    {
        let cache = FORECAST_CACHE.lock().expect("forecast cache mutex poisoned");
        if let Some((fetched_at, forecasts)) = cache.get(&cache_key) {
            if fetched_at.elapsed() < cache_ttl {
                tracing::debug!(
                    "[Weather] forecast cache hit city={} model={} len={}",
                    city_name,
                    model,
                    forecasts.len(),
                );
                return Some(forecasts.clone());
            }
        }
    }

    let info = match city_info(city_name) {
        Some(c) => c,
        None => {
            tracing::warn!("[Weather] 城市 {city_name} 未在 ALL_CITIES 中找到，跳過預測");
            return None;
        }
    };

    let fetch_result = match model {
        WeatherModel::Gfs => openmeteo::fetch_gfs(info, FORECAST_DAYS).await,
        WeatherModel::Ecmwf => openmeteo::fetch_ecmwf(info, FORECAST_DAYS).await,
        WeatherModel::Ensemble => openmeteo::fetch_ensemble(info, FORECAST_DAYS).await,
        WeatherModel::Nws => nws::fetch_nws(info, FORECAST_DAYS).await,
        WeatherModel::MetarShort => fetch_metar_short_blended(info).await,
        WeatherModel::Consensus => {
            tracing::warn!(
                "[Weather] {} requested consensus in single-model fetch; skipping",
                city_name
            );
            return None;
        }
        WeatherModel::Metar => {
            tracing::warn!(
                "[Weather] {} requested METAR for forecast generation; skipping",
                city_name
            );
            return None;
        }
    };

    match fetch_result {
        Ok(forecasts) => {
            tracing::debug!(
                "[Weather] {} {} 預測 {} 天",
                city_name, model, forecasts.len()
            );
            let mut cache = FORECAST_CACHE.lock().expect("forecast cache mutex poisoned");
            cache.insert(cache_key, (Instant::now(), forecasts.clone()));
            Some(forecasts)
        }
        Err(e) => {
            tracing::warn!("[Weather] {} {} 預測獲取失敗: {e}", city_name, model);
            None
        }
    }
}

fn evaluate_consensus_signal_for_market(
    city_fc: &ConsensusCityForecasts,
    market: &WeatherMarket,
    snapshot: &WeatherBookSnapshot,
    cfg: &WeatherDecisionConfig,
    max_divergence: f64,
) -> Option<WeatherSignal> {
    // GFS is required; ECMWF and Ensemble are optional (degraded consensus).
    let gfs_fc = find_forecast_for_date(&city_fc.gfs, market.target_date)?;
    let p_gfs  = weather_decision::probability_yes(gfs_fc, market)?;

    let p_ecmwf = city_fc.ecmwf.as_ref()
        .and_then(|v| find_forecast_for_date(v, market.target_date))
        .and_then(|fc| weather_decision::probability_yes(fc, market));

    let p_ens = city_fc.ensemble.as_ref()
        .and_then(|v| find_forecast_for_date(v, market.target_date))
        .and_then(|fc| weather_decision::probability_yes(fc, market));

    // Build list of available probabilities (GFS always present)
    let mut p_values: Vec<f64> = vec![p_gfs];
    if let Some(p) = p_ecmwf { p_values.push(p); }
    if let Some(p) = p_ens   { p_values.push(p); }

    let p_min = p_values.iter().copied().fold(f64::MAX, f64::min);
    let p_max = p_values.iter().copied().fold(f64::MIN, f64::max);
    let divergence = p_max - p_min;
    let p_yes = p_values.iter().sum::<f64>() / p_values.len() as f64;

    // Compact label for reason strings, e.g. "gfs=0.720 ecmwf=0.690 ens=0.710"
    let model_label = {
        let mut s = format!("gfs={:.3}", p_gfs);
        if let Some(p) = p_ecmwf { s.push_str(&format!(" ecmwf={:.3}", p)); }
        if let Some(p) = p_ens   { s.push_str(&format!(" ens={:.3}", p)); }
        s
    };

    if snapshot.yes_best_ask - snapshot.yes_best_bid > cfg.max_spread {
        return Some(WeatherSignal {
            direction: WeatherDirection::Hold,
            edge_bps: 0.0,
            p_yes,
            entry_price: 0.0,
            reason: format!(
                "YES spread={:.4} > max={:.4}",
                snapshot.yes_best_ask - snapshot.yes_best_bid,
                cfg.max_spread,
            ),
            reason_code: "SPREAD_WIDE".to_string(),
        });
    }

    if snapshot.depth_usdc < cfg.min_depth_usdc {
        return Some(WeatherSignal {
            direction: WeatherDirection::Hold,
            edge_bps: 0.0,
            p_yes,
            entry_price: 0.0,
            reason: format!(
                "depth={:.2} USDC < min={:.2}",
                snapshot.depth_usdc,
                cfg.min_depth_usdc,
            ),
            reason_code: "LOW_DEPTH".to_string(),
        });
    }

    if divergence > max_divergence {
        return Some(WeatherSignal {
            direction: WeatherDirection::Hold,
            edge_bps: 0.0,
            p_yes,
            entry_price: 0.0,
            reason: format!(
                "model divergence too wide: {} delta={:.3} > {:.3}",
                model_label, divergence, max_divergence,
            ),
            reason_code: "MODEL_DIVERGENCE".to_string(),
        });
    }

    if p_yes > (1.0 - cfg.min_model_confidence) && p_yes < cfg.min_model_confidence {
        return Some(WeatherSignal {
            direction: WeatherDirection::Hold,
            edge_bps: 0.0,
            p_yes,
            entry_price: 0.0,
            reason: format!(
                "consensus p_yes={:.3} between [{:.3},{:.3}]",
                p_yes,
                1.0 - cfg.min_model_confidence,
                cfg.min_model_confidence,
            ),
            reason_code: "LOW_CONFIDENCE".to_string(),
        });
    }

    let cost_frac = (cfg.taker_fee_bps + cfg.slippage_buffer_bps) / 10_000.0;
    let min_edge = cfg.min_net_edge_bps / 10_000.0;
    let edge_yes = p_yes - snapshot.yes_best_ask - cost_frac * 2.0;
    let edge_no = (1.0 - p_yes) - snapshot.no_best_ask - cost_frac * 2.0;

    if edge_yes >= min_edge && edge_yes >= edge_no {
        Some(WeatherSignal {
            direction: WeatherDirection::BuyYes,
            edge_bps: edge_yes * 10_000.0,
            p_yes,
            entry_price: snapshot.yes_best_ask,
            reason: format!(
                "consensus BUY_YES p=[{}] yes_ask={:.4} edge={:.0}bps",
                model_label, snapshot.yes_best_ask, edge_yes * 10_000.0,
            ),
            reason_code: "BUY_YES".to_string(),
        })
    } else if edge_no >= min_edge {
        Some(WeatherSignal {
            direction: WeatherDirection::BuyNo,
            edge_bps: edge_no * 10_000.0,
            p_yes,
            entry_price: snapshot.no_best_ask,
            reason: format!(
                "consensus BUY_NO p=[{}] no_ask={:.4} edge={:.0}bps",
                model_label, snapshot.no_best_ask, edge_no * 10_000.0,
            ),
            reason_code: "BUY_NO".to_string(),
        })
    } else {
        let best_edge = edge_yes.max(edge_no) * 10_000.0;
        Some(WeatherSignal {
            direction: WeatherDirection::Hold,
            edge_bps: 0.0,
            p_yes,
            entry_price: 0.0,
            reason: format!(
                "consensus best_edge={:.0}bps < min={:.0}bps",
                best_edge,
                cfg.min_net_edge_bps,
            ),
            reason_code: "LOW_EDGE".to_string(),
        })
    }
}

fn parse_weather_model(model: &str) -> WeatherModel {
    if model.eq_ignore_ascii_case("ecmwf") {
        WeatherModel::Ecmwf
    } else if model.eq_ignore_ascii_case("ensemble") {
        WeatherModel::Ensemble
    } else if model.eq_ignore_ascii_case("metar_short") {
        WeatherModel::MetarShort
    } else if model.eq_ignore_ascii_case("consensus") {
        WeatherModel::Consensus
    } else if model.eq_ignore_ascii_case("nws") {
        WeatherModel::Nws
    } else if model.eq_ignore_ascii_case("metar") {
        WeatherModel::Metar
    } else {
        WeatherModel::Gfs
    }
}

async fn fetch_metar_short_blended(
    info: &crate::api::weather::CityInfo,
) -> Result<Vec<WeatherForecast>, AppError> {
    // METAR is an optional observation anchor.  Transient connectivity errors
    // (connection reset, DNS hiccup, timeout) are common with aviationweather.gov
    // and should not abort the entire metar_short forecast.  When METAR is
    // unavailable we fall back to the pure short-term model.
    let metar_obs = match metar::fetch_metar(info, 3).await {
        Ok(obs) => {
            tracing::debug!("[Weather] {} METAR 觀測成功 ({:.1}°C)", info.name, obs.max_temp_c);
            Some(obs)
        }
        Err(e) => {
            tracing::debug!(
                "[Weather] {} METAR 觀測不可用，使用純短期預測: {e}",
                info.name
            );
            None
        }
    };

    // Short-term forecast backbone: US -> NWS hourly aggregate, non-US -> GFS hourly aggregate
    let short_fc = if info.nws_office.is_some() {
        nws::fetch_nws_hourly_agg(info, 2).await?
    } else {
        openmeteo::fetch_gfs_hourly_agg(info, 2).await?
    };

    let mut out = Vec::new();

    if let Some(ref obs) = metar_obs {
        // Include current observed state as same-day anchor.
        out.push(WeatherForecast {
            city: obs.city.clone(),
            model: WeatherModel::MetarShort,
            forecast_date: obs.forecast_date,
            max_temp_c: obs.max_temp_c,
            min_temp_c: obs.min_temp_c,
            prob_precip: obs.prob_precip,
            ensemble_members: None,
            fetched_at: Utc::now(),
            lead_days: 0,
        });
    }

    // Blend short-term forecast with observed temp to reduce model drift for <24h.
    // Without METAR the forecast is used as-is (no blending).
    for fc in short_fc.into_iter().filter(|f| f.lead_days <= 1) {
        let (blended_max, blended_min) = if let Some(ref obs) = metar_obs {
            (
                0.7 * fc.max_temp_c + 0.3 * obs.max_temp_c,
                0.7 * fc.min_temp_c + 0.3 * obs.min_temp_c,
            )
        } else {
            (fc.max_temp_c, fc.min_temp_c)
        };
        out.push(WeatherForecast {
            city: fc.city,
            model: WeatherModel::MetarShort,
            forecast_date: fc.forecast_date,
            max_temp_c: blended_max,
            min_temp_c: blended_min,
            prob_precip: fc.prob_precip,
            ensemble_members: None,
            fetched_at: Utc::now(),
            lead_days: fc.lead_days,
        });
    }

    out.sort_by_key(|f| f.forecast_date);
    out.dedup_by_key(|f| f.forecast_date);

    if out.is_empty() {
        return Err(AppError::ApiError(format!(
            "[Weather] {} metar_short 無可用預測",
            info.name
        )));
    }

    Ok(out)
}

fn forecast_cache_ttl(model: WeatherModel) -> Duration {
    match model {
        // METAR stations report every 30–60 min; 10-min TTL avoids redundant
        // fetches on 15-min scan cycles while still staying reasonably fresh.
        WeatherModel::MetarShort => Duration::from_secs(600),
        // NWS products update every 1–6 hours; 15-min TTL is plenty.
        WeatherModel::Nws => Duration::from_secs(900),
        // METAR (full observation) updates every 30–60 min; 15-min TTL matches.
        WeatherModel::Metar => Duration::from_secs(900),
        // GFS / ECMWF / Ensemble update every 6–12 hours; 30-min TTL is fine.
        WeatherModel::Gfs | WeatherModel::Ecmwf | WeatherModel::Ensemble => {
            Duration::from_secs(1800)
        }
        // Consensus mode calls each sub-model via their own cached paths;
        // this branch is not reached by fetch_city_forecast_cached.
        WeatherModel::Consensus => Duration::from_secs(1800),
    }
}

/// Find the forecast entry whose `forecast_date` matches `target`.
fn find_forecast_for_date<'a>(
    forecasts: &'a [WeatherForecast],
    target: NaiveDate,
) -> Option<&'a WeatherForecast> {
    forecasts.iter().find(|f| f.forecast_date == target)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::clob::{set_mock_fetch_order_book_outcome, BookSummary, MockFetchBookOutcome};
    use crate::api::weather_market::{WeatherMarket, WeatherMarketType};
    use crate::config::{StrategyType, TradingMode};
    use crate::execution::executor::{
        set_mock_submit_order_outcome, take_mock_last_order_intent, MockSubmitOrderOutcome,
    };
    use crate::risk::capital;
    use chrono::NaiveDate;
    use crate::api::weather::{WeatherForecast, WeatherModel};
    use once_cell::sync::Lazy;
    use tokio::sync::Mutex as AsyncMutex;

    static LIVE_EXIT_TEST_LOCK: Lazy<AsyncMutex<()>> = Lazy::new(|| AsyncMutex::new(()));

    fn make_forecast(date: NaiveDate, max_temp_c: f64) -> WeatherForecast {
        WeatherForecast {
            city:             "NYC".into(),
            model:            WeatherModel::Gfs,
            forecast_date:    date,
            max_temp_c,
            min_temp_c:       max_temp_c - 8.0,
            prob_precip:      0.1,
            ensemble_members: None,
            fetched_at:       Utc::now(),
            lead_days:        3,
        }
    }

    #[test]
    fn find_forecast_returns_correct_date() {
        let d1 = NaiveDate::from_ymd_opt(2026, 6, 15).unwrap();
        let d2 = NaiveDate::from_ymd_opt(2026, 6, 16).unwrap();
        let d3 = NaiveDate::from_ymd_opt(2026, 6, 17).unwrap();
        let forecasts = vec![make_forecast(d1, 28.0), make_forecast(d2, 30.0), make_forecast(d3, 25.0)];

        let found = find_forecast_for_date(&forecasts, d2).unwrap();
        assert_eq!(found.forecast_date, d2);
        assert!((found.max_temp_c - 30.0).abs() < 1e-9);
    }

    #[test]
    fn find_forecast_returns_none_for_missing_date() {
        let d1 = NaiveDate::from_ymd_opt(2026, 6, 15).unwrap();
        let miss = NaiveDate::from_ymd_opt(2026, 6, 20).unwrap();
        let forecasts = vec![make_forecast(d1, 28.0)];

        assert!(find_forecast_for_date(&forecasts, miss).is_none());
    }

    #[test]
    fn find_forecast_empty_slice_returns_none() {
        let target = NaiveDate::from_ymd_opt(2026, 6, 15).unwrap();
        assert!(find_forecast_for_date(&[], target).is_none());
    }

    fn test_strategy_config() -> StrategyConfig {
        StrategyConfig {
            id: "weather_test".to_string(),
            strategy_type: StrategyType::Weather,
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
            stop_loss_delta: 0.05,
            taker_fee_bps: 180.0,
            slippage_buffer_bps: 50.0,
            execution_risk_bps: 20.0,
            min_net_edge_bps: 100.0,
            max_spread: 0.05,
            min_model_confidence: 0.60,
            weather_min_depth_usdc: 50.0,
            weather_min_lead_days: 1,
            weather_max_lead_days: 14,
            city_whitelist: vec![],
            weather_forecast_model: "gfs".to_string(),
            forecast_shift_threshold: 0.15,
            consensus_max_divergence: 0.10,
            loop_interval_sec: 900,
        }
    }

    fn test_live_config(sc: StrategyConfig) -> BotConfig {
        BotConfig {
            mode: TradingMode::Live,
            dry_run: false,
            monitor_window_sec: 120,
            abort_before_close_sec: 30,
            gamma_base: String::new(),
            clob_base: "http://mock-clob".to_string(),
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
            strategies: vec![sc],
        }
    }

    fn make_position(now_ts: u64) -> WeatherPosition {
        let target_date = Utc::now().date_naive();
        let market = WeatherMarket {
            slug: "weather-nyc-test".to_string(),
            question: "Will NYC high temperature be above 20C?".to_string(),
            city: "NYC".to_string(),
            market_type: WeatherMarketType::Extreme,
            target_date,
            temp_range: Some((20.0, 100.0)),
            token_id_yes: "tok_yes".to_string(),
            token_id_no: "tok_no".to_string(),
            close_ts: now_ts + 3600,
        };

        WeatherPosition {
            event_id: "evt1".to_string(),
            market_slug: market.slug.clone(),
            token_id: market.token_id_yes.clone(),
            market,
            side: "YES".to_string(),
            entry_price: 0.45,
            entry_ts: Utc::now().timestamp() - 300,
            size_usdc: 10.0,
            take_profit_price: 0.55,
            stop_loss_price: 0.35,
            p_yes_at_entry: 0.70,
            model: WeatherModel::Gfs,
            lead_days: 1,
            expected_net_edge_bps: 120.0,
        }
    }

    fn seed_forecast_cache_for_nyc() {
        let forecast = WeatherForecast {
            city: "NYC".to_string(),
            model: WeatherModel::Gfs,
            forecast_date: Utc::now().date_naive(),
            max_temp_c: 26.0,
            min_temp_c: 18.0,
            prob_precip: 0.2,
            ensemble_members: None,
            fetched_at: Utc::now(),
            lead_days: 1,
        };
        let key = format!("{}:{}", WeatherModel::Gfs, "nyc");
        FORECAST_CACHE
            .lock()
            .expect("forecast cache mutex poisoned")
            .insert(key, (Instant::now(), vec![forecast]));
    }

    #[tokio::test]
    async fn live_take_profit_submit_success_removes_position() {
        let _guard = LIVE_EXIT_TEST_LOCK.lock().await;
        let sc = test_strategy_config();
        let cap = capital::new_shared(&sc);
        let global = test_live_config(sc.clone());
        let strategy = WeatherStrategy::new(global, sc, cap);
        let db = DbWriter::open(":memory:").expect("open in-memory DB");
        let now_ts = Utc::now().timestamp() as u64;
        let mut positions = vec![make_position(now_ts)];

        seed_forecast_cache_for_nyc();
        set_mock_fetch_order_book_outcome(Some(MockFetchBookOutcome::Success(BookSummary {
            best_bid: 0.60,
            best_ask: 0.61,
            depth_usdc: 500.0,
        })));
        set_mock_submit_order_outcome(Some(MockSubmitOrderOutcome::Success));

        strategy.monitor_positions(&mut positions, &db).await;

        assert!(positions.is_empty(), "successful live close should remove position");

        let intent = take_mock_last_order_intent().expect("must capture submitted intent");
        assert_eq!(intent.leg, 2);
        assert_eq!(intent.side, "SELL");
        assert_eq!(intent.market_slug, "weather-nyc-test");

        set_mock_fetch_order_book_outcome(None);
        set_mock_submit_order_outcome(None);
    }

    #[tokio::test]
    async fn live_take_profit_submit_failure_keeps_position() {
        let _guard = LIVE_EXIT_TEST_LOCK.lock().await;
        let sc = test_strategy_config();
        let cap = capital::new_shared(&sc);
        let global = test_live_config(sc.clone());
        let strategy = WeatherStrategy::new(global, sc, cap);
        let db = DbWriter::open(":memory:").expect("open in-memory DB");
        let now_ts = Utc::now().timestamp() as u64;
        let mut positions = vec![make_position(now_ts)];

        seed_forecast_cache_for_nyc();
        set_mock_fetch_order_book_outcome(Some(MockFetchBookOutcome::Success(BookSummary {
            best_bid: 0.60,
            best_ask: 0.61,
            depth_usdc: 500.0,
        })));
        set_mock_submit_order_outcome(Some(MockSubmitOrderOutcome::Failure(
            "simulated failure".to_string(),
        )));

        strategy.monitor_positions(&mut positions, &db).await;

        assert_eq!(positions.len(), 1, "failed live close should keep position");

        set_mock_fetch_order_book_outcome(None);
        set_mock_submit_order_outcome(None);
    }
}
