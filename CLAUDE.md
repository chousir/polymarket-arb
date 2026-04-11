# Polymarket 自動交易系統

## 核心策略
BTC 15 分鐘 Up/Down 市場，Dump-Hedge 兩腿策略。
開盤後監控急跌（Leg 1 入場），等待 Up+Down 總和 < 0.93 時對沖（Leg 2），
鎖定利潤或限制虧損。

## 語言分工
- Rust：訂單簿監控、訊號計算、訂單執行、DRY_RUN 攔截
- Python：回測分析、參數優化、儀表板前端

## 交易模式（TRADING_MODE）
- dry_run：讀真實資料，完整決策流程，不提交訂單，寫入 dry_run_trades 表
- live：真實下單（啟動時必須加 --confirm-live flag）

## 關鍵規則：DRY_RUN 攔截
任何新增的訂單執行路徑，都必須在提交前檢查：
```rust
if config.dry_run {
    tracing::info!("[DRY_RUN] 模擬提交: {:?}", order);
    self.db.write_dry_run_trade(order).await?;
    return Ok(OrderResult::simulated(order));
}
```
無例外。

## CLOB API 初始設定
首次使用前執行：`python tools/setup_credentials.py`
衍生的 Key/Secret/Passphrase 存入 .env 後，Rust 引擎直接讀取。
Rust 引擎啟動時不重新衍生憑證。
詳細說明見 README.md。

## API 端點
- Gamma API: https://gamma-api.polymarket.com（免認證）
- CLOB API: https://clob.polymarket.com（EIP-712 簽名）
- WebSocket: wss://ws-subscriptions-clob.polymarket.com
- Binance WS: wss://stream.binance.com:9443/ws/btcusdt@ticker
- 伺服器位置: AWS eu-west-2（倫敦），VPS 選愛爾蘭 / 倫敦

## 速率限制
- POST /order: 60/s 均速，3500/10s 爆發
- GET /books: 5/10s（嚴格！優先用 WebSocket）
- WebSocket: 最多 5 條並發連線 per IP

## 費用（2026年）
- Taker fee 加密市場: 最高 1.80%（50% 機率市場）
- Maker（GTC 限價單）: 0%
- 計算 hedge_threshold_sum 必須考慮雙邊 taker fee

## Rust 常用指令
- 建置: `cargo build --release`
- Dry run: `cargo run --release -- --mode dry_run`
- Live（謹慎）: `cargo run --release -- --mode live --confirm-live`
- 測試: `cargo test`

## Python 常用指令
- 初始憑證（只做一次）: `python tools/setup_credentials.py`
- 回測: `python -m python-analytics.src.backtest.engine --days 7`
- 參數優化: `python -m python-analytics.src.strategy.optimizer`
- 儀表板: `uvicorn python-analytics.src.dashboard.app:app --reload --port 8080`

## 環境變數（.env）
POLYGON_PRIVATE_KEY=    # 私鑰，絕不可 cat、git add、截圖、分享
POLYGON_PUBLIC_KEY=
CLOB_API_KEY=           # 由 setup_credentials.py 填入
CLOB_API_SECRET=
CLOB_API_PASSPHRASE=
DB_PATH=./data/market_snapshots.db
TRADING_MODE=dry_run
TELEGRAM_BOT_TOKEN=

## IPC 機制（預留）
Phase 1-2: ipc/sqlite_poller.rs（polling 500ms，已啟用）
Phase 3: ipc/unix_socket.rs（< 1ms，本機單機，切換條件：Python 訊號需 5s 內到達）
Phase 4+: ipc/redis_pubsub.rs（VPS 多進程部署）

## 代碼規範
- Rust: tokio 非同步，thiserror 錯誤類型，主路徑禁止 unwrap()
- Python: type hints 必填，pydantic v2，httpx 非同步
- 日誌: Rust 用 tracing，Python 用 structlog（JSON 格式）

## 安全規則
- .env 不可 cat、不可 git add（hooks 阻擋）
- 私鑰只從環境變數讀取
- live 模式必須加 --confirm-live flag
- 所有執行路徑必須有 DRY_RUN 檢查

## 開發階段
目前：Phase 1（核心引擎）
進入 live 的最低條件：dry_run ≥ 7 天、勝率 ≥ 60%、單輪最大虧損 < 20%
