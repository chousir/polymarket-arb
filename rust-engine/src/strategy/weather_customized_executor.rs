// Weather Customized strategy — Ensemble model with enhanced entry filters.
//
// Architecture
// ────────────
// Shares the same monitoring/exit loop as WeatherStrategy (TAKE_PROFIT,
// STOP_LOSS with tick filter, TIME_DECAY_EXIT with settlement check,
// FORECAST_SHIFT).  The only difference is in scan_for_entries, which runs
// FIVE additional gates before any position is opened:
//
//   Gate 1 – LOOKBACK_SLOPE
//     Store the ask price of the intended trade direction for the last
//     `customized_lookback_ticks` polling cycles per market.
//     Require slope ≥ customized_min_slope (default 0.0 = neutral OK).
//     Prevents entering when the market is already moving against us.
//
//   Gate 2 – MIN_ENTRY_PRICE
//     Token ask must be ≥ customized_min_entry_price (default 0.30).
//     Avoids near-decided markets with poor risk/reward and thin liquidity.
//
//   Gate 3 – MAX_ENTRY_PRICE
//     Token ask must be ≤ customized_max_entry_price (default 0.85).
//     Avoids overpriced tokens where upside is too thin to cover fees.
//
//   Gate 4 – ENSEMBLE_SPREAD
//     Ensemble temperature std-dev must be ≤ customized_max_ensemble_spread_celsius
//     (default 4.0 °C).  High spread = model is uncertain → skip.
//
//   Gate 5 – CITY_EXPOSURE
//     Open positions for the same city must be < customized_max_positions_per_city
//     (default 3).  Prevents concentration risk in one location.
//
// DRY_RUN compliance
// ──────────────────
// Every order path checks is_dry_run() before touching the CLOB.

use std::collections::{HashMap, HashSet, VecDeque};
use std::time::{Duration, Instant};

use chrono::Utc;
use once_cell::sync::Lazy;
use std::sync::Mutex;

use crate::api::weather::{city_info, WeatherForecast, WeatherModel};
use crate::api::weather_market::{fetch_weather_markets, WeatherMarket};
use crate::api::clob;
use crate::api::gamma;
use crate::config::{BotConfig, StrategyConfig};
use crate::db::writer::{DbWriter, WeatherDryRunTrade};
use crate::error::AppError;
use crate::execution::executor;
use crate::risk::capital::SharedCapital;
use crate::strategy::weather_decision::{
    self, WeatherBookSnapshot, WeatherDecisionConfig, WeatherDirection,
};
use crate::strategy::weather_executor::{
    fetch_city_forecast_single, find_forecast_for_date, FORECAST_LEAD_BUFFER,
};
use crate::strategy::weather_filter::{filter_market, WeatherFilterConfig};

// ── Constants ─────────────────────────────────────────────────────────────────

const SLUG_COOLDOWN: Duration = Duration::from_secs(24 * 3600);
const MIN_EXIT_EXECUTION_SEC: u64 = 30;
const MIN_LOOP_INTERVAL_SEC: u64 = 30;

// Shared forecast cache (same as weather_executor — 60-second TTL).
static FORECAST_CACHE: Lazy<Mutex<HashMap<String, (Instant, Vec<WeatherForecast>)>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

// ── Price history for lookback slope gate ─────────────────────────────────────

/// One polling-tick snapshot of both sides of a market.
#[derive(Clone)]
struct PriceTick {
    no_ask:  f64,
    yes_ask: f64,
}

// ── Open position (identical to WeatherStrategy) ──────────────────────────────

#[derive(Debug, Clone)]
struct WeatherCustomPosition {
    event_id:              String,
    market_slug:           String,
    token_id:              String,
    market:                WeatherMarket,
    side:                  String,
    entry_price:           f64,
    entry_ts:              i64,
    abort_at:              Instant,
    size_usdc:             f64,
    profit_target:         f64,
    bid_best:              f64,
    stop_loss_price:       f64,
    p_yes_at_entry:        f64,
    model:                 WeatherModel,
    lead_days:             i64,
    expected_net_edge_bps: f64,
    consecutive_sl_ticks:  u32,
}

// ── Public strategy struct ────────────────────────────────────────────────────

pub struct WeatherCustomizedStrategy {
    global: BotConfig,
    sc:     StrategyConfig,
    capital: SharedCapital,
}

impl WeatherCustomizedStrategy {
    pub fn new(global: BotConfig, sc: StrategyConfig, capital: SharedCapital) -> Self {
        Self { global, sc, capital }
    }

