use std::time::{Duration, Instant};

use tokio::sync::mpsc;
use tokio::time::sleep_until;

use crate::api::gamma::MarketInfo;
use crate::config::{BotConfig, StrategyConfig};
use crate::db::writer::{CycleResult, DbWriter};
use crate::error::AppError;
use crate::execution::cycle::{LegState, TradeCycle};
use crate::execution::executor::Executor;
use crate::risk::capital::SharedCapital;
use crate::strategy::settlement;
use crate::ws::market_feed::PriceSnapshot;

// ── Strategy struct ───────────────────────────────────────────────────────────

pub struct DumpHedgeStrategy {
    global: BotConfig,
    sc: StrategyConfig,
    capital: SharedCapital,
}

impl DumpHedgeStrategy {
    pub fn new(global: BotConfig, sc: StrategyConfig, capital: SharedCapital) -> Self {
        DumpHedgeStrategy { global, sc, capital }
    }

    /// Run one complete 15-minute market cycle.
    ///
    /// Consumes `price_rx` (the combined Binance+Polymarket snapshot feed),
    /// drives the `TradeCycle` state machine on each tick, and returns
    /// a `CycleResult` once the cycle reaches a terminal state or the channel
    /// is closed. The result is also persisted to the database.
    pub async fn run_market_cycle(
        &self,
        info: &MarketInfo,
        db: &DbWriter,
        mut price_rx: mpsc::Receiver<PriceSnapshot>,
    ) -> Result<CycleResult, AppError> {
        let executor =
            Executor::new(self.global.clone(), db.clone(), self.capital.clone());

        // Abort `abort_before_close_sec` seconds before market close
        let now_ts = chrono::Utc::now().timestamp() as u64;
        let secs_to_close = info.close_ts.saturating_sub(now_ts);
        let secs_to_abort =
            secs_to_close.saturating_sub(self.global.abort_before_close_sec);

        let abort_at = Instant::now() + Duration::from_secs(secs_to_abort);

        {
            let cap = self.capital.lock().expect("capital mutex poisoned");
            tracing::info!(
                "[DumpHedge:{}] 開始週期  slug={}  secs_to_close={secs_to_close}  \
                 dump_threshold={:.0}%  hedge_sum_threshold={}  \
                 capital={:.4} USDC  bet_size={:.4} USDC{}",
                self.sc.id,
                info.slug,
                self.sc.dump_threshold_pct * 100.0,
                self.sc.hedge_threshold_sum,
                cap.current_capital,
                cap.current_bet_size(),
                if cap.is_stopped() { "  ⛔ STOPPED" } else { "" },
            );
        }

        let mut cycle = TradeCycle::new(
            self.sc.id.clone(),
            info.slug.clone(),
            info.up_token_id.clone(),
            info.down_token_id.clone(),
            abort_at,
            self.capital.clone(),
        );

        // ── Main event loop ───────────────────────────────────────────────────
        let deadline = tokio::time::Instant::from_std(abort_at);
        loop {
            tokio::select! {
                biased;
                _ = sleep_until(deadline) => {
                    tracing::warn!("[DumpHedge:{}] 外層截止時間觸發，強制退出主循環", self.sc.id);
                    break;
                }
                msg = price_rx.recv() => {
                    match msg {
                        Some(snapshot) => {
                            cycle
                                .on_price_update(&snapshot, &self.sc, &executor)
                                .await?;
                            if cycle.is_terminal() {
                                break;
                            }
                        }
                        None => break, // channel closed
                    }
                }
            }
        }

        let mut settlement_result = settlement::SettlementResult {
            resolved_winner: None,
            pnl_usdc: None,
        };
        if cycle.leg1_fill_price.is_some() {
            settlement_result = settlement::resolve_updown_pnl(
                &self.global,
                &info.slug,
                info.close_ts,
                cycle.leg1_fill_price,
                cycle.leg2_fill_price,
                cycle.leg1_bet_size,
            )
            .await?;

            if settlement_result.pnl_usdc.is_some() {
                cycle.state = LegState::Completed {
                    pnl_usdc: settlement_result.pnl_usdc.unwrap_or_default(),
                };
            }
        }

        // ── Capital accounting: return estimated payout ───────────────────────
        // on_order_submit() already deducted cost; on_cycle_end() adds back the
        // expected return so current_capital stays meaningful between cycles.
        let fee_per_leg = self.global.compute_fee(cycle.leg1_bet_size);
        self.capital
            .lock()
            .expect("capital mutex poisoned")
            .on_cycle_end(
                cycle.leg1_fill_price,
                cycle.leg2_fill_price,
                cycle.leg1_bet_size,
                fee_per_leg,
            );

        // ── Build CycleResult ─────────────────────────────────────────────────
        let mode = if self.global.is_dry_run() { "dry_run" } else { "live" }.to_string();

        let result = match &cycle.state {
            LegState::Completed { pnl_usdc } => CycleResult {
                strategy_id: self.sc.id.clone(),
                market_slug: info.slug.clone(),
                mode,
                leg1_side: Some("BUY".into()),
                leg1_price: cycle.leg1_fill_price,
                leg2_price: cycle.leg2_fill_price,
                resolved_winner: settlement_result.resolved_winner.clone(),
                pnl_usdc: Some(*pnl_usdc),
            },
            LegState::Aborted { reason } => {
                tracing::info!("[DumpHedge:{}] 週期結束 (Aborted): {reason}", self.sc.id);
                CycleResult {
                    strategy_id: self.sc.id.clone(),
                    market_slug: info.slug.clone(),
                    mode,
                    leg1_side: cycle.leg1_fill_price.map(|_| "BUY".into()),
                    leg1_price: cycle.leg1_fill_price,
                    leg2_price: cycle.leg2_fill_price,
                    resolved_winner: settlement_result.resolved_winner.clone(),
                    pnl_usdc: settlement_result.pnl_usdc,
                }
            }
            LegState::HedgingLeg2 => CycleResult {
                strategy_id: self.sc.id.clone(),
                market_slug: info.slug.clone(),
                mode,
                leg1_side: Some("BUY".into()),
                leg1_price: cycle.leg1_fill_price,
                leg2_price: cycle.leg2_fill_price,
                resolved_winner: settlement_result.resolved_winner.clone(),
                pnl_usdc: settlement_result.pnl_usdc,
            },
            _ => CycleResult {
                strategy_id: self.sc.id.clone(),
                market_slug: info.slug.clone(),
                mode,
                leg1_side: None,
                leg1_price: None,
                leg2_price: None,
                resolved_winner: None,
                pnl_usdc: None,
            },
        };

        tracing::info!(
            "[DumpHedge:{}] 週期完成  leg1={:?}  leg2={:?}  pnl={:?}",
            self.sc.id,
            result.leg1_price,
            result.leg2_price,
            result.pnl_usdc,
        );

        db.write_cycle_result(&result).await?;
        Ok(result)
    }
}
