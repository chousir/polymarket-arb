// Weather market edge calculation model.
//
// Core algorithm
// ──────────────
// For every active weather market we compare the scientific model probability
// against the Polymarket order-book price.  The trade signal is:
//
//   edge = model_p_yes − market_yes_ask − cost_fraction
//
// If edge ≥ min_net_edge → BuyYes
// If (1 − model_p_yes) − market_no_ask − cost_fraction ≥ min_net_edge → BuyNo
// Otherwise → Hold
//
// Probability sources (by market type)
// ─────────────────────────────────────
// TempRange  → normal CDF: Φ((hi − μ)/σ) − Φ((lo − μ)/σ)
//              or Ensemble direct member count if model = Ensemble
// Extreme    → normal CDF tail: P(T > threshold) = 1 − Φ((threshold − μ)/σ)
//              direction (above/below) is detected from the question text
// Precip     → forecast.prob_precip directly (provided by NWS or GFS)
// Unknown    → always Hold
//
// Sigma (model error standard deviation) — Phase 5.1 fixed values
// ──────────────────────────────────────────────────────────────────
//   lead 0-1 d:  1.5°C   (all models — short-range is accurate)
//   lead 2-4 d:  ECMWF 2.5°C, GFS/NWS 3.0°C
//   lead 5-7 d:  ECMWF 3.5°C, GFS/NWS 4.5°C
//   lead 8+ d:   6.0°C   (all models — skill degrades sharply)
//
// Phase 5.2 will replace these with per-city/per-model empirical sigmas
// derived from a historical error database.

use crate::api::weather::{WeatherForecast, WeatherModel};
use crate::api::weather_market::{WeatherMarket, WeatherMarketType};

// ── Public types ──────────────────────────────────────────────────────────────

/// Order-book snapshot for a single weather market (both sides).
#[derive(Debug, Clone)]
pub struct WeatherBookSnapshot {
    pub yes_best_ask: f64,
    pub yes_best_bid: f64,
    pub no_best_ask: f64,
    pub no_best_bid: f64,
    /// USDC liquidity at the best ask (top-3 levels, used for depth gate)
    pub depth_usdc: f64,
}

/// Strategy-level parameters for the weather decision layer.
#[derive(Debug, Clone)]
pub struct WeatherDecisionConfig {
    /// Taker fee in basis points (usually 180 bps = 1.80%)
    pub taker_fee_bps: f64,
    /// Assumed slippage/execution cost in basis points
    pub slippage_buffer_bps: f64,
    /// Minimum net edge in basis points before entering a trade
    pub min_net_edge_bps: f64,
    /// Extreme / Precip markets: model must be this confident IN the trading direction.
    /// BUY_YES requires p_yes ≥ this; BUY_NO requires (1-p_yes) ≥ this.
    pub min_model_confidence: f64,
    /// TempRange BUY_NO: (1-p_yes) must be ≥ this value.
    pub min_model_confidence_temprange: f64,
    /// TempRange BUY_YES: p_yes must be ≥ this value (narrow band, max ~0.30).
    pub min_temprange_p_yes: f64,
    /// Minimum valid ensemble members required; fewer → signal rejected (default 10)
    pub min_ensemble_members: usize,
    /// Maximum acceptable bid-ask spread (0.08 = 8 cents on a $1 token)
    pub max_spread: f64,
    /// Minimum order-book depth in USDC — thin books are rejected
    pub min_depth_usdc: f64,
    /// Position size in USDC (used to populate entry metadata)
    pub bet_size_usdc: f64,
}

/// Trade direction returned by the decision model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WeatherDirection {
    BuyYes,
    BuyNo,
    Hold,
}

/// Full signal returned by `evaluate()`.
#[derive(Debug, Clone)]
pub struct WeatherSignal {
    pub direction: WeatherDirection,
    /// Net edge in basis points (can be negative for Hold signals)
    pub edge_bps: f64,
    /// Model probability of the YES outcome (0.0–1.0)
    pub p_yes: f64,
    /// Suggested limit-order entry price
    pub entry_price: f64,
    /// Human-readable explanation for logging / dashboard
    pub reason: String,
    /// Short code stored in the DB (e.g. "LOW_EDGE", "SPREAD_WIDE", "BUY_YES")
    pub reason_code: String,
}

// ── Master entry point ────────────────────────────────────────────────────────