    pub async fn run_loop(&self, db: &DbWriter) {
        let loop_interval_sec = self.sc.loop_interval_sec.max(MIN_LOOP_INTERVAL_SEC);
        let effective_abort_sec = {
            let buf = self.global.abort_before_close_sec
                .max(loop_interval_sec * 2 + MIN_EXIT_EXECUTION_SEC);
            tracing::info!(
                "[Custom:{}] 啟動 loop_interval={}s abort_before_close={}s",
                self.sc.id, loop_interval_sec, buf
            );
            buf
        };

        let mut positions: Vec<WeatherCustomPosition> = Vec::new();
        // Per-market price history for the lookback slope gate.
        // Key: market_slug; Value: ring-buffer of recent PriceTick snapshots.
        let mut price_history: HashMap<String, VecDeque<PriceTick>> = HashMap::new();
        let mut exited_slugs: HashMap<String, Instant> = HashMap::new();
        let mut interval = tokio::time::interval(Duration::from_secs(loop_interval_sec));

        loop {
            interval.tick().await;
            exited_slugs.retain(|_, ts| ts.elapsed() < SLUG_COOLDOWN);

            // ── Circuit-breaker gate ───────────────────────────────────────────
            {
                let cap = self.capital.lock()
                    .unwrap_or_else(|e| { tracing::error!("[Mutex Poisoned] capital: {e}"); e.into_inner() });
                if cap.is_stopped() {
                    tracing::warn!(
                        "[Custom:{}] ⛔ 停損觸發（capital={:.4}），暫停本輪",
                        self.sc.id, cap.current_capital
                    );
                    continue;
                }
            }

            let now_ts = Utc::now().timestamp() as u64;
            let mut to_remove: Vec<usize> = Vec::new();
            let mut newly_exited: Vec<String> = Vec::new();

            // ── Monitor existing positions ─────────────────────────────────────
            for (idx, pos) in positions.iter_mut().enumerate() {
                let secs_to_close = pos.market.close_ts.saturating_sub(now_ts);

                // TIME_DECAY_EXIT: first try settlement, then market-price exit
                if Instant::now() >= pos.abort_at {
                    let hold_sec = now_ts as i64 - pos.entry_ts;
                    match gamma::fetch_token_settlement_price(&pos.market_slug, &pos.token_id).await {
                        Ok(Some(settled_price)) => {
                            let pnl = (settled_price - pos.entry_price) * pos.size_usdc
                                - self.global.compute_fee(pos.size_usdc) * 2.0;
                            tracing::info!(
                                "[Custom:{}] SETTLEMENT {} side={} settled={:.2} hold={}s pnl={:.4}",
                                self.sc.id, pos.market_slug, pos.side, settled_price, hold_sec, pnl
                            );
                            self.write_exit(db, pos, "SETTLEMENT", settled_price, hold_sec, pnl, None).await;
                            self.capital.lock().unwrap_or_else(|e| { tracing::error!("[Mutex Poisoned] capital: {e}"); e.into_inner() })
                                .on_cycle_end(Some(pos.entry_price), Some(settled_price), pos.size_usdc,
                                              self.global.compute_fee(pos.size_usdc));
                            newly_exited.push(pos.market_slug.clone());
                            to_remove.push(idx);
                            continue;
                        }
                        Ok(None) => {}
                        Err(e) => {
                            tracing::warn!(
                                "[Custom:{}] 結算查詢失敗 {}，降級 TIME_DECAY_EXIT: {e}",
                                self.sc.id, pos.market_slug
                            );
                        }
                    }

                    let pnl = -self.global.compute_fee(pos.size_usdc);
                    tracing::info!(
                        "[Custom:{}] TIME_DECAY_EXIT {} secs_left={} hold={}s",
                        self.sc.id, pos.market_slug, secs_to_close, hold_sec
                    );
                    if self.global.is_dry_run() {
                        self.write_exit(db, pos, "TIME_DECAY_EXIT", pos.entry_price, hold_sec, pnl, None).await;
                        self.capital.lock().unwrap_or_else(|e| { tracing::error!("[Mutex Poisoned] capital: {e}"); e.into_inner() })
                            .on_cycle_end(Some(pos.entry_price), None, pos.size_usdc,
                                          self.global.compute_fee(pos.size_usdc));
                    } else {
                        let book = match clob::fetch_order_book(&self.global.clob_base, &pos.token_id).await {
                            Ok(b) => b,
                            Err(e) => {
                                tracing::warn!("[Custom:{}] TIME_DECAY_EXIT book 失敗，延後: {e}", self.sc.id);
                                continue;
                            }
                        };
                        if let Err(e) = self.submit_live_exit(pos, book.best_bid, db, "TIME_DECAY_EXIT").await {
                            tracing::warn!("[Custom:{}] TIME_DECAY_EXIT live 失敗: {e}", self.sc.id);
                            continue;
                        }
                        self.capital.lock().unwrap_or_else(|e| { tracing::error!("[Mutex Poisoned] capital: {e}"); e.into_inner() })
                            .on_cycle_end(Some(pos.entry_price), Some(book.best_bid), pos.size_usdc,
                                          self.global.compute_fee(pos.size_usdc));
                    }
                    newly_exited.push(pos.market_slug.clone());
                    to_remove.push(idx);
                    continue;
                }

                // Fetch current book
                let book = match clob::fetch_order_book(&self.global.clob_base, &pos.token_id).await {
                    Ok(b) => b,
                    Err(e) => {
                        tracing::debug!("[Custom:{}] {} book 失敗: {e}", self.sc.id, pos.market_slug);
                        continue;
                    }
                };

                // TAKE_PROFIT: trailing high-watermark with break-even guard
                pos.bid_best = pos.bid_best.max(book.best_bid);
                let effective_gap = pos.profit_target
                    .min((1.0 - pos.bid_best) * 2.0)
                    .max(0.02);
                let trailing_floor = (pos.bid_best - effective_gap)
                    .max(pos.entry_price + pos.profit_target);
                if pos.bid_best >= pos.entry_price + pos.profit_target
                    && book.best_bid < trailing_floor
                {
                    let exit_price = book.best_bid;
                    let fee_breakeven = 2.0 * self.global.taker_fee_pct;
                    if exit_price < pos.entry_price + fee_breakeven {
                        tracing::debug!(
                            "[Custom:{}] TAKE_PROFIT 觸發但 {} bid={:.4} < 損益平衡={:.4}，暫不平倉",
                            self.sc.id, pos.market_slug, exit_price, pos.entry_price + fee_breakeven
                        );
                        continue;
                    }
                    let hold_sec = now_ts as i64 - pos.entry_ts;
                    let pnl = (exit_price - pos.entry_price) * pos.size_usdc
                        - self.global.compute_fee(pos.size_usdc) * 2.0;
                    tracing::info!(
                        "[Custom:{}] TAKE_PROFIT {} side={} bid={:.4} floor={:.4} pnl={:.4}",
                        self.sc.id, pos.market_slug, pos.side, exit_price, trailing_floor, pnl
                    );
                    if self.global.is_dry_run() {
                        self.write_exit(db, pos, "TAKE_PROFIT", exit_price, hold_sec, pnl, None).await;
                    } else if let Err(e) = self.submit_live_exit(pos, exit_price, db, "TAKE_PROFIT").await {
                        tracing::warn!("[Custom:{}] TAKE_PROFIT live 失敗: {e}", self.sc.id);
                        continue;
                    }
                    self.capital.lock().unwrap_or_else(|e| { tracing::error!("[Mutex Poisoned] capital: {e}"); e.into_inner() })
                        .on_cycle_end(Some(pos.entry_price), Some(exit_price), pos.size_usdc,
                                      self.global.compute_fee(pos.size_usdc));
                    newly_exited.push(pos.market_slug.clone());
                    to_remove.push(idx);
                    continue;
                }

                // STOP_LOSS: trailing with tick filter
                let current_lead_days = (pos.market.target_date - Utc::now().date_naive()).num_days();
                let sl_delta = if current_lead_days >= 1 {
                    self.sc.stop_loss_delta * 2.0
                } else {
                    self.sc.stop_loss_delta
                };
                let effective_sl = pos.stop_loss_price.max(pos.bid_best - sl_delta);
                if book.best_ask <= effective_sl {
                    pos.consecutive_sl_ticks += 1;
                    tracing::debug!(
                        "[Custom:{}] STOP_LOSS 候選 {} ask={:.4} floor={:.4} ticks={}/{}",
                        self.sc.id, pos.market_slug,
                        book.best_ask, effective_sl,
                        pos.consecutive_sl_ticks, self.sc.min_sl_ticks
                    );
                    if pos.consecutive_sl_ticks < self.sc.min_sl_ticks {
                        continue;
                    }
                    let hold_sec = now_ts as i64 - pos.entry_ts;
                    let exit_price = book.best_bid;
                    let pnl = (exit_price - pos.entry_price) * pos.size_usdc
                        - self.global.compute_fee(pos.size_usdc) * 2.0;
                    tracing::info!(
                        "[Custom:{}] STOP_LOSS {} side={} ask={:.4} floor={:.4} pnl={:.4}",
                        self.sc.id, pos.market_slug, pos.side, book.best_ask, effective_sl, pnl
                    );
                    if self.global.is_dry_run() {
                        self.write_exit(db, pos, "STOP_LOSS", exit_price, hold_sec, pnl, None).await;
                    } else if let Err(e) = self.submit_live_exit(pos, exit_price, db, "STOP_LOSS").await {
                        tracing::warn!("[Custom:{}] STOP_LOSS live 失敗: {e}", self.sc.id);
                        continue;
                    }
                    self.capital.lock().unwrap_or_else(|e| { tracing::error!("[Mutex Poisoned] capital: {e}"); e.into_inner() })
                        .on_cycle_end(Some(pos.entry_price), Some(exit_price), pos.size_usdc,
                                      self.global.compute_fee(pos.size_usdc));
                    newly_exited.push(pos.market_slug.clone());
                    to_remove.push(idx);
                    continue;
                } else {
                    pos.consecutive_sl_ticks = 0;
                }

                // FORECAST_SHIFT: model p_yes flipped since entry
                let decision_cfg = self.make_decision_cfg(self.sc.weather_min_depth_usdc * 0.5, &pos.market.city);
                tokio::time::sleep(Duration::from_millis(200)).await;
                let opposite_token = if pos.side == "YES" {
                    &pos.market.token_id_no
                } else {
                    &pos.market.token_id_yes
                };
                let opposite_book = clob::fetch_order_book(&self.global.clob_base, opposite_token)
                    .await
                    .unwrap_or_else(|_| book.symmetric_complement());
                let (yes_book, no_book) = if pos.side == "YES" {
                    (book.clone(), opposite_book)
                } else {
                    (opposite_book, book.clone())
                };
                let snap = WeatherBookSnapshot {
                    yes_best_ask: yes_book.best_ask,
                    yes_best_bid: yes_book.best_bid,
                    no_best_ask:  no_book.best_ask,
                    no_best_bid:  no_book.best_bid,
                    depth_usdc:   yes_book.depth_usdc,
                };

                let forecast_days = self.sc.weather_max_lead_days + FORECAST_LEAD_BUFFER;
                let city_info = city_info(&pos.market.city);
                let fresh_forecasts = if let Some(ci) = city_info {
                    fetch_city_forecast_single(ci, WeatherModel::Ensemble, forecast_days)
                        .await
                        .unwrap_or_default()
                } else {
                    vec![]
                };
                if let Some(fc) = find_forecast_for_date(&fresh_forecasts, pos.market.target_date) {
                    let fresh_signal = weather_decision::evaluate(fc, &pos.market, &snap, &decision_cfg);
                    let shift = (fresh_signal.p_yes - pos.p_yes_at_entry).abs();
                    let shift_threshold = if pos.lead_days >= 1 {
                        self.sc.forecast_shift_threshold * 2.0
                    } else {
                        self.sc.forecast_shift_threshold
                    };
                    let direction_flipped = match pos.side.as_str() {
                        "YES" => fresh_signal.direction == WeatherDirection::BuyNo
                            || fresh_signal.direction == WeatherDirection::Hold,
                        _     => fresh_signal.direction == WeatherDirection::BuyYes
                            || fresh_signal.direction == WeatherDirection::Hold,
                    };
                    if fresh_signal.p_yes > 0.02 && shift >= shift_threshold && direction_flipped {
                        let hold_sec = now_ts as i64 - pos.entry_ts;
                        let exit_price = if pos.side == "YES" { yes_book.best_bid } else { no_book.best_bid };
                        let pnl = (exit_price - pos.entry_price) * pos.size_usdc
                            - self.global.compute_fee(pos.size_usdc) * 2.0;
                        tracing::info!(
                            "[Custom:{}] FORECAST_SHIFT {} Δp={:.3} threshold={:.3} pnl={:.4}",
                            self.sc.id, pos.market_slug, shift, shift_threshold, pnl
                        );
                        if self.global.is_dry_run() {
                            self.write_exit_with_p_exit(
                                db, pos, "FORECAST_SHIFT", exit_price, hold_sec,
                                pnl, Some(fresh_signal.p_yes),
                            ).await;
                        } else if let Err(e) = self.submit_live_exit(pos, exit_price, db, "FORECAST_SHIFT").await {
                            tracing::warn!("[Custom:{}] FORECAST_SHIFT live 失敗: {e}", self.sc.id);
                            continue;
                        }
                        self.capital.lock().unwrap_or_else(|e| { tracing::error!("[Mutex Poisoned] capital: {e}"); e.into_inner() })
                            .on_cycle_end(Some(pos.entry_price), Some(exit_price), pos.size_usdc,
                                          self.global.compute_fee(pos.size_usdc));
                        newly_exited.push(pos.market_slug.clone());
                        to_remove.push(idx);
                    }
                }
            }

            for idx in to_remove.into_iter().rev() {
                positions.swap_remove(idx);
            }
            for slug in newly_exited {
                exited_slugs.insert(slug, Instant::now());
            }

            // ── Scan for new entries ───────────────────────────────────────────
            match self.scan_for_entries(
                &positions,
                db,
                effective_abort_sec,
                &exited_slugs,
                &mut price_history,
            ).await {
                Ok(new) => positions.extend(new),
                Err(e)  => tracing::warn!("[Custom:{}] scan_for_entries 失敗: {e}", self.sc.id),
            }
        }
    }

