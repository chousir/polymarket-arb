// Trump Mention Market — full execution loop.
//
// Architecture
// ────────────
// Each enabled Mention strategy runs as a background tokio task (spawned once
// in main.rs), completely independent of the BTC 15m market cycle.
//
// Every 60 seconds the task:
//   1. Monitors existing open positions → writes TP / SL / TIME_EXIT exits.
//   2. Fetches all active Trump mention markets (60 s TTL cache).
//   3. Applies keyword filter + active_keyword_count cap.
//   4. For each qualifying market (not already open), fetches YES + NO order
//      books and evaluates net edge via mention_decision::evaluate().
//   5. Writes ENTRY / CANCEL records to mention_dry_run_trades (dry_run) or
//      submits a live order to the CLOB (live mode).
//
// DRY_RUN compliance
// ──────────────────
// All order paths call db.write_mention_dry_run_trade() in dry_run mode and
// never touch the CLOB.  Live mode falls through to executor::submit_order().

use std::collections::HashSet;
use std::time::Duration;

use crate::api::{clob, mention_market};
use crate::config::{BotConfig, StrategyConfig};
use crate::db::writer::{DbWriter, MentionDryRunTrade};
use crate::error::AppError;
use crate::execution::executor;
use crate::risk::capital::SharedCapital;
use crate::strategy::mention_decision::{
    self, DirectionMode, MentionBookSnapshot, MentionDecisionConfig, TradeDirection,
};
use crate::strategy::mention_filter::{self, Decision};

const MIN_LOOP_INTERVAL_SEC: u64 = 30;

// ── Open position ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct OpenPosition {
    event_id: String,
    market_slug: String,
    /// CLOB token ID being held (YES or NO token)
    token_id: String,
    keyword: String,
    /// "YES" | "NO"
    side: String,
    entry_price: f64,
    entry_ts: i64,
    size_usdc: f64,
    take_profit_price: f64,
    /// Price must NOT rise above entry + stop_loss_delta (token price ↑ = bad for NO buyer)
    stop_loss_trigger_ask: f64,
    /// Unix ts when the market closes
    close_ts: u64,
    expected_net_edge_bps: f64,
}

// ── Public strategy struct ────────────────────────────────────────────────────

pub struct MentionStrategy {
    global: BotConfig,
    sc: StrategyConfig,
    capital: SharedCapital,
}

impl MentionStrategy {
    pub fn new(global: BotConfig, sc: StrategyConfig, capital: SharedCapital) -> Self {
        MentionStrategy { global, sc, capital }
    }