/// Evaluate a weather market and return a trade signal.
///
/// Dispatches to the appropriate sub-evaluator based on `market.market_type`.
pub fn evaluate(
    forecast: &WeatherForecast,
    market: &WeatherMarket,
    snapshot: &WeatherBookSnapshot,
    cfg: &WeatherDecisionConfig,
) -> WeatherSignal {
    // ── Gate 1: liquidity ──────────────────────────────────────────────────────
    let yes_spread = snapshot.yes_best_ask - snapshot.yes_best_bid;
    if yes_spread > cfg.max_spread {
        return hold(
            0.0,
            format!(
                "YES spread={:.4} > max={:.4}",
                yes_spread, cfg.max_spread
            ),
            "SPREAD_WIDE",
        );
    }
    if snapshot.depth_usdc < cfg.min_depth_usdc {
        return hold(
            0.0,
            format!(
                "depth={:.2} USDC < min={:.2}",
                snapshot.depth_usdc, cfg.min_depth_usdc
            ),
            "LOW_DEPTH",
        );
    }

    // ── Dispatch by market type ────────────────────────────────────────────────
    match market.market_type {
        WeatherMarketType::TempRange => evaluate_temp_range(forecast, market, snapshot, cfg),
        WeatherMarketType::Extreme   => evaluate_extreme(forecast, market, snapshot, cfg),
        WeatherMarketType::Precip    => evaluate_precip(forecast, snapshot, cfg),
        WeatherMarketType::Unknown   => hold(0.0, "unknown market type".into(), "UNKNOWN_TYPE"),
    }
}

/// Compute model probability of YES for a market without order-book gates.
///
/// Used by consensus strategies that combine multiple model probabilities first,
/// then apply a single edge decision against the live order book.
/// `min_ensemble_members`: reject ensemble forecast if fewer valid members present.
pub fn probability_yes(
    forecast: &WeatherForecast,
    market: &WeatherMarket,
    min_ensemble_members: usize,
) -> Option<f64> {
    match market.market_type {
        WeatherMarketType::TempRange => {
            let (lo_raw, hi_raw) = market.temp_range?;
            // Exact-temperature markets have lo == hi; use ±0.5°C window
            let (lo, hi) = if (hi_raw - lo_raw).abs() < 0.01 {
                (lo_raw - 0.5, hi_raw + 0.5)
            } else {
                (lo_raw, hi_raw)
            };
            if hi - lo <= 0.0 {
                return None;
            }

            if forecast.model == WeatherModel::Ensemble {
                if forecast.member_count() < min_ensemble_members { return None; }
                forecast.ensemble_prob_in_range(lo, hi)
            } else {
                let sigma = temp_model_sigma(forecast.model, forecast.lead_days);
                let mu = forecast.max_temp_c;
                Some(normal_cdf((hi - mu) / sigma) - normal_cdf((lo - mu) / sigma))
            }
        }
        WeatherMarketType::Extreme => {
            let (threshold, _) = market.temp_range?;
            if forecast.model == WeatherModel::Ensemble {
                if forecast.member_count() < min_ensemble_members { return None; }
                let p_above = forecast.ensemble_prob_above(threshold)?;
                Some(if extreme_is_above(&market.question) { p_above } else { 1.0 - p_above })
            } else {
                let sigma = temp_model_sigma(forecast.model, forecast.lead_days);
                let mu = forecast.max_temp_c;
                let p_above = 1.0 - normal_cdf((threshold - mu) / sigma);
                Some(if extreme_is_above(&market.question) { p_above } else { 1.0 - p_above })
            }
        }
        WeatherMarketType::Precip => Some(forecast.prob_precip),
        WeatherMarketType::Unknown => None,
    }
}

// ── TempRange evaluator ───────────────────────────────────────────────────────

