// Trump Mention Market — net-edge decision engine.
//
// Net edge model (per token, on a 0–1 payout scale)
// ──────────────────────────────────────────────────
//  For a binary token that resolves to 1 (win) or 0 (loss):
//
//   edge_no  = (1 − p_no_entry)  − total_cost_frac
//   edge_yes = (1 − p_yes_entry) − total_cost_frac
//
//  where total_cost_frac = (taker_fee_bps + slippage_buffer_bps + execution_risk_bps) / 10_000
//
//  This assumes our keyword selection already gives P(resolve-win) ≈ 1.
//  The edge is therefore the expected profit per token *net of all costs*.
//
// Trading condition
// ─────────────────
//   1. edge >= min_net_edge_bps / 10_000
//   2. bid-ask spread <= max_spread (depth / liquidity check)
//   3. depth_usdc >= bet_size_usdc (can fill the order)
//   When direction_mode = NoFirst:  evaluate NO before YES.

// ── Reason codes (written into mention_dry_run_trades.reason_code) ────────────

pub const REASON_EDGE_OK: &str          = "EDGE_OK";
pub const REASON_EDGE_TOO_LOW: &str     = "EDGE_TOO_LOW";
pub const REASON_SPREAD_TOO_WIDE: &str  = "SPREAD_TOO_WIDE";
pub const REASON_DEPTH_TOO_THIN: &str   = "DEPTH_TOO_THIN";
pub const REASON_TIME_EXIT: &str        = "TIME_EXIT";
pub const REASON_TAKE_PROFIT: &str      = "TAKE_PROFIT";
pub const REASON_STOP_LOSS: &str        = "STOP_LOSS";

// ── Config ────────────────────────────────────────────────────────────────────

/// Which side to prefer when both directions have positive edge.
#[derive(Debug, Clone, PartialEq)]
pub enum DirectionMode {
    /// Try NO first; fall back to YES if NO has no edge.
    NoFirst,
    /// Try YES first; fall back to NO if YES has no edge.
    YesFirst,
    /// Only trade NO tokens.
    NoOnly,
    /// Only trade YES tokens.
    YesOnly,
}

/// Full parameter set for one `evaluate()` call.
#[derive(Debug, Clone)]
pub struct MentionDecisionConfig {
    // ── Direction ─────────────────────────────────────────────────────────────
    pub direction_mode: DirectionMode,

    // ── Entry thresholds ──────────────────────────────────────────────────────
    /// Minimum NO ask price to enter (e.g. 0.05 USDC).
    /// Guards against tokens so cheap the taker fee exceeds potential profit.
    pub entry_no_min_price: f64,
    /// Maximum YES ask price to enter (e.g. 0.40 USDC).
    pub entry_yes_max_price: f64,

    // ── Exit targets ──────────────────────────────────────────────────────────
    /// Take-profit target for NO: exit when NO bid >= this.
    pub take_profit_no_price: f64,
    /// Take-profit target for YES: exit when YES bid >= this.
    pub take_profit_yes_price: f64,
    /// Stop-loss: exit if price moves this much (in price units, e.g. 0.10)
    /// *against* the position from the entry price.
    pub stop_loss_delta: f64,

    // ── Cost parameters ───────────────────────────────────────────────────────
    pub taker_fee_bps: f64,
    pub slippage_buffer_bps: f64,
    pub execution_risk_bps: f64,
    /// Minimum net edge required to enter a position (basis points).
    pub min_net_edge_bps: f64,

    // ── Sizing / depth ────────────────────────────────────────────────────────
    pub bet_size_usdc: f64,
    /// Maximum acceptable bid-ask spread (price units, e.g. 0.05).
    pub max_spread: f64,
}

impl Default for MentionDecisionConfig {
    fn default() -> Self {
        MentionDecisionConfig {
            direction_mode: DirectionMode::NoFirst,
            entry_no_min_price: 0.05,
            entry_yes_max_price: 0.40,
            take_profit_no_price: 0.80,
            take_profit_yes_price: 0.80,
            stop_loss_delta: 0.10,
            taker_fee_bps: 180.0,    // 1.80 % worst-case taker
            slippage_buffer_bps: 50.0,
            execution_risk_bps: 20.0,
            min_net_edge_bps: 100.0, // require ≥ 1% net edge
            bet_size_usdc: 25.0,
            max_spread: 0.05,
        }
    }
}

