// Circuit breaker: daily-loss hard stop + consecutive-hedge-fail bet reduction
//
// Thread-safe (Arc<Mutex<CircuitBreaker>>) so main loop and strategy can share it.

use std::sync::{Arc, Mutex};

use crate::db::writer::CycleResult;

// ── Public state ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub enum BreakerState {
    /// Normal operation.
    Normal,
    /// Daily-loss limit exceeded — no new cycles may start.
    DailyLossTripped { loss_usdc: f64 },
    /// Consecutive Leg-2 failures — bet size has been halved.
    HedgeFailReduced { consecutive_fails: u32 },
}

#[derive(Debug)]
pub struct CircuitBreaker {
    pub state: BreakerState,

    // Running daily PnL (reset when the calendar date changes)
    today_date: String,       // "YYYY-MM-DD"
    daily_pnl_usdc: f64,

    // Consecutive cycles where Leg 1 fired but Leg 2 did NOT
    consecutive_hedge_fails: u32,

    // Limits (copied from config at construction)
    max_daily_loss_usdc: f64,
    hedge_fail_threshold: u32,   // default 3
}

impl CircuitBreaker {
    pub fn new(max_daily_loss_usdc: f64) -> Self {
        CircuitBreaker {
            state: BreakerState::Normal,
            today_date: today(),
            daily_pnl_usdc: 0.0,
            consecutive_hedge_fails: 0,
            max_daily_loss_usdc,
            hedge_fail_threshold: 3,
        }
    }

    /// Call once after every completed market cycle.
    /// Returns a list of triggered events the caller should act on.
    pub fn record_cycle(&mut self, result: &CycleResult) -> Vec<BreakerEvent> {
        self.rollover_day_if_needed();

        let mut events = Vec::new();

        // ── Track daily PnL ───────────────────────────────────────────────────
        if let Some(pnl) = result.pnl_usdc {
            self.daily_pnl_usdc += pnl;
        }

        // ── Check daily-loss limit ────────────────────────────────────────────
        if self.daily_pnl_usdc < -self.max_daily_loss_usdc {
            if self.state != (BreakerState::DailyLossTripped { loss_usdc: self.daily_pnl_usdc }) {
                self.state = BreakerState::DailyLossTripped {
                    loss_usdc: self.daily_pnl_usdc,
                };
                events.push(BreakerEvent::DailyLossTripped {
                    loss_usdc: -self.daily_pnl_usdc,
                    limit_usdc: self.max_daily_loss_usdc,
                });
            }
            return events; // already tripped — skip other checks
        }

        // ── Track consecutive Leg-2 failures ──────────────────────────────────
        let leg1_fired = result.leg1_price.is_some();
        let leg2_fired = result.leg2_price.is_some();

        if leg1_fired && !leg2_fired {
            self.consecutive_hedge_fails += 1;
        } else if leg2_fired {
            self.consecutive_hedge_fails = 0; // successful hedge resets counter
        }
        // leg1 not fired → don't change counter

        if self.consecutive_hedge_fails >= self.hedge_fail_threshold {
            if !matches!(self.state, BreakerState::HedgeFailReduced { .. }) {
                self.state = BreakerState::HedgeFailReduced {
                    consecutive_fails: self.consecutive_hedge_fails,
                };
                events.push(BreakerEvent::HedgeFailBetReduced {
                    consecutive_fails: self.consecutive_hedge_fails,
                });
            }
        } else if self.state == BreakerState::Normal
            || matches!(self.state, BreakerState::HedgeFailReduced { .. })
        {
            // Reset bet-reduction once hedge succeeds
            if leg2_fired && matches!(self.state, BreakerState::HedgeFailReduced { .. }) {
                self.state = BreakerState::Normal;
                events.push(BreakerEvent::BetSizeRestored);
            } else if self.state == BreakerState::Normal {
                // already normal, nothing to emit
            }
        }

        events
    }

    /// True if the breaker blocks starting a new market cycle.
    pub fn is_tripped(&self) -> bool {
        matches!(self.state, BreakerState::DailyLossTripped { .. })
    }

