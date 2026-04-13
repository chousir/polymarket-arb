use crate::ws::market_feed::PriceSnapshot;

// ── Order-book level ──────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct Level {
    /// Price in USDC per token (0–1 for Polymarket binary tokens)
    pub price: f64,
    /// Available size in tokens at this price level
    pub size: f64,
}

// ── Depth analysis output ─────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct DepthAnalysis {
    /// Volume-weighted average entry price (USDC per token)
    pub vwap: f64,
    /// USDC amount actually fillable at or below the worst level reached
    pub max_fillable: f64,
    /// (vwap − best_ask) / best_ask — slippage fraction vs best ask
    pub price_impact_pct: f64,
}

// ── Signal functions ──────────────────────────────────────────────────────────

/// Price-drop fraction since open: positive = BTC has fallen.
///
/// `dump_pct = (open − current) / open`
pub fn compute_dump_pct(open_price: f64, current_price: f64) -> f64 {
    if open_price <= 0.0 {
        return 0.0;
    }
    (open_price - current_price) / open_price
}

/// Simulate sweeping `target_usdc` USDC through the ask side, returning the
/// volume-weighted average price and how much of the order was fillable.
///
/// Levels are sorted ascending by price (cheapest first) inside the function;
/// the caller does not need to pre-sort.
pub fn compute_vwap(book_levels: &[Level], target_usdc: f64) -> DepthAnalysis {
    if book_levels.is_empty() || target_usdc <= 0.0 {
        return DepthAnalysis {
            vwap: 0.0,
            max_fillable: 0.0,
            price_impact_pct: 0.0,
        };
    }

    // Sort ascending: best (cheapest) ask first
    let mut sorted = book_levels.to_vec();
    sorted.sort_by(|a, b| a.price.total_cmp(&b.price));

    let best_price = sorted[0].price;
    let mut remaining_usdc = target_usdc;
    let mut total_tokens = 0.0;

    for level in &sorted {
        if remaining_usdc <= 0.0 {
            break;
        }
        // USDC capacity of this level
        let level_usdc = level.size * level.price;
        let fill_usdc = remaining_usdc.min(level_usdc);
        let fill_tokens = fill_usdc / level.price;
        total_tokens += fill_tokens;
        remaining_usdc -= fill_usdc;
    }

    let filled_usdc = target_usdc - remaining_usdc;

    if total_tokens <= 0.0 {
        return DepthAnalysis {
            vwap: 0.0,
            max_fillable: 0.0,
            price_impact_pct: 0.0,
        };
    }

    let vwap = filled_usdc / total_tokens;
    let price_impact_pct = if best_price > 0.0 {
        (vwap - best_price) / best_price
    } else {
        0.0
    };

    DepthAnalysis { vwap, max_fillable: filled_usdc, price_impact_pct }
}

/// True when the combined Up+Down ask sum has fallen below the hedge threshold,
/// meaning both legs together cost less than 1 USDC — a guaranteed-profit hedge.
pub fn is_hedge_condition(snapshot: &PriceSnapshot, threshold: f64) -> bool {
    snapshot.sum <= threshold
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dump_pct_positive_on_drop() {
        let pct = compute_dump_pct(70_000.0, 63_000.0);
        assert!((pct - 0.10).abs() < 1e-9, "expected 10%, got {pct}");
    }

    #[test]
    fn dump_pct_zero_on_flat() {
        assert_eq!(compute_dump_pct(70_000.0, 70_000.0), 0.0);
    }

    #[test]
    fn vwap_single_level_full_fill() {
        let levels = vec![Level { price: 0.08, size: 200.0 }]; // 16 USDC available
        let da = compute_vwap(&levels, 8.0);
        assert!((da.vwap - 0.08).abs() < 1e-9);
        assert!((da.max_fillable - 8.0).abs() < 1e-9);
        assert!((da.price_impact_pct).abs() < 1e-9);
    }

    #[test]
    fn vwap_sweeps_two_levels() {
        // Level 1: 0.08 × 100 = 8 USDC; Level 2: 0.09 × 200 = 18 USDC
        let levels = vec![
            Level { price: 0.09, size: 200.0 }, // intentionally unsorted
            Level { price: 0.08, size: 100.0 },
        ];
        // Buy 20 USDC: 8 from L1 (100 tokens), 12 from L2 (133.33 tokens)
        let da = compute_vwap(&levels, 20.0);
        let expected_vwap = 20.0 / (100.0 + 12.0 / 0.09);
        assert!((da.vwap - expected_vwap).abs() < 1e-6, "vwap={} expected={}", da.vwap, expected_vwap);
        assert!((da.max_fillable - 20.0).abs() < 1e-9);
        assert!(da.price_impact_pct > 0.0, "must have positive slippage");
    }

    #[test]
    fn is_hedge_below_threshold() {
        let snap = PriceSnapshot {
            up_best_ask: 0.40,
            down_best_ask: 0.52,
            sum: 0.92,
            btc_last: 70_000.0,
            ts: 0,
        };
        assert!(is_hedge_condition(&snap, 0.93));
    }

    #[test]
    fn is_hedge_above_threshold() {
        let snap = PriceSnapshot {
            up_best_ask: 0.50,
            down_best_ask: 0.50,
            sum: 1.00,
            btc_last: 70_000.0,
            ts: 0,
        };
        assert!(!is_hedge_condition(&snap, 0.93));
    }
}