// ── Market book snapshot ──────────────────────────────────────────────────────

/// Live order-book data needed by the decision engine.
#[derive(Debug, Clone)]
pub struct MentionBookSnapshot {
    pub yes_best_ask: f64,
    pub yes_best_bid: f64,
    pub no_best_ask: f64,
    pub no_best_bid: f64,
    /// USDC available at or near the ask (liquidity check)
    pub yes_depth_usdc: f64,
    pub no_depth_usdc: f64,
}

// ── Trade signal ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub enum TradeDirection {
    BuyNo,
    BuyYes,
    /// No qualifying edge — do not trade
    Hold,
}

#[derive(Debug, Clone)]
pub struct TradeSignal {
    pub direction: TradeDirection,
    /// Ask price at which we'd enter (0 for Hold)
    pub entry_price: f64,
    /// Net edge in basis points (negative = no edge)
    pub edge_bps: f64,
    /// Structured code for DB logging (EDGE_OK / EDGE_TOO_LOW / etc.)
    pub reason_code: String,
    /// Human-readable explanation (for logs / dry_run output)
    pub reason: String,
}

// ── Core evaluation ───────────────────────────────────────────────────────────

/// Evaluate whether to trade and in which direction.
pub fn evaluate(snapshot: &MentionBookSnapshot, cfg: &MentionDecisionConfig) -> TradeSignal {
    let cost_frac = (cfg.taker_fee_bps + cfg.slippage_buffer_bps + cfg.execution_risk_bps)
        / 10_000.0;
    let min_edge_frac = cfg.min_net_edge_bps / 10_000.0;

    let no_signal  = eval_no(snapshot, cfg, cost_frac, min_edge_frac);
    let yes_signal = eval_yes(snapshot, cfg, cost_frac, min_edge_frac);

    match cfg.direction_mode {
        DirectionMode::NoFirst => {
            if no_signal.direction != TradeDirection::Hold { no_signal }
            else { yes_signal }
        }
        DirectionMode::YesFirst => {
            if yes_signal.direction != TradeDirection::Hold { yes_signal }
            else { no_signal }
        }
        DirectionMode::NoOnly  => no_signal,
        DirectionMode::YesOnly => yes_signal,
    }
}

// ── Internal helpers ──────────────────────────────────────────────────────────

fn eval_no(
    snap: &MentionBookSnapshot,
    cfg: &MentionDecisionConfig,
    cost_frac: f64,
    min_edge_frac: f64,
) -> TradeSignal {
    let p = snap.no_best_ask;
    let edge = (1.0 - p) - cost_frac;

    // ── Entry price gate ──────────────────────────────────────────────────────
    if p < cfg.entry_no_min_price {
        return hold(
            REASON_EDGE_TOO_LOW,
            format!("NO ask {p:.4} < entry_no_min_price {:.4}", cfg.entry_no_min_price),
        );
    }

    // ── Edge gate ─────────────────────────────────────────────────────────────
    if edge < min_edge_frac {
        return hold(
            REASON_EDGE_TOO_LOW,
            format!("NO edge {:.1} bps < min {:.1} bps", edge * 10_000.0, cfg.min_net_edge_bps),
        );
    }

    // ── Spread gate ───────────────────────────────────────────────────────────
    let spread = snap.no_best_ask - snap.no_best_bid;
    if spread > cfg.max_spread {
        return hold(
            REASON_SPREAD_TOO_WIDE,
            format!("NO spread {spread:.4} > max_spread {:.4}", cfg.max_spread),
        );
    }

    // ── Depth gate ────────────────────────────────────────────────────────────
    if snap.no_depth_usdc < cfg.bet_size_usdc {
        return hold(
            REASON_DEPTH_TOO_THIN,
            format!("NO depth {:.2} USDC < bet_size {:.2} USDC", snap.no_depth_usdc, cfg.bet_size_usdc),
        );
    }

    TradeSignal {
        direction: TradeDirection::BuyNo,
        entry_price: p,
        edge_bps: edge * 10_000.0,
        reason_code: REASON_EDGE_OK.into(),
        reason: format!("BuyNO: ask={p:.4}  edge={:.1} bps  spread={spread:.4}", edge * 10_000.0),
    }
}

