// Trump Mention Market — Gamma API fetch with 60-second TTL cache.
//
// Endpoint: GET /markets?tag=trump&active=true&closed=false
// Markets are binary (YES / NO tokens) unlike the BTC Up/Down markets.

use once_cell::sync::Lazy;
use serde::Deserialize;
use std::sync::Mutex;
use std::time::{Duration, Instant};

const GAMMA_BASE: &str = "https://gamma-api.polymarket.com";
const CACHE_TTL: Duration = Duration::from_secs(60);

// ── Shared HTTP client ────────────────────────────────────────────────────────

static HTTP_CLIENT: Lazy<reqwest::Client> = Lazy::new(reqwest::Client::new);

// ── Cache: list-of-markets (refreshed every 60 s) ─────────────────────────────

static MARKET_CACHE: Lazy<Mutex<Option<(Instant, Vec<MentionMarket>)>>> =
    Lazy::new(|| Mutex::new(None));

// ── Public types ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct MentionMarket {
    pub slug: String,
    /// Full question text, e.g. "Will Trump mention 'crypto' in his speech?"
    pub question: String,
    pub token_id_yes: String,
    pub token_id_no: String,
    /// Unix timestamp (seconds) when the market closes
    pub close_ts: u64,
    /// Tags attached to the market by Polymarket
    pub tags: Vec<String>,
}

// ── Fetch with cache ──────────────────────────────────────────────────────────

/// Return all currently-open Trump mention markets, refreshing from the Gamma
/// API at most once per 60 seconds.
pub async fn fetch_trump_mention_markets()
    -> Result<Vec<MentionMarket>, crate::error::AppError>
{
    // ── Cache read ────────────────────────────────────────────────────────────
    {
        let cache = MARKET_CACHE.lock().unwrap_or_else(|e| { tracing::error!("[Mutex Poisoned] mention market cache: {e}"); e.into_inner() });
        if let Some((fetched_at, ref markets)) = *cache {
            if fetched_at.elapsed() < CACHE_TTL {
                tracing::debug!("[MentionMkt] cache hit ({} markets)", markets.len());
                return Ok(markets.clone());
            }
        }
    }

    // ── Live fetch ────────────────────────────────────────────────────────────
    // Query params:
    //   tag=trump         — only markets tagged "trump"
    //   active=true       — currently tradeable
    //   closed=false      — not yet resolved
    //   limit=100         — upper bound; typically <20 open at a time
    let url = format!(
        "{GAMMA_BASE}/markets?tag=trump&active=true&closed=false&limit=100"
    );
    tracing::debug!("[MentionMkt] GET {url}");

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

    let raw: Vec<GammaMentionMarket> = serde_json::from_str(&text)
        .map_err(|e| crate::error::AppError::Json(e))?;

    // Parse + keep only markets with a valid YES/NO token pair
    let markets: Vec<MentionMarket> = raw
        .into_iter()
        .filter_map(|m| match m.into_mention_market() {
            Ok(mm) => Some(mm),
            Err(e) => {
                tracing::debug!("[MentionMkt] 略過市場 (parse error): {e}");
                None
            }
        })
        .collect();

    tracing::info!("[MentionMkt] 獲取 {} 個 Trump mention 市場", markets.len());

    // ── Cache write ───────────────────────────────────────────────────────────
    {
        let mut cache = MARKET_CACHE.lock().unwrap_or_else(|e| { tracing::error!("[Mutex Poisoned] mention market cache: {e}"); e.into_inner() });
        *cache = Some((Instant::now(), markets.clone()));
    }

    Ok(markets)
}

// ── Private serde types ───────────────────────────────────────────────────────

/// Raw Gamma API response fields needed for mention markets.
#[derive(Debug, Deserialize)]
struct GammaMentionMarket {
    slug: String,
    /// Human-readable question text
    question: Option<String>,
    /// JSON-encoded string array, e.g. `"[\"Yes\",\"No\"]"`
    outcomes: String,
    /// JSON-encoded string array of CLOB token IDs (same order as outcomes)
    #[serde(rename = "clobTokenIds")]
    clob_token_ids: String,
    /// Full RFC 3339 close datetime, e.g. "2026-05-01T20:00:00Z"
    #[serde(rename = "endDate")]
    end_date: String,
    /// Array of tag strings — may be absent
    #[serde(default)]
    tags: Vec<serde_json::Value>,
}

