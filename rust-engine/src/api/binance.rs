use std::time::Duration;

use futures_util::StreamExt;
use serde::{Deserialize, Deserializer};
use tokio::sync::mpsc;
use tokio_tungstenite::{connect_async, tungstenite::Message};

const WS_URL: &str = "wss://stream.binance.com:9443/ws/btcusdt@ticker";
const MAX_RETRIES: u32 = 10;
const RECONNECT_DELAY: Duration = Duration::from_secs(5);

// ── Serde helper: Binance sends prices as quoted strings ──────────────────────

fn parse_f64_str<'de, D: Deserializer<'de>>(d: D) -> Result<f64, D::Error> {
    let s = String::deserialize(d)?;
    s.parse::<f64>().map_err(serde::de::Error::custom)
}

/// Raw 24hrTicker JSON from Binance (only the fields we need).
#[derive(Debug, Deserialize)]
struct BinanceRaw {
    /// Event time (ms since epoch)
    #[serde(rename = "E")]
    ts: u64,
    /// Best bid price (string)
    #[serde(rename = "b", deserialize_with = "parse_f64_str")]
    best_bid: f64,
    /// Best ask price (string)
    #[serde(rename = "a", deserialize_with = "parse_f64_str")]
    best_ask: f64,
    /// Last trade price (string)
    #[serde(rename = "c", deserialize_with = "parse_f64_str")]
    last_price: f64,
}

// ── Public types ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct BinanceTicker {
    pub best_bid: f64,
    pub best_ask: f64,
    pub last_price: f64,
    /// Event time — milliseconds since Unix epoch
    pub ts: u64,
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Spawn a background task that streams BTC/USDT 24hr tickers from Binance.
/// Auto-reconnects on error (up to MAX_RETRIES consecutive failures).
/// Returns a channel receiver; drop it to stop the background task.
pub fn connect_binance_ws() -> mpsc::Receiver<BinanceTicker> {
    let (tx, rx) = mpsc::channel(64);
    tokio::spawn(run_ws(tx));
    rx
}

// ── Internal ──────────────────────────────────────────────────────────────────

async fn run_ws(tx: mpsc::Sender<BinanceTicker>) {
    let mut consecutive_errors: u32 = 0;

    loop {
        tracing::info!("[Binance] 連線 {WS_URL}");

        match try_connect(&tx).await {
            Ok(()) => {
                // Server closed cleanly — reset counter and reconnect
                tracing::info!("[Binance] 伺服器正常關閉連線，重連中...");
                consecutive_errors = 0;
            }
            Err(e) => {
                consecutive_errors += 1;
                tracing::warn!(
                    "[Binance] 連線錯誤 ({consecutive_errors}/{MAX_RETRIES}): {e}"
                );
                if consecutive_errors >= MAX_RETRIES {
                    tracing::error!(
                        "[Binance] 已達最大重連次數 ({MAX_RETRIES})，放棄重連"
                    );
                    return;
                }
            }
        }

        tokio::time::sleep(RECONNECT_DELAY).await;
    }
}

async fn try_connect(
    tx: &mpsc::Sender<BinanceTicker>,
) -> Result<(), crate::error::AppError> {
    let (mut ws_stream, _) = connect_async(WS_URL)
        .await
        .map_err(|e| crate::error::AppError::WsError(e.to_string()))?;

    while let Some(msg) = ws_stream.next().await {
        match msg {
            Ok(Message::Text(text)) => {
                match serde_json::from_str::<BinanceRaw>(&text) {
                    Ok(raw) => {
                        let ticker = BinanceTicker {
                            best_bid: raw.best_bid,
                            best_ask: raw.best_ask,
                            last_price: raw.last_price,
                            ts: raw.ts,
                        };
                        if tx.send(ticker).await.is_err() {
                            // Receiver was dropped — exit silently
                            return Ok(());
                        }
                    }
                    Err(e) => tracing::warn!("[Binance] JSON 解析失敗: {e}"),
                }
            }
            Ok(Message::Close(_)) => {
                return Err(crate::error::AppError::WsError(
                    "伺服器關閉連線".into(),
                ));
            }
            Ok(_) => {} // Ping / Pong / Binary — ignore
            Err(e) => {
                return Err(crate::error::AppError::WsError(e.to_string()));
            }
        }
    }

    Ok(())
}