fn eval_yes(
    snap: &MentionBookSnapshot,
    cfg: &MentionDecisionConfig,
    cost_frac: f64,
    min_edge_frac: f64,
) -> TradeSignal {
    let p = snap.yes_best_ask;
    let edge = (1.0 - p) - cost_frac;

    // ── Entry price gate ──────────────────────────────────────────────────────
    if p > cfg.entry_yes_max_price {
        return hold(
            REASON_EDGE_TOO_LOW,
            format!("YES ask {p:.4} > entry_yes_max_price {:.4}", cfg.entry_yes_max_price),
        );
    }

    // ── Edge gate ─────────────────────────────────────────────────────────────
    if edge < min_edge_frac {
        return hold(
            REASON_EDGE_TOO_LOW,
            format!("YES edge {:.1} bps < min {:.1} bps", edge * 10_000.0, cfg.min_net_edge_bps),
        );
    }

    // ── Spread gate ───────────────────────────────────────────────────────────
    let spread = snap.yes_best_ask - snap.yes_best_bid;
    if spread > cfg.max_spread {
        return hold(
            REASON_SPREAD_TOO_WIDE,
            format!("YES spread {spread:.4} > max_spread {:.4}", cfg.max_spread),
        );
    }

    // ── Depth gate ────────────────────────────────────────────────────────────
    if snap.yes_depth_usdc < cfg.bet_size_usdc {
        return hold(
            REASON_DEPTH_TOO_THIN,
            format!("YES depth {:.2} USDC < bet_size {:.2} USDC", snap.yes_depth_usdc, cfg.bet_size_usdc),
        );
    }

    TradeSignal {
        direction: TradeDirection::BuyYes,
        entry_price: p,
        edge_bps: edge * 10_000.0,
        reason_code: REASON_EDGE_OK.into(),
        reason: format!("BuyYES: ask={p:.4}  edge={:.1} bps  spread={spread:.4}", edge * 10_000.0),
    }
}

