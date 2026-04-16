// Weather Ladder strategy — 氣象階梯式非對稱跨式套利
//
// Scans for clusters of adjacent exact-temperature markets priced extremely low.
// Enters multiple adjacent legs simultaneously to create a "ladder" capturing
// a temperature range, with a very high payout ratio (80x–500x).
//
// Entry: buy YES on all legs in the identified cluster.
// Exit:
//   HOLD_TO_RESOLUTION — wait for market close, collect $1.00 on the winning leg.
//   CATASTROPHIC_SHIFT_EXIT — exit early if weather forecast shifts dramatically.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use chrono::{NaiveDate, Utc};
use tokio::time::interval;
use uuid::Uuid;

use crate::api::weather::{city_info, WeatherForecast, WeatherModel};
use crate::api::weather_market::{fetch_weather_markets, WeatherMarket, WeatherMarketType};
use crate::api::clob;
use crate::config::{BotConfig, StrategyConfig};
use crate::db::writer::{DbWriter, WeatherLadderTrade};
use crate::risk::capital::SharedCapital;
use crate::strategy::{weather_decision, weather_executor};

// ── Types ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct LadderLeg {
    market: WeatherMarket,
    entry_price: f64,
    size_usdc: f64,
    p_yes: f64,
    /// Effective temperature midpoint for this leg (°C)
    temp_mid: f64,
}

#[derive(Debug, Clone)]
struct OpenLadder {
    ladder_id: String,
    city: String,
    target_date: NaiveDate,
    legs: Vec<LadderLeg>,
    entry_ts: i64,
    /// Sum of entry prices (determines payout ratio)
    sum_prices: f64,
    /// Combined p_yes at entry
    combined_p_yes_at_entry: f64,
    /// Total USDC deployed (all legs combined)
    total_size_usdc: f64,
    /// Forecast model label
    model: String,
    /// Lead days at entry
    lead_days: i64,
}

// ── Public strategy struct ─────────────────────────────────────────────────────

pub struct WeatherLadderStrategy {
    global: BotConfig,
    sc: StrategyConfig,
    capital: SharedCapital,
}

impl WeatherLadderStrategy {
    pub fn new(global: BotConfig, sc: StrategyConfig, capital: SharedCapital) -> Self {
        WeatherLadderStrategy { global, sc, capital }
    }

    pub async fn run_loop(&self, db: &DbWriter) {
        let interval_sec = self.sc.loop_interval_sec.max(600); // min 10 minutes
        tracing::info!(
            "[Ladder:{}] 啟動  min_payout={:.0}x  min_p={:.2}  \
             leg_price=[{:.4}, {:.4}]  min_legs={}  interval={}s",
            self.sc.id,
            self.sc.ladder_min_payout_ratio,
            self.sc.ladder_min_combined_p_yes,
            self.sc.ladder_min_leg_price,
            self.sc.ladder_max_leg_price,
            self.sc.ladder_min_legs,
            interval_sec,
        );

        let mut open_ladders: Vec<OpenLadder> = Vec::new();
        let mut entered_keys: HashSet<String> = HashSet::new(); // city+date dedup
        let mut ticker = interval(Duration::from_secs(interval_sec));

        loop {
            ticker.tick().await;

            // Circuit breaker
            {
                let cap = self.capital.lock().expect("capital mutex poisoned");
                if cap.is_stopped() {
                    tracing::warn!("[Ladder:{}] 停損觸發，暫停本輪", self.sc.id);
                    continue;
                }
            }

            // 1. Monitor open ladders
            self.monitor_ladders(&mut open_ladders, db).await;

            // 2. Scan for new ladder entries
            match self.scan_for_ladders(&open_ladders, &entered_keys, db).await {
                Ok(new_ladders) => {
                    for ladder in new_ladders {
                        let key = format!("{}:{}", ladder.city, ladder.target_date);
                        entered_keys.insert(key);
                        open_ladders.push(ladder);
                    }
                }
                Err(e) => {
                    tracing::warn!("[Ladder:{}] scan_for_ladders 失敗: {e}", self.sc.id);
                }
            }
        }
    }

