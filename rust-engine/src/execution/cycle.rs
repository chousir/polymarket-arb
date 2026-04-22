use std::time::Instant;

use crate::config::StrategyConfig;
use crate::error::AppError;
use crate::execution::executor::{Executor, OrderIntent};
use crate::risk::capital::SharedCapital;
use crate::strategy::signal;
use crate::ws::market_feed::PriceSnapshot;

// ── State machine ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum LegState {
    /// Waiting for the first BTC-dump signal to enter Leg 1.
    WaitingLeg1,
    /// Leg 1 order submitted; waiting for hedge condition (Up+Down sum < threshold).
    Leg1Filled { price: f64, ts: Instant },
    /// Both legs placed; waiting for market resolution.
    HedgingLeg2,
    /// Both legs resolved; PnL computed.
    Completed { pnl_usdc: f64 },
    /// Cycle ended without completing the hedge (timeout, abort deadline, etc.).
    Aborted { reason: String },
}

// ── TradeCycle ────────────────────────────────────────────────────────────────

pub struct TradeCycle {
    pub state: LegState,
    /// BTC price recorded on the very first snapshot (market open reference).
    pub open_price: f64,
    /// Hard deadline: at or after this instant the cycle is force-closed.
    pub abort_at: Instant,

    // Recorded fill prices (used for CycleResult and on_cycle_end)
    pub leg1_fill_price: Option<f64>,
    pub leg2_fill_price: Option<f64>,

    // Recorded bet sizes per leg (used for on_cycle_end capital accounting)
    pub leg1_bet_size: f64,
    pub leg2_bet_size: f64,

    // Market identifiers
    strategy_id: String,
    market_slug: String,
    up_token_id: String,
    down_token_id: String,

    // Capital tracker (shared with Executor)
    capital: SharedCapital,
}

impl TradeCycle {
    pub fn new(
        strategy_id: impl Into<String>,
        market_slug: impl Into<String>,
        up_token_id: impl Into<String>,
        down_token_id: impl Into<String>,
        abort_at: Instant,
        capital: SharedCapital,
    ) -> Self {
        TradeCycle {
            state: LegState::WaitingLeg1,
            open_price: 0.0,
            abort_at,
            leg1_fill_price: None,
            leg2_fill_price: None,
            leg1_bet_size: 0.0,
            leg2_bet_size: 0.0,
            strategy_id: strategy_id.into(),
            market_slug: market_slug.into(),
            up_token_id: up_token_id.into(),
            down_token_id: down_token_id.into(),
            capital,
        }
    }

    pub fn is_terminal(&self) -> bool {
        matches!(self.state, LegState::Completed { .. } | LegState::Aborted { .. })
    }

