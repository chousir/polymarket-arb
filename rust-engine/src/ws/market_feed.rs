use std::collections::{BTreeMap, HashMap};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use tokio::sync::mpsc;
use tokio_tungstenite::{connect_async, tungstenite::Message};

const WS_URL: &str = "wss://ws-subscriptions-clob.polymarket.com/ws/market";
const MAX_RETRIES: u32 = 10;
const RECONNECT_DELAY: Duration = Duration::from_secs(5);

// ── Public types ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct PriceSnapshot {
    pub up_best_ask: f64,
    pub down_best_ask: f64,
    pub sum: f64,
    /// BTC spot last price from Binance (0.0 until the combined feed sets it).
    pub btc_last: f64,
    pub ts: u64,
}

pub struct MarketFeed {
    up_token_id: String,
    down_token_id: String,
}

impl MarketFeed {
    pub fn new(
        up_token_id: impl Into<String>,
        down_token_id: impl Into<String>,
    ) -> Self {
        MarketFeed {
            up_token_id: up_token_id.into(),
            down_token_id: down_token_id.into(),
        }
    }

    /// Spawn background reconnect loop; return the receiving end of the price channel.
    pub fn subscribe(self) -> mpsc::Receiver<PriceSnapshot> {
        let (tx, rx) = mpsc::channel(64);
        tokio::spawn(run_feed(self.up_token_id, self.down_token_id, tx));
        rx
    }
}

// ── Local order book (asks side only) ────────────────────────────────────────
//
// Keys are price_ticks = round(price × 10_000) so the BTreeMap minimum is
// always the best ask without floating-point ordering pitfalls.

struct LocalBook {
    asks: BTreeMap<u64, f64>, // price_ticks → size
}

impl LocalBook {
    fn new() -> Self {
        Self { asks: BTreeMap::new() }
    }

    fn price_to_ticks(p: f64) -> u64 {
        (p * 10_000.0).round() as u64
    }

    fn set_level(&mut self, price: f64, size: f64) {
        let ticks = Self::price_to_ticks(price);
        if size <= 0.0 {
            self.asks.remove(&ticks);
        } else {
            self.asks.insert(ticks, size);
        }
    }

    fn best_ask(&self) -> Option<f64> {
        self.asks.keys().next().map(|&t| t as f64 / 10_000.0)
    }
}

// ── Private serde types ───────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct PriceLevel {
    price: String,
    size: String,
}

#[derive(Debug, Deserialize)]
struct BookMsg {
    asset_id: String,
    asks: Vec<PriceLevel>,
}

#[derive(Debug, Deserialize)]
struct ChangeEntry {
    price: String,
    size: String,
    side: String, // "BUY" | "SELL"
}

#[derive(Debug, Deserialize)]
struct PriceChangeMsg {
    asset_id: String,
    changes: Vec<ChangeEntry>,
}

enum WsMsg {
    Book(BookMsg),
    PriceChange(PriceChangeMsg),
}

// ── Reconnect loop ────────────────────────────────────────────────────────────

async fn run_feed(up_id: String, down_id: String, tx: mpsc::Sender<PriceSnapshot>) {
    let mut consecutive_errors: u32 = 0;

    loop {
        tracing::info!("[PM-WS] 連線 {WS_URL}");

        match try_connect(&up_id, &down_id, &tx).await {
            Ok(()) => {
                tracing::info!("[PM-WS] 伺服器正常關閉，重連中...");
                consecutive_errors = 0;
            }
            Err(e) => {
                consecutive_errors += 1;
                tracing::warn!(
                    "[PM-WS] 連線錯誤 ({consecutive_errors}/{MAX_RETRIES}): {e}"
                );
                if consecutive_errors >= MAX_RETRIES {
                    tracing::error!("[PM-WS] 已達最大重連次數，放棄");
                    return;
                }
            }
        }

        tokio::time::sleep(RECONNECT_DELAY).await;
    }
}

