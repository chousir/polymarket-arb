// CLOB REST client — EIP-712 signing, HMAC-SHA256 auth, order lifecycle
//
// Covers:
//   POST   /order           — submit a signed limit order
//   GET    /order/{id}      — poll status until FILLED / CANCELLED
//   Writes live trade row to SQLite on completion

use std::time::Duration;

use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use hmac::{Hmac, Mac};
use k256::ecdsa::{RecoveryId, Signature, SigningKey};
use k256::ecdsa::signature::hazmat::PrehashSigner as _;
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use sha3::{Digest, Keccak256};

use crate::config::BotConfig;
use crate::db::writer::{DbWriter, LiveTrade};
use crate::error::AppError;
use crate::execution::executor::OrderIntent;

// ── Order-book REST fetch (public endpoint, no auth) ─────────────────────────

use once_cell::sync::Lazy;

#[cfg(test)]
use std::sync::Mutex;

static BOOK_CLIENT: Lazy<reqwest::Client> = Lazy::new(|| {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .expect("build book client")
});

/// Summary of the top-of-book for a single CLOB token.
#[derive(Debug, Clone)]
pub struct BookSummary {
    pub best_bid: f64,
    pub best_ask: f64,
    /// USDC value at the best-ask level (top-3 ask levels, size × price)
    pub depth_usdc: f64,
}

impl BookSummary {
    /// Derive the complementary token's book using binary-market symmetry
    /// (YES.ask + NO.bid ≈ 1, YES.bid + NO.ask ≈ 1).
    /// Used as a fallback when a live fetch of the other side fails.
    pub fn symmetric_complement(&self) -> Self {
        BookSummary {
            best_bid:   1.0 - self.best_ask,
            best_ask:   1.0 - self.best_bid,
            depth_usdc: self.depth_usdc,
        }
    }
}

#[cfg(test)]
#[derive(Debug, Clone)]
pub enum MockFetchBookOutcome {
    Success(BookSummary),
    Failure(String),
}

#[cfg(test)]
static MOCK_FETCH_BOOK_OUTCOME: Lazy<Mutex<Option<MockFetchBookOutcome>>> =
    Lazy::new(|| Mutex::new(None));

#[cfg(test)]
pub fn set_mock_fetch_order_book_outcome(outcome: Option<MockFetchBookOutcome>) {
    *MOCK_FETCH_BOOK_OUTCOME
        .lock()
        .expect("mock fetch book outcome mutex poisoned") = outcome;
}

/// Fetch the current order book for `token_id` from `GET /book?token_id=...`.
///
/// This is a public endpoint — no auth headers required.
/// Note: singular `/book`, not `/books`.  The plural endpoint takes a JSON body.
/// Returns [`BookSummary`] with best bid/ask and approximate depth.
pub async fn fetch_order_book(clob_base: &str, token_id: &str)
    -> Result<BookSummary, AppError>
{
    #[cfg(test)]
    {
        if let Some(outcome) = MOCK_FETCH_BOOK_OUTCOME
            .lock()
            .expect("mock fetch book outcome mutex poisoned")
            .clone()
        {
            return match outcome {
                MockFetchBookOutcome::Success(book) => Ok(book),
                MockFetchBookOutcome::Failure(msg) => {
                    Err(AppError::ApiError(format!("mock fetch book failure: {msg}")))
                }
            };
        }
    }

    let url = format!("{clob_base}/book?token_id={token_id}");

    let resp = BOOK_CLIENT
        .get(&url)
        .send()
        .await
        .map_err(|e| AppError::Http(e))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(AppError::ApiError(format!("GET /book HTTP {status}: {body}")));
    }

    let book: OrderBookResponse = resp.json().await.map_err(|e| AppError::Http(e))?;

    // Best ask = lowest ask price (token resolution ≡ 1 → high ask = cheap token)
    let best_ask = book
        .asks
        .iter()
        .filter_map(|l| l.price.parse::<f64>().ok())
        .fold(f64::INFINITY, f64::min);

    // Best bid = highest bid price
    let best_bid = book
        .bids
        .iter()
        .filter_map(|l| l.price.parse::<f64>().ok())
        .fold(f64::NEG_INFINITY, f64::max);

    // Depth ≈ USDC available at the three cheapest ask levels
    let mut sorted_asks: Vec<(f64, f64)> = book
        .asks
        .iter()
        .filter_map(|l| {
            let p = l.price.parse::<f64>().ok()?;
            let s = l.size.parse::<f64>().ok()?;
            Some((p, s * p)) // token_count × price ≈ USDC notional
        })
        .collect();
    sorted_asks.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    let depth_usdc: f64 = sorted_asks.iter().take(3).map(|(_, s)| s).sum();

    Ok(BookSummary {
        best_bid: if best_bid == f64::NEG_INFINITY { 0.0 } else { best_bid },
        best_ask: if best_ask == f64::INFINITY { 1.0 } else { best_ask },
        depth_usdc,
    })
}