    // ── Monitor ───────────────────────────────────────────────────────────────

    async fn monitor_ladders(&self, ladders: &mut Vec<OpenLadder>, db: &DbWriter) {
        let now_ts = Utc::now().timestamp() as u64;
        let mut to_remove: Vec<usize> = Vec::new();

        for (idx, ladder) in ladders.iter().enumerate() {
            let any_leg_expired = ladder.legs.iter().any(|leg| {
                leg.market.close_ts <= now_ts
            });

            if any_leg_expired {
                // HOLD_TO_RESOLUTION — market closed, record payout
                let hold_sec = now_ts as i64 - ladder.entry_ts;
                // In dry_run: we don't know which leg won, mark pnl as null
                // (backtest engine can resolve via outcome data)
                tracing::info!(
                    "[Ladder:{}] HOLD_TO_RESOLUTION {} {}  legs={}  sum_price={:.4}  \
                     payout_ratio={:.0}x  hold={}s",
                    self.sc.id, ladder.city, ladder.target_date,
                    ladder.legs.len(),
                    ladder.sum_prices,
                    1.0 / ladder.sum_prices,
                    hold_sec,
                );
                for (i, leg) in ladder.legs.iter().enumerate() {
                    let _ = db.write_weather_ladder_trade(&WeatherLadderTrade {
                        strategy_id: self.sc.id.clone(),
                        ladder_id: ladder.ladder_id.clone(),
                        market_slug: leg.market.slug.clone(),
                        city: ladder.city.clone(),
                        target_date: ladder.target_date.to_string(),
                        action: "HOLD_TO_RESOLUTION".into(),
                        leg_index: i as i64,
                        price: leg.entry_price,
                        size_usdc: leg.size_usdc,
                        p_yes: Some(leg.p_yes),
                        lead_days: Some(ladder.lead_days),
                        ladder_legs: ladder.legs.len() as i64,
                        ladder_sum_price: ladder.sum_prices,
                        ladder_payout_ratio: 1.0 / ladder.sum_prices,
                        ladder_combined_p: ladder.combined_p_yes_at_entry,
                        realized_pnl_usdc: None, // resolved by settlement layer
                        model: ladder.model.clone(),
                        reason_code: "HOLD_TO_RESOLUTION".into(),
                        note: Some(format!("hold_sec={hold_sec}")),
                    }).await;
                }
                let fee = self.sc.global_taker_fee() * ladder.total_size_usdc;
                let _ = self.capital.lock().map(|mut cap| {
                    cap.on_cycle_end(None, None, ladder.total_size_usdc, fee);
                });
                to_remove.push(idx);
                continue;
            }

            // Check for catastrophic forecast shift
            if let Some(current_combined_p) = self.evaluate_current_combined_p(ladder).await {
                let shift = ladder.combined_p_yes_at_entry - current_combined_p;
                if shift >= self.sc.ladder_catastrophic_shift_threshold {
                    let hold_sec = now_ts as i64 - ladder.entry_ts;
                    let realized_pnl = -ladder.total_size_usdc; // lose everything (no SL)
                    tracing::warn!(
                        "[Ladder:{}] CATASTROPHIC_SHIFT_EXIT {} {}  \
                         combined_p: {:.3}→{:.3} (delta={:.3})  hold={}s",
                        self.sc.id, ladder.city, ladder.target_date,
                        ladder.combined_p_yes_at_entry, current_combined_p,
                        shift, hold_sec,
                    );
                    for (i, leg) in ladder.legs.iter().enumerate() {
                        let _ = db.write_weather_ladder_trade(&WeatherLadderTrade {
                            strategy_id: self.sc.id.clone(),
                            ladder_id: ladder.ladder_id.clone(),
                            market_slug: leg.market.slug.clone(),
                            city: ladder.city.clone(),
                            target_date: ladder.target_date.to_string(),
                            action: "CATASTROPHIC_SHIFT_EXIT".into(),
                            leg_index: i as i64,
                            price: leg.entry_price,
                            size_usdc: leg.size_usdc,
                            p_yes: Some(leg.p_yes),
                            lead_days: Some(ladder.lead_days),
                            ladder_legs: ladder.legs.len() as i64,
                            ladder_sum_price: ladder.sum_prices,
                            ladder_payout_ratio: 1.0 / ladder.sum_prices,
                            ladder_combined_p: ladder.combined_p_yes_at_entry,
                            realized_pnl_usdc: Some(realized_pnl / ladder.legs.len() as f64),
                            model: ladder.model.clone(),
                            reason_code: "CATASTROPHIC_SHIFT".into(),
                            note: Some(format!(
                                "current_p={current_combined_p:.3} shift={shift:.3}"
                            )),
                        }).await;
                    }
                    let _ = self.capital.lock().map(|mut cap| {
                        cap.on_cycle_end(None, Some(0.0), ladder.total_size_usdc, ladder.total_size_usdc);
                    });
                    to_remove.push(idx);
                }
            }
        }

        for idx in to_remove.into_iter().rev() {
            ladders.swap_remove(idx);
        }
    }

