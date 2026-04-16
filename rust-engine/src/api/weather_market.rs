// Weather market discovery — Gamma API fetch with 60-second TTL cache.
//
// Fetches active weather markets via the Gamma Events API:
//   GET /events?tag_slug=temperature&active=true&closed=false&limit=100
//
// Each event (e.g. "highest-temperature-in-london-on-april-13-2026") contains
// one or more markets (temperature range buckets).  We flatten all markets
// across events, deduplicate by slug, and parse each market's question text
// to extract:
//
//   • city          — matched against our supported city list
//   • market_type   — TempRange | Extreme | Precip | Unknown
//   • target_date   — derived from the market's close timestamp
//   • temp_range    — (low_c, high_c) in °C for TempRange markets
//
// Parsing heuristics are intentionally tolerant: any market we can't fully
// classify is returned as WeatherMarketType::Unknown and filtered out by the
// strategy layer rather than silently dropped here.

use chrono::{NaiveDate, TimeZone, Utc};
use once_cell::sync::Lazy;
use serde::Deserialize;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tokio::sync::Mutex as AsyncMutex;

use crate::error::AppError;

const GAMMA_BASE: &str = "https://gamma-api.polymarket.com";
// Gamma API returns ~4-5 MB per page and can take 30-40s on slow runs.
// Use a 5-minute TTL so we don't hammer it every 60s.
const CACHE_TTL: Duration = Duration::from_secs(300);
const PAGE_SIZE: usize = 100;
const MAX_PAGES: usize = 10;

// tag_slug used by the Gamma Events API for temperature markets.
// "daily-temperature" is a superset of the old "temperature" tag and correctly
// covers all active city daily-temperature events (e.g. Paris Apr 19 only has
// "daily-temperature", not "temperature").
const TAG_SLUG: &str = "daily-temperature";

// ── Shared HTTP client ────────────────────────────────────────────────────────

static HTTP_CLIENT: Lazy<reqwest::Client> = Lazy::new(|| {
    reqwest::Client::builder()
        // connect_timeout guards the TCP+TLS handshake only.
        .connect_timeout(Duration::from_secs(10))
        // timeout covers the full request lifecycle including body download.
        // Gamma API returns ~4-5 MB; allow up to 90s for slow connections.
        .timeout(Duration::from_secs(90))
        .user_agent("polymarket-arb/1.0")
        .build()
        .expect("build weather_market client")
});

// ── Cache ─────────────────────────────────────────────────────────────────────

static MARKET_CACHE: Lazy<Mutex<Option<(Instant, Vec<WeatherMarket>)>>> =
    Lazy::new(|| Mutex::new(None));
static FETCH_IN_FLIGHT_LOCK: Lazy<AsyncMutex<()>> = Lazy::new(|| AsyncMutex::new(()));

// ── Public types ──────────────────────────────────────────────────────────────

/// Classification of a weather market's subject matter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WeatherMarketType {
    /// "Will the high temperature be between X°C and Y°C?"
    TempRange,
    /// "Will temperature exceed X°C?" / "Will it be above/below X°F?"
    Extreme,
    /// "Will there be rain/snow/hurricane/…?"
    Precip,
    /// Could not be classified — strategy layer should skip these.
    Unknown,
}

impl std::fmt::Display for WeatherMarketType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WeatherMarketType::TempRange => write!(f, "temp_range"),
            WeatherMarketType::Extreme   => write!(f, "extreme"),
            WeatherMarketType::Precip    => write!(f, "precip"),
            WeatherMarketType::Unknown   => write!(f, "unknown"),
        }
    }
}

