/// 純套利策略（Pure Arb）
///
/// 不等 BTC 急跌，只要 Up ask + Down ask 的總和低於 `hedge_threshold_sum`，
/// 就立刻同時買進兩腿，鎖定確定利潤。
///
/// 數學前提：
///   市場到期後，Up 或 Down 必有一個結算為 $1，另一個為 $0。
///   若 up_ask + down_ask < 1 − fee，則兩腿同時持有保證獲利。
///
/// 狀態機（比 DumpHedge 更簡單）：
///   WaitingArb → BothPlaced → Completed | Aborted
use std::time::{Duration, Instant};

use tokio::sync::mpsc;
use tokio::time::sleep_until;

use crate::api::gamma::MarketInfo;
use crate::config::{BotConfig, StrategyConfig};
use crate::db::writer::{CycleResult, DbWriter};
use crate::error::AppError;
use crate::execution::executor::{Executor, OrderIntent};
use crate::risk::capital::SharedCapital;
use crate::strategy::settlement;
use crate::strategy::signal;
use crate::ws::market_feed::PriceSnapshot;

// ── State machine ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
enum ArbState {
    /// 等待 Up+Down sum 低於閾值
    WaitingArb,
    /// 兩腿均已提交，等待市場結算
    BothPlaced,
    /// 結算完成（Phase 3 後填入 pnl）
    Completed { pnl_usdc: f64 },
    /// 到期前未出現套利機會，或其他中止原因
    Aborted { reason: String },
}

// ── Strategy struct ───────────────────────────────────────────────────────────

pub struct PureArbStrategy {
    global: BotConfig,
    sc: StrategyConfig,
    capital: SharedCapital,
}

impl PureArbStrategy {
    pub fn new(global: BotConfig, sc: StrategyConfig, capital: SharedCapital) -> Self {
        PureArbStrategy { global, sc, capital }
    }