/// Evaluate a "Will the high temperature be between X°C and Y°C?" market.
///
/// Uses the normal CDF for deterministic models and direct ensemble member
/// counting for Ensemble forecasts.
pub fn evaluate_temp_range(
    forecast: &WeatherForecast,
    market: &WeatherMarket,
    snapshot: &WeatherBookSnapshot,
    cfg: &WeatherDecisionConfig,
) -> WeatherSignal {
    let (lo, hi) = match market.temp_range {
        Some(r) => {
            // Exact-temperature markets have lo == hi; use ±0.5°C window
            if (r.1 - r.0).abs() < 0.01 {
                (r.0 - 0.5, r.1 + 0.5)
            } else if r.1 - r.0 > 0.0 {
                r
            } else {
                return hold(0.0, "temp_range degenerate".into(), "NO_RANGE");
            }
        }
        None => {
            return hold(
                0.0,
                "temp_range missing".into(),
                "NO_RANGE",
            )
        }
    };

    // Compute p_yes
    let p_yes = if forecast.model == WeatherModel::Ensemble {
        let n = forecast.member_count();
        if n < cfg.min_ensemble_members {
            return hold(0.0,
                format!("ensemble only {n} members (min {})", cfg.min_ensemble_members),
                "FEW_MEMBERS");
        }
        // Ensemble: direct fraction of members falling in [lo, hi]
        match forecast.ensemble_prob_in_range(lo, hi) {
            Some(p) => p,
            None => return hold(0.0, "ensemble has no members".into(), "NO_MEMBERS"),
        }
    } else {
        // Deterministic model: normal CDF
        let sigma = temp_model_sigma(forecast.model, forecast.lead_days);
        let mu = forecast.max_temp_c;
        normal_cdf((hi - mu) / sigma) - normal_cdf((lo - mu) / sigma)
    };

    // Ensemble uses direct member counting so p_yes can exceed the ~0.26 CDF ceiling.
    // Require min_model_confidence (not min_temprange_p_yes) for Ensemble TempRange.
    let is_narrow_temprange = forecast.model != WeatherModel::Ensemble;
    score_signal(p_yes, forecast, snapshot, cfg, lo, hi, is_narrow_temprange)
}

// ── Extreme evaluator ─────────────────────────────────────────────────────────

/// Evaluate an "extreme" market: "Will temp exceed/be above/be below X°C?".
fn evaluate_extreme(
    forecast: &WeatherForecast,
    market: &WeatherMarket,
    snapshot: &WeatherBookSnapshot,
    cfg: &WeatherDecisionConfig,
) -> WeatherSignal {
    let threshold = match market.temp_range {
        Some((lo, _)) => lo, // for Extreme, lo == hi == threshold
        None => return hold(0.0, "no threshold found".into(), "NO_THRESHOLD"),
    };

    let is_above = extreme_is_above(&market.question);

    let p_yes = if forecast.model == WeatherModel::Ensemble {
        let n = forecast.member_count();
        if n < cfg.min_ensemble_members {
            return hold(0.0,
                format!("ensemble only {n} members (min {})", cfg.min_ensemble_members),
                "FEW_MEMBERS");
        }
        let prob = if is_above {
            forecast.ensemble_prob_above(threshold)
        } else {
            forecast.ensemble_prob_above(threshold).map(|p| 1.0 - p)
        };
        match prob {
            Some(p) => p,
            None => return hold(0.0, "ensemble has no members".into(), "NO_MEMBERS"),
        }
    } else {
        let sigma = temp_model_sigma(forecast.model, forecast.lead_days);
        let mu = forecast.max_temp_c;
        let p_above = 1.0 - normal_cdf((threshold - mu) / sigma);
        if is_above { p_above } else { 1.0 - p_above }
    };

    let (lo, hi) = (threshold, threshold);
    score_signal(p_yes, forecast, snapshot, cfg, lo, hi, false)
}

// ── Precip evaluator ──────────────────────────────────────────────────────────

/// Evaluate a precipitation market using the model's precip probability directly.
fn evaluate_precip(
    forecast: &WeatherForecast,
    snapshot: &WeatherBookSnapshot,
    cfg: &WeatherDecisionConfig,
) -> WeatherSignal {
    let p_yes = forecast.prob_precip; // already 0.0–1.0

    // Use dummy range (0,0) — threshold doesn't apply for precip
    score_signal(p_yes, forecast, snapshot, cfg, 0.0, 0.0, false)
}

// ── Shared scoring logic ──────────────────────────────────────────────────────

