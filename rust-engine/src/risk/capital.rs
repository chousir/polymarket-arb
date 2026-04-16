/// 每策略獨立的資金追蹤器
///
/// # 資金流向（Phase 1 近似模型）
///
/// 下單時：`current_capital -= (bet_size + fee_usdc)`（資金「鎖定」）
/// 週期結束時，依結果估算回收：
///   - 兩腿均入場（套利）：期望回收 = 平均 token 回報（見 `on_cycle_end`）
///   - 僅 Leg1（等待對沖）：回收 bet_size（保守：視為平手，費用已永久扣除）
///   - 未入場：回收 0（無任何成本）
///
/// Phase 3 啟用實際結算後，`on_cycle_end` 的估算值會替換為真實 PnL。
///
/// # 停損邏輯
///
/// `is_stopped()` 返回 true 時策略不應再下單。
/// 停損條件：`current_capital < initial_allocated * (1 - max_drawdown_pct)`
use std::sync::{Arc, Mutex};

use crate::config::StrategyConfig;

// ── Public struct ─────────────────────────────────────────────────────────────

#[derive(Debug)]
pub struct CapitalTracker {
    pub strategy_id: String,
    /// 初始分配金額（initial_capital × allocation_pct）
    pub initial_allocated: f64,
    /// 目前可用資金（含在途訂單的鎖定金額）
    pub current_capital: f64,
    /// 累計已支付費用（永久損失，顯示用）
    pub total_fees_paid: f64,
    /// 累計估算 PnL（Phase 1 近似值）
    pub total_estimated_pnl: f64,

    trade_size_pct: f64,
    max_drawdown_pct: f64,
    /// 最低資金門檻，低於此值停止入場
    min_capital: f64,
}

pub type SharedCapital = Arc<Mutex<CapitalTracker>>;

impl CapitalTracker {
    pub fn new(sc: &StrategyConfig) -> Self {
        let initial = sc.initial_allocated_usdc;
        CapitalTracker {
            strategy_id: sc.id.clone(),
            initial_allocated: initial,
            current_capital: initial,
            total_fees_paid: 0.0,
            total_estimated_pnl: 0.0,
            trade_size_pct: sc.trade_size_pct,
            max_drawdown_pct: sc.max_drawdown_pct,
            min_capital: initial * (1.0 - sc.max_drawdown_pct),
        }
    }

    // ── 查詢 ──────────────────────────────────────────────────────────────────

    /// 本次下注金額 = 可用資金 × trade_size_pct，至少 0.01 USDC
    pub fn current_bet_size(&self) -> f64 {
        (self.current_capital * self.trade_size_pct).max(0.01)
    }

    /// 是否已觸發停損（不應再入場）
    pub fn is_stopped(&self) -> bool {
        self.current_capital <= self.min_capital
    }

    /// 剩餘資金佔初始分配的比例（1.0 = 未虧損，0.7 = 虧損 30%）
    pub fn capital_ratio(&self) -> f64 {
        if self.initial_allocated > 0.0 {
            self.current_capital / self.initial_allocated
        } else {
            0.0
        }
    }

    /// 目前虧損百分比（0.0 = 無虧損，0.30 = 虧損 30%）
    pub fn drawdown_pct(&self) -> f64 {
        (1.0 - self.capital_ratio()).max(0.0)
    }

    // ── 更新 ──────────────────────────────────────────────────────────────────

    /// 下單後立即呼叫：鎖定資金 + 記錄費用
    pub fn on_order_submit(&mut self, bet_size: f64, fee_usdc: f64) {
        self.current_capital -= bet_size + fee_usdc;
        self.total_fees_paid += fee_usdc;
    }