/// A parsed Polymarket weather market ready for the decision layer.
#[derive(Debug, Clone)]
pub struct WeatherMarket {
    pub slug: String,
    pub question: String,
    /// Canonical city name ("NYC", "London", …).  Empty string if unrecognised.
    pub city: String,
    pub market_type: WeatherMarketType,
    /// Calendar date the forecast question resolves on (derived from close_ts).
    pub target_date: NaiveDate,
    /// (low_c, high_c) in °C — populated for TempRange and Extreme markets.
    pub temp_range: Option<(f64, f64)>,
    /// CLOB token ID for the YES side
    pub token_id_yes: String,
    /// CLOB token ID for the NO side
    pub token_id_no: String,
    /// Unix timestamp (seconds) when the market closes / resolves
    pub close_ts: u64,
    /// CLOB liquidity (USDC), from Gamma API liquidityClob field
    pub liquidity_clob: f64,
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Return all currently-open weather markets, refreshing from the Gamma API
/// at most once every 60 seconds.
///
/// Uses GET /events?tag_slug=temperature to discover events, then flattens
/// each event's markets array.  Markets that cannot be parsed (missing YES/NO
/// tokens, unparseable endDate) are silently skipped.  Markets with an
/// unrecognised city or type are included with `city = ""` or
/// `market_type = Unknown` so the filter layer can decide.
pub async fn fetch_weather_markets() -> Result<Vec<WeatherMarket>, AppError> {
    // ── Cache read ────────────────────────────────────────────────────────────
    {
        let cache = MARKET_CACHE.lock().unwrap();
        if let Some((fetched_at, ref markets)) = *cache {
            if fetched_at.elapsed() < CACHE_TTL {
                tracing::debug!("[WeatherMkt] cache hit ({} markets)", markets.len());
                return Ok(markets.clone());
            }
        }
    }

    // In-flight dedupe: when cache just expired, allow only one concurrent
    // caller to perform remote fetch; others wait and re-check cache.
    let _fetch_guard = FETCH_IN_FLIGHT_LOCK.lock().await;

    // Double-check cache after waiting for in-flight fetch to complete.
    {
        let cache = MARKET_CACHE.lock().unwrap();
        if let Some((fetched_at, ref markets)) = *cache {
            if fetched_at.elapsed() < CACHE_TTL {
                tracing::debug!(
                    "[WeatherMkt] cache hit after in-flight wait ({} markets)",
                    markets.len()
                );
                return Ok(markets.clone());
            }
        }
    }

    // ── Fetch events pages and flatten markets ────────────────────────────────
    let mut seen_slugs: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut all_markets: Vec<WeatherMarket> = Vec::new();

    for page in 0..MAX_PAGES {
        let offset = page * PAGE_SIZE;
        let events = match fetch_events_page(offset).await {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("[WeatherMkt] page={page} offset={offset} 失敗: {e}");
                break;
            }
        };

        let event_count = events.len();

        for event in events {
            for market in event.markets {
                if seen_slugs.contains(&market.slug) {
                    continue;
                }
                seen_slugs.insert(market.slug.clone());
                match market.into_weather_market() {
                    Ok(m)  => all_markets.push(m),
                    Err(e) => tracing::debug!("[WeatherMkt] 略過市場: {e}"),
                }
            }
        }

        // Last page — stop paging
        if event_count < PAGE_SIZE {
            break;
        }
    }

    tracing::info!("[WeatherMkt] 獲取 {} 個溫度市場", all_markets.len());

    // ── Cache write ───────────────────────────────────────────────────────────
    {
        let mut cache = MARKET_CACHE.lock().unwrap();
        *cache = Some((Instant::now(), all_markets.clone()));
    }

    Ok(all_markets)
}

async fn fetch_events_page(offset: usize) -> Result<Vec<GammaEvent>, AppError> {
    let url = format!(
        "{GAMMA_BASE}/events?tag_slug={TAG_SLUG}&active=true&closed=false&limit={PAGE_SIZE}&offset={offset}"
    );
    tracing::debug!("[WeatherMkt] GET {url}");

    let text = HTTP_CLIENT
        .get(&url)
        .send()
        .await
        .map_err(AppError::Http)?
        .error_for_status()
        .map_err(AppError::Http)?
        .text()
        .await
        .map_err(AppError::Http)?;

    let events: Vec<GammaEvent> = serde_json::from_str(&text).map_err(AppError::Json)?;
    Ok(events)
}

// ── Parsing functions (pub(crate) for unit testing) ───────────────────────────