fn score_signal(
    p_yes: f64,
    forecast: &WeatherForecast,
    snapshot: &WeatherBookSnapshot,
    cfg: &WeatherDecisionConfig,
    lo: f64,
    hi: f64,
    is_temprange: bool,
) -> WeatherSignal {
    let cost_frac = (cfg.taker_fee_bps + cfg.slippage_buffer_bps) / 10_000.0;
    let min_edge = cfg.min_net_edge_bps / 10_000.0;

    let edge_yes = p_yes - snapshot.yes_best_ask - cost_frac * 2.0;
    let edge_no  = (1.0 - p_yes) - snapshot.no_best_ask - cost_frac * 2.0;

    if edge_yes >= min_edge && edge_yes >= edge_no {
        // Direction-specific confidence gate for BUY_YES
        let min_conf = if is_temprange { cfg.min_temprange_p_yes } else { cfg.min_model_confidence };
        if p_yes < min_conf {
            return hold(
                p_yes,
                format!(
                    "BUY_YES blocked: p_yes={:.3} < min={:.3} ({})",
                    p_yes, min_conf,
                    if is_temprange { "temprange" } else { "extreme" }
                ),
                "LOW_CONFIDENCE",
            );
        }
        WeatherSignal {
            direction:   WeatherDirection::BuyYes,
            edge_bps:    edge_yes * 10_000.0,
            p_yes,
            entry_price: snapshot.yes_best_ask,
            reason: format!(
                "model={} p_yes={:.3} yes_ask={:.4} range=[{:.1},{:.1}] lead={}d  edge={:.0}bps",
                forecast.model, p_yes, snapshot.yes_best_ask, lo, hi,
                forecast.lead_days, edge_yes * 10_000.0
            ),
            reason_code: "BUY_YES".into(),
        }
    } else if edge_no >= min_edge {
        // Direction-specific confidence gate for BUY_NO
        let min_conf_no = if is_temprange {
            cfg.min_model_confidence_temprange
        } else {
            cfg.min_model_confidence
        };
        if (1.0 - p_yes) < min_conf_no {
            return hold(
                p_yes,
                format!(
                    "BUY_NO blocked: (1-p_yes)={:.3} < min={:.3} ({})",
                    1.0 - p_yes, min_conf_no,
                    if is_temprange { "temprange" } else { "extreme" }
                ),
                "LOW_CONFIDENCE",
            );
        }
        WeatherSignal {
            direction:   WeatherDirection::BuyNo,
            edge_bps:    edge_no * 10_000.0,
            p_yes,
            entry_price: snapshot.no_best_ask,
            reason: format!(
                "model={} p_yes={:.3} no_ask={:.4} range=[{:.1},{:.1}] lead={}d  edge={:.0}bps",
                forecast.model, p_yes, snapshot.no_best_ask, lo, hi,
                forecast.lead_days, edge_no * 10_000.0
            ),
            reason_code: "BUY_NO".into(),
        }
    } else {
        let best_edge = edge_yes.max(edge_no);
        hold(
            p_yes,
            format!(
                "best_edge={:.0}bps < min={:.0}bps (p_yes={:.3})",
                best_edge * 10_000.0, cfg.min_net_edge_bps, p_yes
            ),
            "LOW_EDGE",
        )
    }
}

// ── Helper constructors ───────────────────────────────────────────────────────

fn hold(p_yes: f64, reason: String, reason_code: &str) -> WeatherSignal {
    WeatherSignal {
        direction: WeatherDirection::Hold,
        edge_bps: 0.0,
        p_yes,
        entry_price: 0.0,
        reason,
        reason_code: reason_code.to_string(),
    }
}

// ── Sigma table (Phase 5.1 fixed values) ─────────────────────────────────────

/// Return the assumed forecast error standard deviation (°C) for the given
/// model and lead time.  These are conservative Phase 5.1 values; Phase 5.2
/// will replace them with per-city empirical estimates.
pub fn temp_model_sigma(model: WeatherModel, lead_days: u32) -> f64 {
    match (model, lead_days) {
        // MetarShort uses the CURRENT observation as the daily-max anchor.
        // A noon reading of 14°C can easily peak at 17-18°C later — the
        // uncertainty between current observation and eventual daily max is
        // much larger than a proper model forecast RMSE.
        //   lead=0: same-day, obs already partially constrains daily max → 2.5°C
        //   lead=1: next-day, today's obs says very little about tomorrow    → 3.5°C
        (WeatherModel::MetarShort, 0) => 2.5,
        (WeatherModel::MetarShort, _) => 3.5,
        // NWS / GFS / ECMWF deterministic models — very accurate short range
        (_, 0..=1) => 1.5,
        // ECMWF IFS is more accurate than GFS in medium range
        (WeatherModel::Ecmwf, 2..=4) => 2.5,
        (WeatherModel::Nws, 2..=4)   => 2.8,   // NWS uses local downscaling
        (_, 2..=4)                   => 3.0,   // GFS, Ensemble
        (WeatherModel::Ecmwf, 5..=7) => 3.5,
        (WeatherModel::Nws, 5..=7)   => 4.0,
        (_, 5..=7)                   => 4.5,
        // Beyond 7 days skill degrades substantially
        (_, _)                       => 6.0,
    }
}