impl GammaMentionMarket {
    fn into_mention_market(self) -> Result<MentionMarket, crate::error::AppError> {
        let close_ts = parse_iso_to_ts(&self.end_date)?;

        let outcomes: Vec<String> = serde_json::from_str(&self.outcomes)
            .map_err(|e| crate::error::AppError::Json(e))?;
        let token_ids: Vec<String> = serde_json::from_str(&self.clob_token_ids)
            .map_err(|e| crate::error::AppError::Json(e))?;

        if outcomes.len() < 2 || token_ids.len() < 2 {
            return Err(crate::error::AppError::ApiError(
                "outcomes 或 clobTokenIds 長度不足".into(),
            ));
        }

        // Find YES / NO by case-insensitive match
        let mut token_yes: Option<String> = None;
        let mut token_no: Option<String> = None;
        for (i, outcome) in outcomes.iter().enumerate() {
            let lo = outcome.to_lowercase();
            if lo == "yes" {
                token_yes = Some(token_ids[i].clone());
            } else if lo == "no" {
                token_no = Some(token_ids[i].clone());
            }
        }

        let token_id_yes = token_yes.ok_or_else(|| {
            crate::error::AppError::ApiError(format!("找不到 YES token: slug={}", self.slug))
        })?;
        let token_id_no = token_no.ok_or_else(|| {
            crate::error::AppError::ApiError(format!("找不到 NO token: slug={}", self.slug))
        })?;

        // Flatten tags — each entry may be a string or {"id":…, "label":…} object
        let tags = self
            .tags
            .into_iter()
            .filter_map(|v| match v {
                serde_json::Value::String(s) => Some(s),
                serde_json::Value::Object(m) => {
                    m.get("label").and_then(|l| l.as_str()).map(str::to_string)
                }
                _ => None,
            })
            .collect();

        Ok(MentionMarket {
            slug: self.slug,
            question: self.question.unwrap_or_default(),
            token_id_yes,
            token_id_no,
            close_ts,
            tags,
        })
    }
}

fn parse_iso_to_ts(iso: &str) -> Result<u64, crate::error::AppError> {
    chrono::DateTime::parse_from_rfc3339(iso)
        .map(|dt| dt.timestamp() as u64)
        .map_err(|e| {
            crate::error::AppError::ApiError(format!("日期解析失敗 '{iso}': {e}"))
        })
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_raw(slug: &str, question: &str, outcomes: &[&str], tokens: &[&str], end_date: &str)
        -> GammaMentionMarket
    {
        let outcomes_json = serde_json::to_string(
            &outcomes.iter().map(|s| s.to_string()).collect::<Vec<_>>()
        ).unwrap();
        let tokens_json = serde_json::to_string(
            &tokens.iter().map(|s| s.to_string()).collect::<Vec<_>>()
        ).unwrap();
        GammaMentionMarket {
            slug: slug.into(),
            question: Some(question.into()),
            outcomes: outcomes_json,
            clob_token_ids: tokens_json,
            end_date: end_date.into(),
            tags: vec![serde_json::Value::String("trump".into())],
        }
    }

    #[test]
    fn parses_yes_no_tokens() {
        let raw = make_raw(
            "trump-mentions-crypto",
            "Will Trump mention crypto?",
            &["Yes", "No"],
            &["token_yes_id", "token_no_id"],
            "2026-05-01T20:00:00Z",
        );
        let mm = raw.into_mention_market().unwrap();
        assert_eq!(mm.token_id_yes, "token_yes_id");
        assert_eq!(mm.token_id_no, "token_no_id");
        assert_eq!(mm.question, "Will Trump mention crypto?");
    }

    #[test]
    fn parses_no_yes_reverse_order() {
        let raw = make_raw(
            "trump-mentions-crypto",
            "Will Trump mention crypto?",
            &["No", "Yes"],
            &["token_no_id", "token_yes_id"],
            "2026-05-01T20:00:00Z",
        );
        let mm = raw.into_mention_market().unwrap();
        assert_eq!(mm.token_id_yes, "token_yes_id");
        assert_eq!(mm.token_id_no, "token_no_id");
    }

    #[test]
    fn errors_on_missing_yes_token() {
        let raw = make_raw(
            "bad-market",
            "Q?",
            &["Up", "Down"],
            &["t1", "t2"],
            "2026-05-01T20:00:00Z",
        );
        assert!(raw.into_mention_market().is_err());
    }
}