/// Identify a supported city from free-text question.
///
/// Returns the canonical city name ("NYC", "London", …) or an empty string if
/// no known city is found.  Aliases are checked longest-first to avoid "LA"
/// matching "Los Angeles" incorrectly.
pub(crate) fn parse_city(question: &str) -> String {
    let q = question.to_lowercase();

    // Ordered by alias length descending to avoid prefix collisions.
    // Multi-word aliases must come before their shorter prefixes.
    let aliases: &[(&str, &str)] = &[
        // ── United States ──────────────────────────────────────────────────
        ("new york city",   "NYC"),
        ("new york",        "NYC"),
        ("nyc",             "NYC"),
        ("san francisco",   "San Francisco"),
        ("los angeles",     "LA"),
        ("chicago",         "Chicago"),
        ("miami",           "Miami"),
        ("houston",         "Houston"),
        ("phoenix",         "Phoenix"),
        ("boston",          "Boston"),
        ("seattle",         "Seattle"),
        ("atlanta",         "Atlanta"),
        ("dallas",          "Dallas"),
        ("denver",          "Denver"),
        // ── Canada ─────────────────────────────────────────────────────────
        ("toronto",         "Toronto"),
        // ── Europe ─────────────────────────────────────────────────────────
        ("london",          "London"),
        ("paris",           "Paris"),
        ("berlin",          "Berlin"),
        ("amsterdam",       "Amsterdam"),
        ("madrid",          "Madrid"),
        ("rome",            "Rome"),
        // ── Asia ───────────────────────────────────────────────────────────
        ("tokyo",           "Tokyo"),
        ("seoul",           "Seoul"),
        ("dubai",           "Dubai"),
        ("singapore",       "Singapore"),
        ("hong kong",       "Hong Kong"),
        ("bangkok",         "Bangkok"),
        ("mumbai",          "Mumbai"),
        // ── Oceania ────────────────────────────────────────────────────────
        ("sydney",          "Sydney"),
        ("melbourne",       "Melbourne"),
        // ── Extended cities ────────────────────────────────────────────────
        ("beijing",         "Beijing"),
        ("moscow",          "Moscow"),
        ("sao paulo",       "São Paulo"),
        ("buenos aires",    "Buenos Aires"),
        ("ankara",          "Ankara"),
        ("wellington",      "Wellington"),
        ("munich",          "Munich"),
        ("tel aviv",        "Tel Aviv"),
        ("austin",          "Austin"),
        ("lucknow",         "Lucknow"),
        // ── Abbreviations — checked last ───────────────────────────────────
        (" la ",            "LA"),
        ("(la)",            "LA"),
        ("(nyc)",           "NYC"),
    ];

    for (alias, canonical) in aliases {
        if q.contains(alias) {
            return canonical.to_string();
        }
    }
    String::new()
}

/// Extract a temperature value in °C from a single token.
///
/// Recognises: `"25°C"`, `"25C"`, `"77°F"`, `"77F"`, `"25 celsius"`,
/// `"77 fahrenheit"`.  The degree symbol (°) is optional.  Trailing
/// punctuation (`?`, `!`, `.`, `,`) is stripped before parsing.
pub(crate) fn parse_temp_token(s: &str) -> Option<f64> {
    // Strip trailing punctuation that can get attached to the last token in a sentence
    let s = s.trim().trim_end_matches(|c: char| {
        matches!(c, '?' | '!' | '.' | ',' | ';' | ':' | '\'' | '"')
    });
    // Normalise: remove degree symbol, lowercase
    let normalized = s.replace('°', "").to_lowercase();
    let s = normalized.trim();

    // Try stripping known suffixes
    let (num_str, is_fahrenheit) =
        if let Some(n) = s.strip_suffix("fahrenheit") {
            (n.trim(), true)
        } else if let Some(n) = s.strip_suffix("celsius") {
            (n.trim(), false)
        } else if let Some(n) = s.strip_suffix('f') {
            // Only treat as Fahrenheit if the remaining string looks numeric
            let n = n.trim();
            if n.chars().all(|c| c.is_ascii_digit() || c == '.' || c == '-') {
                (n, true)
            } else {
                return None;
            }
        } else if let Some(n) = s.strip_suffix('c') {
            let n = n.trim();
            if n.chars().all(|c| c.is_ascii_digit() || c == '.' || c == '-') {
                (n, false)
            } else {
                return None;
            }
        } else {
            return None; // no unit → can't classify
        };

    let val: f64 = num_str.parse().ok()?;

    // Sanity-check: reject obviously non-temperature numbers.
    // Check against the pre-conversion unit to handle Fahrenheit properly.
    // Fahrenheit range for weather: -60°F … 140°F; Celsius: -50°C … 55°C.
    let in_range = if is_fahrenheit {
        val >= -60.0 && val <= 140.0
    } else {
        val >= -50.0 && val <= 55.0
    };
    if !in_range {
        return None;
    }
    let val_c = if is_fahrenheit { f_to_c(val) } else { val };

    Some(val_c)
}