// ── Maths: normal CDF and helpers ────────────────────────────────────────────

/// Compute Φ(x) — the standard normal CDF.
///
/// Uses the Abramowitz & Stegun (1964) rational approximation for erfc.
/// Maximum absolute error < 1.5 × 10⁻⁷ for all x.
pub fn normal_cdf(x: f64) -> f64 {
    0.5 * erfc_approx(-x / std::f64::consts::SQRT_2)
}

/// Approximation of erfc(x) — valid for x ≥ 0; reflected for x < 0.
fn erfc_approx(x: f64) -> f64 {
    if x < 0.0 {
        return 2.0 - erfc_approx(-x);
    }
    let t = 1.0 / (1.0 + 0.3275911 * x);
    let poly = t * (0.254_829_592
        + t * (-0.284_496_736
        + t * (1.421_413_741
        + t * (-1.453_152_027
        + t * 1.061_405_429))));
    poly * (-x * x).exp()
}

/// Determine whether an extreme weather market asks "above/exceed" (true) vs
/// "below/under" (false).  Defaults to "above" when no keyword is found.
fn extreme_is_above(question: &str) -> bool {
    let q = question.to_lowercase();
    !q.contains("below")
        && !q.contains("under")
        && !q.contains("lower than")
        && !q.contains("at most")
        && !q.contains("no more than")
        && !q.contains("freeze")
        && !q.contains("freezing")
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::weather::WeatherModel;
    use crate::api::weather_market::WeatherMarketType;
    use chrono::Utc;

    // ── normal_cdf ────────────────────────────────────────────────────────────

    #[test]
    fn cdf_symmetry() {
        assert!((normal_cdf(0.0) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn cdf_known_values() {
        // Φ(1.96) ≈ 0.9750  (95% CI boundary)
        assert!((normal_cdf(1.96) - 0.9750).abs() < 0.0002);
        // Φ(-1.96) ≈ 0.0250
        assert!((normal_cdf(-1.96) - 0.0250).abs() < 0.0002);
        // Φ(1.0) ≈ 0.8413
        assert!((normal_cdf(1.0) - 0.8413).abs() < 0.0002);
        // Φ(-1.0) ≈ 0.1587
        assert!((normal_cdf(-1.0) - 0.1587).abs() < 0.0002);
        // Φ(2.0) ≈ 0.9772
        assert!((normal_cdf(2.0) - 0.9772).abs() < 0.0002);
    }

    #[test]
    fn cdf_tail_bounds() {
        assert!(normal_cdf(4.0) > 0.99);
        assert!(normal_cdf(-4.0) < 0.01);
        // Probabilities must be in [0,1]
        assert!(normal_cdf(10.0) <= 1.0);
        assert!(normal_cdf(-10.0) >= 0.0);
    }

    // ── temp_model_sigma ──────────────────────────────────────────────────────

    #[test]
    fn sigma_increases_with_lead_days() {
        let s0 = temp_model_sigma(WeatherModel::Ecmwf, 0);
        let s3 = temp_model_sigma(WeatherModel::Ecmwf, 3);
        let s7 = temp_model_sigma(WeatherModel::Ecmwf, 7);
        let s14 = temp_model_sigma(WeatherModel::Ecmwf, 14);
        assert!(s0 < s3);
        assert!(s3 < s7);
        assert!(s7 <= s14);
    }

    #[test]
    fn ecmwf_better_than_gfs_medium_range() {
        assert!(temp_model_sigma(WeatherModel::Ecmwf, 5) < temp_model_sigma(WeatherModel::Gfs, 5));
    }

    #[test]
    fn same_day_sigma() {
        // NWS/GFS/ECMWF/Ensemble share the tight short-range sigma
        for model in [WeatherModel::Nws, WeatherModel::Gfs, WeatherModel::Ecmwf, WeatherModel::Ensemble] {
            assert!((temp_model_sigma(model, 0) - 1.5).abs() < 1e-9);
            assert!((temp_model_sigma(model, 1) - 1.5).abs() < 1e-9);
        }
        // MetarShort uses a wider sigma because current obs ≠ daily max
        assert!((temp_model_sigma(WeatherModel::MetarShort, 0) - 2.5).abs() < 1e-9);
        assert!((temp_model_sigma(WeatherModel::MetarShort, 1) - 3.5).abs() < 1e-9);
        assert!(temp_model_sigma(WeatherModel::MetarShort, 0) > temp_model_sigma(WeatherModel::Gfs, 0));
    }

    // ── extreme_is_above ──────────────────────────────────────────────────────

    #[test]
    fn extreme_direction_above() {
        assert!(extreme_is_above("Will London exceed 35°C?"));
        assert!(extreme_is_above("Will NYC be above 38°C?"));
        assert!(extreme_is_above("Will Dubai hit 45°C?"));
    }

    #[test]
    fn extreme_direction_below() {
        assert!(!extreme_is_above("Will NYC be below 32°F?"));
        assert!(!extreme_is_above("Will London see temps under 0°C?"));
        assert!(!extreme_is_above("Will Chicago have a freeze event?"));
    }

    // ── evaluate_temp_range (deterministic) ───────────────────────────────────

    fn make_forecast(model: WeatherModel, max_temp_c: f64, lead_days: u32) -> WeatherForecast {
        WeatherForecast {
            city: "NYC".into(),
            model,
            forecast_date: Utc::now().date_naive(),
            max_temp_c,
            min_temp_c: max_temp_c - 10.0,
            prob_precip: 0.1,
            ensemble_members: None,
            fetched_at: Utc::now(),
            lead_days,
        }
    }

    fn make_market(market_type: WeatherMarketType, lo: f64, hi: f64) -> WeatherMarket {
        WeatherMarket {
            slug: "test-market".into(),
            question: "Will NYC high be between 25°C and 30°C?".into(),
            city: "NYC".into(),
            market_type,
            target_date: Utc::now().date_naive(),
            temp_range: if lo == hi { None } else { Some((lo, hi)) }
                .or_else(|| if market_type == WeatherMarketType::TempRange { Some((lo, hi)) } else { None })
                .or_else(|| if market_type != WeatherMarketType::Precip { Some((lo, hi)) } else { None }),
            token_id_yes: "yes_token".into(),
            token_id_no: "no_token".into(),
            close_ts: (Utc::now().timestamp() + 86400) as u64,
            liquidity_clob: 100.0,
        }
    }

    fn make_snapshot(yes_ask: f64, no_ask: f64, depth: f64) -> WeatherBookSnapshot {
        WeatherBookSnapshot {
            yes_best_ask: yes_ask,
            yes_best_bid: yes_ask - 0.03,
            no_best_ask: no_ask,
            no_best_bid: no_ask - 0.03,
            depth_usdc: depth,
        }
    }

    fn default_cfg() -> WeatherDecisionConfig {
        WeatherDecisionConfig {
            taker_fee_bps: 180.0,
            slippage_buffer_bps: 50.0,
            min_net_edge_bps: 800.0,
            min_model_confidence: 0.60,
            min_model_confidence_temprange: 0.60,
            min_temprange_p_yes: 0.28,
            min_ensemble_members: 10,
            max_spread: 0.08,
            min_depth_usdc: 50.0,
            bet_size_usdc: 20.0,
        }
    }

    #[test]
    fn buy_yes_when_model_above_market() {
        // lead_days=1 → σ=1.5°C (all models accurate same/next day)
        // Φ((30-27)/1.5) - Φ((25-27)/1.5) = Φ(2.0) - Φ(-1.333) = 0.977 - 0.091 = 0.886
        // p_yes ≈ 0.886, yes_ask=0.40, cost = (180+50)/10000*2 = 0.046
        // edge = 0.886 - 0.40 - 0.046 = 0.440 = 4400 bps >> 800 bps threshold
        let forecast = make_forecast(WeatherModel::Gfs, 27.0, 1);
        let market = make_market(WeatherMarketType::TempRange, 25.0, 30.0);
        let snapshot = make_snapshot(0.40, 0.63, 200.0);
        let cfg = default_cfg();

        let sig = evaluate_temp_range(&forecast, &market, &snapshot, &cfg);
        assert_eq!(sig.direction, WeatherDirection::BuyYes);
        assert!(sig.edge_bps > 800.0, "edge={:.0} bps", sig.edge_bps);
        assert!(sig.p_yes > 0.80, "p_yes={:.3}", sig.p_yes);
    }

    #[test]
    fn buy_no_when_model_strongly_against_range() {
        // Model says forecast is 35°C — well above range [25,30]°C
        // p_yes will be very low → buy NO
        let forecast = make_forecast(WeatherModel::Gfs, 35.0, 3);
        let market = make_market(WeatherMarketType::TempRange, 25.0, 30.0);
        let snapshot = make_snapshot(0.85, 0.20, 200.0); // NO is cheap at 0.20
        let cfg = default_cfg();

        let sig = evaluate_temp_range(&forecast, &market, &snapshot, &cfg);
        // p_yes ≈ Φ((30-35)/3) - Φ((25-35)/3) = Φ(-1.67) - Φ(-3.33) ≈ 0.048 - 0.0004 ≈ 0.047
        // edge_no = (1 - 0.047) - 0.20 - 0.046 ≈ 0.707
        assert_eq!(sig.direction, WeatherDirection::BuyNo);
        assert!(sig.edge_bps > 800.0);
        assert!(sig.p_yes < 0.15);
    }

    #[test]
    fn hold_when_edge_too_small() {
        // Model says 55% probability; market prices YES at 0.52 — tiny edge
        let forecast = make_forecast(WeatherModel::Gfs, 27.5, 3); // center of [25,30]
        let market = make_market(WeatherMarketType::TempRange, 25.0, 30.0);
        // Price YES at ~same as model probability → no edge
        let snapshot = make_snapshot(0.65, 0.40, 200.0);
        let cfg = default_cfg();

        let sig = evaluate_temp_range(&forecast, &market, &snapshot, &cfg);
        assert_eq!(sig.direction, WeatherDirection::Hold);
    }

    #[test]
    fn hold_when_spread_too_wide() {
        let forecast = make_forecast(WeatherModel::Gfs, 27.0, 3);
        let market = make_market(WeatherMarketType::TempRange, 25.0, 30.0);
        let snapshot = WeatherBookSnapshot {
            yes_best_ask: 0.70,
            yes_best_bid: 0.55, // spread = 0.15 > max 0.08
            no_best_ask: 0.35,
            no_best_bid: 0.20,
            depth_usdc: 200.0,
        };
        let cfg = default_cfg();

        let sig = evaluate(&forecast, &market, &snapshot, &cfg);
        assert_eq!(sig.direction, WeatherDirection::Hold);
        assert_eq!(sig.reason_code, "SPREAD_WIDE");
    }

    #[test]
    fn hold_when_depth_insufficient() {
        let forecast = make_forecast(WeatherModel::Gfs, 27.0, 3);
        let market = make_market(WeatherMarketType::TempRange, 25.0, 30.0);
        let snapshot = make_snapshot(0.40, 0.63, 10.0); // only $10 depth
        let cfg = default_cfg();

        let sig = evaluate(&forecast, &market, &snapshot, &cfg);
        assert_eq!(sig.direction, WeatherDirection::Hold);
        assert_eq!(sig.reason_code, "LOW_DEPTH");
    }

    // ── Ensemble probability ──────────────────────────────────────────────────

    #[test]
    fn ensemble_uses_member_count_not_cdf() {
        // 7 out of 10 members fall in [25,30]°C → p_yes = 0.70
        let mut forecast = make_forecast(WeatherModel::Ensemble, 27.0, 3);
        forecast.ensemble_members =
            Some(vec![24.0, 25.5, 26.0, 27.0, 28.0, 29.5, 30.0, 31.0, 22.0, 33.0]);
        // in [25,30]: 25.5, 26, 27, 28, 29.5, 30 → 6/10 = 0.6... let me check
        // 24.0 NO, 25.5 YES, 26.0 YES, 27.0 YES, 28.0 YES, 29.5 YES, 30.0 YES, 31.0 NO, 22.0 NO, 33.0 NO
        // → 6 in range / 10 = 0.60
        let market = make_market(WeatherMarketType::TempRange, 25.0, 30.0);
        let snapshot = make_snapshot(0.42, 0.62, 200.0);
        let cfg = default_cfg();

        let sig = evaluate_temp_range(&forecast, &market, &snapshot, &cfg);
        // p_yes = 0.60 (direct count), not from CDF
        assert!((sig.p_yes - 0.60).abs() < 0.01, "p_yes={}", sig.p_yes);
    }

    // ── Extreme market ────────────────────────────────────────────────────────

    #[test]
    fn extreme_above_correctly_uses_tail() {
        // "Will London exceed 35°C?"
        // Model: μ=38°C, σ=3.5 (ECMWF lead=7)
        // P(T > 35) = 1 - Φ((35-38)/3.5) = 1 - Φ(-0.857) ≈ 0.804
        let forecast = make_forecast(WeatherModel::Ecmwf, 38.0, 7);
        let mut market = make_market(WeatherMarketType::Extreme, 35.0, 35.0);
        market.question = "Will London exceed 35°C?".into();

        let snapshot = make_snapshot(0.55, 0.49, 200.0);
        let cfg = default_cfg();

        let sig = evaluate(&forecast, &market, &snapshot, &cfg);
        // p_yes ≈ 0.804, yes_ask=0.55, edge = 0.804-0.55-0.046=0.208 = 2080 bps
        assert_eq!(sig.direction, WeatherDirection::BuyYes);
        assert!(sig.p_yes > 0.75);
    }

    #[test]
    fn extreme_below_inverts_probability() {
        // "Will NYC be below 32°F (0°C)?"
        // Model: μ = 5°C, σ = 3.0 (GFS lead=3)
        // P(T < 0) = Φ((0-5)/3) = Φ(-1.67) ≈ 0.048
        let forecast = make_forecast(WeatherModel::Gfs, 5.0, 3);
        let mut market = make_market(WeatherMarketType::Extreme, 0.0, 0.0);
        market.question = "Will NYC temperature be below 0°C?".into();

        let snapshot = make_snapshot(0.20, 0.83, 200.0); // NO expensive, YES cheap
        let cfg = default_cfg();

        let sig = evaluate(&forecast, &market, &snapshot, &cfg);
        // p_yes ≈ 0.048 (low), edge_no = (1-0.048) - 0.83 - 0.046 ≈ 0.076 — small
        // May Hold or BuyNo depending on precise value
        assert!(sig.p_yes < 0.15, "p_yes should be low for 'below 0°C' when mu=5°C");
    }

    // ── Precip market ─────────────────────────────────────────────────────────

    #[test]
    fn precip_uses_prob_precip_directly() {
        let mut forecast = make_forecast(WeatherModel::Nws, 20.0, 2);
        forecast.prob_precip = 0.80; // 80% rain probability from NWS

        let market = WeatherMarket {
            slug: "nyc-rain".into(),
            question: "Will NYC see significant rain today?".into(),
            city: "NYC".into(),
            market_type: WeatherMarketType::Precip,
            target_date: Utc::now().date_naive(),
            temp_range: None,
            token_id_yes: "yes_token".into(),
            token_id_no: "no_token".into(),
            close_ts: (Utc::now().timestamp() + 86400) as u64,
            liquidity_clob: 100.0,
        };

        let snapshot = make_snapshot(0.55, 0.48, 200.0);
        let cfg = default_cfg();

        let sig = evaluate(&forecast, &market, &snapshot, &cfg);
        assert!((sig.p_yes - 0.80).abs() < 0.01);
        // edge_yes = 0.80 - 0.55 - 0.046 = 0.204 = 2040 bps
        assert_eq!(sig.direction, WeatherDirection::BuyYes);
    }

    // ── Edge formula ──────────────────────────────────────────────────────────

    #[test]
    fn edge_formula_manual_check() {
        // Manual: p_yes=0.70, yes_ask=0.52, fee=180+50=230 bps = 0.023
        // edge_yes = 0.70 - 0.52 - 0.046 = 0.134 = 1340 bps (entry+exit fee)
        let p_yes = 0.70_f64;
        let yes_ask = 0.52_f64;
        let cost_frac = (180.0 + 50.0) / 10_000.0; // one-way fee
        let edge = p_yes - yes_ask - cost_frac * 2.0;
        assert!((edge - 0.134).abs() < 0.001);
        assert!((edge * 10_000.0 - 1340.0).abs() < 1.0);
    }
}