    async fn evaluate_current_combined_p(&self, ladder: &OpenLadder) -> Option<f64> {
        let info = city_info(&ladder.city)?;
        let forecasts = weather_executor::fetch_city_forecast_single(info, WeatherModel::Gfs).await?;
        let fc = find_forecast_for_date(&forecasts, ladder.target_date)?;

        let mut combined = 0.0_f64;
        for leg in &ladder.legs {
            if let Some(p) = weather_decision::probability_yes(fc, &leg.market, self.sc.min_ensemble_members) {
                combined += p;
            }
        }
        Some(combined.min(1.0))
    }

    // ── Scan ──────────────────────────────────────────────────────────────────

    async fn scan_for_ladders(
        &self,
        existing: &[OpenLadder],
        entered_keys: &HashSet<String>,
        db: &DbWriter,
    ) -> Result<Vec<OpenLadder>, crate::error::AppError> {
        let now_date = Utc::now().date_naive();

        // 1. Fetch markets
        let all_markets = match fetch_weather_markets().await {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!("[Ladder:{}] 市場獲取失敗: {e}", self.sc.id);
                return Ok(Vec::new());
            }
        };

        // 2. Filter to TempRange markets; group by (city, target_date)
        let mut by_city_date: HashMap<(String, NaiveDate), Vec<WeatherMarket>> = HashMap::new();

        for m in all_markets {
            if m.market_type != WeatherMarketType::TempRange {
                continue;
            }
            if m.city.is_empty() || city_info(&m.city).is_none() {
                continue;
            }
            // Lead day filter
            let lead = (m.target_date - now_date).num_days();
            if lead < self.sc.weather_min_lead_days as i64
                || lead > self.sc.weather_max_lead_days as i64 {
                continue;
            }
            // Already entered this (city, date)?
            let key = format!("{}:{}", m.city, m.target_date);
            if entered_keys.contains(&key) {
                continue;
            }
            // Already have open ladder for this (city, date)?
            if existing.iter().any(|l| l.city == m.city && l.target_date == m.target_date) {
                continue;
            }
            by_city_date
                .entry((m.city.clone(), m.target_date))
                .or_default()
                .push(m);
        }

        if by_city_date.is_empty() {
            return Ok(Vec::new());
        }

        // 3. For each (city, date) group, fetch CLOB prices and find ladder candidates
        let mut new_ladders: Vec<OpenLadder> = Vec::new();