/// Extract all temperature values (in °C) from a question string.
///
/// Tokenises by whitespace and punctuation, then tries `parse_temp_token` on
/// each token and adjacent token-pairs (handles "25 °C" split across tokens).
pub(crate) fn extract_temperatures(question: &str) -> Vec<f64> {
    // Split on whitespace + common punctuation, keeping numbers together
    let tokens: Vec<&str> = question
        .split(|c: char| c == ' ' || c == ',' || c == '(' || c == ')')
        .filter(|t| !t.is_empty())
        .collect();

    let mut results: Vec<f64> = Vec::new();

    for i in 0..tokens.len() {
        // Single-token: "25°C", "77°F", "25C", "77F"
        if let Some(v) = parse_temp_token(tokens[i]) {
            if !results.iter().any(|&x: &f64| (x - v).abs() < 0.1) {
                results.push(v);
            }
            continue;
        }

        // Two-token: "25 °C" → token[i]="25", token[i+1]="°C" (or "C", "F", etc.)
        if i + 1 < tokens.len() {
            let combined = format!("{}{}", tokens[i], tokens[i + 1]);
            if let Some(v) = parse_temp_token(&combined) {
                if !results.iter().any(|&x: &f64| (x - v).abs() < 0.1) {
                    results.push(v);
                }
            }
        }
    }

    results.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    results
}

/// Parse the temperature range (low_c, high_c) from a market question.
///
/// Returns `Some((low, high))` for TempRange markets, or `Some((threshold, threshold))`
/// for Extreme markets, or `None` if no temperatures were found.
pub(crate) fn parse_temp_range(question: &str) -> Option<(f64, f64)> {
    let temps = extract_temperatures(question);
    match temps.len() {
        0 => None,
        1 => Some((temps[0], temps[0])), // single threshold (Extreme markets)
        _ => Some((temps[0], *temps.last().unwrap())), // min → max
    }
}

/// Classify a weather market into one of the known types.
pub(crate) fn parse_market_type(question: &str) -> WeatherMarketType {
    let q = question.to_lowercase();

    // Precipitation keywords take priority over temperature
    let precip_kws = [
        "rain", "rainfall", "precipitation", "snow", "snowfall",
        "hurricane", "typhoon", "cyclone", "tornado", "flood", "flooding",
        "thunderstorm", "hail", "sleet", "drizzle",
    ];
    if precip_kws.iter().any(|kw| q.contains(kw)) {
        return WeatherMarketType::Precip;
    }

    // Extract temperatures from the question
    let temps = extract_temperatures(question);

    // Two distinct temperatures → range market
    if temps.len() >= 2 && (temps.last().unwrap() - temps[0]) > 0.5 {
        return WeatherMarketType::TempRange;
    }

    // One temperature + extreme keyword → Extreme
    if !temps.is_empty() {
        let extreme_kws = [
            "exceed", "exceeds", "above", "below", "over", "under",
            "higher than", "lower than", "or higher", "or above",
            "at least", "heat wave", "record", "freeze", "freezing",
        ];
        if extreme_kws.iter().any(|kw| q.contains(kw)) {
            return WeatherMarketType::Extreme;
        }
    }

    // Keyword-only extreme (no explicit number in question)
    let extreme_only_kws = ["heat wave", "cold snap", "freeze", "blizzard", "polar vortex"];
    if extreme_only_kws.iter().any(|kw| q.contains(kw)) {
        return WeatherMarketType::Extreme;
    }

    // Single exact-temperature market (e.g. "Will highest temp be 24°C?")
    if !temps.is_empty() {
        return WeatherMarketType::TempRange;
    }

    WeatherMarketType::Unknown
}

