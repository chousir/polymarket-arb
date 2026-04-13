/// Polygon 鏈上查詢
///
/// 用於 live 模式啟動時查詢錢包的實際 USDC 餘額。
///
/// # 實作方式
///
/// 對 Polygon 公共 RPC 發送 `eth_call`，呼叫 USDC ERC-20 合約的
/// `balanceOf(address)` 函數。USDC on Polygon 有 6 位小數。

use crate::error::AppError;

/// Polygon USDC 合約地址（Bridged USDC）
const USDC_CONTRACT: &str = "0x2791Bca1f2de4661ED88A30C99A7a9449Aa84174";

/// `balanceOf(address)` 的 function selector（keccak256 前 4 bytes）
const BALANCE_OF_SELECTOR: &str = "70a08231";

/// 查詢錢包的 USDC 餘額（返回 USDC，保留小數）
///
/// # 錯誤
/// 網路失敗或 RPC 回應異常時返回 `AppError`。
/// 啟動時如果查詢失敗，建議退回使用 `config.initial_capital_usdc`。
pub async fn fetch_usdc_balance(
    rpc_url: &str,
    wallet_address: &str,
) -> Result<f64, AppError> {
    // 移除 0x 前綴並補齊 32 bytes（address 右對齊）
    let addr_clean = wallet_address.trim_start_matches("0x").to_lowercase();
    let padded = format!("{:0>64}", addr_clean);
    let data = format!("0x{BALANCE_OF_SELECTOR}{padded}");

    let payload = serde_json::json!({
        "jsonrpc": "2.0",
        "method":  "eth_call",
        "params": [
            { "to": USDC_CONTRACT, "data": data },
            "latest"
        ],
        "id": 1
    });

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| AppError::Other(format!("build HTTP client: {e}")))?;

    let resp = client
        .post(rpc_url)
        .json(&payload)
        .send()
        .await
        .map_err(|e| AppError::Other(format!("Polygon RPC 請求失敗: {e}")))?;

    let json: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| AppError::Other(format!("Polygon RPC JSON 解析失敗: {e}")))?;

    let hex = json["result"]
        .as_str()
        .ok_or_else(|| AppError::Other("Polygon RPC 回應缺少 result 欄位".into()))?;

    // 解析十六進位 uint256 → USDC（6 decimals）
    let hex_clean = hex.trim_start_matches("0x");
    let raw = u128::from_str_radix(hex_clean, 16)
        .map_err(|e| AppError::Other(format!("餘額解析失敗: {e}")))?;

    let usdc = raw as f64 / 1_000_000.0;

    tracing::info!(
        "[Polygon] 錢包 {} USDC 餘額: {:.6}",
        &wallet_address[..6.min(wallet_address.len())],
        usdc
    );

    Ok(usdc)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn address_padding() {
        let addr = "0x2791Bca1f2de4661ED88A30C99A7a9449Aa84174";
        let clean = addr.trim_start_matches("0x").to_lowercase();
        let padded = format!("{:0>64}", clean);
        assert_eq!(padded.len(), 64);
        assert!(padded.ends_with(&clean));
    }

    #[test]
    fn hex_balance_decode() {
        // 100.5 USDC = 100_500_000 (6 decimals)
        let hex = "0x0000000000000000000000000000000000000000000000000000000005F5E100";
        // 0x5F5E100 = 100_000_000 = 100.0 USDC
        let raw =
            u128::from_str_radix(hex.trim_start_matches("0x"), 16).unwrap();
        let usdc = raw as f64 / 1_000_000.0;
        assert!((usdc - 100.0).abs() < 1e-6);
    }
}
