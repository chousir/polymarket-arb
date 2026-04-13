// Weather market filter — pre-flight checks before the decision layer.
//
// `filter_market` returns a `FilterResult` explaining why a market was accepted
// or rejected.  Every rejection has a machine-readable `reason_code` stored in
// the DB so we can tune thresholds over time.
//
// Checks (in order):
//   1. Market type supported (not Unknown)
//   2. City in the configured whitelist
//   3. Lead time in valid forecast window (1–14 days)
//   4. Order-book depth ≥ min_depth_usdc
//   5. Time-to-close ≥ abort_before_close_sec

use chrono::{NaiveDate, Utc};

use crate::api::weather_market::{WeatherMarket, WeatherMarketType};

// ── Public types ──────────────────────────────────────────────────────────────

/// Configuration for the weather market filter.
#[derive(Debug, Clone)]
pub struct WeatherFilterConfig {
    /// Canonical city names that this instance will trade (e.g. ["NYC", "London"]).
    /// Empty list means "accept all recognised cities".
    pub city_whitelist: Vec<String>,

    /// Market types to trade.  Defaults to TempRange + Extreme + Precip.
    /// Set to a subset to restrict to specific types.
    pub allowed_types: Vec<WeatherMarketType>,

    /// Minimum lead time in days (inclusive).  Markets for today (lead=0) or
    /// yesterday are rejected — forecast is too stale to act on.
    pub min_lead_days: u32,

    /// Maximum lead time in days (inclusive).  Forecasts beyond this horizon
    /// have poor skill and wide sigma; defaults to 14.
    pub max_lead_days: u32,

    /// Minimum order-book depth in USDC before entry.
    pub min_depth_usdc: f64,

    /// Reject markets closing within this many seconds.
    /// Prevents entering positions minutes before resolution.
    pub abort_before_close_sec: u64,
}

impl Default for WeatherFilterConfig {
    fn default() -> Self {
        Self {
            city_whitelist:          vec![],
            allowed_types:           vec![
                WeatherMarketType::TempRange,
                WeatherMarketType::Extreme,
                WeatherMarketType::Precip,
            ],
            min_lead_days:           1,
            max_lead_days:           14,
            min_depth_usdc:          50.0,
            abort_before_close_sec:  3600, // 1 hour
        }
    }
}

/// Outcome of `filter_market`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FilterVerdict {
    Accept,
    Reject,
}

/// Detailed result from `filter_market`.
#[derive(Debug, Clone)]
pub struct FilterResult {
    pub verdict:     FilterVerdict,
    /// Human-readable explanation
    pub reason:      String,
    /// Short DB-friendly code (e.g. "UNKNOWN_TYPE", "CITY_NOT_WHITELISTED")
    pub reason_code: String,
    /// Computed lead days (days from today to market target date)
    pub lead_days:   i64,
}