// ── Private helpers ───────────────────────────────────────────────────────────

fn f_to_c(f: f64) -> f64 {
    (f - 32.0) * 5.0 / 9.0
}

fn parse_iso_to_ts(iso: &str) -> Result<u64, AppError> {
    chrono::DateTime::parse_from_rfc3339(iso)
        .map(|dt| dt.timestamp() as u64)
        .map_err(|e| AppError::ApiError(format!("日期解析失敗 '{iso}': {e}")))
}

fn close_ts_to_date(close_ts: u64) -> NaiveDate {
    Utc.timestamp_opt(close_ts as i64, 0)
        .single()
        .map(|dt| dt.date_naive())
        .unwrap_or_else(|| Utc::now().date_naive())
}

// ── Private serde types ───────────────────────────────────────────────────────

/// Top-level event returned by /events?tag_slug=temperature.
/// Each event (e.g. "highest-temperature-in-london-on-april-13-2026") wraps
/// one or more markets (temperature range buckets).
#[derive(Debug, Deserialize)]
struct GammaEvent {
    /// Event-level slug — used only for debug logging.
    #[serde(default)]
    slug: String,
    /// Individual market options within this event.
    #[serde(default)]
    markets: Vec<GammaEventMarket>,
}

/// A single market inside a GammaEvent.  All fields are optional to tolerate
/// incomplete API responses — missing required fields cause the market to be
/// skipped rather than crashing the fetch loop.
#[derive(Debug, Deserialize)]
struct GammaEventMarket {
    #[serde(default)]
    slug: String,
    question: Option<String>,
    outcomes: Option<String>,
    #[serde(rename = "clobTokenIds")]
    clob_token_ids: Option<String>,
    #[serde(rename = "endDate")]
    end_date: Option<String>,
    #[serde(rename = "liquidityClob", default)]
    liquidity_clob: f64,
}

impl GammaEventMarket {
    fn into_weather_market(self) -> Result<WeatherMarket, AppError> {
        let end_date = self.end_date.ok_or_else(|| {
            AppError::ApiError(format!("缺少 endDate: slug={}", self.slug))
        })?;
        let close_ts = parse_iso_to_ts(&end_date)?;

        let outcomes_str = self.outcomes.unwrap_or_default();
        let clob_str     = self.clob_token_ids.unwrap_or_default();

        let outcomes: Vec<String> = serde_json::from_str(&outcomes_str)
            .map_err(|_| AppError::ApiError(format!("outcomes 解析失敗: slug={}", self.slug)))?;
        let token_ids: Vec<String> = serde_json::from_str(&clob_str)
            .map_err(|_| AppError::ApiError(format!("clobTokenIds 解析失敗: slug={}", self.slug)))?;

        if outcomes.len() < 2 || token_ids.len() < 2 {
            return Err(AppError::ApiError(
                "outcomes 或 clobTokenIds 長度不足".into(),
            ));
        }

        let mut token_yes: Option<String> = None;
        let mut token_no: Option<String> = None;
        for (i, outcome) in outcomes.iter().enumerate() {
            match outcome.to_lowercase().as_str() {
                "yes" => token_yes = Some(token_ids[i].clone()),
                "no"  => token_no  = Some(token_ids[i].clone()),
                _ => {}
            }
        }

        let token_id_yes = token_yes.ok_or_else(|| {
            AppError::ApiError(format!("找不到 YES token: slug={}", self.slug))
        })?;
        let token_id_no = token_no.ok_or_else(|| {
            AppError::ApiError(format!("找不到 NO token: slug={}", self.slug))
        })?;

        let question    = self.question.unwrap_or_default();
        let city        = parse_city(&question);
        let market_type = parse_market_type(&question);
        let temp_range  = parse_temp_range(&question);
        let target_date = close_ts_to_date(close_ts);

        Ok(WeatherMarket {
            slug: self.slug,
            question,
            city,
            market_type,
            target_date,
            temp_range,
            token_id_yes,
            token_id_no,
            close_ts,
            liquidity_clob: self.liquidity_clob,
        })
    }
}

