use chrono::DateTime;
use once_cell::sync::Lazy;
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

const GAMMA_BASE: &str = "https://gamma-api.polymarket.com";
const CACHE_TTL: Duration = Duration::from_secs(60);

// ── Shared HTTP client (one per process) ──────────────────────────────────────

static HTTP_CLIENT: Lazy<reqwest::Client> = Lazy::new(|| reqwest::Client::new());

// ── 60-second market cache (keyed by slug) ────────────────────────────────────

static MARKET_CACHE: Lazy<Mutex<HashMap<String, (Instant, MarketInfo)>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

// ── Public types ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct MarketInfo {
    pub slug: String,
    pub condition_id: String,
    pub up_token_id: String,
    pub down_token_id: String,
    /// Unix timestamp (seconds) — market close time
    pub close_ts: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedOutcome {
    Up,
    Down,
}

// ── Slug helpers ──────────────────────────────────────────────────────────────

/// Deterministic slug for the currently-open 15-minute BTC Up/Down window.
/// Formula: `btc-updown-15m-{window_start}` where
///   `window_start = unix_now - (unix_now % 900)`
pub fn current_slug() -> String {
    let now = chrono::Utc::now().timestamp() as u64;
    let window_ts = now - (now % 900);
    format!("btc-updown-15m-{window_ts}")
}

/// Unix timestamp (seconds) when the next 15-minute window opens.
pub fn next_open_ts() -> u64 {
    let now = chrono::Utc::now().timestamp() as u64;
    now - (now % 900) + 900
}

// ── Market fetch with cache ───────────────────────────────────────────────────

/// Fetch MarketInfo for `slug` from the Gamma API.
/// Results are cached locally for 60 seconds to avoid hammering the endpoint.
pub async fn fetch_market(slug: &str) -> Result<MarketInfo, crate::error::AppError> {
    // ── Cache read (lock released before any await) ───────────────────────────
    {
        let cache = MARKET_CACHE.lock().unwrap_or_else(|e| { tracing::error!("[Mutex Poisoned] market cache: {e}"); e.into_inner() });
        if let Some((fetched_at, info)) = cache.get(slug) {
            if fetched_at.elapsed() < CACHE_TTL {
                return Ok(info.clone());
            }
        }
    }

    // ── Live fetch ────────────────────────────────────────────────────────────
    let market = fetch_market_raw(slug).await?;

    let info = market.into_market_info()?;

    // ── Cache write ───────────────────────────────────────────────────────────
    {
        let mut cache = MARKET_CACHE.lock().unwrap_or_else(|e| { tracing::error!("[Mutex Poisoned] market cache: {e}"); e.into_inner() });
        cache.retain(|_, (fetched_at, _)| fetched_at.elapsed() < CACHE_TTL);
        cache.insert(slug.to_string(), (Instant::now(), info.clone()));
    }

    Ok(info)
}

/// Fetch resolved outcome for `slug` from Gamma.
///
/// Returns `Ok(None)` while the market is still unresolved.
pub async fn fetch_resolved_outcome(
    slug: &str,
) -> Result<Option<ResolvedOutcome>, crate::error::AppError> {
    let market = fetch_market_raw(slug).await?;
    Ok(market.resolved_outcome())
}

/// Check the settlement price of a specific token in any market (works for weather,
/// Up/Down, and any multi-outcome market).
///
/// Returns:
///   `Ok(Some(1.0))` — our token is the winner (worth $1 at settlement).
///   `Ok(Some(0.0))` — our token lost (worth $0 at settlement).
///   `Ok(None)`      — market not yet resolved; caller should retry later.
pub async fn fetch_token_settlement_price(
    slug: &str,
    token_id: &str,
) -> Result<Option<f64>, crate::error::AppError> {
    let market = fetch_market_raw(slug).await?;

    // Fast path: winning token ID is explicitly provided.
    if let Some(winner_token) = market.winner.as_deref() {
        if !winner_token.is_empty() {
            return Ok(Some(if winner_token == token_id { 1.0 } else { 0.0 }));
        }
    }

    // Fallback: scan outcome_prices aligned with clob_token_ids.
    if let (Some(prices_raw), Ok(token_ids)) = (
        market.outcome_prices.as_deref(),
        serde_json::from_str::<Vec<String>>(&market.clob_token_ids),
    ) {
        if let Ok(prices) = serde_json::from_str::<Vec<String>>(prices_raw) {
            for (i, tid) in token_ids.iter().enumerate() {
                let price = prices.get(i)
                    .and_then(|p| p.parse::<f64>().ok())
                    .unwrap_or(0.5);
                if tid == token_id {
                    if price >= 0.999 { return Ok(Some(1.0)); }
                    if price <= 0.001 { return Ok(Some(0.0)); }
                    // Price is still mid-range — not settled yet.
                    return Ok(None);
                }
            }
        }
    }

    Ok(None)
}

async fn fetch_market_raw(slug: &str) -> Result<GammaMarket, crate::error::AppError> {
    let url = format!("{GAMMA_BASE}/markets?slug={slug}");
    tracing::debug!("[Gamma] GET {url}");

    let text = HTTP_CLIENT
        .get(&url)
        .send()
        .await
        .map_err(|e| crate::error::AppError::ApiError(e.to_string()))?
        .error_for_status()
        .map_err(|e| crate::error::AppError::ApiError(e.to_string()))?
        .text()
        .await
        .map_err(|e| crate::error::AppError::ApiError(e.to_string()))?;

    let markets: Vec<GammaMarket> = serde_json::from_str(&text)
        .map_err(|e| crate::error::AppError::Json(e))?;

    markets
        .into_iter()
        .next()
        .ok_or_else(|| crate::error::AppError::ApiError(format!("市場未找到: {slug}")))
}