    /// Drive the state machine with the latest price snapshot.
    ///
    /// Called once per `PriceSnapshot` from the combined Binance+Polymarket feed.
    /// Transitions are logged; actual orders are submitted via `executor`.
    pub async fn on_price_update(
        &mut self,
        snapshot: &PriceSnapshot,
        sc: &StrategyConfig,
        executor: &Executor,
    ) -> Result<(), AppError> {
        if self.is_terminal() {
            return Ok(());
        }

        // ── Hard abort deadline ───────────────────────────────────────────────
        if Instant::now() >= self.abort_at {
            let reason = match &self.state {
                LegState::WaitingLeg1 => "到期未入場".to_string(),
                LegState::Leg1Filled { .. } => "到期未對沖，持有 Leg1".to_string(),
                LegState::HedgingLeg2 => "到期，Leg1+Leg2 均已提交".to_string(),
                _ => "到期".to_string(),
            };
            tracing::warn!("[Cycle:{}] 截止時間到，強制結束: {reason}", self.strategy_id);
            self.state = LegState::Aborted { reason };
            return Ok(());
        }

        // Clone to avoid borrow conflict when assigning self.state below
        let current = self.state.clone();

        match current {
            // ── Phase 1: Record open price, detect dump ───────────────────────
            LegState::WaitingLeg1 => {
                if self.open_price == 0.0 {
                    if snapshot.btc_last > 0.0 {
                        self.open_price = snapshot.btc_last;
                        tracing::info!(
                            "[Cycle:{}] 開盤 BTC={:.2}",
                            self.strategy_id,
                            self.open_price
                        );
                    }
                    return Ok(());
                }

                if snapshot.btc_last <= 0.0 {
                    return Ok(());
                }

                let dump_pct =
                    signal::compute_dump_pct(self.open_price, snapshot.btc_last);
                tracing::debug!(
                    "[Cycle:{}] BTC={:.2}  dump={:.4}%  threshold={:.4}%",
                    self.strategy_id,
                    snapshot.btc_last,
                    dump_pct * 100.0,
                    sc.dump_threshold_pct * 100.0,
                );

                if dump_pct >= sc.dump_threshold_pct {
                    // ── Check drawdown stop ───────────────────────────────────
                    {
                        let cap = self.capital.lock().unwrap_or_else(|e| { tracing::error!("[Mutex Poisoned] capital: {e}"); e.into_inner() });
                        if cap.is_stopped() {
                            tracing::warn!(
                                "[Cycle:{}] ⛔ 停損觸發，跳過 Leg1  drawdown={:.1}%",
                                self.strategy_id,
                                cap.drawdown_pct() * 100.0,
                            );
                            return Ok(());
                        }
                    }

                    let bet_size = self
                        .capital
                        .lock()
                        .unwrap_or_else(|e| { tracing::error!("[Mutex Poisoned] capital: {e}"); e.into_inner() })
                        .current_bet_size();
                    let fee_usdc = executor.config.compute_fee(bet_size);

                    tracing::info!(
                        "[Cycle:{}] Leg1 觸發  dump={:.4}%  bet={:.4} USDC  fee={:.4} USDC",
                        self.strategy_id,
                        dump_pct * 100.0,
                        bet_size,
                        fee_usdc,
                    );

                    executor
                        .submit(&OrderIntent {
                            strategy_id: self.strategy_id.clone(),
                            market_slug: self.market_slug.clone(),
                            token_id: self.up_token_id.clone(),
                            side: "BUY".to_string(),
                            price: snapshot.up_best_ask,
                            size_usdc: bet_size,
                            fee_usdc,
                            leg: 1,
                            signal_dump_pct: Some(dump_pct),
                            hedge_sum: None,
                        })
                        .await?;

                    self.leg1_fill_price = Some(snapshot.up_best_ask);
                    self.leg1_bet_size = bet_size;
                    self.state = LegState::Leg1Filled {
                        price: snapshot.up_best_ask,
                        ts: Instant::now(),
                    };
                }
            }

            // ── Phase 2: Wait for hedge condition ─────────────────────────────
            LegState::Leg1Filled { ts: leg1_ts, .. } => {
                // Leg 2 wait-limit exceeded → give up hedge, hold Leg 1
                if leg1_ts.elapsed().as_secs() > sc.hedge_wait_limit_sec {
                    tracing::warn!(
                        "[Cycle:{}] Leg2 等待逾時 ({}s)，持有 Leg1 等結算",
                        self.strategy_id,
                        sc.hedge_wait_limit_sec
                    );
                    self.state =
                        LegState::Aborted { reason: "hedge_wait_timeout".into() };
                    return Ok(());
                }

                if signal::is_hedge_condition(snapshot, sc.hedge_threshold_sum) {
                    // ── Check drawdown stop ───────────────────────────────────
                    {
                        let cap = self.capital.lock().unwrap_or_else(|e| { tracing::error!("[Mutex Poisoned] capital: {e}"); e.into_inner() });
                        if cap.is_stopped() {
                            tracing::warn!(
                                "[Cycle:{}] ⛔ 停損觸發，跳過 Leg2  drawdown={:.1}%",
                                self.strategy_id,
                                cap.drawdown_pct() * 100.0,
                            );
                            return Ok(());
                        }
                    }

                    let bet_size = self
                        .capital
                        .lock()
                        .unwrap_or_else(|e| { tracing::error!("[Mutex Poisoned] capital: {e}"); e.into_inner() })
                        .current_bet_size();
                    let fee_usdc = executor.config.compute_fee(bet_size);

                    tracing::info!(
                        "[Cycle:{}] Leg2 觸發  sum={:.4}  threshold={}  bet={:.4} USDC",
                        self.strategy_id,
                        snapshot.sum,
                        sc.hedge_threshold_sum,
                        bet_size,
                    );

                    executor
                        .submit(&OrderIntent {
                            strategy_id: self.strategy_id.clone(),
                            market_slug: self.market_slug.clone(),
                            token_id: self.down_token_id.clone(),
                            side: "BUY".to_string(),
                            price: snapshot.down_best_ask,
                            size_usdc: bet_size,
                            fee_usdc,
                            leg: 2,
                            signal_dump_pct: None,
                            hedge_sum: Some(snapshot.sum),
                        })
                        .await?;

                    self.leg2_fill_price = Some(snapshot.down_best_ask);
                    self.leg2_bet_size = bet_size;
                    self.state = LegState::HedgingLeg2;
                }
            }

            // ── Phase 3: Both legs placed — wait for resolution ───────────────
            LegState::HedgingLeg2 => {
                tracing::debug!("[Cycle:{}] HedgingLeg2 — 等待市場結算", self.strategy_id);
            }

            LegState::Completed { .. } | LegState::Aborted { .. } => unreachable!(),
        }

        Ok(())
    }
}