        for ((city, target_date), mut markets) in by_city_date {
            // Sort by temperature midpoint
            markets.sort_by(|a, b| {
                let ta = temp_mid(a);
                let tb = temp_mid(b);
                ta.partial_cmp(&tb).unwrap_or(std::cmp::Ordering::Equal)
            });

            // Fetch CLOB book prices for each market
            let mut priced: Vec<(WeatherMarket, f64)> = Vec::new();
            for m in &markets {
                match clob::fetch_order_book(&self.global.clob_base, &m.token_id_yes).await {
                    Ok(book) => {
                        let ask = book.best_ask;
                        if ask >= self.sc.ladder_min_leg_price
                            && ask <= self.sc.ladder_max_leg_price {
                            priced.push((m.clone(), ask));
                        }
                    }
                    Err(e) => {
                        let msg = e.to_string();
                        if !msg.contains("404") {
                            tracing::debug!("[Ladder:{}] {} book 失敗: {e}", self.sc.id, m.slug);
                        }
                    }
                }
            }

            if priced.len() < self.sc.ladder_min_legs {
                continue;
            }

            // 4. Find consecutive adjacent clusters within priced markets
            let clusters = find_adjacent_clusters(
                &priced,
                3.0,
                self.sc.ladder_min_legs,
                self.sc.ladder_max_legs,
            );

            // 5. Evaluate each cluster
            for cluster in clusters {
                let sum_prices: f64 = cluster.iter().map(|(_, p)| *p).sum();
                if sum_prices <= 0.0 {
                    continue;
                }
                let payout_ratio = 1.0 / sum_prices;
                if payout_ratio < self.sc.ladder_min_payout_ratio {
                    continue;
                }

                // Get weather model forecast for this city
                let info = match city_info(&city) {
                    Some(i) => i,
                    None => continue,
                };
                let forecast_model = WeatherModel::Gfs;
                let forecasts = match weather_executor::fetch_city_forecast_single(
                    info, forecast_model
                ).await {
                    Some(f) => f,
                    None => {
                        tracing::debug!("[Ladder:{}] {} 預測獲取失敗，跳過", self.sc.id, city);
                        continue;
                    }
                };
                let fc = match find_forecast_for_date(&forecasts, target_date) {
                    Some(f) => f,
                    None => continue,
                };

                // Calculate combined p_yes
                let mut combined_p = 0.0_f64;
                let mut leg_ps: Vec<f64> = Vec::new();
                for (market, _price) in &cluster {
                    let p = weather_decision::probability_yes(fc, market, self.sc.min_ensemble_members).unwrap_or(0.0);
                    leg_ps.push(p);
                    combined_p += p;
                }
                let combined_p = combined_p.min(1.0);

                if combined_p < self.sc.ladder_min_combined_p_yes {
                    tracing::debug!(
                        "[Ladder:{}] {} {} combined_p={:.3} < {:.3}，跳過",
                        self.sc.id, city, target_date, combined_p,
                        self.sc.ladder_min_combined_p_yes
                    );
                    continue;
                }

                // 6. Calculate size per leg
                let leg_count = cluster.len();
                let max_size_per_leg = (self.sc.ladder_max_total_usdc / leg_count as f64)
                    .min(self.sc.initial_allocated_usdc * self.sc.trade_size_pct);
                let size_per_leg = max_size_per_leg.max(0.01); // min $0.01

                let lead_days = (target_date - Utc::now().date_naive()).num_days();
                let ladder_id = Uuid::new_v4().to_string();

                tracing::info!(
                    "[Ladder:{}] LADDER_ENTRY {} {}  legs={}  sum_price={:.4}  \
                     payout={:.0}x  combined_p={:.3}  size=${:.3}/leg  lead={}d",
                    self.sc.id, city, target_date,
                    leg_count, sum_prices, payout_ratio, combined_p,
                    size_per_leg, lead_days,
                );

                // 7. Write LADDER_ENTRY records + build OpenLadder
                let mut legs: Vec<LadderLeg> = Vec::new();
                for (i, (market, price)) in cluster.iter().enumerate() {
                    let p = leg_ps[i];
                    let _ = db.write_weather_ladder_trade(&WeatherLadderTrade {
                        strategy_id: self.sc.id.clone(),
                        ladder_id: ladder_id.clone(),
                        market_slug: market.slug.clone(),
                        city: city.clone(),
                        target_date: target_date.to_string(),
                        action: "LADDER_ENTRY".into(),
                        leg_index: i as i64,
                        price: *price,
                        size_usdc: size_per_leg,
                        p_yes: Some(p),
                        lead_days: Some(lead_days),
                        ladder_legs: leg_count as i64,
                        ladder_sum_price: sum_prices,
                        ladder_payout_ratio: payout_ratio,
                        ladder_combined_p: combined_p,
                        realized_pnl_usdc: None,
                        model: forecast_model.to_string(),
                        reason_code: "LADDER_ENTRY".into(),
                        note: Some(format!(
                            "temp_mid={:.1}C  p_yes={:.4}  payout={:.0}x",
                            temp_mid(market), p, payout_ratio
                        )),
                    }).await;

                    legs.push(LadderLeg {
                        market: market.clone(),
                        entry_price: *price,
                        size_usdc: size_per_leg,
                        p_yes: p,
                        temp_mid: temp_mid(market),
                    });
                }

                let total_size = size_per_leg * leg_count as f64;
                let fee = self.sc.global_taker_fee() * total_size;
                let _ = self.capital.lock().map(|mut cap| {
                    cap.on_order_submit(total_size, fee);
                });

                new_ladders.push(OpenLadder {
                    ladder_id,
                    city: city.clone(),
                    target_date,
                    legs,
                    entry_ts: Utc::now().timestamp(),
                    sum_prices,
                    combined_p_yes_at_entry: combined_p,
                    total_size_usdc: total_size,
                    model: forecast_model.to_string(),
                    lead_days,
                });
            }
        }