    // ── Entry scanner ─────────────────────────────────────────────────────────

    async fn scan_for_entries(
        &self,
        existing: &[WeatherCustomPosition],
        db: &DbWriter,
        effective_abort_sec: u64,
        exited_slugs: &HashMap<String, Instant>,
        price_history: &mut HashMap<String, VecDeque<PriceTick>>,
    ) -> Result<Vec<WeatherCustomPosition>, AppError> {
        let now_ts = Utc::now().timestamp() as u64;

        let markets = match fetch_weather_markets().await {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!("[Custom:{}] 獲取市場失敗: {e}", self.sc.id);
                return Ok(Vec::new());
            }
        };

        let filter_cfg = WeatherFilterConfig {
            city_whitelist:         self.sc.city_whitelist.clone(),
            min_lead_days:          self.sc.weather_min_lead_days,
            max_lead_days:          self.sc.weather_max_lead_days,
            min_depth_usdc:         self.sc.weather_min_depth_usdc,
            abort_before_close_sec: effective_abort_sec,
            ..WeatherFilterConfig::default()
        };
        let open_slugs: HashSet<&str> = existing.iter().map(|p| p.market_slug.as_str()).collect();

        // Group qualifying markets by city
        let mut by_city: HashMap<String, Vec<WeatherMarket>> = HashMap::new();
        for market in &markets {
            if open_slugs.contains(market.slug.as_str()) { continue; }
            if exited_slugs.get(&market.slug).map_or(false, |t| t.elapsed() < SLUG_COOLDOWN) {
                continue;
            }
            let result = filter_market(market, &filter_cfg, 0.0, now_ts);
            if result.is_accepted() || result.reason_code == "LOW_DEPTH" {
                by_city.entry(market.city.clone()).or_default().push(market.clone());
            }
        }