    pub async fn run_market_cycle(
        &self,
        info: &MarketInfo,
        db: &DbWriter,
        mut price_rx: mpsc::Receiver<PriceSnapshot>,
    ) -> Result<CycleResult, AppError> {
        let executor =
            Executor::new(self.global.clone(), db.clone(), self.capital.clone());

        let now_ts = chrono::Utc::now().timestamp() as u64;
        let secs_to_close = info.close_ts.saturating_sub(now_ts);
        let secs_to_abort =
            secs_to_close.saturating_sub(self.global.abort_before_close_sec);

        let abort_at = Instant::now() + Duration::from_secs(secs_to_abort);

        {
            let cap = self.capital.lock().expect("capital mutex poisoned");
            tracing::info!(
                "[PureArb:{}] 開始週期  slug={}  secs_to_close={secs_to_close}  \
                 hedge_sum_threshold={}  capital={:.4} USDC  bet_size={:.4} USDC{}",
                self.sc.id,
                info.slug,
                self.sc.hedge_threshold_sum,
                cap.current_capital,
                cap.current_bet_size(),
                if cap.is_stopped() { "  ⛔ STOPPED" } else { "" },
            );
        }

        let mut state = ArbState::WaitingArb;
        let mut leg1_price: Option<f64> = None;
        let mut leg2_price: Option<f64> = None;
        let mut leg1_bet_size: f64 = 0.0;

        let deadline = tokio::time::Instant::from_std(abort_at);

        loop {
            tokio::select! {
                biased;
                _ = sleep_until(deadline) => {
                    tracing::warn!("[PureArb:{}] 截止時間到，強制退出主循環", self.sc.id);
                    break;
                }
                msg = price_rx.recv() => {
                    match msg {
                        Some(snapshot) => {
                            state = self
                                .on_tick(
                                    state,
                                    &snapshot,
                                    &executor,
                                    info,
                                    &mut leg1_price,
                                    &mut leg2_price,
                                    &mut leg1_bet_size,
                                )
                                .await?;

                            if matches!(
                                state,
                                ArbState::Completed { .. } | ArbState::Aborted { .. }
                            ) {
                                break;
                            }
                        }
                        None => break,
                    }
                }
            }
        }

        let mut settlement_result = settlement::SettlementResult {
            resolved_winner: None,
            pnl_usdc: None,
        };
        if matches!(state, ArbState::BothPlaced | ArbState::Aborted { .. }) {
            settlement_result = settlement::resolve_updown_pnl(
                &self.global,
                &info.slug,
                info.close_ts,
                leg1_price,
                leg2_price,
                leg1_bet_size,
            )
            .await?;

            if let Some(pnl) = settlement_result.pnl_usdc {
                state = ArbState::Completed { pnl_usdc: pnl };
            }
        }

        // ── Capital accounting: return estimated payout ───────────────────────
        let fee_per_leg = self.global.compute_fee(leg1_bet_size);
        self.capital
            .lock()
            .expect("capital mutex poisoned")
            .on_cycle_end(leg1_price, leg2_price, leg1_bet_size, fee_per_leg);

        // ── Build CycleResult ─────────────────────────────────────────────────
        let mode = if self.global.is_dry_run() { "dry_run" } else { "live" }.to_string();

        let result = match &state {
            ArbState::Completed { pnl_usdc } => CycleResult {
                strategy_id: self.sc.id.clone(),
                market_slug: info.slug.clone(),
                mode,
                leg1_side: Some("BUY".into()),
                leg1_price,
                leg2_price,
                resolved_winner: settlement_result.resolved_winner.clone(),
                pnl_usdc: Some(*pnl_usdc),
            },
            ArbState::Aborted { reason } => {
                tracing::info!("[PureArb:{}] 週期結束 (Aborted): {reason}", self.sc.id);
                CycleResult {
                    strategy_id: self.sc.id.clone(),
                    market_slug: info.slug.clone(),
                    mode,
                    leg1_side: leg1_price.map(|_| "BUY".into()),
                    leg1_price,
                    leg2_price,
                    resolved_winner: settlement_result.resolved_winner.clone(),
                    pnl_usdc: settlement_result.pnl_usdc,
                }
            }
            ArbState::BothPlaced => CycleResult {
                strategy_id: self.sc.id.clone(),
                market_slug: info.slug.clone(),
                mode,
                leg1_side: Some("BUY".into()),
                leg1_price,
                leg2_price,
                resolved_winner: settlement_result.resolved_winner.clone(),
                pnl_usdc: settlement_result.pnl_usdc,
            },
            ArbState::WaitingArb => CycleResult {
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
            "[PureArb:{}] 週期完成  leg1={:?}  leg2={:?}  pnl={:?}",
            self.sc.id,
            result.leg1_price,
            result.leg2_price,
            result.pnl_usdc,
        );

        db.write_cycle_result(&result).await?;
        Ok(result)
    }

    // ── Single-tick state transition ──────────────────────────────────────────

    async fn on_tick(
        &self,
        state: ArbState,
        snapshot: &PriceSnapshot,
        executor: &Executor,
        info: &MarketInfo,
        leg1_price: &mut Option<f64>,
        leg2_price: &mut Option<f64>,
        leg1_bet_size: &mut f64,
    ) -> Result<ArbState, AppError> {
        match state {
            ArbState::WaitingArb => {
                tracing::debug!(
                    "[PureArb:{}] sum={:.4}  threshold={}",
                    self.sc.id,
                    snapshot.sum,
                    self.sc.hedge_threshold_sum,
                );

                if !signal::is_hedge_condition(snapshot, self.sc.hedge_threshold_sum) {
                    return Ok(ArbState::WaitingArb);
                }

                // ── Check drawdown stop ───────────────────────────────────────
                {
                    let cap = self.capital.lock().expect("capital mutex poisoned");
                    if cap.is_stopped() {
                        tracing::warn!(
                            "[PureArb:{}] ⛔ 停損觸發，跳過本次套利  drawdown={:.1}%",
                            self.sc.id,
                            cap.drawdown_pct() * 100.0,
                        );
                        return Ok(ArbState::WaitingArb);
                    }
                }

                let bet_size = self
                    .capital
                    .lock()
                    .expect("capital mutex poisoned")
                    .current_bet_size();
                let fee_usdc = self.global.compute_fee(bet_size);

                tracing::info!(
                    "[PureArb:{}] 套利觸發  sum={:.4}  up_ask={:.4}  down_ask={:.4}  \
                     bet={:.4} USDC  fee={:.4} USDC",
                    self.sc.id,
                    snapshot.sum,
                    snapshot.up_best_ask,
                    snapshot.down_best_ask,
                    bet_size,
                    fee_usdc,
                );

                // 提交 Leg 1（Up token）
                executor
                    .submit(&OrderIntent {
                        strategy_id: self.sc.id.clone(),
                        market_slug: info.slug.clone(),
                        token_id: info.up_token_id.clone(),
                        side: "BUY".to_string(),
                        price: snapshot.up_best_ask,
                        size_usdc: bet_size,
                        fee_usdc,
                        leg: 1,
                        signal_dump_pct: None,
                        hedge_sum: Some(snapshot.sum),
                    })
                    .await?;

                // 提交 Leg 2（Down token）— 同一個 tick
                executor
                    .submit(&OrderIntent {
                        strategy_id: self.sc.id.clone(),
                        market_slug: info.slug.clone(),
                        token_id: info.down_token_id.clone(),
                        side: "BUY".to_string(),
                        price: snapshot.down_best_ask,
                        size_usdc: bet_size,
                        fee_usdc,
                        leg: 2,
                        signal_dump_pct: None,
                        hedge_sum: Some(snapshot.sum),
                    })
                    .await?;

                *leg1_price = Some(snapshot.up_best_ask);
                *leg2_price = Some(snapshot.down_best_ask);
                *leg1_bet_size = bet_size;

                Ok(ArbState::BothPlaced)
            }

            ArbState::BothPlaced => {
                // 等待市場結算，Phase 3 會填入 pnl
                tracing::debug!("[PureArb:{}] BothPlaced — 等待市場結算", self.sc.id);
                Ok(ArbState::BothPlaced)
            }

            // 已是終態，不應再被呼叫
            other => Ok(other),
        }
    }
}