    /// Scaling factor for bet size (1.0 = full, 0.5 = halved after hedge fails).
    pub fn bet_scale(&self) -> f64 {
        if matches!(self.state, BreakerState::HedgeFailReduced { .. }) {
            0.5
        } else {
            1.0
        }
    }

    /// Today's cumulative PnL.
    pub fn daily_pnl(&self) -> f64 {
        self.daily_pnl_usdc
    }

    // ── Internal ──────────────────────────────────────────────────────────────

    fn rollover_day_if_needed(&mut self) {
        let t = today();
        if t != self.today_date {
            tracing::info!(
                "[CB] 日期換日 {} → {}，重置每日 PnL ({:.4} USDC)",
                self.today_date, t, self.daily_pnl_usdc
            );
            self.today_date = t;
            self.daily_pnl_usdc = 0.0;
            // Reset daily-loss trip on new day; keep hedge-fail counter
            if matches!(self.state, BreakerState::DailyLossTripped { .. }) {
                self.state = BreakerState::Normal;
            }
        }
    }
}

// ── Events ────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum BreakerEvent {
    DailyLossTripped { loss_usdc: f64, limit_usdc: f64 },
    HedgeFailBetReduced { consecutive_fails: u32 },
    BetSizeRestored,
}

// ── Shared handle ─────────────────────────────────────────────────────────────

pub type SharedBreaker = Arc<Mutex<CircuitBreaker>>;

pub fn new_shared(max_daily_loss_usdc: f64) -> SharedBreaker {
    Arc::new(Mutex::new(CircuitBreaker::new(max_daily_loss_usdc)))
}

// ── Helper ────────────────────────────────────────────────────────────────────

fn today() -> String {
    chrono::Utc::now().format("%Y-%m-%d").to_string()
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::writer::CycleResult;

    fn cycle(leg1: bool, leg2: bool, pnl: Option<f64>) -> CycleResult {
        CycleResult {
            strategy_id: "test".into(),
            market_slug: "test".into(),
            mode: "dry_run".into(),
            leg1_side: if leg1 { Some("BUY".into()) } else { None },
            leg1_price: if leg1 { Some(0.40) } else { None },
            leg2_price: if leg2 { Some(0.55) } else { None },
            resolved_winner: None,
            pnl_usdc: pnl,
        }
    }

    #[test]
    fn no_trip_on_normal_cycles() {
        let mut cb = CircuitBreaker::new(50.0);
        let events = cb.record_cycle(&cycle(true, true, Some(0.10)));
        assert!(events.is_empty());
        assert!(!cb.is_tripped());
        assert_eq!(cb.bet_scale(), 1.0);
    }

    #[test]
    fn daily_loss_trips_breaker() {
        let mut cb = CircuitBreaker::new(50.0);
        let events = cb.record_cycle(&cycle(true, true, Some(-60.0)));
        assert!(cb.is_tripped());
        assert!(matches!(events[0], BreakerEvent::DailyLossTripped { .. }));
    }

    #[test]
    fn consecutive_hedge_fail_halves_bet() {
        let mut cb = CircuitBreaker::new(1000.0);
        // 3 consecutive leg1-only failures
        for _ in 0..3 {
            cb.record_cycle(&cycle(true, false, None));
        }
        assert_eq!(cb.bet_scale(), 0.5);
        assert!(matches!(cb.state, BreakerState::HedgeFailReduced { .. }));
    }

    #[test]
    fn successful_hedge_resets_fail_counter() {
        let mut cb = CircuitBreaker::new(1000.0);
        for _ in 0..3 {
            cb.record_cycle(&cycle(true, false, None));
        }
        assert_eq!(cb.bet_scale(), 0.5);
        // A successful hedge restores bet size
        cb.record_cycle(&cycle(true, true, Some(0.10)));
        assert_eq!(cb.bet_scale(), 1.0);
        assert_eq!(cb.state, BreakerState::Normal);
    }

    #[test]
    fn no_trip_when_leg1_never_fires() {
        let mut cb = CircuitBreaker::new(1000.0);
        for _ in 0..5 {
            cb.record_cycle(&cycle(false, false, None));
        }
        assert!(!cb.is_tripped());
        assert_eq!(cb.consecutive_hedge_fails, 0);
    }
}