        if by_city.is_empty() {
            return Ok(Vec::new());
        }

        let forecast_days = self.sc.weather_max_lead_days + FORECAST_LEAD_BUFFER;
        let cities: HashSet<String> = by_city.keys().cloned().collect();
        let mut city_forecasts: HashMap<String, Vec<WeatherForecast>> = HashMap::new();
        for city in &cities {
            if let Some(ci) = city_info(city) {
                if let Some(fcs) = fetch_city_forecast_single(ci, WeatherModel::Ensemble, forecast_days).await {
                    city_forecasts.insert(city.clone(), fcs);
                }
            }
        }

        let mut new_positions = Vec::new();

        for (city, city_markets) in &by_city {
            // Gate 5 (CITY_EXPOSURE): check before fetching books
            let city_open = existing.iter().filter(|p| p.market.city == *city).count();
            if city_open >= self.sc.customized_max_positions_per_city {
                tracing::debug!(
                    "[Custom:{}] {} 城市已有 {} 個持倉，上限 {}，跳過",
                    self.sc.id, city, city_open, self.sc.customized_max_positions_per_city
                );
                continue;
            }

            let forecasts = match city_forecasts.get(city) {
                Some(f) => f,
                None => {
                    tracing::warn!("[Custom:{}] 城市 {city} 無法獲取預測", self.sc.id);
                    continue;
                }
            };

            let decision_cfg = self.make_decision_cfg(filter_cfg.min_depth_usdc, city);

            for market in city_markets {
                let lead_days = (market.target_date - Utc::now().date_naive()).num_days();
                let fc = match find_forecast_for_date(forecasts, market.target_date) {
                    Some(f) => f,
                    None => continue,
                };

                // Gate 4 (ENSEMBLE_SPREAD): reject high-uncertainty forecasts
                if let Some(members) = fc.ensemble_members.as_deref() {
                    let spread = ensemble_std_dev(members);
                    if spread > self.sc.customized_max_ensemble_spread_celsius {
                        tracing::debug!(
                            "[Custom:{}] {} 集成預報離散度 {:.2}°C > {:.2}°C 上限，跳過",
                            self.sc.id, market.slug, spread,
                            self.sc.customized_max_ensemble_spread_celsius
                        );
                        continue;
                    }
                }

                // Fetch YES + NO order books
                tokio::time::sleep(Duration::from_millis(150)).await;
                let yes_book = match clob::fetch_order_book(&self.global.clob_base, &market.token_id_yes).await {
                    Ok(b) => b,
                    Err(e) => {
                        tracing::debug!("[Custom:{}] {} YES book 失敗: {e}", self.sc.id, market.slug);
                        continue;
                    }
                };
                let no_book = match clob::fetch_order_book(&self.global.clob_base, &market.token_id_no).await {
                    Ok(b) => b,
                    Err(e) => {
                        tracing::debug!("[Custom:{}] {} NO book 失敗: {e}", self.sc.id, market.slug);
                        continue;
                    }
                };

                let snap = WeatherBookSnapshot {
                    yes_best_ask: yes_book.best_ask,
                    yes_best_bid: yes_book.best_bid,
                    no_best_ask:  no_book.best_ask,
                    no_best_bid:  no_book.best_bid,
                    depth_usdc:   yes_book.depth_usdc,
                };

                // ── Update price history (always, regardless of signal) ────────
                let history = price_history
                    .entry(market.slug.clone())
                    .or_insert_with(VecDeque::new);
                history.push_back(PriceTick {
                    no_ask:  snap.no_best_ask,
                    yes_ask: snap.yes_best_ask,
                });
                let max_ticks = self.sc.customized_lookback_ticks as usize + 1;
                while history.len() > max_ticks {
                    history.pop_front();
                }

                // ── Core signal from ensemble model ───────────────────────────
                let signal = weather_decision::evaluate(fc, market, &snap, &decision_cfg);
                if signal.direction == WeatherDirection::Hold {
                    continue;
                }

                // ── Gate 2/3 (ENTRY_PRICE range) ─────────────────────────────
                let entry_ask = match signal.direction {
                    WeatherDirection::BuyYes => snap.yes_best_ask,
                    WeatherDirection::BuyNo  => snap.no_best_ask,
                    WeatherDirection::Hold   => unreachable!(),
                };
                if entry_ask < self.sc.customized_min_entry_price {
                    tracing::debug!(
                        "[Custom:{}] {} ask={:.4} < MIN_ENTRY={:.2}，跳過",
                        self.sc.id, market.slug, entry_ask, self.sc.customized_min_entry_price
                    );
                    continue;
                }
                if entry_ask > self.sc.customized_max_entry_price {
                    tracing::debug!(
                        "[Custom:{}] {} ask={:.4} > MAX_ENTRY={:.2}，跳過",
                        self.sc.id, market.slug, entry_ask, self.sc.customized_max_entry_price
                    );
                    continue;
                }

                // ── Gate 1 (LOOKBACK_SLOPE) ───────────────────────────────────
                let history = price_history.get(&market.slug).unwrap();
                if history.len() >= self.sc.customized_min_history_ticks as usize {
                    let price_series: Vec<f64> = history.iter().map(|t| {
                        match signal.direction {
                            WeatherDirection::BuyNo  => t.no_ask,
                            WeatherDirection::BuyYes => t.yes_ask,
                            WeatherDirection::Hold   => t.yes_ask,
                        }
                    }).collect();
                    let slope = slope_per_tick(&price_series);
                    if slope < self.sc.customized_min_slope {
                        tracing::debug!(
                            "[Custom:{}] {} 斜率 {:.5}/tick < 最低要求 {:.5}，跳過",
                            self.sc.id, market.slug, slope, self.sc.customized_min_slope
                        );
                        continue;
                    }
                    tracing::debug!(
                        "[Custom:{}] {} 斜率 {:.5}/tick ✓ ({} ticks)",
                        self.sc.id, market.slug, slope, history.len()
                    );
                } else {
                    tracing::debug!(
                        "[Custom:{}] {} 歷史 tick 不足 ({}/{})，略過斜率閘門",
                        self.sc.id, market.slug,
                        history.len(), self.sc.customized_min_history_ticks
                    );
                }

                // ── All gates passed — compute position ───────────────────────
                let (side, token_id) = match signal.direction {
                    WeatherDirection::BuyYes => ("YES", market.token_id_yes.clone()),
                    WeatherDirection::BuyNo  => ("NO",  market.token_id_no.clone()),
                    WeatherDirection::Hold   => unreachable!(),
                };

                let bet_size = {
                    let cap = self.capital.lock()
                        .unwrap_or_else(|e| { tracing::error!("[Mutex Poisoned] capital: {e}"); e.into_inner() });
                    cap.current_bet_size()
                };

                let profit_target = (self.sc.taker_fee_bps * 2.0
                    + self.sc.slippage_buffer_bps * 2.0
                    + self.sc.min_net_edge_bps * 0.5)
                    / 10_000.0;
                let effective_sl_delta = if lead_days >= 1 {
                    self.sc.stop_loss_delta * 2.0
                } else {
                    self.sc.stop_loss_delta
                };
                let stop_loss_price = entry_ask - effective_sl_delta;

                let secs_to_deadline = market.close_ts
                    .saturating_sub(now_ts)
                    .saturating_sub(effective_abort_sec);

                tracing::info!(
                    "[Custom:{}] ENTRY {} side={} ask={:.4} p_yes={:.3} edge={:.0}bps lead={}d",
                    self.sc.id, market.slug, side, entry_ask,
                    signal.p_yes, signal.edge_bps, lead_days
                );

                if self.global.is_dry_run() {
                    let event_id = format!("{}-{}-{}",
                        self.sc.id, market.slug,
                        Utc::now().timestamp_nanos_opt().unwrap_or_default());
                    let _ = db.write_weather_dry_run_trade(&WeatherDryRunTrade {
                        strategy_id:            self.sc.id.clone(),
                        event_id:               event_id.clone(),
                        market_slug:            market.slug.clone(),
                        city:                   city.clone(),
                        market_type:            market.market_type.to_string(),
                        side:                   side.to_string(),
                        action:                 "ENTRY".into(),
                        price:                  entry_ask,
                        size_usdc:              bet_size,
                        spread_at_decision:     Some(snap.yes_best_ask - snap.yes_best_bid),
                        depth_usdc_at_decision: Some(snap.depth_usdc),
                        entry_price:            Some(entry_ask),
                        exit_price:             None,
                        hold_sec:               None,
                        model:                  "ensemble".into(),
                        p_yes_at_entry:         Some(signal.p_yes),
                        p_yes_at_exit:          None,
                        lead_days:              Some(lead_days),
                        taker_fee_bps:          Some(self.sc.taker_fee_bps as i64),
                        slippage_buffer_bps:    Some(self.sc.slippage_buffer_bps as i64),
                        expected_net_edge_bps:  Some(signal.edge_bps),
                        realized_pnl_usdc:      None,
                        reason_code:            "BUY_NO".into(),
                        note:                   None,
                        token_id:               token_id.clone(),
                        close_ts:               market.close_ts as i64,
                    }).await;

                    new_positions.push(WeatherCustomPosition {
                        event_id,
                        market_slug:           market.slug.clone(),
                        token_id,
                        market:                market.clone(),
                        side:                  side.to_string(),
                        entry_price:           entry_ask,
                        entry_ts:              Utc::now().timestamp(),
                        abort_at:              Instant::now() + Duration::from_secs(secs_to_deadline),
                        size_usdc:             bet_size,
                        profit_target,
                        bid_best:              entry_ask,
                        stop_loss_price,
                        p_yes_at_entry:        signal.p_yes,
                        model:                 WeatherModel::Ensemble,
                        lead_days,
                        expected_net_edge_bps: signal.edge_bps,
                        consecutive_sl_ticks:  0,
                    });
                } else {
                    // Live mode
                    let fee_usdc = self.global.compute_fee(bet_size);
                    let intent = executor::OrderIntent {
                        strategy_id: self.sc.id.clone(),
                        market_slug: market.slug.clone(),
                        token_id:    token_id.clone(),
                        side:        "BUY".to_string(),
                        price:       entry_ask,
                        size_usdc:   bet_size,
                        fee_usdc,
                        leg:         1,
                        signal_dump_pct: None,
                        hedge_sum:       None,
                    };
                    match executor::submit_order(&intent, &self.global, db, &self.capital).await {
                        Ok(_) => {}
                        Err(e) => {
                            tracing::error!("[Custom:{}] LIVE 訂單失敗 {}: {e}", self.sc.id, market.slug);
                            continue;
                        }
                    }
                    new_positions.push(WeatherCustomPosition {
                        event_id:              format!("{}-live-{}", self.sc.id, market.slug),
                        market_slug:           market.slug.clone(),
                        token_id,
                        market:                market.clone(),
                        side:                  side.to_string(),
                        entry_price:           entry_ask,
                        entry_ts:              Utc::now().timestamp(),
                        abort_at:              Instant::now() + Duration::from_secs(secs_to_deadline),
                        size_usdc:             bet_size,
                        profit_target,
                        bid_best:              entry_ask,
                        stop_loss_price,
                        p_yes_at_entry:        signal.p_yes,
                        model:                 WeatherModel::Ensemble,
                        lead_days,
                        expected_net_edge_bps: signal.edge_bps,
                        consecutive_sl_ticks:  0,
                    });
                }
            }
        }