    /// Continuous scan loop.  Designed to run forever inside `tokio::spawn`.
    pub async fn run_loop(&self, db: &DbWriter) {
        let loop_interval_sec = self.sc.loop_interval_sec.max(MIN_LOOP_INTERVAL_SEC);
        tracing::info!(
            "[Mention:{}] 啟動掃描循環  direction={}  keywords={}/scan  \
             min_edge={:.0}bps  entry_no_min={:.4}  entry_yes_max={:.4}  interval={}s",
            self.sc.id,
            self.sc.direction_mode,
            self.sc.active_keyword_count,
            self.sc.min_net_edge_bps,
            self.sc.entry_no_min_price,
            self.sc.entry_yes_max_price,
            loop_interval_sec,
        );

        let mut open_positions: Vec<OpenPosition> = Vec::new();
        let mut interval = tokio::time::interval(Duration::from_secs(loop_interval_sec));

        loop {
            interval.tick().await;

            // ── Circuit-breaker / drawdown gate ────────────────────────────────
            {
                let cap = self.capital.lock().unwrap_or_else(|e| { tracing::error!("[Mutex Poisoned] capital: {e}"); e.into_inner() });
                if cap.is_stopped() {
                    tracing::warn!(
                        "[Mention:{}] ⛔ 停損觸發（capital={:.4}），暫停本輪掃描",
                        self.sc.id, cap.current_capital
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
                            "[Mention:{}] 本輪新增 {} 個持倉  總持倉={}",
                            self.sc.id, new_pos.len(),
                            open_positions.len() + new_pos.len()
                        );
                    }
                    open_positions.extend(new_pos);
                }
                Err(e) => {
                    tracing::warn!("[Mention:{}] scan_for_entries 失敗: {e}", self.sc.id);
                }
            }
        }
    }

    // ── Monitor open positions ────────────────────────────────────────────────

    async fn monitor_positions(&self, positions: &mut Vec<OpenPosition>, db: &DbWriter) {
        let now_ts = chrono::Utc::now().timestamp();
        let mut to_remove: Vec<usize> = Vec::new();

        for (idx, pos) in positions.iter().enumerate() {
            // ── TIME_EXIT: market closed ────────────────────────────────────────
            if pos.close_ts <= now_ts as u64 {
                let hold_sec = now_ts - pos.entry_ts;
                // Conservative: assume token resolved at current book fair-value ≈ entry
                let realized_pnl =
                    -self.global.compute_fee(pos.size_usdc); // fee is certain loss

                tracing::info!(
                    "[Mention:{}] TIME_EXIT {} side={} hold={}s  est_pnl={:.4}",
                    self.sc.id, pos.market_slug, pos.side, hold_sec, realized_pnl
                );
                if self.global.is_dry_run() {
                    let _ = db.write_mention_dry_run_trade(&MentionDryRunTrade {
                        strategy_id: self.sc.id.clone(),
                        event_id:    pos.event_id.clone(),
                        market_slug: pos.market_slug.clone(),
                        speaker:     "trump".to_string(),
                        keyword:     pos.keyword.clone(),
                        side:        pos.side.clone(),
                        action:      "TIME_EXIT".to_string(),
                        price:       pos.entry_price,
                        size_usdc:   pos.size_usdc,
                        spread_at_decision:       None,
                        depth_usdc_at_decision:   None,
                        entry_price:              Some(pos.entry_price),
                        exit_price:               Some(pos.entry_price),
                        hold_sec:                 Some(hold_sec),
                        taker_fee_bps:            Some(self.sc.taker_fee_bps as i64),
                        slippage_buffer_bps:      Some(self.sc.slippage_buffer_bps as i64),
                        execution_risk_bps:       Some(self.sc.execution_risk_bps as i64),
                        expected_net_edge_bps:    Some(pos.expected_net_edge_bps),
                        realized_pnl_usdc:        Some(realized_pnl),
                        reason_code:              "TIME_EXIT".to_string(),
                        note:                     None,
                    }).await;
                }
                self.capital.lock().unwrap_or_else(|e| { tracing::error!("[Mutex Poisoned] capital: {e}"); e.into_inner() })
                    .on_cycle_end(Some(pos.entry_price), None, pos.size_usdc,
                                  self.global.compute_fee(pos.size_usdc));
                to_remove.push(idx);
                continue;
            }

            // ── Fetch current book ──────────────────────────────────────────────
            let book = match clob::fetch_order_book(&self.global.clob_base, &pos.token_id).await {
                Ok(b) => b,
                Err(e) => {
                    tracing::debug!(
                        "[Mention:{}] 監控 {}: 訂單簿獲取失敗: {e}",
                        self.sc.id, pos.market_slug
                    );
                    continue;
                }
            };

            // ── TAKE_PROFIT: bid crossed the target ─────────────────────────────
            if book.best_bid >= pos.take_profit_price {
                let hold_sec = now_ts - pos.entry_ts;
                let exit_price = book.best_bid;
                let realized_pnl = (exit_price - pos.entry_price) * pos.size_usdc
                    - self.global.compute_fee(pos.size_usdc) * 2.0; // entry + exit fees
                tracing::info!(
                    "[Mention:{}] TAKE_PROFIT {} side={} bid={:.4} tp={:.4}  \
                     hold={}s  pnl={:.4}",
                    self.sc.id, pos.market_slug, pos.side,
                    book.best_bid, pos.take_profit_price, hold_sec, realized_pnl
                );
                if self.global.is_dry_run() {
                    let _ = db.write_mention_dry_run_trade(&MentionDryRunTrade {
                        strategy_id: self.sc.id.clone(),
                        event_id:    pos.event_id.clone(),
                        market_slug: pos.market_slug.clone(),
                        speaker:     "trump".to_string(),
                        keyword:     pos.keyword.clone(),
                        side:        pos.side.clone(),
                        action:      "TAKE_PROFIT".to_string(),
                        price:       exit_price,
                        size_usdc:   pos.size_usdc,
                        spread_at_decision:     Some(book.best_ask - book.best_bid),
                        depth_usdc_at_decision: None,
                        entry_price:            Some(pos.entry_price),
                        exit_price:             Some(exit_price),
                        hold_sec:               Some(hold_sec),
                        taker_fee_bps:          Some(self.sc.taker_fee_bps as i64),
                        slippage_buffer_bps:    Some(self.sc.slippage_buffer_bps as i64),
                        execution_risk_bps:     Some(self.sc.execution_risk_bps as i64),
                        expected_net_edge_bps:  Some(pos.expected_net_edge_bps),
                        realized_pnl_usdc:      Some(realized_pnl),
                        reason_code:            "TAKE_PROFIT".to_string(),
                        note:                   None,
                    }).await;
                }
                self.capital.lock().unwrap_or_else(|e| { tracing::error!("[Mutex Poisoned] capital: {e}"); e.into_inner() })
                    .on_cycle_end(Some(pos.entry_price), Some(exit_price), pos.size_usdc,
                                  self.global.compute_fee(pos.size_usdc));
                to_remove.push(idx);
                continue;
            }

            // ── STOP_LOSS: ask rose above the trigger level ─────────────────────
            if book.best_ask >= pos.stop_loss_trigger_ask {
                let hold_sec = now_ts - pos.entry_ts;
                // Approximate exit at best_ask (we'd be selling tokens back)
                let exit_price = pos.entry_price - self.sc.stop_loss_delta;
                let realized_pnl = (exit_price - pos.entry_price) * pos.size_usdc
                    - self.global.compute_fee(pos.size_usdc) * 2.0;
                tracing::info!(
                    "[Mention:{}] STOP_LOSS {} side={} ask={:.4} trigger={:.4}  \
                     hold={}s  pnl={:.4}",
                    self.sc.id, pos.market_slug, pos.side,
                    book.best_ask, pos.stop_loss_trigger_ask, hold_sec, realized_pnl
                );
                if self.global.is_dry_run() {
                    let _ = db.write_mention_dry_run_trade(&MentionDryRunTrade {
                        strategy_id: self.sc.id.clone(),
                        event_id:    pos.event_id.clone(),
                        market_slug: pos.market_slug.clone(),
                        speaker:     "trump".to_string(),
                        keyword:     pos.keyword.clone(),
                        side:        pos.side.clone(),
                        action:      "STOP_LOSS".to_string(),
                        price:       exit_price,
                        size_usdc:   pos.size_usdc,
                        spread_at_decision:     Some(book.best_ask - book.best_bid),
                        depth_usdc_at_decision: None,
                        entry_price:            Some(pos.entry_price),
                        exit_price:             Some(exit_price),
                        hold_sec:               Some(hold_sec),
                        taker_fee_bps:          Some(self.sc.taker_fee_bps as i64),
                        slippage_buffer_bps:    Some(self.sc.slippage_buffer_bps as i64),
                        execution_risk_bps:     Some(self.sc.execution_risk_bps as i64),
                        expected_net_edge_bps:  Some(pos.expected_net_edge_bps),
                        realized_pnl_usdc:      Some(realized_pnl),
                        reason_code:            "STOP_LOSS".to_string(),
                        note:                   None,
                    }).await;
                }
                self.capital.lock().unwrap_or_else(|e| { tracing::error!("[Mutex Poisoned] capital: {e}"); e.into_inner() })
                    .on_cycle_end(Some(pos.entry_price), Some(exit_price), pos.size_usdc,
                                  self.global.compute_fee(pos.size_usdc));
                to_remove.push(idx);
            }
        }

        // Remove closed positions (reverse order preserves indices)
        for idx in to_remove.into_iter().rev() {
            positions.swap_remove(idx);
        }
    }

    // ── Scan for new entries ──────────────────────────────────────────────────

    async fn scan_for_entries(
        &self,
        existing: &[OpenPosition],
        db: &DbWriter,
    ) -> Result<Vec<OpenPosition>, AppError> {
        // 1. Fetch markets (60 s TTL cache in mention_market module)
        let markets = match mention_market::fetch_trump_mention_markets().await {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!("[Mention:{}] 獲取市場失敗: {e}", self.sc.id);
                return Ok(Vec::new());
            }
        };
        tracing::debug!(
            "[Mention:{}] 獲取 {} 個 Trump mention 市場",
            self.sc.id, markets.len()
        );

        // 2. Keyword filter
        let all_verdicts = mention_filter::filter_markets(&markets);
        mention_filter::log_verdicts(&all_verdicts);

        // 3. Cap by active_keyword_count
        let verdicts: Vec<_> = all_verdicts
            .iter()
            .filter(|v| v.decision != Decision::Skip)
            .take(self.sc.active_keyword_count)
            .collect();

        if verdicts.is_empty() {
            tracing::debug!("[Mention:{}] 本輪無符合條件的市場", self.sc.id);
            return Ok(Vec::new());
        }

        // 4. Build decision config (snapshot bet_size from capital)
        let decision_cfg = self.make_decision_cfg();

        // Already-open slugs — don't re-enter
        let open_slugs: HashSet<&str> =
            existing.iter().map(|p| p.market_slug.as_str()).collect();

        let now_ts = chrono::Utc::now().timestamp();
        let mut new_positions = Vec::new();

        for verdict in &verdicts {
            let slug = &verdict.market.slug;

            if open_slugs.contains(slug.as_str()) {
                tracing::debug!("[Mention:{}] {} — 已有持倉，跳過", self.sc.id, slug);
                continue;
            }

            // Skip markets closing soon
            if verdict.market.close_ts
                <= (now_ts as u64).saturating_add(self.global.abort_before_close_sec)
            {
                tracing::info!(
                    "[Mention:{}] {} — 即將收盤（{}s），跳過",
                    self.sc.id, slug,
                    verdict.market.close_ts.saturating_sub(now_ts as u64)
                );
                continue;
            }

            // 5. Fetch YES + NO order books (small delay between requests)
            let yes_book = clob::fetch_order_book(&self.global.clob_base,
                                                  &verdict.market.token_id_yes).await;
            tokio::time::sleep(Duration::from_millis(400)).await;
            let no_book  = clob::fetch_order_book(&self.global.clob_base,
                                                  &verdict.market.token_id_no).await;

            let (yes_book, no_book) = match (yes_book, no_book) {
                (Ok(y), Ok(n)) => (y, n),
                (Err(e), _) | (_, Err(e)) => {
                    tracing::warn!(
                        "[Mention:{}] {} — 訂單簿獲取失敗: {e}", self.sc.id, slug
                    );
                    continue;
                }
            };

            let snapshot = MentionBookSnapshot {
                yes_best_ask: yes_book.best_ask,
                yes_best_bid: yes_book.best_bid,
                no_best_ask:  no_book.best_ask,
                no_best_bid:  no_book.best_bid,
                yes_depth_usdc: yes_book.depth_usdc,
                no_depth_usdc:  no_book.depth_usdc,
            };

            // 6. Evaluate edge
            let signal = mention_decision::evaluate(&snapshot, &decision_cfg);

            // Spread/depth context for DB records
            let (ctx_spread, ctx_depth) = match &verdict.decision {
                Decision::No  => (no_book.best_ask  - no_book.best_bid,  no_book.depth_usdc),
                Decision::Yes => (yes_book.best_ask - yes_book.best_bid, yes_book.depth_usdc),
                Decision::Skip => (0.0, 0.0),
            };

            let keyword = extract_keyword(&verdict.reason);
            let event_id = format!(
                "{}-{}-{}",
                self.sc.id, slug,
                chrono::Utc::now()
                    .timestamp_nanos_opt()
                    .unwrap_or_default()
            );

            tracing::info!(
                "[Mention:{}] {} | verdict={:?} signal={:?} edge={:.0}bps | {}",
                self.sc.id, slug, verdict.decision,
                signal.direction, signal.edge_bps, signal.reason
            );

            // 7. Act on signal
            if signal.direction == TradeDirection::Hold {
                // NO_TRADE — evaluate but don't trade
                if self.global.is_dry_run() {
                    let side_str = match &verdict.decision {
                        Decision::No  => "NO",
                        Decision::Yes => "YES",
                        _             => "SKIP",
                    };
                    let _ = db.write_mention_dry_run_trade(&MentionDryRunTrade {
                        strategy_id: self.sc.id.clone(),
                        event_id:    event_id.clone(),
                        market_slug: slug.clone(),
                        speaker:     "trump".to_string(),
                        keyword:     keyword.clone(),
                        side:        side_str.to_string(),
                        action:      "NO_TRADE".to_string(),
                        price:       0.0,
                        size_usdc:   0.0,
                        spread_at_decision:     Some(ctx_spread),
                        depth_usdc_at_decision: Some(ctx_depth),
                        entry_price:            None,
                        exit_price:             None,
                        hold_sec:               None,
                        taker_fee_bps:          Some(self.sc.taker_fee_bps as i64),
                        slippage_buffer_bps:    Some(self.sc.slippage_buffer_bps as i64),
                        execution_risk_bps:     Some(self.sc.execution_risk_bps as i64),
                        expected_net_edge_bps:  Some(signal.edge_bps),
                        realized_pnl_usdc:      None,
                        reason_code:            signal.reason_code.clone(),
                        note:                   Some(signal.reason.clone()),
                    }).await;
                }
            } else {
                // ENTRY — check capital then place order
                let (side, token_id, take_profit_price) = match &signal.direction {
                    TradeDirection::BuyNo => (
                        "NO",
                        verdict.market.token_id_no.clone(),
                        self.sc.take_profit_no_price,
                    ),
                    TradeDirection::BuyYes => (
                        "YES",
                        verdict.market.token_id_yes.clone(),
                        self.sc.take_profit_yes_price,
                    ),
                    TradeDirection::Hold => unreachable!(),
                };

                let bet_size = {
                    let cap = self.capital.lock().unwrap_or_else(|e| { tracing::error!("[Mutex Poisoned] capital: {e}"); e.into_inner() });
                    if cap.is_stopped() {
                        tracing::warn!(
                            "[Mention:{}] {} — 停損觸發，跳過入場", self.sc.id, slug
                        );
                        continue;
                    }
                    cap.current_bet_size()
                };
                let fee_usdc = self.global.compute_fee(bet_size);
                let stop_loss_trigger_ask = signal.entry_price + self.sc.stop_loss_delta;

                if self.global.is_dry_run() {
                    let _ = db.write_mention_dry_run_trade(&MentionDryRunTrade {
                        strategy_id: self.sc.id.clone(),
                        event_id:    event_id.clone(),
                        market_slug: slug.clone(),
                        speaker:     "trump".to_string(),
                        keyword:     keyword.clone(),
                        side:        side.to_string(),
                        action:      "ENTRY".to_string(),
                        price:       signal.entry_price,
                        size_usdc:   bet_size,
                        spread_at_decision:     Some(ctx_spread),
                        depth_usdc_at_decision: Some(ctx_depth),
                        entry_price:            Some(signal.entry_price),
                        exit_price:             None,
                        hold_sec:               None,
                        taker_fee_bps:          Some(self.sc.taker_fee_bps as i64),
                        slippage_buffer_bps:    Some(self.sc.slippage_buffer_bps as i64),
                        execution_risk_bps:     Some(self.sc.execution_risk_bps as i64),
                        expected_net_edge_bps:  Some(signal.edge_bps),
                        realized_pnl_usdc:      None,
                        reason_code:            signal.reason_code.clone(),
                        note:                   None,
                    }).await;

                    self.capital
                        .lock()
                        .unwrap_or_else(|e| { tracing::error!("[Mutex Poisoned] capital: {e}"); e.into_inner() })
                        .on_order_submit(bet_size, fee_usdc);

                    tracing::info!(
                        "[Mention:{}] [DRY_RUN] ENTRY {} {} @ {:.4}  \
                         edge={:.0}bps  size={:.2} USDC  TP={:.4}  SL_trigger={:.4}",
                        self.sc.id, side, slug, signal.entry_price,
                        signal.edge_bps, bet_size, take_profit_price,
                        stop_loss_trigger_ask
                    );
                } else {
                    // ── Live order ────────────────────────────────────────────
                    let intent = executor::OrderIntent {
                        strategy_id: self.sc.id.clone(),
                        market_slug: slug.clone(),
                        token_id:    token_id.clone(),
                        side:        "BUY".to_string(),
                        price:       signal.entry_price,
                        size_usdc:   bet_size,
                        fee_usdc,
                        leg:         1,
                        signal_dump_pct: None,
                        hedge_sum:       None,
                    };
                    match executor::submit_order(
                        &intent, &self.global, db, &self.capital
                    ).await {
                        Ok(result) => {
                            tracing::info!(
                                "[Mention:{}] LIVE 訂單已提交 slug={} side={} result={:?}",
                                self.sc.id, slug, side, result
                            );
                        }
                        Err(e) => {
                            tracing::error!(
                                "[Mention:{}] LIVE 訂單失敗 slug={} side={} err={e}",
                                self.sc.id, slug, side
                            );
                            continue; // don't track position if order failed
                        }
                    }
                }

                new_positions.push(OpenPosition {
                    event_id,
                    market_slug: slug.clone(),
                    token_id,
                    keyword,
                    side: side.to_string(),
                    entry_price: signal.entry_price,
                    entry_ts: now_ts,
                    size_usdc: bet_size,
                    take_profit_price,
                    stop_loss_trigger_ask,
                    close_ts: verdict.market.close_ts,
                    expected_net_edge_bps: signal.edge_bps,
                });
            }
        }

        Ok(new_positions)
    }

    // ── Helpers ───────────────────────────────────────────────────────────────

    fn make_decision_cfg(&self) -> MentionDecisionConfig {
        let bet_size = {
            let cap = self.capital.lock().unwrap_or_else(|e| { tracing::error!("[Mutex Poisoned] capital: {e}"); e.into_inner() });
            cap.current_bet_size()
        };
        MentionDecisionConfig {
            direction_mode: match self.sc.direction_mode.as_str() {
                "yes_first" => DirectionMode::YesFirst,
                "yes_only"  => DirectionMode::YesOnly,
                "no_only"   => DirectionMode::NoOnly,
                _           => DirectionMode::NoFirst,
            },
            entry_no_min_price:   self.sc.entry_no_min_price,
            entry_yes_max_price:  self.sc.entry_yes_max_price,
            take_profit_no_price: self.sc.take_profit_no_price,
            take_profit_yes_price:self.sc.take_profit_yes_price,
            stop_loss_delta:      self.sc.stop_loss_delta,
            taker_fee_bps:        self.sc.taker_fee_bps,
            slippage_buffer_bps:  self.sc.slippage_buffer_bps,
            execution_risk_bps:   self.sc.execution_risk_bps,
            min_net_edge_bps:     self.sc.min_net_edge_bps,
            bet_size_usdc:        bet_size,
            max_spread:           self.sc.max_spread,
        }
    }
}

// ── Utility ───────────────────────────────────────────────────────────────────

/// Extract the keyword string from a verdict reason string.
///
/// Reason format examples:
///   `NO pool keyword match: "crypto"`
///   `YES whitelist keyword match: "rigged election"`
fn extract_keyword(reason: &str) -> String {
    // Find content inside double-quotes
    if let (Some(s), Some(e)) = (reason.find('"'), reason.rfind('"')) {
        if e > s {
            return reason[s + 1..e].to_string();
        }
    }
    reason.to_string()
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_keyword_works() {
        assert_eq!(
            extract_keyword(r#"NO pool keyword match: "crypto""#),
            "crypto"
        );
        assert_eq!(
            extract_keyword(r#"YES whitelist keyword match: "rigged election""#),
            "rigged election"
        );
        // Falls back to full string when no quotes
        assert_eq!(
            extract_keyword("no keyword match"),
            "no keyword match"
        );
    }
}