#[derive(serde::Deserialize)]
struct OrderBookResponse {
    #[serde(default)]
    bids: Vec<BookLevel>,
    #[serde(default)]
    asks: Vec<BookLevel>,
}

#[derive(serde::Deserialize)]
struct BookLevel {
    price: String,
    size: String,
}

// ── Contract addresses (Polygon PoS) ─────────────────────────────────────────

const CTF_EXCHANGE: &str   = "0x4bFb41d5B3570DeFd03C39a9A4D8dE6Bd8B8982E";
const NEG_RISK_CTF: &str   = "0xC5d563A36AE78145C45a50134d48A1215220f80a";
const ZERO_ADDRESS: &str   = "0x0000000000000000000000000000000000000000";

// EIP-712 type string (must match the on-chain struct exactly)
const ORDER_TYPE_STR: &str =
    "Order(uint256 salt,address maker,address signer,address taker,\
     uint256 tokenId,uint256 makerAmount,uint256 takerAmount,\
     uint256 expiration,uint256 nonce,uint256 feeRateBps,\
     uint8 side,uint8 signatureType)";

// ── Public result type ────────────────────────────────────────────────────────

pub struct LiveOrderResult {
    pub order_id: String,
    pub status: String, // FILLED | CANCELLED | EXPIRED | TIMEOUT
    pub filled_usdc: Option<f64>,
}

// ── ClobClient ────────────────────────────────────────────────────────────────

pub struct ClobClient {
    http: reqwest::Client,
    config: BotConfig,
}