async fn try_connect(
    up_id: &str,
    down_id: &str,
    tx: &mpsc::Sender<PriceSnapshot>,
) -> Result<(), crate::error::AppError> {
    let (ws_stream, _) = connect_async(WS_URL)
        .await
        .map_err(|e| crate::error::AppError::WsError(e.to_string()))?;

    let (mut sink, mut stream) = ws_stream.split();

    // Send subscription for both token order books
    let sub = serde_json::json!({
        "assets_ids": [up_id, down_id],
        "type": "market"
    })
    .to_string();

    sink.send(Message::Text(sub.into()))
        .await
        .map_err(|e| crate::error::AppError::WsError(e.to_string()))?;

    tracing::info!("[PM-WS] 已訂閱  up_token={up_id}  down_token={down_id}");

    // Per-token order books (asks side only)
    let mut books: HashMap<String, LocalBook> = HashMap::from([
        (up_id.to_string(), LocalBook::new()),
        (down_id.to_string(), LocalBook::new()),
    ]);

    while let Some(msg) = stream.next().await {
        match msg {
            Ok(Message::Text(text)) => {
                for ws_msg in parse_messages(&text) {
                    if process_msg(ws_msg, &mut books, up_id, down_id, tx).await {
                        return Ok(()); // receiver dropped
                    }
                }
            }
            Ok(Message::Close(_)) => {
                return Err(crate::error::AppError::WsError("伺服器關閉連線".into()));
            }
            Ok(_) => {} // Ping / Pong / Binary
            Err(e) => return Err(crate::error::AppError::WsError(e.to_string())),
        }
    }

    Ok(())
}

// ── Message parsing ───────────────────────────────────────────────────────────

/// Handles both single-object `{...}` and batch-array `[{...}, ...]` frames.
fn parse_messages(text: &str) -> Vec<WsMsg> {
    let values: Vec<serde_json::Value> = if text.trim().starts_with('[') {
        serde_json::from_str(text).unwrap_or_else(|e| {
            tracing::warn!("[PM-WS] 陣列解析失敗: {e}");
            vec![]
        })
    } else {
        match serde_json::from_str::<serde_json::Value>(text) {
            Ok(v) => vec![v],
            Err(e) => {
                tracing::warn!("[PM-WS] JSON 解析失敗: {e}");
                vec![]
            }
        }
    };

    values
        .into_iter()
        .filter_map(|v| match v.get("event_type").and_then(|t| t.as_str()) {
            Some("book") => serde_json::from_value::<BookMsg>(v)
                .map(WsMsg::Book)
                .ok(),
            Some("price_change") => serde_json::from_value::<PriceChangeMsg>(v)
                .map(WsMsg::PriceChange)
                .ok(),
            _ => None,
        })
        .collect()
}

// ── State update + snapshot emission ─────────────────────────────────────────

/// Returns `true` if the receiver was dropped (caller should exit the loop).
async fn process_msg(
    msg: WsMsg,
    books: &mut HashMap<String, LocalBook>,
    up_id: &str,
    down_id: &str,
    tx: &mpsc::Sender<PriceSnapshot>,
) -> bool {
    match msg {
        WsMsg::Book(b) => {
            if let Some(book) = books.get_mut(&b.asset_id) {
                book.asks.clear();
                for lvl in &b.asks {
                    if let (Ok(p), Ok(s)) =
                        (lvl.price.parse::<f64>(), lvl.size.parse::<f64>())
                    {
                        book.set_level(p, s);
                    }
                }
                tracing::debug!(
                    "[PM-WS] book snap  asset={}  best_ask={:?}",
                    b.asset_id,
                    books.get(&b.asset_id).and_then(|b| b.best_ask())
                );
            }
        }
        WsMsg::PriceChange(pc) => {
            if let Some(book) = books.get_mut(&pc.asset_id) {
                for ch in &pc.changes {
                    if ch.side.eq_ignore_ascii_case("SELL") {
                        if let (Ok(p), Ok(s)) =
                            (ch.price.parse::<f64>(), ch.size.parse::<f64>())
                        {
                            book.set_level(p, s);
                        }
                    }
                }
            }
        }
    }

    // Emit only when both tokens have a valid best ask
    if let (Some(up_ask), Some(down_ask)) = (
        books.get(up_id).and_then(|b| b.best_ask()),
        books.get(down_id).and_then(|b| b.best_ask()),
    ) {
        let snap = PriceSnapshot {
            up_best_ask: up_ask,
            down_best_ask: down_ask,
            sum: up_ask + down_ask,
            btc_last: 0.0, // filled in by the combined-feed caller
            ts: chrono::Utc::now().timestamp() as u64,
        };
        if tx.send(snap).await.is_err() {
            return true;
        }
    }

    false
}
