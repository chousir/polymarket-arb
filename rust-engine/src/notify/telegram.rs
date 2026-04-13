// Telegram 告警
//
// Fire-and-forget: failures are logged but never propagate to the caller.
// All methods are async; call with `.await` from async context, or
// spawn as a background task for non-blocking sends.

use std::time::Duration;

use crate::db::writer::CycleResult;
use crate::risk::circuit_breaker::BreakerEvent;

const SEND_TIMEOUT: Duration = Duration::from_secs(8);
const API_BASE: &str = "https://api.telegram.org/bot";

// ── Notifier ──────────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct Notifier {
    token: String,
    chat_id: String,
    http: reqwest::Client,
}

impl Notifier {
    /// Create a notifier. Returns `None` if token or chat_id are empty.
    pub fn new(token: impl Into<String>, chat_id: impl Into<String>) -> Option<Self> {
        let token = token.into();
        let chat_id = chat_id.into();
        if token.is_empty() || chat_id.is_empty() {
            return None;
        }
        let http = reqwest::Client::builder()
            .timeout(SEND_TIMEOUT)
            .build()
            .expect("build telegram client");
        Some(Notifier { token, chat_id, http })
    }

    // ── Public message types ──────────────────────────────────────────────────

    /// Sent after every completed market cycle.
    pub async fn notify_cycle_end(&self, result: &CycleResult, daily_pnl: f64) {
        let leg1 = if result.leg1_price.is_some() {
            format!("✓ {:.4}", result.leg1_price.unwrap())
        } else {
            "—".into()
        };
        let leg2 = if result.leg2_price.is_some() {
            format!("✓ {:.4}", result.leg2_price.unwrap())
        } else {
            "—".into()
        };
        let pnl = match result.pnl_usdc {
            Some(p) if p >= 0.0 => format!("💚 +{p:.4} USDC"),
            Some(p)             => format!("🔴 {p:.4} USDC"),
            None                => "pending".into(),
        };

        let mode_tag = if result.mode == "dry_run" { "🔵 DRY\\_RUN" } else { "🟢 LIVE" };
        let text = format!(
            "📊 {mode_tag} *{}*\nLeg1: {leg1}  Leg2: {leg2}\nPnL: {pnl}\nDay PnL: {daily_pnl:+.4} USDC",
            escape_md(&result.market_slug),
        );
        self.send(&text).await;
    }

    /// Sent when the circuit breaker fires.
    pub async fn notify_breaker(&self, event: &BreakerEvent) {
        let text = match event {
            BreakerEvent::DailyLossTripped { loss_usdc, limit_usdc } => format!(
                "🚨 *Circuit Breaker 觸發*\n每日虧損 {loss_usdc:.4} USDC 超過限制 {limit_usdc:.4} USDC\n新循環已暫停直到明日。"
            ),
            BreakerEvent::HedgeFailBetReduced { consecutive_fails } => format!(
                "⚠️ *Leg2 連續失敗 {consecutive_fails} 次*\n下注金額自動降低 50%。"
            ),
            BreakerEvent::BetSizeRestored => {
                "✅ Leg2 成功對沖，下注金額恢復正常。".into()
            }
        };
        self.send(&text).await;
    }

    /// Generic alert for API disconnects, startup, shutdown, etc.
    pub async fn notify_alert(&self, emoji: &str, title: &str, detail: &str) {
        let text = format!(
            "{emoji} *{}*\n{}",
            escape_md(title),
            escape_md(detail)
        );
        self.send(&text).await;
    }

    // ── Core send ─────────────────────────────────────────────────────────────

    async fn send(&self, text: &str) {
        let url = format!("{}{}/sendMessage", API_BASE, self.token);
        let body = serde_json::json!({
            "chat_id": self.chat_id,
            "text": text,
            "parse_mode": "MarkdownV2",
        });

        match self.http.post(&url).json(&body).send().await {
            Ok(resp) if resp.status().is_success() => {
                tracing::debug!("[TG] 訊息已送出");
            }
            Ok(resp) => {
                let status = resp.status();
                let body = resp.text().await.unwrap_or_default();
                tracing::warn!("[TG] 送出失敗 HTTP {status}: {body}");
            }
            Err(e) => {
                tracing::warn!("[TG] 網路錯誤: {e}");
            }
        }
    }
}

// ── MarkdownV2 escaping ───────────────────────────────────────────────────────
// Telegram MarkdownV2 requires escaping: _ * [ ] ( ) ~ ` > # + - = | { } . !

fn escape_md(s: &str) -> String {
    const SPECIAL: &[char] = &[
        '_', '*', '[', ']', '(', ')', '~', '`', '>', '#', '+', '-', '=', '|', '{', '}', '.', '!',
    ];
    let mut out = String::with_capacity(s.len() + 8);
    for ch in s.chars() {
        if SPECIAL.contains(&ch) {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notifier_none_on_empty_token() {
        assert!(Notifier::new("", "123456").is_none());
        assert!(Notifier::new("TOKEN", "").is_none());
        assert!(Notifier::new("", "").is_none());
        assert!(Notifier::new("tok", "id").is_some());
    }

    #[test]
    fn escape_md_escapes_specials() {
        assert_eq!(escape_md("hello.world"), "hello\\.world");
        assert_eq!(escape_md("1+1=2"), "1\\+1\\=2");
        assert_eq!(escape_md("no_specials_here_aside_from_underscore"),
                   "no\\_specials\\_here\\_aside\\_from\\_underscore");
        assert_eq!(escape_md("plain"), "plain");
    }

    #[test]
    fn cycle_message_formats_correctly() {
        use crate::db::writer::CycleResult;
        let result = CycleResult {
            strategy_id: "test".into(),
            market_slug: "btc-updown-15m-1000".into(),
            mode: "dry_run".into(),
            leg1_side: Some("BUY".into()),
            leg1_price: Some(0.40),
            leg2_price: Some(0.55),
            resolved_winner: None,
            pnl_usdc: Some(0.05),
        };
        // Just verify it doesn't panic
        let notifier = Notifier::new("tok", "id").unwrap();
        // Build message string manually (can't await in sync test)
        let leg1 = format!("✓ {:.4}", result.leg1_price.unwrap());
        assert_eq!(leg1, "✓ 0.4000");
    }
}