// ── Test-only serde helper ────────────────────────────────────────────────────
// Used by the round-trip tests below to construct synthetic markets without
// going through the events wrapper.

#[cfg(test)]
#[derive(Debug, Deserialize)]
struct GammaWeatherMarket {
    slug: String,
    question: Option<String>,
    outcomes: String,
    #[serde(rename = "clobTokenIds")]
    clob_token_ids: String,
    #[serde(rename = "endDate")]
    end_date: String,
}

#[cfg(test)]
impl GammaWeatherMarket {
    fn into_weather_market(self) -> Result<WeatherMarket, AppError> {
        GammaEventMarket {
            slug:          self.slug,
            question:      self.question,
            outcomes:      Some(self.outcomes),
            clob_token_ids: Some(self.clob_token_ids),
            end_date:      Some(self.end_date),
            liquidity_clob: 0.0,
        }
        .into_weather_market()
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── parse_city ────────────────────────────────────────────────────────────

    #[test]
    fn city_new_york_variants() {
        assert_eq!(parse_city("Will New York City high temp exceed 35°C?"), "NYC");
        assert_eq!(parse_city("Will New York see rain?"), "NYC");
        assert_eq!(parse_city("NYC temperature range June 15"), "NYC");
        assert_eq!(parse_city("(NYC) high temperature forecast"), "NYC");
    }

    #[test]
    fn city_los_angeles() {
        assert_eq!(parse_city("Will Los Angeles exceed 40°C this week?"), "LA");
    }

    #[test]
    fn city_other_cities() {
        assert_eq!(parse_city("London temperature above 35°C in July?"), "London");
        assert_eq!(parse_city("Will Miami see a hurricane this season?"), "Miami");
        assert_eq!(parse_city("Tokyo high temperature forecast"), "Tokyo");
        assert_eq!(parse_city("Seoul weather forecast"), "Seoul");
        assert_eq!(parse_city("Will Dubai exceed 45°C?"), "Dubai");
        assert_eq!(parse_city("Sydney rainfall this week"), "Sydney");
    }

    #[test]
    fn city_unknown_returns_empty() {
        assert_eq!(parse_city("Will it rain in Reykjavik tomorrow?"), "");
        assert_eq!(parse_city("Global temperature anomaly"), "");
    }

    // ── parse_temp_token ──────────────────────────────────────────────────────

    #[test]
    fn temp_token_celsius_with_symbol() {
        assert!((parse_temp_token("25°C").unwrap() - 25.0).abs() < 0.01);
        assert!((parse_temp_token("35.5°C").unwrap() - 35.5).abs() < 0.01);
        assert!((parse_temp_token("-5°C").unwrap() - (-5.0)).abs() < 0.01);
    }

    #[test]
    fn temp_token_fahrenheit_with_symbol() {
        // 32°F = 0°C
        assert!((parse_temp_token("32°F").unwrap() - 0.0).abs() < 0.01);
        // 95°F = 35°C (realistic summer extreme)
        assert!((parse_temp_token("95°F").unwrap() - f_to_c(95.0)).abs() < 0.01);
        // 77°F = 25°C
        assert!((parse_temp_token("77°F").unwrap() - 25.0).abs() < 0.01);
        // Out-of-range value is rejected
        assert!(parse_temp_token("212°F").is_none());
    }

    #[test]
    fn temp_token_no_degree_symbol() {
        assert!((parse_temp_token("30C").unwrap() - 30.0).abs() < 0.01);
        assert!((parse_temp_token("86F").unwrap() - 30.0).abs() < 0.01); // 86°F = 30°C
    }

    #[test]
    fn temp_token_word_suffix() {
        assert!((parse_temp_token("25celsius").unwrap() - 25.0).abs() < 0.01);
        assert!((parse_temp_token("77fahrenheit").unwrap() - f_to_c(77.0)).abs() < 0.01);
    }

    #[test]
    fn temp_token_invalid_returns_none() {
        assert!(parse_temp_token("notatemp").is_none());
        assert!(parse_temp_token("25").is_none()); // no unit
        assert!(parse_temp_token("").is_none());
        assert!(parse_temp_token("200°C").is_none()); // out of plausible range
    }

    // ── extract_temperatures ──────────────────────────────────────────────────

    #[test]
    fn extract_range_from_question() {
        let q = "Will NYC high temperature be between 25°C and 30°C on June 15?";
        let temps = extract_temperatures(q);
        assert_eq!(temps.len(), 2);
        assert!((temps[0] - 25.0).abs() < 0.01);
        assert!((temps[1] - 30.0).abs() < 0.01);
    }

    #[test]
    fn extract_fahrenheit_range() {
        // 75°F = 23.89°C, 85°F = 29.44°C
        let q = "Will New York high be between 75°F and 85°F?";
        let temps = extract_temperatures(q);
        assert_eq!(temps.len(), 2);
        assert!((temps[0] - f_to_c(75.0)).abs() < 0.01);
        assert!((temps[1] - f_to_c(85.0)).abs() < 0.01);
    }

    #[test]
    fn extract_single_threshold() {
        let q = "Will London exceed 35°C in July?";
        let temps = extract_temperatures(q);
        assert_eq!(temps.len(), 1);
        assert!((temps[0] - 35.0).abs() < 0.01);
    }

    #[test]
    fn extract_no_temps() {
        let q = "Will Miami see a hurricane this season?";
        assert!(extract_temperatures(q).is_empty());
    }

    // ── parse_market_type ─────────────────────────────────────────────────────

    #[test]
    fn type_temp_range() {
        let t = parse_market_type(
            "Will NYC high temperature be between 25°C and 30°C on June 15?"
        );
        assert_eq!(t, WeatherMarketType::TempRange);
    }

    #[test]
    fn type_extreme_exceed() {
        let t = parse_market_type("Will London exceed 35°C in July 2026?");
        assert_eq!(t, WeatherMarketType::Extreme);
    }

    #[test]
    fn type_extreme_above() {
        let t = parse_market_type("Will Miami be above 38°C this summer?");
        assert_eq!(t, WeatherMarketType::Extreme);
    }

    #[test]
    fn type_extreme_or_higher() {
        let t = parse_market_type("Will the highest temperature in Paris be 23°C or higher on April 18?");
        assert_eq!(t, WeatherMarketType::Extreme);
    }

    #[test]
    fn type_extreme_or_below() {
        let t = parse_market_type("Will the highest temperature in Paris be 13°C or below on April 16?");
        assert_eq!(t, WeatherMarketType::Extreme);
    }

    #[test]
    fn type_temprange_exact_bucket() {
        let t = parse_market_type("Will the highest temperature in Paris be 21°C on April 18?");
        assert_eq!(t, WeatherMarketType::TempRange);
    }

    #[test]
    fn type_precip_rain() {
        let t = parse_market_type("Will Chicago see significant rainfall this weekend?");
        assert_eq!(t, WeatherMarketType::Precip);
    }

    #[test]
    fn type_precip_hurricane() {
        let t = parse_market_type("Will a hurricane hit Miami in June 2026?");
        assert_eq!(t, WeatherMarketType::Precip);
    }

    #[test]
    fn type_precip_beats_temperature() {
        // Even if a temperature is mentioned alongside rain, Precip wins
        let t = parse_market_type(
            "Will NYC see rain and temperatures below 10°C this weekend?"
        );
        assert_eq!(t, WeatherMarketType::Precip);
    }

    #[test]
    fn type_unknown_no_signals() {
        let t = parse_market_type("Will the weather be nice in Tokyo next week?");
        assert_eq!(t, WeatherMarketType::Unknown);
    }

    // ── parse_temp_range ──────────────────────────────────────────────────────

    #[test]
    fn range_two_temps() {
        let r = parse_temp_range("Will high temp be between 25°C and 30°C?").unwrap();
        assert!((r.0 - 25.0).abs() < 0.01);
        assert!((r.1 - 30.0).abs() < 0.01);
    }

    #[test]
    fn range_single_threshold() {
        let r = parse_temp_range("Will temp exceed 35°C?").unwrap();
        assert!((r.0 - 35.0).abs() < 0.01);
        assert!((r.1 - 35.0).abs() < 0.01); // same value for single threshold
    }

    #[test]
    fn range_none_for_precip() {
        assert!(parse_temp_range("Will it rain heavily in London?").is_none());
    }

    // ── full round-trip via GammaWeatherMarket ────────────────────────────────

    fn make_gamma(slug: &str, question: &str, end_date: &str) -> GammaWeatherMarket {
        GammaWeatherMarket {
            slug: slug.into(),
            question: Some(question.into()),
            outcomes: r#"["Yes","No"]"#.into(),
            clob_token_ids: r#"["token_yes","token_no"]"#.into(),
            end_date: end_date.into(),
        }
    }

    #[test]
    fn roundtrip_temp_range_market() {
        let raw = make_gamma(
            "nyc-high-temp-25-30c-june-15",
            "Will NYC's high temperature be between 25°C and 30°C on June 15, 2026?",
            "2026-06-15T20:00:00Z",
        );
        let m = raw.into_weather_market().unwrap();
        assert_eq!(m.slug, "nyc-high-temp-25-30c-june-15");
        assert_eq!(m.city, "NYC");
        assert_eq!(m.market_type, WeatherMarketType::TempRange);
        assert_eq!(m.token_id_yes, "token_yes");
        assert_eq!(m.token_id_no, "token_no");
        let (lo, hi) = m.temp_range.unwrap();
        assert!((lo - 25.0).abs() < 0.01);
        assert!((hi - 30.0).abs() < 0.01);
    }

    #[test]
    fn roundtrip_extreme_fahrenheit() {
        let raw = make_gamma(
            "london-above-95f-july",
            "Will London's temperature exceed 95°F in July 2026?",
            "2026-07-31T23:00:00Z",
        );
        let m = raw.into_weather_market().unwrap();
        assert_eq!(m.city, "London");
        assert_eq!(m.market_type, WeatherMarketType::Extreme);
        // 95°F = 35°C
        let (lo, hi) = m.temp_range.unwrap();
        assert!((lo - f_to_c(95.0)).abs() < 0.01);
        assert!((hi - f_to_c(95.0)).abs() < 0.01);
    }

    #[test]
    fn roundtrip_precip_no_temp_range() {
        let raw = make_gamma(
            "miami-hurricane-june",
            "Will a Category 3+ hurricane hit Miami in June 2026?",
            "2026-06-30T23:59:00Z",
        );
        let m = raw.into_weather_market().unwrap();
        assert_eq!(m.city, "Miami");
        assert_eq!(m.market_type, WeatherMarketType::Precip);
        assert!(m.temp_range.is_none());
    }

    #[test]
    fn roundtrip_unknown_city_still_parsed() {
        let raw = make_gamma(
            "reykjavik-temp-july",
            "Will Reykjavik high be above 20°C on July 4, 2026?",
            "2026-07-04T23:59:00Z",
        );
        let m = raw.into_weather_market().unwrap();
        assert_eq!(m.city, ""); // not in supported list
        assert_eq!(m.market_type, WeatherMarketType::Extreme);
    }

    #[test]
    fn roundtrip_missing_yes_token_errors() {
        let raw = GammaWeatherMarket {
            slug: "bad".into(),
            question: Some("Will it be hot?".into()),
            outcomes: r#"["Up","Down"]"#.into(), // no Yes/No
            clob_token_ids: r#"["t1","t2"]"#.into(),
            end_date: "2026-06-15T20:00:00Z".into(),
        };
        assert!(raw.into_weather_market().is_err());
    }
}