impl ClobClient {
    pub fn new(config: BotConfig) -> Self {
        ClobClient {
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(15))
                .build()
                .expect("build reqwest client"),
            config,
        }
    }

    /// Full live-order flow: sign → submit → poll → write DB.
    pub async fn submit_live_order(
        &self,
        intent: &OrderIntent,
        db: &DbWriter,
    ) -> Result<LiveOrderResult, AppError> {
        // 1. Build + sign the EIP-712 order
        let signed = self.build_signed_order(intent)?;
        let body = serde_json::to_string(&signed)?;

        // 2. POST /order
        let path = "/order";
        let ts = chrono::Utc::now().timestamp_millis().to_string();
        let headers = self.auth_headers(&ts, "POST", path, &body);

        let url = format!("{}{}", self.config.clob_base, path);
        let mut req = self.http.post(&url).header("Content-Type", "application/json").body(body);
        for (k, v) in &headers {
            req = req.header(k.as_str(), v.as_str());
        }

        let resp = req.send().await.map_err(|e| AppError::Http(e))?;
        let status_code = resp.status();
        let resp_text = resp.text().await.map_err(|e| AppError::Http(e))?;

        if !status_code.is_success() {
            return Err(AppError::ApiError(format!(
                "POST /order HTTP {status_code}: {resp_text}"
            )));
        }

        let submit_resp: SubmitOrderResponse = serde_json::from_str(&resp_text)
            .map_err(|e| AppError::Other(format!("parse /order response: {e}: {resp_text}")))?;

        if !submit_resp.success {
            return Err(AppError::ApiError(format!(
                "CLOB rejected order: {}",
                submit_resp.error_msg.unwrap_or_default()
            )));
        }

        let order_id = submit_resp.order_id;
        tracing::info!(
            "[CLOB] 訂單已提交  order_id={order_id}  slug={}  leg={}  price={:.4}  size={:.2}",
            intent.market_slug, intent.leg, intent.price, intent.size_usdc
        );

        // 3. Poll until FILLED / CANCELLED / timeout (30 s)
        let result = self.poll_until_terminal(&order_id).await;

        // 4. Write to live_trades regardless of outcome
        let live_trade = LiveTrade {
            strategy_id: intent.strategy_id.clone(),
            market_slug: intent.market_slug.clone(),
            leg: intent.leg,
            side: intent.side.clone(),
            order_id: order_id.clone(),
            price: intent.price,
            size_usdc: intent.size_usdc,
            fee_usdc: intent.fee_usdc,
            filled_usdc: result.filled_usdc,
            status: result.status.clone(),
            tx_hash: None,
        };
        db.write_live_trade(&live_trade).await?;

        tracing::info!(
            "[CLOB] 訂單完成  order_id={order_id}  status={}  filled={:?}",
            result.status, result.filled_usdc
        );

        Ok(result)
    }

    // ── EIP-712 signing ───────────────────────────────────────────────────────

    fn build_signed_order(&self, intent: &OrderIntent) -> Result<SignedOrderBody, AppError> {
        let priv_key = &self.config.polygon_private_key;
        if priv_key.is_empty() {
            return Err(AppError::ConfigError("POLYGON_PRIVATE_KEY is not set".into()));
        }

        let key_bytes = parse_hex_key(priv_key)?;
        let signing_key = SigningKey::from_bytes((&key_bytes).into())
            .map_err(|e| AppError::Other(format!("invalid private key: {e}")))?;
        let maker = eth_address_from_key(&signing_key);

        // Generate random 8-byte salt (sufficient entropy for non-replay)
        let salt = {
            use std::time::{SystemTime, UNIX_EPOCH};
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(0)
        };

        // USDC has 6 decimals; token amounts also use 6 decimals on Polygon
        let (maker_amount, taker_amount) = if intent.side.eq_ignore_ascii_case("BUY") {
            // Paying USDC, receiving tokens
            let usdc_units = (intent.size_usdc * 1_000_000.0).round() as u64;
            let token_units = (intent.size_usdc / intent.price * 1_000_000.0).round() as u64;
            (usdc_units, token_units)
        } else {
            // Paying tokens, receiving USDC
            let token_units = (intent.size_usdc / intent.price * 1_000_000.0).round() as u64;
            let usdc_units = (intent.size_usdc * 1_000_000.0).round() as u64;
            (token_units, usdc_units)
        };

        let side_u8: u8 = if intent.side.eq_ignore_ascii_case("BUY") { 0 } else { 1 };
        let token_id_bytes = decimal_to_bytes32(&intent.token_id)?;

        // Domain separator — use NEG_RISK_CTF for NegRisk markets (BTC 15m)
        // Currently we use the regular CTF Exchange; extend MarketInfo.is_neg_risk later
        let verifying = hex_to_address(CTF_EXCHANGE)?;
        let domain_sep = eip712_domain_separator(self.config.chain_id, &verifying);

        // Order struct hash
        let type_hash = keccak256(ORDER_TYPE_STR.as_bytes());
        let mut enc = Vec::with_capacity(32 * 13);
        enc.extend_from_slice(&type_hash);
        enc.extend_from_slice(&u64_to_bytes32(salt));
        enc.extend_from_slice(&address_to_bytes32(&maker));
        enc.extend_from_slice(&address_to_bytes32(&maker)); // signer == maker for EOA
        enc.extend_from_slice(&[0u8; 32]);                 // taker = zero address
        enc.extend_from_slice(&token_id_bytes);
        enc.extend_from_slice(&u64_to_bytes32(maker_amount));
        enc.extend_from_slice(&u64_to_bytes32(taker_amount));
        enc.extend_from_slice(&[0u8; 32]); // expiration = 0 (no expiry)
        enc.extend_from_slice(&[0u8; 32]); // nonce = 0
        enc.extend_from_slice(&[0u8; 32]); // feeRateBps = 0 (maker GTC)
        enc.extend_from_slice(&u64_to_bytes32(side_u8 as u64));
        enc.extend_from_slice(&[0u8; 32]); // signatureType = 0 (EOA)
        let struct_hash = keccak256(&enc);

        // Final EIP-712 hash: "\x19\x01" || domain_sep || struct_hash
        let mut digest_input = Vec::with_capacity(66);
        digest_input.extend_from_slice(b"\x19\x01");
        digest_input.extend_from_slice(&domain_sep);
        digest_input.extend_from_slice(&struct_hash);
        let final_hash = keccak256(&digest_input);

        // Sign
        let (sig, recovery_id): (Signature, RecoveryId) = signing_key
            .sign_prehash_recoverable(&final_hash)
            .map_err(|e| AppError::Other(format!("signing failed: {e}")))?;

        let r = sig.r().to_bytes();
        let s = sig.s().to_bytes();
        let v = recovery_id.to_byte() + 27;
        let mut sig_bytes = [0u8; 65];
        sig_bytes[..32].copy_from_slice(&r);
        sig_bytes[32..64].copy_from_slice(&s);
        sig_bytes[64] = v;
        let signature = format!("0x{}", hex::encode(sig_bytes));

        let maker_hex = format!("0x{}", hex::encode(maker));
        Ok(SignedOrderBody {
            order: OrderFields {
                salt: salt.to_string(),
                maker: maker_hex.clone(),
                signer: maker_hex.clone(),
                taker: ZERO_ADDRESS.to_string(),
                token_id: intent.token_id.clone(),
                maker_amount: maker_amount.to_string(),
                taker_amount: taker_amount.to_string(),
                expiration: "0".to_string(),
                nonce: "0".to_string(),
                fee_rate_bps: "0".to_string(),
                side: side_u8,
                signature_type: 0,
                signature,
            },
            owner: maker_hex,
            order_type: "GTC".to_string(),
        })
    }

    // ── Poll order status ─────────────────────────────────────────────────────

    async fn poll_until_terminal(&self, order_id: &str) -> LiveOrderResult {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        let mut interval = tokio::time::interval(Duration::from_secs(2));

        loop {
            interval.tick().await;

            if tokio::time::Instant::now() >= deadline {
                tracing::warn!("[CLOB] 訂單輪詢逾時 30s  order_id={order_id}");
                return LiveOrderResult {
                    order_id: order_id.to_string(),
                    status: "TIMEOUT".to_string(),
                    filled_usdc: None,
                };
            }

            match self.get_order_status(order_id).await {
                Ok(s) if s.is_terminal() => {
                    return LiveOrderResult {
                        order_id: order_id.to_string(),
                        filled_usdc: s.filled_usdc(),
                        status: s.status,
                    };
                }
                Ok(s) => {
                    tracing::debug!("[CLOB] 訂單狀態={} order_id={order_id}", s.status);
                }
                Err(e) => {
                    tracing::warn!("[CLOB] 狀態查詢失敗: {e}");
                }
            }
        }
    }

    async fn get_order_status(&self, order_id: &str) -> Result<OrderStatusResponse, AppError> {
        let path = format!("/order/{order_id}");
        let ts = chrono::Utc::now().timestamp_millis().to_string();
        let headers = self.auth_headers(&ts, "GET", &path, "");

        let url = format!("{}{}", self.config.clob_base, path);
        let mut req = self.http.get(&url);
        for (k, v) in &headers {
            req = req.header(k.as_str(), v.as_str());
        }

        let resp = req.send().await.map_err(|e| AppError::Http(e))?;
        let text = resp.text().await.map_err(|e| AppError::Http(e))?;
        serde_json::from_str::<OrderStatusResponse>(&text)
            .map_err(|e| AppError::Other(format!("parse order status: {e}: {text}")))
    }

    // ── HMAC-SHA256 auth headers ──────────────────────────────────────────────

    fn auth_headers(
        &self,
        ts: &str,
        method: &str,
        path: &str,
        body: &str,
    ) -> Vec<(String, String)> {
        let message = format!("{ts}{method}{path}{body}");
        let mut mac = Hmac::<Sha256>::new_from_slice(self.config.clob_api_secret.as_bytes())
            .expect("HMAC accepts any key length");
        mac.update(message.as_bytes());
        let signature = B64.encode(mac.finalize().into_bytes());

        vec![
            ("POLY-ADDRESS".to_string(),    eth_address_hex_from_key_str(&self.config.polygon_private_key)),
            ("POLY-SIGNATURE".to_string(),  signature),
            ("POLY-TIMESTAMP".to_string(),  ts.to_string()),
            ("POLY-API-KEY".to_string(),    self.config.clob_api_key.clone()),
            ("POLY-PASSPHRASE".to_string(), self.config.clob_api_passphrase.clone()),
        ]
    }
}