        Ok(new_positions)
    }

    // ── Decision config builder ───────────────────────────────────────────────

    fn make_decision_cfg(&self, min_depth_usdc: f64, city: &str) -> WeatherDecisionConfig {
        let bet_size = {
            let cap = self.capital.lock()
                .unwrap_or_else(|e| { tracing::error!("[Mutex Poisoned] capital: {e}"); e.into_inner() });
            cap.current_bet_size()
        };
        WeatherDecisionConfig {
            taker_fee_bps:                  self.sc.taker_fee_bps,
            slippage_buffer_bps:            self.sc.slippage_buffer_bps,
            min_net_edge_bps:               self.sc.min_net_edge_bps,
            min_model_confidence:           self.sc.min_model_confidence,
            min_model_confidence_temprange: self.sc.min_model_confidence_temprange,
            min_temprange_p_yes:            self.sc.min_temprange_p_yes,
            min_ensemble_members:           self.sc.min_ensemble_members,
            max_spread:                     self.sc.max_spread,
            min_depth_usdc,
            bet_size_usdc:                  bet_size,
            city_sigma_mult:                self.global.city_sigma_mult(city),
            forecast_temp_bias_celsius:     self.sc.forecast_temp_bias_celsius,
        }
    }

    // ── Exit helpers ──────────────────────────────────────────────────────────

    async fn write_exit(
        &self, db: &DbWriter, pos: &WeatherCustomPosition,
        action: &str, exit_price: f64, hold_sec: i64, pnl: f64, p_yes_exit: Option<f64>,
    ) {
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
            model:                  "ensemble".to_string(),
            p_yes_at_entry:         Some(pos.p_yes_at_entry),
            p_yes_at_exit:          p_yes_exit,
            lead_days:              Some(pos.lead_days),
            taker_fee_bps:          Some(self.sc.taker_fee_bps as i64),
            slippage_buffer_bps:    Some(self.sc.slippage_buffer_bps as i64),
            expected_net_edge_bps:  Some(pos.expected_net_edge_bps),
            realized_pnl_usdc:      Some(pnl),
            reason_code:            action.to_string(),
            note:                   None,
            token_id:               pos.token_id.clone(),
            close_ts:               pos.market.close_ts as i64,
        }).await;
    }

    async fn write_exit_with_p_exit(
        &self, db: &DbWriter, pos: &WeatherCustomPosition,
        action: &str, exit_price: f64, hold_sec: i64, pnl: f64, p_yes_exit: Option<f64>,
    ) {
        self.write_exit(db, pos, action, exit_price, hold_sec, pnl, p_yes_exit).await;
    }

    async fn submit_live_exit(
        &self, pos: &WeatherCustomPosition, exit_price: f64, db: &DbWriter, action: &str,
    ) -> Result<(), AppError> {
        let fee_usdc = self.global.compute_fee(pos.size_usdc);
        let intent = executor::OrderIntent {
            strategy_id: self.sc.id.clone(),
            market_slug: pos.market_slug.clone(),
            token_id:    pos.token_id.clone(),
            side:        "SELL".to_string(),
            price:       exit_price,
            size_usdc:   pos.size_usdc,
            fee_usdc,
            leg:         2,
            signal_dump_pct: None,
            hedge_sum:       None,
        };
        let result = executor::submit_order(&intent, &self.global, db, &self.capital).await?;
        tracing::info!(
            "[Custom:{}] LIVE {} 已送出平倉單 slug={} price={:.4} result={:?}",
            self.sc.id, action, pos.market_slug, exit_price, result
        );
        Ok(())
    }
}

// ── Module-level helpers ──────────────────────────────────────────────────────

/// Compute ensemble standard deviation of temperature forecasts (°C).
/// Returns 0.0 if fewer than 2 members.
pub fn ensemble_std_dev(members: &[f64]) -> f64 {
    if members.len() < 2 {
        return 0.0;
    }
    let mean = members.iter().sum::<f64>() / members.len() as f64;
    let variance = members.iter().map(|&x| (x - mean).powi(2)).sum::<f64>()
        / (members.len() - 1) as f64;
    variance.sqrt()
}

/// Compute simple rise-over-run slope of a price series (price change per tick).
/// Returns 0.0 for series with fewer than 2 points.
pub fn slope_per_tick(prices: &[f64]) -> f64 {
    let n = prices.len();
    if n < 2 {
        return 0.0;
    }
    (prices[n - 1] - prices[0]) / (n - 1) as f64
}