// ── Private serde types ───────────────────────────────────────────────────────
//
// The Gamma API encodes `outcomes` and `clobTokenIds` as JSON-serialised
// strings inside the outer JSON object, e.g.:
//   "outcomes":     "[\"Up\", \"Down\"]"
//   "clobTokenIds": "[\"<token_a>\", \"<token_b>\"]"
// Token order mirrors outcomes order.
// `endDate` is a full RFC 3339 datetime; `endDateIso` is date-only.

/// Only the fields we need from the Gamma market object.
/// Unknown fields are silently ignored by serde.
#[derive(Debug, Deserialize)]
struct GammaMarket {
    slug: String,
    #[serde(rename = "conditionId")]
    condition_id: String,
    /// JSON-encoded string array: "[\"Up\",\"Down\"]"
    outcomes: String,
    /// JSON-encoded string array of CLOB token IDs (same order as outcomes)
    #[serde(rename = "clobTokenIds")]
    clob_token_ids: String,
    /// Full RFC 3339 close datetime, e.g. "2026-04-11T08:00:00Z"
    #[serde(rename = "endDate")]
    end_date: String,
    /// Resolved winner token ID (if resolved)
    winner: Option<String>,
    /// Resolved winner label, e.g. "Up" / "Down" (if provided)
    #[serde(rename = "winningOutcome")]
    winning_outcome: Option<String>,
    /// True when market has closed for trading
    closed: Option<bool>,
    /// JSON-encoded array of prices aligned with outcomes, e.g. ["1","0"]
    #[serde(rename = "outcomePrices")]
    outcome_prices: Option<String>,
}

impl GammaMarket {
    fn into_market_info(self) -> Result<MarketInfo, crate::error::AppError> {
        let close_ts = parse_iso_to_ts(&self.end_date)?;

        // Decode the JSON-in-JSON string arrays
        let outcomes: Vec<String> = serde_json::from_str(&self.outcomes)
            .map_err(|e| crate::error::AppError::Json(e))?;
        let token_ids: Vec<String> = serde_json::from_str(&self.clob_token_ids)
            .map_err(|e| crate::error::AppError::Json(e))?;

        if outcomes.len() < 2 || token_ids.len() < 2 {
            return Err(crate::error::AppError::ApiError(
                "outcomes 或 clobTokenIds 長度不足".into(),
            ));
        }

        let mut up_token_id: Option<String> = None;
        let mut down_token_id: Option<String> = None;

        for (i, outcome) in outcomes.iter().enumerate() {
            let lower = outcome.to_lowercase();
            if lower.contains("up") {
                up_token_id = Some(token_ids[i].clone());
            } else if lower.contains("down") {
                down_token_id = Some(token_ids[i].clone());
            }
        }

        let up_token_id = up_token_id
            .ok_or_else(|| crate::error::AppError::ApiError("找不到 Up token".into()))?;
        let down_token_id = down_token_id
            .ok_or_else(|| crate::error::AppError::ApiError("找不到 Down token".into()))?;

        Ok(MarketInfo {
            slug: self.slug,
            condition_id: self.condition_id,
            up_token_id,
            down_token_id,
            close_ts,
        })
    }

    fn resolved_outcome(&self) -> Option<ResolvedOutcome> {
        // 1) Prefer explicit winning outcome label.
        if let Some(outcome) = self.winning_outcome.as_deref() {
            let lo = outcome.to_ascii_lowercase();
            if lo.contains("up") {
                return Some(ResolvedOutcome::Up);
            }
            if lo.contains("down") {
                return Some(ResolvedOutcome::Down);
            }
        }

        // 2) Map winner token id back to outcome label.
        if let Some(winner_token_id) = self.winner.as_deref() {
            if let (Ok(outcomes), Ok(token_ids)) = (
                serde_json::from_str::<Vec<String>>(&self.outcomes),
                serde_json::from_str::<Vec<String>>(&self.clob_token_ids),
            ) {
                for (i, token_id) in token_ids.iter().enumerate() {
                    if token_id == winner_token_id {
                        let lo = outcomes
                            .get(i)
                            .map(|s| s.to_ascii_lowercase())
                            .unwrap_or_default();
                        if lo.contains("up") {
                            return Some(ResolvedOutcome::Up);
                        }
                        if lo.contains("down") {
                            return Some(ResolvedOutcome::Down);
                        }
                    }
                }
            }
        }

        // 3) Fallback: if closed and prices are near binary settlement.
        if self.closed.unwrap_or(false) {
            if let (Ok(outcomes), Some(prices_raw)) = (
                serde_json::from_str::<Vec<String>>(&self.outcomes),
                self.outcome_prices.as_ref(),
            ) {
                if let Ok(prices) = serde_json::from_str::<Vec<String>>(prices_raw) {
                    for (i, p) in prices.iter().enumerate() {
                        if p.parse::<f64>().ok().unwrap_or(0.0) >= 0.999 {
                            let lo = outcomes
                                .get(i)
                                .map(|s| s.to_ascii_lowercase())
                                .unwrap_or_default();
                            if lo.contains("up") {
                                return Some(ResolvedOutcome::Up);
                            }
                            if lo.contains("down") {
                                return Some(ResolvedOutcome::Down);
                            }
                        }
                    }
                }
            }
        }

        None
    }
}

fn parse_iso_to_ts(iso: &str) -> Result<u64, crate::error::AppError> {
    DateTime::parse_from_rfc3339(iso)
        .map(|dt| dt.timestamp() as u64)
        .map_err(|e| {
            crate::error::AppError::ApiError(format!("日期解析失敗 '{iso}': {e}"))
        })
}