// ── Serde types ───────────────────────────────────────────────────────────────

#[derive(Serialize)]
struct SignedOrderBody {
    order: OrderFields,
    owner: String,
    #[serde(rename = "orderType")]
    order_type: String,
}

#[derive(Serialize)]
struct OrderFields {
    salt: String,
    maker: String,
    signer: String,
    taker: String,
    #[serde(rename = "tokenId")]
    token_id: String,
    #[serde(rename = "makerAmount")]
    maker_amount: String,
    #[serde(rename = "takerAmount")]
    taker_amount: String,
    expiration: String,
    nonce: String,
    #[serde(rename = "feeRateBps")]
    fee_rate_bps: String,
    side: u8,
    #[serde(rename = "signatureType")]
    signature_type: u8,
    signature: String,
}

#[derive(Deserialize)]
struct SubmitOrderResponse {
    #[serde(default)]
    success: bool,
    #[serde(rename = "orderID", alias = "order_id", default)]
    order_id: String,
    #[serde(rename = "errorMsg", alias = "error_msg")]
    error_msg: Option<String>,
}

#[derive(Deserialize)]
struct OrderStatusResponse {
    #[serde(default)]
    status: String,
    #[serde(rename = "makerAmount", default)]
    maker_amount: String,
    #[serde(rename = "remainingAmount", default)]
    remaining_amount: String,
}

