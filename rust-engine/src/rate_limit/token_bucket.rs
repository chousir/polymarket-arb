// Token Bucket 速率限制器 + 指數退避
// POST /order: 60/s 均速
// GET /books: 5/10s（嚴格限制）
// 429 回應時指數退避：1s → 2s → 4s + jitter

use std::time::{Duration, Instant};

use tokio::time::sleep;

// ── TokenBucket ───────────────────────────────────────────────────────────────

/// A simple token-bucket rate limiter (single-threaded; wrap in `Arc<Mutex<>>` if shared).
///
/// - `capacity`    : maximum burst size (tokens)
/// - `refill_rate` : tokens added per second
pub struct TokenBucket {
    capacity: f64,
    refill_rate: f64, // tokens / second
    tokens: f64,
    last_refill: Instant,
}

impl TokenBucket {
    /// Create a new bucket that starts full.
    ///
    /// # Arguments
    /// * `capacity`    – maximum burst (e.g. 60 for 60/s orders)
    /// * `refill_rate` – tokens per second (e.g. 60.0)
    pub fn new(capacity: u32, refill_rate: f64) -> Self {
        TokenBucket {
            capacity: capacity as f64,
            refill_rate,
            tokens: capacity as f64,
            last_refill: Instant::now(),
        }
    }

    /// Refill based on elapsed time since last call (clamped to capacity).
    fn refill(&mut self) {
        let elapsed = self.last_refill.elapsed().as_secs_f64();
        self.tokens = (self.tokens + elapsed * self.refill_rate).min(self.capacity);
        self.last_refill = Instant::now();
    }

    /// Acquire one token, sleeping until one is available if the bucket is empty.
    pub async fn acquire(&mut self) {
        loop {
            self.refill();
            if self.tokens >= 1.0 {
                self.tokens -= 1.0;
                return;
            }
            // Time until the next token arrives
            let wait_secs = (1.0 - self.tokens) / self.refill_rate;
            sleep(Duration::from_secs_f64(wait_secs)).await;
        }
    }
}

// ── Exponential back-off helper ───────────────────────────────────────────────

/// Sleep with exponential back-off on HTTP 429 / transient errors.
///
/// `attempt` is 0-indexed; jitter is ±25 % of the computed delay.
pub async fn backoff(attempt: u32, base_ms: u64) {
    let delay_ms = (base_ms << attempt).min(30_000); // cap at 30 s
    // Simple deterministic jitter: ±25 % using microsecond clock residue
    let jitter_ms = delay_ms / 4;
    let now_us = Instant::now().elapsed().subsec_micros() as u64; // 0..1_000_000
    let offset = now_us % (jitter_ms * 2 + 1); // 0..jitter_ms*2
    let actual_ms = delay_ms - jitter_ms + offset;
    tracing::warn!("[RateLimit] backoff attempt={attempt}  sleep={actual_ms}ms");
    sleep(Duration::from_millis(actual_ms)).await;
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn bucket_starts_full_and_drains() {
        let mut bucket = TokenBucket::new(3, 1.0);
        // Should be able to acquire 3 tokens without sleeping
        for _ in 0..3 {
            bucket.acquire().await;
        }
        assert!(bucket.tokens < 1.0, "bucket should be (nearly) empty after 3 acquires");
    }

    #[tokio::test]
    async fn bucket_refills_over_time() {
        let mut bucket = TokenBucket::new(5, 100.0); // 100 tokens/s → 1 token per 10ms
        // Drain completely
        for _ in 0..5 {
            bucket.acquire().await;
        }
        // Wait 50ms → should accumulate ~5 tokens
        sleep(Duration::from_millis(50)).await;
        bucket.refill();
        assert!(bucket.tokens >= 4.0, "expected ~5 tokens after 50ms at 100/s, got {}", bucket.tokens);
    }
}
