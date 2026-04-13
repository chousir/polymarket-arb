// Trump Mention Market — keyword filter and strategy-side direction logic.
//
// Strategy overview
// ─────────────────
// NO  (main strategy):
//   Markets where Trump is highly unlikely to use the exact phrase.
//   We buy NO tokens cheaply and collect if he does not say it.
//   Phrase pool: technical / contrition vocabulary he almost never uses.
//
// YES (auxiliary strategy):
//   Markets where Trump reliably uses the phrase in every speech.
//   We buy YES tokens before he speaks and sell / hold to resolution.
//   Phrase whitelist: rhetorical staples.
//
// SKIP:
//   Market does not match either pool — too uncertain, or phrase is
//   generic enough that model has no edge.

use crate::api::mention_market::MentionMarket;

// ── Keyword pools ─────────────────────────────────────────────────────────────

/// Phrases Trump almost never uses → buy NO.
const NO_KEYWORDS: &[&str] = &[
    "crypto",
    "bitcoin",
    "doge",
    "quantitative easing",
    "macroeconomics",
    "apologize",
    "sorry",
    "my mistake",
];

/// Phrases Trump uses in virtually every speech → buy YES.
const YES_KEYWORDS: &[&str] = &[
    "rigged election",
    "worst president",
    "terrible",
    "great",
];

// ── Public types ──────────────────────────────────────────────────────────────

/// Strategy direction for a single mention market.
#[derive(Debug, Clone, PartialEq)]
pub enum Decision {
    /// Buy NO token (main strategy)
    No,
    /// Buy YES token (auxiliary strategy)
    Yes,
    /// Market not matched by either pool — skip
    Skip,
}

/// Result of applying the keyword filter to one market.
#[derive(Debug, Clone)]
pub struct MarketVerdict {
    pub market: MentionMarket,
    pub decision: Decision,
    /// Human-readable explanation for dry_run logging
    pub reason: String,
}

// ── Filter ────────────────────────────────────────────────────────────────────

/// Classify each market and return verdicts for all of them (including SKIPs).
///
/// In dry_run the caller should log every verdict so we can audit which
/// markets are included or excluded and why.
pub fn filter_markets(markets: &[MentionMarket]) -> Vec<MarketVerdict> {
    markets.iter().map(|m| classify(m)).collect()
}

fn classify(market: &MentionMarket) -> MarketVerdict {
    let q = market.question.to_lowercase();

    // NO keywords checked first (higher-frequency, lower-risk edge)
    for &kw in NO_KEYWORDS {
        if q.contains(kw) {
            return MarketVerdict {
                market: market.clone(),
                decision: Decision::No,
                reason: format!("NO pool keyword match: \"{kw}\""),
            };
        }
    }

    // YES keywords second
    for &kw in YES_KEYWORDS {
        if q.contains(kw) {
            return MarketVerdict {
                market: market.clone(),
                decision: Decision::Yes,
                reason: format!("YES whitelist keyword match: \"{kw}\""),
            };
        }
    }

    MarketVerdict {
        market: market.clone(),
        decision: Decision::Skip,
        reason: "no keyword match".into(),
    }
}

// ── Dry-run logger ────────────────────────────────────────────────────────────

/// Print a structured log line for every verdict.
/// Call this in dry_run mode after `filter_markets()`.
pub fn log_verdicts(verdicts: &[MarketVerdict]) {
    let included: Vec<_> = verdicts
        .iter()
        .filter(|v| v.decision != Decision::Skip)
        .collect();
    let skipped: Vec<_> = verdicts
        .iter()
        .filter(|v| v.decision == Decision::Skip)
        .collect();

    tracing::info!(
        "[MentionFilter] {} 市場已納入 / {} 市場已略過（共 {} 個）",
        included.len(), skipped.len(), verdicts.len()
    );

    for v in &included {
        tracing::info!(
            "[MentionFilter] ✅ {:?} slug={}  q=\"{}\"  reason={}",
            v.decision, v.market.slug, v.market.question, v.reason
        );
    }
    for v in &skipped {
        tracing::debug!(
            "[MentionFilter] ⏭  SKIP  slug={}  reason={}",
            v.market.slug, v.reason
        );
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn market(question: &str) -> MentionMarket {
        MentionMarket {
            slug: "test-slug".into(),
            question: question.into(),
            token_id_yes: "yes_token".into(),
            token_id_no: "no_token".into(),
            close_ts: 9_999_999_999,
            tags: vec!["trump".into()],
        }
    }

    #[test]
    fn no_keyword_matches_crypto() {
        let v = classify(&market("Will Trump mention crypto in his speech?"));
        assert_eq!(v.decision, Decision::No);
        assert!(v.reason.contains("crypto"));
    }

    #[test]
    fn no_keyword_matches_bitcoin() {
        let v = classify(&market("Will Trump say bitcoin today?"));
        assert_eq!(v.decision, Decision::No);
    }

    #[test]
    fn no_keyword_matches_apologize() {
        let v = classify(&market("Will Trump apologize to Biden?"));
        assert_eq!(v.decision, Decision::No);
        assert!(v.reason.contains("apologize"));
    }

    #[test]
    fn yes_keyword_matches_terrible() {
        let v = classify(&market("Will Trump say 'terrible' about the economy?"));
        assert_eq!(v.decision, Decision::Yes);
        assert!(v.reason.contains("terrible"));
    }

    #[test]
    fn yes_keyword_matches_rigged_election() {
        let v = classify(&market("Will Trump mention 'rigged election'?"));
        assert_eq!(v.decision, Decision::Yes);
    }

    #[test]
    fn no_takes_priority_over_yes_for_great() {
        // "macroeconomics" is in NO pool; "great" is in YES pool
        // This question has both → NO wins (checked first)
        let v = classify(&market("Will Trump mention macroeconomics in a great speech?"));
        assert_eq!(v.decision, Decision::No);
    }

    #[test]
    fn skip_when_no_keywords_match() {
        let v = classify(&market("Will Trump talk about immigration reform?"));
        assert_eq!(v.decision, Decision::Skip);
        assert_eq!(v.reason, "no keyword match");
    }

    #[test]
    fn case_insensitive_matching() {
        let v = classify(&market("Will Trump mention BITCOIN in a tweet?"));
        assert_eq!(v.decision, Decision::No);
    }

    #[test]
    fn filter_markets_classifies_all() {
        let markets = vec![
            market("Will Trump mention crypto?"),
            market("Will Trump say terrible?"),
            market("Will Trump talk about cats?"),
        ];
        let verdicts = filter_markets(&markets);
        assert_eq!(verdicts.len(), 3);
        assert_eq!(verdicts[0].decision, Decision::No);
        assert_eq!(verdicts[1].decision, Decision::Yes);
        assert_eq!(verdicts[2].decision, Decision::Skip);
    }
}