    /// 週期結束後呼叫：根據結果回收資金（Phase 1 近似估算）
    ///
    /// # 參數
    /// - `leg1_price`: Leg1 入場時的 token 價格（None = 未入場）
    /// - `leg2_price`: Leg2 入場時的 token 價格（None = 未入場）
    /// - `leg_bet_size`: 每腿下注金額（兩腿相同）
    ///
    /// # 估算邏輯
    ///
    /// **兩腿均入場（套利）**
    /// 期望回收 = `leg_bet_size * (1/leg1 + 1/leg2) / 2`
    /// 此為等金額下注時的期望報酬（50/50 機率）。
    /// 注意：真正的無風險套利需等數量 token，等金額下注仍有方向性風險。
    ///
    /// **僅 Leg1**
    /// 保守處理：回收 `leg_bet_size`（平手估算，費用已永久扣除）
    pub fn on_cycle_end(
        &mut self,
        leg1_price: Option<f64>,
        leg2_price: Option<f64>,
        leg_bet_size: f64,
        fee_per_leg: f64,
    ) {
        match (leg1_price, leg2_price) {
            (Some(p1), Some(p2)) => {
                // 等金額下注的期望報酬
                let expected_payout = leg_bet_size * (1.0 / p1 + 1.0 / p2) / 2.0;
                // 回收：bet × 2 已在 on_order_submit 扣除，這裡加回報酬
                self.current_capital += expected_payout;
                let estimated_pnl =
                    expected_payout - 2.0 * leg_bet_size - 2.0 * fee_per_leg;
                self.total_estimated_pnl += estimated_pnl;
                tracing::debug!(
                    "[Capital:{}] 套利週期  sum={:.4}  expected_payout={:.4}  est_pnl={:.4}  \
                     remaining={:.4}",
                    self.strategy_id,
                    p1 + p2,
                    expected_payout,
                    estimated_pnl,
                    self.current_capital,
                );
            }
            (Some(_), None) => {
                // 僅 Leg1：保守回收下注金額（費用永久損失）
                self.current_capital += leg_bet_size;
                tracing::debug!(
                    "[Capital:{}] 僅 Leg1  保守回收 {:.4}  remaining={:.4}",
                    self.strategy_id, leg_bet_size, self.current_capital,
                );
            }
            _ => {}
        }

        if self.is_stopped() {
            tracing::warn!(
                "[Capital:{}] ⛔ 觸發停損！drawdown={:.1}%  current={:.4}  min={:.4}",
                self.strategy_id,
                self.drawdown_pct() * 100.0,
                self.current_capital,
                self.min_capital,
            );
        }
    }

    /// Live 模式：用鏈上查詢的真實餘額覆蓋當前資金
    /// （僅在策略啟動時呼叫一次）
    pub fn override_from_onchain(&mut self, balance_usdc: f64) {
        tracing::info!(
            "[Capital:{}] 鏈上餘額覆蓋: {:.4} → {:.4} USDC",
            self.strategy_id,
            self.current_capital,
            balance_usdc,
        );
        self.current_capital = balance_usdc;
        self.initial_allocated = balance_usdc;
        self.min_capital = balance_usdc * (1.0 - self.max_drawdown_pct);
    }
}

// ── Factory ───────────────────────────────────────────────────────────────────

pub fn new_shared(sc: &StrategyConfig) -> SharedCapital {
    Arc::new(Mutex::new(CapitalTracker::new(sc)))
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{StrategyConfig, StrategyType};

    fn make_sc(allocated: f64) -> StrategyConfig {
        StrategyConfig {
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
            initial_allocated_usdc: allocated,
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
            ladder_min_leg_price: 0.0002,
            ladder_max_leg_price: 0.15,
            ladder_min_payout_ratio: 80.0,
            ladder_min_combined_p_yes: 0.20,
            ladder_min_legs: 3,
            ladder_max_legs: 7,
            ladder_max_total_usdc: 5.0,
            ladder_catastrophic_shift_threshold: 0.35,
        }
    }

    #[test]
    fn bet_size_is_10pct_of_capital() {
        let sc = make_sc(100.0);
        let cap = CapitalTracker::new(&sc);
        assert!((cap.current_bet_size() - 10.0).abs() < 1e-9);
    }

    #[test]
    fn fee_deducted_on_submit() {
        let sc = make_sc(100.0);
        let mut cap = CapitalTracker::new(&sc);
        cap.on_order_submit(10.0, 0.185); // 10 USDC bet + 0.185 fee
        assert!((cap.current_capital - (100.0 - 10.185)).abs() < 1e-9);
        assert!((cap.total_fees_paid - 0.185).abs() < 1e-9);
    }

    #[test]
    fn stopped_after_30pct_loss() {
        let sc = make_sc(100.0);
        let mut cap = CapitalTracker::new(&sc);
        assert!(!cap.is_stopped());
        // Simulate 3 × (10 USDC bet + 0.185 fee) lost
        for _ in 0..3 {
            cap.on_order_submit(10.0, 0.185);
            // No payout (lose entire bet)
        }
        // current ≈ 100 - 3 * 10.185 = 69.445 < 70 (min)
        assert!(cap.is_stopped());
    }

    #[test]
    fn arb_cycle_adds_expected_payout() {
        let sc = make_sc(100.0);
        let mut cap = CapitalTracker::new(&sc);
        // Two legs at sum=0.93
        cap.on_order_submit(10.0, 0.185); // leg1
        cap.on_order_submit(10.0, 0.185); // leg2
        let capital_before = cap.current_capital;
        // leg1=0.41, leg2=0.52
        cap.on_cycle_end(Some(0.41), Some(0.52), 10.0, 0.185);
        let expected_payout = 10.0 * (1.0 / 0.41 + 1.0 / 0.52) / 2.0;
        assert!((cap.current_capital - capital_before - expected_payout).abs() < 1e-6);
    }
}