impl OrderStatusResponse {
    fn is_terminal(&self) -> bool {
        matches!(
            self.status.as_str(),
            "FILLED" | "MATCHED" | "CANCELLED" | "CANCELED" | "EXPIRED"
        )
    }

    fn filled_usdc(&self) -> Option<f64> {
        // filled = makerAmount - remainingAmount (in USDC units ÷ 1e6)
        let maker: u64 = self.maker_amount.parse().unwrap_or(0);
        let remaining: u64 = self.remaining_amount.parse().unwrap_or(0);
        let filled = maker.saturating_sub(remaining);
        if filled > 0 {
            Some(filled as f64 / 1_000_000.0)
        } else {
            None
        }
    }
}

// ── EIP-712 helpers ───────────────────────────────────────────────────────────

fn keccak256(data: &[u8]) -> [u8; 32] {
    let mut h = Keccak256::new();
    h.update(data);
    h.finalize().into()
}

fn eip712_domain_separator(chain_id: u64, verifying_contract: &[u8; 20]) -> [u8; 32] {
    const DOMAIN_TYPE_STR: &str =
        "EIP712Domain(string name,string version,uint256 chainId,address verifyingContract)";
    let type_hash  = keccak256(DOMAIN_TYPE_STR.as_bytes());
    let name_hash  = keccak256(b"Polymarket CTF Exchange");
    let ver_hash   = keccak256(b"1");

    let mut enc = Vec::with_capacity(32 * 5);
    enc.extend_from_slice(&type_hash);
    enc.extend_from_slice(&name_hash);
    enc.extend_from_slice(&ver_hash);
    enc.extend_from_slice(&u64_to_bytes32(chain_id));
    enc.extend_from_slice(&address_to_bytes32(verifying_contract));
    keccak256(&enc)
}

/// Encode a u64 as a 32-byte big-endian word.
fn u64_to_bytes32(n: u64) -> [u8; 32] {
    let mut b = [0u8; 32];
    b[24..].copy_from_slice(&n.to_be_bytes());
    b
}

/// Left-pad a 20-byte address to 32 bytes.
fn address_to_bytes32(addr: &[u8; 20]) -> [u8; 32] {
    let mut b = [0u8; 32];
    b[12..].copy_from_slice(addr);
    b
}

/// Parse a decimal string (potentially 256-bit) into a big-endian 32-byte array.
fn decimal_to_bytes32(s: &str) -> Result<[u8; 32], AppError> {
    let s = s.trim();
    let mut result = [0u8; 32];
    for ch in s.chars() {
        let digit = ch
            .to_digit(10)
            .ok_or_else(|| AppError::Other(format!("invalid decimal digit in token_id: {ch}")))?;
        let mut carry = digit as u64;
        for b in result.iter_mut().rev() {
            let val = (*b as u64) * 10 + carry;
            *b = (val & 0xff) as u8;
            carry = val >> 8;
        }
    }
    Ok(result)
}