impl FilterResult {
    pub fn is_accepted(&self) -> bool {
        self.verdict == FilterVerdict::Accept
    }
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Run all pre-flight checks on `market` against `cfg`.
///
/// `depth_usdc` is the current order-book depth — pass 0.0 if unknown so the
/// depth gate rejects the market rather than allowing a thin-book trade.
///
/// `now_ts` is the current Unix timestamp in seconds.  Pass `Utc::now()
/// .timestamp() as u64` in production; inject a fixed value in tests.
pub fn filter_market(
    market: &WeatherMarket,
    cfg: &WeatherFilterConfig,
    depth_usdc: f64,
    now_ts: u64,
) -> FilterResult {
    let today = Utc::now().date_naive();
    let lead_days = lead_days_from_today(market.target_date, today);

    // ── 1. Market type ────────────────────────────────────────────────────────
    if market.market_type == WeatherMarketType::Unknown {
        return reject(lead_days, "market type could not be classified", "UNKNOWN_TYPE");
    }
    if !cfg.allowed_types.contains(&market.market_type) {
        return reject(
            lead_days,
            format!("market type '{}' not in allowed_types", market.market_type),
            "TYPE_NOT_ALLOWED",
        );
    }

    // ── 2. City whitelist ─────────────────────────────────────────────────────
    if market.city.is_empty() {
        return reject(lead_days, "city could not be parsed from question", "UNKNOWN_CITY");
    }
    if !cfg.city_whitelist.is_empty()
        && !cfg.city_whitelist.iter().any(|c| c.eq_ignore_ascii_case(&market.city))
    {
        return reject(
            lead_days,
            format!("city '{}' not in city_whitelist", market.city),
            "CITY_NOT_WHITELISTED",
        );
    }

    // ── 3. Lead time window ───────────────────────────────────────────────────
    if lead_days < cfg.min_lead_days as i64 {
        return reject(
            lead_days,
            format!(
                "lead_days={lead_days} < min_lead_days={}",
                cfg.min_lead_days
            ),
            "LEAD_TOO_SHORT",
        );
    }
    if lead_days > cfg.max_lead_days as i64 {
        return reject(
            lead_days,
            format!(
                "lead_days={lead_days} > max_lead_days={}",
                cfg.max_lead_days
            ),
            "LEAD_TOO_LONG",
        );
    }

    // ── 4. Order-book depth ───────────────────────────────────────────────────
    if depth_usdc < cfg.min_depth_usdc {
        return reject(
            lead_days,
            format!(
                "depth={depth_usdc:.2} USDC < min={:.2}",
                cfg.min_depth_usdc
            ),
            "LOW_DEPTH",
        );
    }

    // ── 5. Time-to-close ──────────────────────────────────────────────────────
    let secs_to_close = market.close_ts.saturating_sub(now_ts);
    if secs_to_close < cfg.abort_before_close_sec {
        return reject(
            lead_days,
            format!(
                "closes in {secs_to_close}s < abort_before_close_sec={}",
                cfg.abort_before_close_sec
            ),
            "TOO_CLOSE_TO_EXPIRY",
        );
    }

    // ── Accepted ──────────────────────────────────────────────────────────────
    FilterResult {
        verdict:     FilterVerdict::Accept,
        reason:      format!(
            "city={} type={} lead={}d depth={:.0} USDC secs_to_close={secs_to_close}",
            market.city, market.market_type, lead_days, depth_usdc
        ),
        reason_code: "ACCEPT".into(),
        lead_days,
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn lead_days_from_today(target: NaiveDate, today: NaiveDate) -> i64 {
    (target - today).num_days()
}

fn reject(lead_days: i64, reason: impl Into<String>, reason_code: &str) -> FilterResult {
    FilterResult {
        verdict:     FilterVerdict::Reject,
        reason:      reason.into(),
        reason_code: reason_code.to_string(),
        lead_days,
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn make_market(
        city: &str,
        market_type: WeatherMarketType,
        target_date: NaiveDate,
        close_ts: u64,
    ) -> WeatherMarket {
        WeatherMarket {
            slug:          format!("test-{city}"),
            question:      format!("Will {city} have extreme weather?"),
            city:          city.to_string(),
            market_type,
            target_date,
            temp_range:    Some((20.0, 30.0)),
            token_id_yes:  "yes_token".into(),
            token_id_no:   "no_token".into(),
            close_ts,
        }
    }

    fn default_cfg() -> WeatherFilterConfig {
        WeatherFilterConfig::default()
    }

    fn now_ts() -> u64 {
        Utc::now().timestamp() as u64
    }

    /// A target date N days from today.
    fn date_in(days: i64) -> NaiveDate {
        (Utc::now() + chrono::Duration::days(days)).date_naive()
    }

    /// A close_ts N seconds from now.
    fn close_in(secs: u64) -> u64 {
        now_ts() + secs
    }

    // ── Accept ────────────────────────────────────────────────────────────────

    #[test]
    fn accepts_valid_market() {
        let market = make_market("NYC", WeatherMarketType::TempRange, date_in(3), close_in(7200));
        let result = filter_market(&market, &default_cfg(), 200.0, now_ts());
        assert!(result.is_accepted(), "reason: {}", result.reason);
        assert_eq!(result.lead_days, 3);
    }

    #[test]
    fn accepts_extreme_type() {
        let market = make_market("London", WeatherMarketType::Extreme, date_in(5), close_in(7200));
        let result = filter_market(&market, &default_cfg(), 100.0, now_ts());
        assert!(result.is_accepted());
    }

    #[test]
    fn accepts_precip_type() {
        let market = make_market("Miami", WeatherMarketType::Precip, date_in(2), close_in(7200));
        let result = filter_market(&market, &default_cfg(), 75.0, now_ts());
        assert!(result.is_accepted());
    }

    // ── Market type ───────────────────────────────────────────────────────────

    #[test]
    fn rejects_unknown_type() {
        let market = make_market("NYC", WeatherMarketType::Unknown, date_in(3), close_in(7200));
        let result = filter_market(&market, &default_cfg(), 200.0, now_ts());
        assert_eq!(result.verdict, FilterVerdict::Reject);
        assert_eq!(result.reason_code, "UNKNOWN_TYPE");
    }

    #[test]
    fn rejects_type_not_in_allowed_list() {
        let mut cfg = default_cfg();
        cfg.allowed_types = vec![WeatherMarketType::TempRange]; // only TempRange
        let market = make_market("NYC", WeatherMarketType::Precip, date_in(3), close_in(7200));
        let result = filter_market(&market, &cfg, 200.0, now_ts());
        assert_eq!(result.reason_code, "TYPE_NOT_ALLOWED");
    }

    // ── City whitelist ────────────────────────────────────────────────────────

    #[test]
    fn rejects_unknown_city() {
        let market = make_market("", WeatherMarketType::TempRange, date_in(3), close_in(7200));
        let result = filter_market(&market, &default_cfg(), 200.0, now_ts());
        assert_eq!(result.reason_code, "UNKNOWN_CITY");
    }

    #[test]
    fn accepts_any_city_when_whitelist_empty() {
        // empty whitelist = no restriction
        let market = make_market("Tokyo", WeatherMarketType::TempRange, date_in(3), close_in(7200));
        let result = filter_market(&market, &default_cfg(), 200.0, now_ts());
        assert!(result.is_accepted(), "reason: {}", result.reason);
    }

    #[test]
    fn rejects_city_not_in_whitelist() {
        let mut cfg = default_cfg();
        cfg.city_whitelist = vec!["NYC".into(), "London".into()];
        let market = make_market("Tokyo", WeatherMarketType::TempRange, date_in(3), close_in(7200));
        let result = filter_market(&market, &cfg, 200.0, now_ts());
        assert_eq!(result.reason_code, "CITY_NOT_WHITELISTED");
    }

    #[test]
    fn city_whitelist_is_case_insensitive() {
        let mut cfg = default_cfg();
        cfg.city_whitelist = vec!["nyc".into()];
        let market = make_market("NYC", WeatherMarketType::TempRange, date_in(3), close_in(7200));
        let result = filter_market(&market, &cfg, 200.0, now_ts());
        assert!(result.is_accepted(), "reason: {}", result.reason);
    }

    // ── Lead time ─────────────────────────────────────────────────────────────

    #[test]
    fn rejects_same_day_market() {
        // lead = 0 < min_lead_days = 1
        let market = make_market("NYC", WeatherMarketType::TempRange, date_in(0), close_in(7200));
        let result = filter_market(&market, &default_cfg(), 200.0, now_ts());
        assert_eq!(result.reason_code, "LEAD_TOO_SHORT");
        assert_eq!(result.lead_days, 0);
    }

    #[test]
    fn rejects_past_market() {
        // lead = -1
        let market = make_market("NYC", WeatherMarketType::TempRange, date_in(-1), close_in(7200));
        let result = filter_market(&market, &default_cfg(), 200.0, now_ts());
        assert_eq!(result.reason_code, "LEAD_TOO_SHORT");
        assert!(result.lead_days < 0);
    }

    #[test]
    fn accepts_lead_at_min_boundary() {
        // lead = 1 == min_lead_days
        let market = make_market("NYC", WeatherMarketType::TempRange, date_in(1), close_in(7200));
        let result = filter_market(&market, &default_cfg(), 200.0, now_ts());
        assert!(result.is_accepted(), "reason: {}", result.reason);
    }

    #[test]
    fn accepts_lead_at_max_boundary() {
        // lead = 14 == max_lead_days
        let market = make_market("NYC", WeatherMarketType::TempRange, date_in(14), close_in(7200));
        let result = filter_market(&market, &default_cfg(), 200.0, now_ts());
        assert!(result.is_accepted(), "reason: {}", result.reason);
    }

    #[test]
    fn rejects_lead_beyond_max() {
        let market = make_market("NYC", WeatherMarketType::TempRange, date_in(15), close_in(7200));
        let result = filter_market(&market, &default_cfg(), 200.0, now_ts());
        assert_eq!(result.reason_code, "LEAD_TOO_LONG");
    }

    // ── Depth ─────────────────────────────────────────────────────────────────

    #[test]
    fn rejects_thin_order_book() {
        let market = make_market("NYC", WeatherMarketType::TempRange, date_in(3), close_in(7200));
        // depth = 49.9 < min = 50.0
        let result = filter_market(&market, &default_cfg(), 49.9, now_ts());
        assert_eq!(result.reason_code, "LOW_DEPTH");
    }

    #[test]
    fn accepts_depth_at_min_boundary() {
        let market = make_market("NYC", WeatherMarketType::TempRange, date_in(3), close_in(7200));
        let result = filter_market(&market, &default_cfg(), 50.0, now_ts());
        assert!(result.is_accepted(), "reason: {}", result.reason);
    }

    // ── Time-to-close ─────────────────────────────────────────────────────────

    #[test]
    fn rejects_market_closing_soon() {
        let market = make_market(
            "NYC",
            WeatherMarketType::TempRange,
            date_in(1),
            close_in(3599), // 1 second short of the 1-hour gate
        );
        let result = filter_market(&market, &default_cfg(), 200.0, now_ts());
        assert_eq!(result.reason_code, "TOO_CLOSE_TO_EXPIRY");
    }

    #[test]
    fn accepts_market_just_above_close_gate() {
        let market = make_market(
            "NYC",
            WeatherMarketType::TempRange,
            date_in(1),
            close_in(3601),
        );
        let result = filter_market(&market, &default_cfg(), 200.0, now_ts());
        assert!(result.is_accepted(), "reason: {}", result.reason);
    }

    // ── Ordering: earlier checks win ──────────────────────────────────────────

    #[test]
    fn unknown_type_checked_before_city() {
        // Even if city is whitelisted, Unknown type is rejected first
        let mut cfg = default_cfg();
        cfg.city_whitelist = vec!["NYC".into()];
        let market = make_market("NYC", WeatherMarketType::Unknown, date_in(3), close_in(7200));
        let result = filter_market(&market, &cfg, 200.0, now_ts());
        assert_eq!(result.reason_code, "UNKNOWN_TYPE");
    }

    #[test]
    fn city_checked_before_lead() {
        // City is not in whitelist — should reject before checking lead time
        let mut cfg = default_cfg();
        cfg.city_whitelist = vec!["NYC".into()];
        // lead_days = 0 would also fail, but city check fires first
        let market = make_market("Tokyo", WeatherMarketType::TempRange, date_in(0), close_in(7200));
        let result = filter_market(&market, &cfg, 200.0, now_ts());
        assert_eq!(result.reason_code, "CITY_NOT_WHITELISTED");
    }
}