fn hold(reason_code: &str, reason: String) -> TradeSignal {
    TradeSignal {
        direction: TradeDirection::Hold,
        entry_price: 0.0,
        edge_bps: f64::NEG_INFINITY,
        reason_code: reason_code.into(),
        reason,
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn default_snap() -> MentionBookSnapshot {
        MentionBookSnapshot {
            yes_best_ask: 0.08,
            yes_best_bid: 0.05,
            no_best_ask: 0.10,
            no_best_bid: 0.08,
            yes_depth_usdc: 50.0,
            no_depth_usdc: 50.0,
        }
    }

    fn cfg() -> MentionDecisionConfig {
        MentionDecisionConfig::default()
    }

    // ── NO path ───────────────────────────────────────────────────────────────

    #[test]
    fn no_first_selects_no_when_both_qualify() {
        // NO ask=0.10: edge = (1-0.10) - (180+50+20)/10000 = 0.90 - 0.025 = 0.875 = 8750 bps ✓
        let sig = evaluate(&default_snap(), &cfg());
        assert_eq!(sig.direction, TradeDirection::BuyNo);
        assert!((sig.edge_bps - 8750.0).abs() < 1.0, "edge_bps={}", sig.edge_bps);
    }

    #[test]
    fn no_edge_computed_correctly() {
        // cost = (180+50+20)/10000 = 0.025; edge = 1-0.10-0.025 = 0.875
        let snap = default_snap();
        let sig = evaluate(&snap, &cfg());
        assert!((sig.edge_bps - 8750.0).abs() < 1.0);
    }

    #[test]
    fn hold_when_no_below_min_price() {
        let mut snap = default_snap();
        snap.no_best_ask = 0.02; // < entry_no_min_price 0.05
        let sig = evaluate(&snap, &cfg());
        // Falls back to YES
        assert_eq!(sig.direction, TradeDirection::BuyYes);
    }

    #[test]
    fn hold_when_no_spread_too_wide() {
        let mut snap = default_snap();
        snap.no_best_bid = 0.01; // spread = 0.09 > max_spread 0.05
        let c = MentionDecisionConfig { direction_mode: DirectionMode::NoOnly, ..cfg() };
        let sig = evaluate(&snap, &c);
        assert_eq!(sig.direction, TradeDirection::Hold);
        assert!(sig.reason.contains("spread"));
    }

    #[test]
    fn hold_when_no_depth_insufficient() {
        let mut snap = default_snap();
        snap.no_depth_usdc = 10.0; // < bet_size 25.0
        let c = MentionDecisionConfig { direction_mode: DirectionMode::NoOnly, ..cfg() };
        let sig = evaluate(&snap, &c);
        assert_eq!(sig.direction, TradeDirection::Hold);
        assert!(sig.reason.contains("depth"));
    }

    // ── YES path ──────────────────────────────────────────────────────────────

    #[test]
    fn yes_only_mode_selects_yes() {
        let c = MentionDecisionConfig { direction_mode: DirectionMode::YesOnly, ..cfg() };
        // YES ask=0.08: edge = (1-0.08) - 0.025 = 0.895 = 8950 bps ✓
        let sig = evaluate(&default_snap(), &c);
        assert_eq!(sig.direction, TradeDirection::BuyYes);
        assert!((sig.edge_bps - 8950.0).abs() < 1.0, "edge_bps={}", sig.edge_bps);
    }

    #[test]
    fn hold_when_yes_too_expensive() {
        let mut snap = default_snap();
        snap.yes_best_ask = 0.50; // > entry_yes_max_price 0.40
        let c = MentionDecisionConfig { direction_mode: DirectionMode::YesOnly, ..cfg() };
        let sig = evaluate(&snap, &c);
        assert_eq!(sig.direction, TradeDirection::Hold);
    }

    #[test]
    fn hold_when_yes_edge_below_min() {
        // edge = (1 - 0.97) - 0.025 = 0.005 = 50 bps < min_net_edge 100 bps
        let mut snap = default_snap();
        snap.yes_best_ask = 0.97;
        snap.yes_best_bid = 0.94;
        snap.yes_depth_usdc = 50.0;
        let c = MentionDecisionConfig {
            direction_mode: DirectionMode::YesOnly,
            entry_yes_max_price: 0.99,
            ..cfg()
        };
        let sig = evaluate(&snap, &c);
        assert_eq!(sig.direction, TradeDirection::Hold);
        assert!(sig.reason.contains("bps"));
    }

    // ── Mode switching ────────────────────────────────────────────────────────

    #[test]
    fn yes_first_mode_selects_yes_first() {
        let c = MentionDecisionConfig { direction_mode: DirectionMode::YesFirst, ..cfg() };
        let sig = evaluate(&default_snap(), &c);
        assert_eq!(sig.direction, TradeDirection::BuyYes);
    }

    #[test]
    fn no_first_falls_back_to_yes_when_no_held() {
        let mut snap = default_snap();
        snap.no_depth_usdc = 0.0; // forces NO to Hold
        let sig = evaluate(&snap, &cfg()); // NoFirst
        assert_eq!(sig.direction, TradeDirection::BuyYes);
    }

    #[test]
    fn hold_when_all_gates_fail() {
        let snap = MentionBookSnapshot {
            yes_best_ask: 0.99,
            yes_best_bid: 0.90,
            no_best_ask: 0.01, // below min price
            no_best_bid: 0.00,
            yes_depth_usdc: 0.0, // no YES depth
            no_depth_usdc: 0.0,
        };
        let sig = evaluate(&snap, &cfg());
        assert_eq!(sig.direction, TradeDirection::Hold);
    }

    // ── Edge formula correctness ──────────────────────────────────────────────

    #[test]
    fn edge_formula_matches_manual_calculation() {
        // taker=180, slippage=50, exec=20 → cost = 250 bps = 0.025
        // NO ask = 0.15 → edge = (1 - 0.15) - 0.025 = 0.825 = 8250 bps
        let mut snap = default_snap();
        snap.no_best_ask = 0.15;
        snap.no_best_bid = 0.12; // spread = 0.03 ✓
        let sig = evaluate(&snap, &cfg());
        assert_eq!(sig.direction, TradeDirection::BuyNo);
        assert!((sig.edge_bps - 8250.0).abs() < 1.0);
    }
}