/// Parse "0x…" hex address string to [u8; 20].
fn hex_to_address(s: &str) -> Result<[u8; 20], AppError> {
    let s = s.strip_prefix("0x").unwrap_or(s);
    let bytes = hex::decode(s)
        .map_err(|e| AppError::Other(format!("hex_to_address: {e}")))?;
    bytes
        .try_into()
        .map_err(|_| AppError::Other("address must be 20 bytes".into()))
}

/// Parse a hex private key string (with or without 0x) to 32 bytes.
fn parse_hex_key(s: &str) -> Result<[u8; 32], AppError> {
    let s = s.strip_prefix("0x").unwrap_or(s);
    let bytes = hex::decode(s)
        .map_err(|e| AppError::Other(format!("private key hex: {e}")))?;
    bytes
        .try_into()
        .map_err(|_| AppError::Other("private key must be 32 bytes".into()))
}

/// Derive the 20-byte Ethereum address from a SigningKey.
fn eth_address_from_key(key: &SigningKey) -> [u8; 20] {
    use k256::elliptic_curve::sec1::ToEncodedPoint as _;
    let vk = key.verifying_key();
    let point = vk.to_encoded_point(false); // uncompressed
    let pub_bytes = &point.as_bytes()[1..]; // drop 0x04 prefix → 64 bytes
    let hash = keccak256(pub_bytes);
    let mut addr = [0u8; 20];
    addr.copy_from_slice(&hash[12..]);
    addr
}

fn eth_address_hex_from_key_str(priv_key: &str) -> String {
    if priv_key.is_empty() {
        return String::new();
    }
    match parse_hex_key(priv_key) {
        Ok(bytes) => match SigningKey::from_bytes((&bytes).into()) {
            Ok(k) => format!("0x{}", hex::encode(eth_address_from_key(&k))),
            Err(_) => String::new(),
        },
        Err(_) => String::new(),
    }
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decimal_to_bytes32_small() {
        let b = decimal_to_bytes32("255").unwrap();
        assert_eq!(b[31], 0xff);
        assert!(b[..31].iter().all(|&x| x == 0));
    }

    #[test]
    fn decimal_to_bytes32_large() {
        // 256 = 0x0100
        let b = decimal_to_bytes32("256").unwrap();
        assert_eq!(b[30], 0x01);
        assert_eq!(b[31], 0x00);
    }

    #[test]
    fn keccak256_known_vector() {
        // keccak256("") = c5d2460186f7233c927e7db2dcc703c0e500b653ca82273b7bfad8045d85a470
        let h = keccak256(b"");
        assert_eq!(h[0], 0xc5);
        assert_eq!(h[1], 0xd2);
    }

    #[test]
    fn u64_to_bytes32_round_trip() {
        let b = u64_to_bytes32(0xDEAD_BEEF);
        // 0xDEADBEEF = 4 bytes, stored at positions 28..32
        assert_eq!(&b[28..], &[0xDE, 0xAD, 0xBE, 0xEF]);
        assert!(b[..28].iter().all(|&x| x == 0));
    }

    #[test]
    fn address_to_bytes32_pads_left() {
        let addr = [0xABu8; 20];
        let b = address_to_bytes32(&addr);
        assert!(b[..12].iter().all(|&x| x == 0));
        assert_eq!(&b[12..], &addr);
    }

    #[test]
    fn domain_separator_is_deterministic() {
        let addr = hex_to_address(CTF_EXCHANGE).unwrap();
        let s1 = eip712_domain_separator(137, &addr);
        let s2 = eip712_domain_separator(137, &addr);
        assert_eq!(s1, s2);
        // Must differ on different chain ID
        let s3 = eip712_domain_separator(1, &addr);
        assert_ne!(s1, s3);
    }

    #[test]
    fn eth_address_derivation() {
        // Well-known test vector: private key all-ones
        let key_bytes = [1u8; 32];
        let key = SigningKey::from_bytes((&key_bytes).into()).unwrap();
        let addr = eth_address_from_key(&key);
        // Just check it's 20 non-zero bytes (deterministic)
        assert_eq!(addr.len(), 20);
        assert!(addr.iter().any(|&b| b != 0));
    }
}