        Ok(new_ladders)
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Extract the temperature midpoint in °C from a WeatherMarket's temp_range.
fn temp_mid(m: &WeatherMarket) -> f64 {
    match m.temp_range {
        Some((lo, hi)) => (lo + hi) / 2.0,
        None => 0.0,
    }
}

/// Find the forecast for `target_date` in a slice of forecasts.
fn find_forecast_for_date(
    forecasts: &[WeatherForecast],
    target: NaiveDate,
) -> Option<&WeatherForecast> {
    forecasts.iter().find(|f| f.forecast_date == target)
}

/// Find all clusters of adjacent markets (temp_mid values within `gap_c` °C of each other).
/// Returns groups of (market, price) pairs.
fn find_adjacent_clusters(
    sorted: &[(WeatherMarket, f64)],
    gap_c: f64,
    min_legs: usize,
    max_legs: usize,
) -> Vec<Vec<(WeatherMarket, f64)>> {
    let mut clusters: Vec<Vec<(WeatherMarket, f64)>> = Vec::new();

    if sorted.is_empty() {
        return clusters;
    }

    let mut current: Vec<(WeatherMarket, f64)> = vec![sorted[0].clone()];

    for pair in sorted.iter().skip(1) {
        let prev_mid = temp_mid(&current.last().unwrap().0);
        let this_mid = temp_mid(&pair.0);
        if (this_mid - prev_mid).abs() <= gap_c {
            current.push(pair.clone());
            if current.len() > max_legs {
                // Sliding window — drop oldest to stay within max_legs
                current.remove(0);
            }
        } else {
            if current.len() >= min_legs {
                clusters.push(current.clone());
            }
            current = vec![pair.clone()];
        }
    }
    if current.len() >= min_legs {
        clusters.push(current);
    }

    clusters
}
