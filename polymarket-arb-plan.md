# Polymarket 自動交易系統｜完整規劃書 v3.0

> 適用對象：Claude Code 初始化 + 開發執行  
> 語言選型：**Rust**（核心引擎）+ Python（分析 / 回測 / 儀表板）  
> 核心策略：BTC 15 分鐘 Up/Down 市場（Dump-Hedge 兩腿策略）  
> 更新日期：2026-04

---

## 一、語言選型：為什麼用 Rust？

| 項目 | Python | Rust |
|------|--------|------|
| 訂單簽名延遲 | ~1000ms | ~5ms |
| WebSocket 吞吐 | 單執行緒受限 | tokio 多工，無 GC 停頓 |
| 記憶體安全 | 執行期錯誤 | 編譯期保證 |
| 開發速度 | 快 | 較慢（但 Claude Code 補足） |
| 機會窗口掌握 | 常常來不及 | sub-100ms 可達成 |

核心掃描器 + 執行引擎用 Rust；回測分析、策略研究、儀表板用 Python。  
Claude Code 可在 40 分鐘內生成可運行的 Rust bot 骨架，開發成本已大幅降低。

**Rust 生態**：
- `polymarket-client-sdk`：官方 Rust SDK（核心引擎使用）
- `polyfill-rs`：零分配熱路徑，SIMD JSON 解析，效能提升約 21%

**Python 官方 SDK**（初始設定工具使用）：
- `py-clob-client`：官方 Python SDK，用於一次性衍生 API 憑證

---

## 二、核心策略：BTC 15 分鐘 Dump-Hedge 兩腿策略

### 策略原理

參考來源：[86% ROI 回測報告](https://www.blocktempo.com/polymarket-trading-bot-86-percent-roi-btc-market/)  
參考案例：知名交易者 gabagool 的非同步兩腿倉位累積法

每個 BTC 15 分鐘 Up/Down 市場開盤後，市場情緒的劇烈波動往往集中在前 2 分鐘。策略邏輯：

```
開盤後監控（預設前 2 分鐘）
  → 若任一方（Up 或 Down）價格在短時間內急跌（>= dump_threshold_pct）
    → Leg 1：買入急跌的那一方（以低價入場）
  → 等待市場穩定後
    → 若 Up_price + Down_price 總和 <= hedge_threshold_sum（例如 $0.93）
      → Leg 2：買入對面方，完成對沖（鎖定利潤或限制虧損）
```

### 關鍵參數（可調）

| 參數 | 預設值 | 說明 |
|------|--------|------|
| `monitor_window_sec` | 120 | 開盤後監控時間（秒） |
| `dump_threshold_pct` | 0.15 | 觸發 Leg 1 的價格跌幅 % |
| `hedge_threshold_sum` | 0.93 | 觸發 Leg 2 的 Up+Down 總和上限 |
| `min_token_price` | 0.05 | 最低進場價格 |
| `max_token_price` | 0.95 | 最高進場價格（利潤太薄） |
| `bet_size_usdc` | 10.0 | 每腿下注金額（USDC，從小開始） |
| `max_position_usdc` | 200.0 | 單市場最大曝險 |
| `hedge_wait_limit_sec` | 180 | Leg 1 後等待 Leg 2 的最長時間 |
| `abort_before_close_sec` | 30 | 結算前強制結束等待 |

### 市場時間特性

BTC 15 分鐘市場 slug 由時間戳決定性計算，不需要 API 搜索：

```rust
let now = unix_timestamp();
let window_ts = now - (now % 900);  // 900 秒 = 15 分鐘
let slug = format!("btc-updown-15m-{}", window_ts);
let close_time = window_ts + 900;
```

### 衍生策略（Phase 3 穩定後才評估，每次只評估一個）

**衍生 A：ETH / SOL 同結構策略**  
相同邏輯，不同幣種需要獨立回測最佳參數，禁止直接套用 BTC 參數。

**衍生 B：5 分鐘市場**  
窗口更短（`now % 300`），機會更多但噪訊更高，所有參數需重新校正。

**衍生 C：非對稱兩腿（gabagool 法）**  
Leg 1 和 Leg 2 在不同輪次的市場執行，分散時間風險，需追蹤跨輪次部位。

---

## 三、核心技術設計

### 3.1 DRY_RUN 模擬模式

模擬模式的核心原則：**讀取真實資料、執行完整決策流程、只在最後提交訂單前阻擋**。

```rust
pub async fn submit_order(
    &self,
    order: &OrderIntent,
    config: &BotConfig,
) -> Result<OrderResult> {
    // ✅ 計算訊號（真實 Polymarket + Binance 資料）
    // ✅ 建構訂單（真實 token_id、真實價格）
    // ✅ 簽名訂單（真實 EIP-712 簽名流程）
    // ✅ 記錄到 SQLite dry_run_trades 表

    if config.dry_run {                              // ← 在此攔截
        tracing::info!(
            "[DRY_RUN] 模擬提交: {:?} {} USDC @ {}",
            order.side, order.size_usdc, order.price
        );
        self.db.write_dry_run_trade(order).await?;
        return Ok(OrderResult::simulated(order));    // ← 不呼叫 CLOB API
    }

    // 真實路徑（LIVE 模式）
    self.clob_client.post_order(order).await
}
```

**三種模式**（由 `.env` 的 `TRADING_MODE` 控制）：

| 模式 | 行為 | 啟動方式 |
|------|------|---------|
| `dry_run` | 讀真實資料，完整決策，記錄日誌，**不下單** | `cargo run -- --mode dry_run` |
| `live` | 真實下單（需額外確認） | `cargo run -- --mode live --confirm-live` |

> 任何新增的執行路徑都必須包含 DRY_RUN 檢查，無例外。

**DRY_RUN 記錄**（SQLite `dry_run_trades` 表）：

```sql
CREATE TABLE dry_run_trades (
    id              INTEGER PRIMARY KEY,
    ts              DATETIME DEFAULT CURRENT_TIMESTAMP,
    market_slug     TEXT NOT NULL,
    leg             INTEGER NOT NULL,   -- 1 或 2
    side            TEXT NOT NULL,      -- 'UP' 或 'DOWN'
    price           REAL NOT NULL,
    size_usdc       REAL NOT NULL,
    signal_dump_pct REAL,               -- 觸發時的跌幅
    hedge_sum       REAL,               -- 觸發時的 Up+Down 總和
    would_profit    REAL                -- 估計獲利（假設全額成交）
);
```

### 3.2 單邊成交敞口風險（Legging Risk）

Dump-Hedge 兩腿非原子執行，若 Leg 1 成交但 Leg 2 條件未達，形成單邊曝險。

```rust
pub enum LegState {
    WaitingLeg1,
    Leg1Filled { price: f64, ts: Instant },
    HedgingLeg2,
    Completed { pnl_usdc: f64 },
    Aborted { reason: String },
}

pub struct TradeCycle {
    pub state: LegState,
    pub abort_at: Instant,  // 市場結算前 30 秒強制結束等待
}
```

**風控規則**：
- 市場結算前 30 秒：若 Leg 2 未觸發，持有 Leg 1 等結算（接受單腿風險）
- Leg 1 入場後超過 `hedge_wait_limit_sec` 未觸發 Leg 2：放棄對沖，持有 Leg 1 等結算
- 禁止同一輪市場、同一方向重複入場

### 3.3 訂單簿深度（Depth）與 VWAP 評估

進場前需評估成交量對價格的衝擊，不能只看最佳買賣價。

```rust
pub struct DepthAnalysis {
    pub vwap: f64,            // 成交量加權平均進場價
    pub max_fillable: f64,    // 目標價格內最大可成交量（USDC）
    pub price_impact_pct: f64,
}

fn compute_vwap(book_side: &[Level], target_usdc: f64) -> DepthAnalysis {
    let mut remaining = target_usdc;
    let mut total_cost = 0.0;
    for level in book_side.iter().sorted_by(|a, b| a.price.partial_cmp(&b.price)) {
        let fill = remaining.min(level.size * level.price);
        total_cost += fill;
        remaining -= fill;
        if remaining <= 0.0 { break; }
    }
    DepthAnalysis {
        vwap: total_cost / (target_usdc - remaining),
        max_fillable: target_usdc - remaining,
        price_impact_pct: (total_cost / (target_usdc - remaining) - book_side[0].price) 
                          / book_side[0].price,
    }
}
// VWAP 超過 max_token_price（0.95）則放棄此輪機會
```

### 3.4 API 速率限制精細管理

**最新限制（2026年3月，AWS eu-west-2 倫敦）**：

| 端點 | 爆發限制 | 持續均速 |
|------|---------|---------|
| `POST /order` | 3,500 / 10s | 60/s |
| `DELETE /order` | 3,000 / 10s | 50/s |
| `POST /orders`（批次，最多 15 筆） | 1,000 / 10s | — |
| `GET /books` | 50 / 10s | 嚴格，優先用 WebSocket |
| WebSocket 並發連線 | 5 條 / IP | — |

**策略**：
- 市場資料優先用 WebSocket，不消耗 REST 配額
- 15 分鐘市場每輪最多 2 筆訂單，遠低於速率上限
- Token Bucket 保護，429 回應時指數退避（1s → 2s → 4s + jitter）

### 3.5 Rust ↔ Python IPC 預留架構

Phase 1-2 透過 SQLite 共享資料足夠（輪詢延遲 ~500ms 可接受）。  
Phase 3 若引入 Python 策略訊號回饋，需切換至低延遲 IPC。

```
ipc/
├── README.md          ← 三種方案的延遲、依賴、適用場景說明
├── sqlite_poller.rs   ← 目前使用（Phase 1-2，polling 間隔 500ms）
├── unix_socket.rs     ← Phase 3 預留（< 1ms，本機單機部署）
└── redis_pubsub.rs    ← Phase 4+ 預留（VPS 多進程部署，需 Redis）
```

**切換時機**：Phase 3 的 Python 策略訊號需在 5 秒內傳遞給 Rust 時，切換為 Unix Socket。

---

## 四、CLOB API 初始設定（獨立工具，先執行一次）

CLOB API 認證需從私鑰衍生 Key/Secret/Passphrase。**只需衍生一次，存入 .env 後不再需要呼叫**。詳細說明見 README.md 的「初始設定」章節。

```python
# tools/setup_credentials.py
from py_clob_client.client import ClobClient

HOST = "https://clob.polymarket.com"
PRIVATE_KEY = input("輸入你的 Polygon 私鑰（本程式不儲存）: ").strip()

client = ClobClient(HOST, key=PRIVATE_KEY, chain_id=137)
creds = client.create_or_derive_api_creds()

print("\n請將以下三行貼入 .env 對應欄位：")
print(f"CLOB_API_KEY={creds.api_key}")
print(f"CLOB_API_SECRET={creds.api_secret}")
print(f"CLOB_API_PASSPHRASE={creds.api_passphrase}")
print("\n⚠️  私鑰請勿填入此工具以外的任何地方")
```

---

## 五、完整目錄結構

```
polymarket-arb/
├── CLAUDE.md
├── README.md                        ← 含「初始設定」CLOB 憑證說明
├── Dockerfile
├── docker-compose.yml
├── docker-entrypoint.sh
├── .dockerignore
├── Makefile
├── .claude/
│   ├── settings.json                ← hooks 設定（含 .env 保護）
│   └── commands/
│       ├── dryrun.md                ← /dryrun  查看 dry_run 統計
│       ├── backtest.md              ← /backtest 執行回測
│       └── positions.md             ← /positions 查詢當前部位
│
├── tools/
│   └── setup_credentials.py        ← 一次性 CLOB 憑證衍生工具
│
├── rust-engine/
│   ├── Cargo.toml
│   └── src/
│       ├── main.rs
│       ├── config.rs                ← BotConfig + dry_run flag
│       ├── error.rs                 ← AppError（thiserror）
│       ├── api/
│       │   ├── mod.rs
│       │   ├── clob.rs              ← CLOB REST client
│       │   ├── gamma.rs             ← 市場資訊（slug 計算）
│       │   └── binance.rs           ← BTC 現貨 WebSocket
│       ├── ws/
│       │   ├── mod.rs
│       │   └── market_feed.rs       ← Polymarket 訂單簿 WebSocket
│       ├── strategy/
│       │   ├── mod.rs
│       │   ├── dump_hedge.rs        ← 核心策略邏輯
│       │   └── signal.rs            ← dump_pct、VWAP 計算
│       ├── execution/
│       │   ├── mod.rs
│       │   ├── executor.rs          ← submit_order（DRY_RUN 攔截）
│       │   └── cycle.rs             ← TradeCycle 狀態機
│       ├── risk/
│       │   ├── mod.rs
│       │   ├── limits.rs            ← 倉位、每日曝險限制
│       │   └── circuit_breaker.rs   ← 緊急停止
│       ├── rate_limit/
│       │   ├── mod.rs
│       │   └── token_bucket.rs      ← Token Bucket + 指數退避
│       ├── ipc/
│       │   ├── mod.rs
│       │   ├── README.md            ← 三種 IPC 方案說明
│       │   ├── sqlite_poller.rs     ← Phase 1-2（目前使用）
│       │   ├── unix_socket.rs       ← Phase 3 預留
│       │   └── redis_pubsub.rs      ← Phase 4+ 預留
│       └── db/
│           ├── mod.rs
│           └── writer.rs            ← 非同步寫入 SQLite
│
├── python-analytics/
│   ├── requirements.txt
│   └── src/
│       ├── data/
│       │   ├── __init__.py
│       │   └── storage.py           ← SQLite 讀取 ORM
│       ├── backtest/
│       │   ├── __init__.py
│       │   ├── engine.py            ← 從 dry_run_trades 回測
│       │   └── metrics.py           ← Sharpe, WinRate, MaxDD
│       ├── strategy/
│       │   ├── __init__.py
│       │   └── optimizer.py         ← 參數網格搜索（27 組）
│       └── dashboard/
│           ├── __init__.py
│           ├── app.py               ← FastAPI 後端
│           └── frontend/            ← React 儀表板
│
├── config/
│   ├── settings.toml
│   └── .env.example
├── data/
│   └── .gitkeep
└── .gitignore
```

---

## 六、SQLite Schema

```sql
CREATE TABLE markets (
    slug        TEXT PRIMARY KEY,
    window_ts   INTEGER NOT NULL,
    close_ts    INTEGER NOT NULL,
    open_up     REAL,
    open_down   REAL,
    created_at  DATETIME DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE dry_run_trades (
    id              INTEGER PRIMARY KEY,
    ts              DATETIME DEFAULT CURRENT_TIMESTAMP,
    market_slug     TEXT NOT NULL,
    leg             INTEGER NOT NULL,
    side            TEXT NOT NULL,
    price           REAL NOT NULL,
    size_usdc       REAL NOT NULL,
    signal_dump_pct REAL,
    hedge_sum       REAL,
    would_profit    REAL
);

CREATE TABLE live_trades (
    id          INTEGER PRIMARY KEY,
    ts          DATETIME DEFAULT CURRENT_TIMESTAMP,
    market_slug TEXT NOT NULL,
    leg         INTEGER NOT NULL,
    side        TEXT NOT NULL,
    order_id    TEXT,
    price       REAL NOT NULL,
    size_usdc   REAL NOT NULL,
    filled_usdc REAL,
    status      TEXT NOT NULL,  -- PENDING / FILLED / CANCELLED / FAILED
    tx_hash     TEXT
);

CREATE TABLE cycle_results (
    id              INTEGER PRIMARY KEY,
    market_slug     TEXT NOT NULL,
    mode            TEXT NOT NULL,   -- dry_run / live
    leg1_side       TEXT,
    leg1_price      REAL,
    leg2_price      REAL,
    resolved_winner TEXT,            -- UP / DOWN / NULL（未觸發）
    pnl_usdc        REAL,
    created_at      DATETIME DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE strategy_versions (
    id               INTEGER PRIMARY KEY,
    version          TEXT NOT NULL,
    params_json      TEXT NOT NULL,
    backtest_sharpe  REAL,
    backtest_winrate REAL,
    deployed_at      DATETIME,
    retired_at       DATETIME
);
```

---

## 七、CLAUDE.md 內容

```markdown
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
```

---

## 八、Hooks 設定（`.claude/settings.json`）

```json
{
  "hooks": {
    "PreToolUse": [
      {
        "matcher": "Bash",
        "hooks": [
          {
            "type": "command",
            "command": "bash -c 'cmd=$(echo \"$CLAUDE_TOOL_INPUT\" | python3 -c \"import sys,json; d=json.load(sys.stdin); print(d.get(\\\"command\\\",\\\"\\\"))\" 2>/dev/null || echo \"\"); echo \"$cmd\" | grep -qE \"(rm -rf /|DROP TABLE|cat (\\.env|\\./.env|/.*\\.env)|git add (\\.env|.*\\.env))\" && echo \"BLOCKED: Dangerous command detected\" && exit 1 || exit 0'"
          }
        ]
      }
    ],
    "PostToolUse": [
      {
        "matcher": "Write|Edit|Create",
        "hooks": [
          {
            "type": "command",
            "command": "bash -c 'file=$(echo \"$CLAUDE_TOOL_INPUT\" | python3 -c \"import sys,json; print(json.load(sys.stdin).get(\\\"path\\\",\\\"\\\"))\" 2>/dev/null || echo \"\"); [ -n \"$file\" ] && [ -f \"$file\" ] && grep -qE \"(PRIVATE_KEY|SECRET|PASSPHRASE)\\s*=\\s*[a-fA-F0-9]{20,}\" \"$file\" 2>/dev/null && echo \"BLOCKED: Hardcoded secret detected in $file\" && exit 1 || exit 0'"
          }
        ]
      }
    ]
  },
  "permissions": {
    "allow": [
      "Bash(cargo build*)",
      "Bash(cargo run*)",
      "Bash(cargo test*)",
      "Bash(pytest*)",
      "Bash(uvicorn*)",
      "Bash(python -m*)",
      "Bash(python tools/*)",
      "Bash(pip install*)",
      "Bash(git add src/*)",
      "Bash(git add rust-engine/*)",
      "Bash(git add python-analytics/*)",
      "Bash(git add config/settings.toml)",
      "Bash(git add README.md)",
      "Bash(git add CLAUDE.md)",
      "Bash(git commit*)",
      "Bash(git status)",
      "Bash(git log*)",
      "Bash(ls*)",
      "Bash(tree*)"
    ],
    "deny": [
      "Bash(cat .env*)",
      "Bash(cat */.env*)",
      "Bash(cat *.env)",
      "Bash(git add .env*)",
      "Bash(git add */.env*)",
      "Bash(rm -rf /)"
    ]
  }
}
```

---

## 九、README.md 初始設定章節

````markdown
## 初始設定（第一次必做，之後不需重複）

### Step 1：安裝系統依賴

```bash
# Rust
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh && source ~/.cargo/env

# Python（建議 Python 3.11+）
pip install py-clob-client python-dotenv
```

### Step 1.5：建立本地測試用 Python venv（建議）

```bash
python3 -m venv .venv
source .venv/bin/activate
python -m pip install --upgrade pip
python -m pip install -r python-analytics/requirements.txt
```

完成後可用以下指令確認環境：

```bash
python -V
python -m python-analytics.src.backtest.engine --help
```

離開虛擬環境：

```bash
deactivate
```

### Step 2：建立 .env

```bash
cp config/.env.example .env
# 用編輯器填入：
# POLYGON_PRIVATE_KEY=你的私鑰
# POLYGON_PUBLIC_KEY=你的錢包地址
```

### Step 3：一次性衍生 CLOB API 憑證

```bash
python tools/setup_credentials.py
```

程式印出三行，貼入 .env 對應欄位：
- `CLOB_API_KEY`
- `CLOB_API_SECRET`
- `CLOB_API_PASSPHRASE`

完成後 Rust 引擎直接從 .env 讀取，不需再執行此工具。

### Step 4：建置並啟動（Dry Run 模式）

```bash
cd rust-engine
cargo build --release
cargo run --release -- --mode dry_run
```

Dry run 模式讀取真實市場資料、執行完整策略決策，但不提交任何訂單。
所有「會執行」的操作記錄到 `data/market_snapshots.db` 的 `dry_run_trades` 表。

### Step 5：啟動儀表板

```bash
uvicorn python-analytics.src.dashboard.app:app --reload --port 8080
# 打開 http://localhost:8080
```

### 進入 Live 模式的前置條件

- Dry run 資料 ≥ 7 天
- 回測勝率 ≥ 60%
- 單輪最大虧損 < 20%

確認後執行：
```bash
cargo run --release -- --mode live --confirm-live
```

## Docker 與 Makefile 快速使用

專案根目錄已提供 `Dockerfile`、`docker-compose.yml`、`Makefile`，可直接用以下流程本地啟動。

```bash
# 1) 建置映像（make 預設目標就是 build-image）
make

# 2) 啟動 dry_run（Rust 引擎 + 儀表板）
make run-dry-run

# 3) 查看日誌
make logs SERVICE=app

# 4) 停止服務
make down
```

常用指令：
- `make help`：列出全部指令
- `make run-dashboard`：只啟動儀表板
- `make run-live CONFIRM_LIVE=1`：啟動 live 模式（高風險，務必先滿足 7 天 dry_run 門檻）

若不使用 Makefile，也可直接用 Docker Compose：

```bash
docker-compose build
docker-compose --profile all up -d app
docker-compose logs -f app
docker-compose down
```
````

---

## 十、分階段計畫

### Phase 1：核心引擎（第 1-2 週）

目標：Rust 引擎在 DRY_RUN 模式下完整執行 Dump-Hedge 策略，所有「會執行」的訂單都記錄到 SQLite。

包含：
- tools/setup_credentials.py（CLOB 憑證衍生工具）
- Binance BTC 即時 WebSocket
- Polymarket 訂單簿 WebSocket（Up/Down 價格）
- 市場 slug 決定性計算（不需 API 搜索）
- DRY_RUN 攔截層（executor.rs 的 submit_order）
- TradeCycle 狀態機（WaitingLeg1 → Leg1Filled → HedgingLeg2 → Completed）
- SQLite 寫入（dry_run_trades、markets、cycle_results 表）
- Token Bucket 速率限制器
- IPC 目錄骨架預留（三個 stub 文件 + README）

不包含：儀表板、回測、Live 交易

---

### Phase 2：回測 + 儀表板（第 2-3 週）

目標：7 天 dry_run 資料後，能執行回測評估策略，有視覺化儀表板。

包含：
- Python 回測引擎（從 dry_run_trades 計算 Sharpe、WinRate、MaxDD）
- 參數網格搜索（9 × 3 = 27 組配置）
- 策略版本控制（strategy_versions 表）
- FastAPI 後端
- React 儀表板（今日統計 / 每輪結果 / 勝率折線圖）

---

### Phase 3：Live 交易（7 天 dry_run 後，達門檻）

進入門檻：dry_run ≥ 7 天、勝率 ≥ 60%、單輪最大虧損 < 20%

包含：
- Live 執行器（CLOB API 真實下單）
- Polygon 合約授權腳本（tools/approve_contracts.py）
- Telegram 告警（每輪結果 + 異常 + 停止通知）
- Circuit Breaker（每日虧損上限自動停止）
- Live 儀表板（真實 P&L、餘額、鏈上確認）

衍生策略評估（Phase 3 穩定後才開始，每次只評估一個）：
- 衍生 A：ETH/SOL 同結構
- 衍生 B：5 分鐘市場
- 衍生 C：非對稱兩腿

---

## 十一、硬體需求

### 開發期（本機）

| 項目 | 最低 | 建議 |
|------|------|------|
| CPU | 4 核 | 8 核（Rust 編譯快） |
| RAM | 8 GB | 16 GB |
| 儲存 | SSD 50 GB | SSD 200 GB |
| OS | Linux / macOS | Ubuntu 22.04 LTS |

### 生產期 VPS

| 項目 | 規格 | 原因 |
|------|------|------|
| 位置 | 愛爾蘭 / 倫敦 | 靠近 Polymarket 伺服器（AWS eu-west-2） |
| CPU | 4 vCPU | Rust 多工 + WebSocket |
| RAM | 8 GB | SQLite + Python 並行 |
| 儲存 | 80 GB SSD | |
| 推薦 | Hetzner（愛爾蘭）或 AWS eu-west-1 | |
| 月費 | $20–$40 USD | |

---

## 十二、未來擴充：川普提及詞策略（Mention Market）可執行規格

定位：此章節屬於未來擴充，不納入當前 Phase 1-3 的主線交付。

策略在做什麼：
- 核心是交易「是否會提到特定詞彙」的事件市場（YES/NO）。
- 交易方向以「NO 為主、YES 為輔」：
  - NO 主策略：當市場對低機率詞過度樂觀（YES 被買貴）時，買入 NO。
  - YES 輔策略：僅在高把握詞且價格折價明顯時，才做小倉位 YES。
- 在開講後預期快速 repricing 階段（而非最終結算）分批止盈，降低持有到結算的事件風險。

範圍限定：初版只交易「川普演講」提及詞市場，不擴其他講者。

### 12.1 參數表（Phase 1：dry_run）

| 類別 | 參數 | 預設值 | 說明 |
|------|------|--------|------|
| 市場過濾 | `speaker` | `trump` | 只允許講者為川普的市場 |
| 市場過濾 | `market_category` | `mention` | 只允許提及詞市場 |
| 方向設定 | `direction_mode` | `no_first` | 預設先找 NO 錯價，YES 僅輔助 |
| NO 詞彙池 | `no_bias_keywords` | `"crypto", "bitcoin", "doge", "quantitative easing", "macroeconomics", "apologize", "sorry", "my mistake"` | 偏向不會提及的詞（主策略） |
| 詞彙白名單 | `mention_keywords` | `"rigged election", "worst president", "terrible", "great"` | 只交易白名單詞彙 |
| 入場（NO） | `entry_no_min_price` | `0.60` | NO bid 低於此值通常不做（邊際不足） |
| 出場（NO） | `take_profit_no_price` | `0.78` | NO bid 到價分批止盈 |
| 入場 | `entry_yes_max_price` | `0.82` | YES ask 高於此值不入場 |
| 出場 | `take_profit_yes_price` | `0.90` | YES bid 到價分批止盈 |
| 風控 | `stop_loss_delta` | `0.05` | 低於入場價 0.05 觸發止損 |
| 風控 | `max_hold_sec` | `60` | 開講後最長持有秒數 |
| 風控 | `max_delay_sec` | `900` | 演講延遲超過此值直接平倉 |
| 流動性 | `max_spread` | `0.03` | spread 超過不交易 |
| 流動性 | `min_depth_multiple` | `3.0` | 可成交深度至少為目標部位 3 倍 |
| 成本模型 | `taker_fee_bps` | `180` | 單邊 taker fee（bps） |
| 成本模型 | `slippage_buffer_bps` | `50` | 雙邊滑價緩衝 |
| 成本模型 | `execution_risk_bps` | `50` | 非原子成交風險緩衝 |
| 決策門檻 | `min_net_edge_bps` | `100` | 預估淨邊際至少 1.00% 才下單 |
| 倉位 | `bet_size_usdc` | `10.0` | 每筆投入 USDC |
| 倉位 | `max_open_positions` | `3` | 同時最多持倉市場數 |
| 倉位 | `max_daily_loss_usdc` | `50.0` | 每日虧損上限（達上限停機） |

計算公式（bps 轉小數後，NO / YES 雙路徑）：

$$
edge_{no} = (1 - p_{no,entry}) - takerFee - slippageBuffer - executionRiskBuffer
$$

$$
edge_{yes} = (1 - p_{yes,entry}) - takerFee - slippageBuffer - executionRiskBuffer
$$

執行條件：

$$
edge \ge minNetEdge
$$

方向優先權：
- `direction_mode = no_first` 時優先檢查 `edge_no`。
- 僅當 NO 不成立且 YES 成立時，才允許 YES 輔助倉位。

### 12.2 dry_run 日誌欄位規格

用途：保留可重放、可回測、可審計的事件資料，避免只看最終 PnL。

建議新增資料表：`mention_dry_run_trades`

```sql
CREATE TABLE mention_dry_run_trades (
  id                           INTEGER PRIMARY KEY,
  ts                           DATETIME DEFAULT CURRENT_TIMESTAMP,
  strategy_id                  TEXT NOT NULL,
  event_id                     TEXT NOT NULL,
  market_slug                  TEXT NOT NULL,
  speaker                      TEXT NOT NULL,
  keyword                      TEXT NOT NULL,
  side                         TEXT NOT NULL,        -- YES / NO
  action                       TEXT NOT NULL,        -- ENTRY / TAKE_PROFIT / STOP_LOSS / TIME_EXIT / CANCEL
  price                        REAL NOT NULL,
  size_usdc                    REAL NOT NULL,
  spread_at_decision           REAL,
  depth_usdc_at_decision       REAL,
  entry_yes_price              REAL,
  exit_yes_price               REAL,
  take_profit_yes_price        REAL,
  stop_loss_delta              REAL,
  hold_sec                     INTEGER,
  schedule_delay_sec           INTEGER,
  taker_fee_bps                INTEGER,
  slippage_buffer_bps          INTEGER,
  execution_risk_bps           INTEGER,
  expected_net_edge_bps        REAL,
  realized_pnl_usdc            REAL,
  reason_code                  TEXT,                 -- EDGE_OK / EDGE_TOO_LOW / SPREAD_TOO_WIDE / DEPTH_TOO_THIN / TIME_EXIT
  note                         TEXT
);
```

最小必填欄位（MVP）：
- `ts`
- `strategy_id`
- `market_slug`
- `keyword`
- `action`
- `price`
- `size_usdc`
- `expected_net_edge_bps`
- `reason_code`

### 12.3 live 前檢核清單（Go / No-Go）

資料門檻：
- [ ] dry_run 天數 >= 14 天
- [ ] 有效樣本（ENTRY 筆數）>= 100
- [ ] 成本後平均 `realized_pnl_usdc` > 0
- [ ] 成本後勝率 >= 58%
- [ ] 最大回撤 <= 預設上限（建議 20%）

執行品質：
- [ ] `SPREAD_TOO_WIDE` 拒單率在可接受範圍（建議 <= 30%）
- [ ] `DEPTH_TOO_THIN` 拒單率在可接受範圍（建議 <= 30%）
- [ ] 兩腿或入出場成交成功率 >= 95%
- [ ] 延遲事件（`schedule_delay_sec > max_delay_sec`）有完整退出紀錄

風控與安全：
- [ ] `max_daily_loss_usdc` 觸發後確實停機
- [ ] 連續虧損保護機制（降倉或停機）已驗證
- [ ] `.env` 金鑰保護與 hook 攔截正常
- [ ] live 模式需 `--confirm-live` 才能啟動

運維與監控：
- [ ] Telegram 告警可即時接收（啟動、錯誤、停機）
- [ ] 異常中斷後可在 60 秒內恢復
- [ ] 日誌可追溯到單筆決策（含 `reason_code`）

上線方式（建議）：
- [ ] 第 1 週 live：`bet_size_usdc = 5~10`，僅 1 個 keyword 白名單
- [ ] 第 2 週起，僅在連續 7 天成本後為正時增加 keyword 或加倉

---

---

# 區塊一：直接貼給 Claude Code 的初始化指令

```
你是我的 Polymarket 自動交易系統開發助手。
核心策略：BTC 15 分鐘 Up/Down 市場，Dump-Hedge 兩腿策略。
架構：Rust 核心引擎 + Python 分析層，透過 SQLite 共享資料。

## 任務：建立完整 Phase 1 專案骨架

### 1. 建立目錄結構
mkdir -p polymarket-arb/{.claude/commands,tools,rust-engine/src/{api,ws,strategy,execution,risk,rate_limit,ipc,db},python-analytics/src/{data,backtest,strategy,dashboard/frontend},config,data,docs}
cd polymarket-arb

### 2. rust-engine/Cargo.toml
[package]
name = "polymarket-engine"
version = "0.1.0"
edition = "2021"

[dependencies]
tokio = { version = "1", features = ["full"] }
reqwest = { version = "0.12", features = ["json", "rustls-tls"] }
tokio-tungstenite = { version = "0.24", features = ["rustls-tls-webpki-roots"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
rusqlite = { version = "0.31", features = ["bundled"] }
thiserror = "1"
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
dotenvy = "0.15"
hmac = "0.12"
sha2 = "0.10"
hex = "0.4"
uuid = { version = "1", features = ["v4"] }
chrono = { version = "0.4", features = ["serde"] }
once_cell = "1"
config = "0.14"

### 3. python-analytics/requirements.txt
fastapi==0.111.0
uvicorn[standard]==0.30.0
httpx==0.27.0
pydantic==2.7.0
sqlalchemy==2.0.30
aiosqlite==0.20.0
structlog==24.2.0
python-dotenv==1.0.1
py-clob-client==0.34.5

### 4. config/.env.example
POLYGON_PRIVATE_KEY=your_polygon_private_key_here
POLYGON_PUBLIC_KEY=your_polygon_wallet_address_here
CLOB_API_KEY=
CLOB_API_SECRET=
CLOB_API_PASSPHRASE=
DB_PATH=./data/market_snapshots.db
TRADING_MODE=dry_run
TELEGRAM_BOT_TOKEN=
MONITOR_WINDOW_SEC=120
DUMP_THRESHOLD_PCT=0.15
HEDGE_THRESHOLD_SUM=0.93
BET_SIZE_USDC=10.0
MAX_POSITION_USDC=200.0
HEDGE_WAIT_LIMIT_SEC=180
ABORT_BEFORE_CLOSE_SEC=30
LOG_LEVEL=info

### 5. config/settings.toml
[trading]
mode = "dry_run"
monitor_window_sec = 120
dump_threshold_pct = 0.15
hedge_threshold_sum = 0.93
min_token_price = 0.05
max_token_price = 0.95
bet_size_usdc = 10.0
max_position_usdc = 200.0
hedge_wait_limit_sec = 180
abort_before_close_sec = 30

[api]
gamma_base = "https://gamma-api.polymarket.com"
clob_base = "https://clob.polymarket.com"
ws_market_url = "wss://ws-subscriptions-clob.polymarket.com/ws/market"
binance_ws_url = "wss://stream.binance.com:9443/ws/btcusdt@ticker"
chain_id = 137

[rate_limits]
orders_per_second = 60
books_per_10s = 5
max_ws_connections = 5
retry_base_ms = 1000
retry_max_attempts = 3

[risk]
max_daily_loss_usdc = 50.0
circuit_breaker_loss_pct = 0.20

### 6. .gitignore
.env
*.env.local
data/*.db
data/*.db-shm
data/*.db-wal
target/
__pycache__/
*.pyc
.venv/
node_modules/
dist/
*.log

### 執行順序
1. 建立所有目錄（使用 mkdir -p）
2. 建立 rust-engine/Cargo.toml（內容如上）
3. 建立 python-analytics/requirements.txt
4. 建立 config/.env.example 和 config/settings.toml
5. 建立 .gitignore
6. 建立 CLAUDE.md（使用規劃書第七節的完整內容，逐字複製）
7. 建立 .claude/settings.json（使用規劃書第八節的完整 JSON）
8. 建立 tools/setup_credentials.py（使用規劃書第四節的 Python 代碼）
9. 建立 README.md（使用規劃書第九節的模板內容）
10. 建立容器與自動化文件：Dockerfile、docker-entrypoint.sh、docker-compose.yml、.dockerignore、Makefile
11. 建立 rust-engine/src/ipc/ 下的文件：
    - mod.rs：pub mod sqlite_poller; // pub mod unix_socket;  // Phase 3 // pub mod redis_pubsub; // Phase 4+
    - sqlite_poller.rs：// Phase 1-2：SQLite polling，輪詢間隔 500ms（含 pub fn poll() stub）
    - unix_socket.rs：// Phase 3 預留：Unix Domain Socket IPC，延遲 < 1ms
    - redis_pubsub.rs：// Phase 4+ 預留：Redis Pub/Sub，適合 VPS 多進程部署
    - README.md：說明三種方案的延遲、依賴、切換條件
12. 建立所有其他 src/ 下的 mod.rs stub（每個 mod 目錄一個，含對子模組的 pub mod 聲明）
13. 執行：cd rust-engine && cargo build 2>&1 | tail -5（確認編譯通過）
14. 執行：git init && git add -A && git commit -m "chore: initial project scaffold"
15. 列出完整目錄樹（tree -L 4 或 find . -not -path './.git/*' -not -path './target/*'）

完成後告訴我：
- 成功建立的文件數量
- cargo build 是否通過
- Phase 1 第一個實作步驟（Step 1-1）的建議
```

---

---

# 區塊二：Claude Code 啟動序列

每個 Step 完成並確認後，再貼下一條。

```
=== Phase 1 — 核心引擎 ===

[Step 1-1] CLOB 憑證工具
確認 tools/setup_credentials.py 已建立且內容正確：
使用 py-clob-client 的 create_or_derive_api_creds()，
讀取使用者輸入私鑰（不存入任何文件），
印出 CLOB_API_KEY / CLOB_API_SECRET / CLOB_API_PASSPHRASE 三行。
執行 python tools/setup_credentials.py 確認腳本無語法錯誤（Ctrl+C 中斷即可，不需要真實私鑰）。

[Step 1-2] Config + Error 類型
實作 rust-engine/src/config.rs：
- BotConfig struct，從 dotenvy 讀取 .env + config crate 讀取 settings.toml
- TradingMode enum：DryRun, Live
- is_dry_run() → bool
- is_live_confirmed(args: &[String]) → bool（需要 --confirm-live 命令列參數）
- 所有策略參數欄位（dump_threshold_pct, hedge_threshold_sum 等）

實作 rust-engine/src/error.rs：
- AppError enum（thiserror），包含 ApiError, WsError, DbError, ConfigError, NotImplemented
執行 cargo build 確認通過。

[Step 1-3] Binance 即時 BTC 價格
實作 rust-engine/src/api/binance.rs：
- BinanceTicker struct { best_bid: f64, best_ask: f64, last_price: f64, ts: u64 }
- connect_binance_ws() → mpsc::Receiver<BinanceTicker>
  使用 tokio-tungstenite 連線 wss://stream.binance.com:9443/ws/btcusdt@ticker
  解析 24hrTicker JSON 訊息
  自動重連：斷線等 5 秒後重試，最多 10 次
在 main.rs 中啟動並印出第一條 BTC 報價。
執行 cargo run --release -- --mode dry_run 確認收到 BTC 價格日誌。

[Step 1-4] Gamma API + Slug 計算
實作 rust-engine/src/api/gamma.rs：
- current_slug() → String（決定性計算，now - now % 900，不需要 API）
- next_open_ts() → u64
- MarketInfo struct { condition_id, up_token_id, down_token_id, close_ts }
- fetch_market(slug: &str) → Result<MarketInfo>（帶 60 秒本地快取）
印出當前市場 slug 和兩個 token ID 確認正確。

[Step 1-5] Polymarket 訂單簿 WebSocket
實作 rust-engine/src/ws/market_feed.rs：
- PriceSnapshot struct { up_best_ask: f64, down_best_ask: f64, sum: f64, ts: u64 }
- MarketFeed：訂閱 up_token_id 和 down_token_id
  使用 wss://ws-subscriptions-clob.polymarket.com/ws/market
  解析 price_change 訊息，維護本地最佳報價
  透過 mpsc channel 發送 PriceSnapshot
  自動重連（5 秒間隔，最多 10 次）
整合到 main.rs，讓 Binance feed 和 Polymarket feed 並行運行。

[Step 1-6] DRY_RUN 攔截層 + SQLite
實作 rust-engine/src/db/writer.rs：
- 在啟動時建立 SQLite（若不存在）並執行所有 CREATE TABLE
- Schema：markets, dry_run_trades, live_trades, cycle_results, strategy_versions（見規劃書第六節）
- async fn write_dry_run_trade(trade: &DryRunTrade) → Result<()>
- async fn write_cycle_result(result: &CycleResult) → Result<()>

實作 rust-engine/src/execution/executor.rs：
- OrderIntent struct（token_id, side, price, size_usdc）
- OrderResult enum { Simulated { order_id: String }, Filled { order_id: String, tx: String } }
- submit_order(intent: &OrderIntent, config: &BotConfig, db: &DbWriter) → Result<OrderResult>
  DRY_RUN 路徑：tracing::info!("[DRY_RUN] ...") + write_dry_run_trade + 返回 Simulated
  Live 路徑：返回 Err(AppError::NotImplemented)（Phase 3 實作）

寫單元測試：模擬 DRY_RUN 提交一筆，確認 SQLite 有記錄。

[Step 1-7] 策略 + 狀態機
實作 rust-engine/src/strategy/signal.rs：
- compute_dump_pct(open_price: f64, current_price: f64) → f64
- compute_vwap(book_levels: &[Level], target_usdc: f64) → DepthAnalysis
- is_hedge_condition(snapshot: &PriceSnapshot, threshold: f64) → bool

實作 rust-engine/src/execution/cycle.rs：
- LegState enum（見規劃書 3.2 節）
- TradeCycle struct { state: LegState, open_price: f64, abort_at: Instant }
- on_price_update(&mut self, snapshot: &PriceSnapshot, config: &BotConfig, executor: &Executor) → Result<()>
  依當前狀態決定動作（記錄開盤價 → 偵測 dump → Leg 1 → 偵測 hedge → Leg 2 → 完成）
  到達 abort_at 時強制轉為 Aborted 或 Completed（記錄 cycle_result）

實作 rust-engine/src/strategy/dump_hedge.rs：
- DumpHedgeStrategy struct（持有 config 引用）
- run_market_cycle(info: &MarketInfo, price_rx: mpsc::Receiver<PriceSnapshot>) → Result<CycleResult>

[Step 1-8] 主循環整合 + 速率限制
實作 rust-engine/src/rate_limit/token_bucket.rs：
- TokenBucket { capacity: u32, refill_rate: f64, tokens: f64, last_refill: Instant }
- async fn acquire(&mut self) → ()（等待直到有 token）

整合 main.rs 主循環：
1. 讀取 .env + config
2. 初始化 DB writer（建立 schema）
3. 啟動 Binance WebSocket
4. 計算當前市場 slug → fetch_market → 訂閱訂單簿
5. 建立 TradeCycle → 等待市場結束 → 記錄 cycle_result
6. 計算下一個市場 → 重複步驟 4-6
每 30 秒印出統計：已完成循環數、DRY_RUN 記錄數

執行 cargo run --release -- --mode dry_run，
等待跨越一個完整 15 分鐘市場（或部分市場），確認：
- 無 panic / crash
- dry_run_trades 表有記錄（若訊號觸發）
- cycle_results 表有記錄（每輪市場結束時）
- 日誌顯示狀態機轉換

=== Phase 2 — 回測 + 儀表板 ===

[Step 2-1] Python 回測引擎
實作 python-analytics/src/backtest/engine.py：
- 從 SQLite dry_run_trades 和 cycle_results 讀取資料
- 計算每輪市場 PnL（基於 would_profit 欄位）
- BacktestReport dataclass：
  total_cycles, triggered_cycles, win_rate
  total_pnl_usdc, max_drawdown_pct, sharpe_ratio
  avg_leg1_price, avg_leg2_price, avg_profit_per_win
- 命令列：python -m python-analytics.src.backtest.engine --days 3
若資料 < 3 天，使用現有資料並標注樣本量。

[Step 2-2] 參數優化
實作 python-analytics/src/strategy/optimizer.py：
- dump_threshold 從 0.05 到 0.25，間距 0.025（9 個值）
- bet_size：10, 25, 50 USDC（3 個值）
- 共 27 組，對 dry_run_trades 重新計算 WinRate 和 PnL
- 輸出排序後的配置表（WinRate 由高到低）
- 命令列：python -m python-analytics.src.strategy.optimizer

[Step 2-3] FastAPI + React 儀表板
實作 python-analytics/src/dashboard/app.py：
- GET /api/stats：今日統計（循環數、勝率、估計獲利）
- GET /api/cycles：每輪市場結果（最近 50 筆）
- GET /api/dry-runs：dry_run_trades 最近 100 筆
- WebSocket /ws/live：每 5 秒廣播最新 stats

React 前端（Vite + TypeScript + Recharts）：
- 統計卡片（今日循環數 / 觸發率 / 估計 PnL）
- 每輪市場列表（slug / 觸發情況 / 估計結果）
- 勝率隨時間折線圖（按天）
啟動並確認 http://localhost:8080 可訪問。

=== Phase 3 — Live 交易（達門檻後） ===

[Step 3-1] 確認門檻（必須全部通過才繼續）
執行以下查詢，全部通過再繼續：
1. SELECT COUNT(DISTINCT DATE(ts)) FROM dry_run_trades >= 7
2. 執行回測：WinRate >= 60%
3. 執行回測：最大單輪虧損 < 20%
4. cargo test 全部通過
回報每項結果，若任何條件不滿足，停止。

[Step 3-2] Polygon 合約授權
建立 tools/approve_contracts.py（pip install web3==7.12.1）：
授權 USDC 和 CTF Exchange 合約（MAX_INT 授權）。
執行前顯示錢包地址和合約地址，等待使用者輸入 'yes' 確認再繼續。
合約地址：
- USDC: 0x2791Bca1f2de4661ED88A30C99A7a9449Aa84174
- CTF Exchange: 0x4bFb41d5B3570DeFd03C39a9A4D8dE6Bd8B8982E
- NegRisk CTF: 0xC5d563A36AE78145C45a50134d48A1215220f80a

[Step 3-3] Live 執行器
實作 executor.rs 的 live 路徑：
- EIP-712 訂單簽名（POLYGON_PRIVATE_KEY）
- POST /order 提交，帶 HMAC-SHA256 認證頭
- 輪詢訂單狀態直到 FILLED / CANCELLED（最多 30 秒）
- 寫入 live_trades 表
先以 $5 USDC 最小測試單驗證流程，確認 Polymarket 網站可看到訂單後再正式使用。

[Step 3-4] Circuit Breaker + Telegram 告警
實作 circuit_breaker.rs：
- 當日虧損 > max_daily_loss_usdc → 停止所有新循環 + Telegram 告警
- 連續 3 輪 Leg 1 觸發但 Leg 2 失敗 → 自動降低 bet_size 50%

實作 Telegram 告警：
- 每輪市場結束推送（Leg1 是否觸發 / Leg2 是否觸發 / 估計 PnL）
- 異常情況即時推送（API 斷線、Circuit Breaker 觸發）

=== Phase 4 — 未來擴充：川普提及詞策略（Mention Market） ===

[Step 4-1] ✅ 完成 — 市場辨識與白名單
目標：只抓「川普演講提及詞」市場，避免全市場掃描造成噪音與風險。
- ✅ 建立 market filter：speaker=trump、category=mention（mention_market.rs）
- ✅ 建立 keyword 白名單（YES 輔策略）："rigged election", "worst president", "terrible", "great"
- ✅ 建立 NO 主策略詞池："crypto", "bitcoin", "doge", "quantitative easing", "macroeconomics", "apologize", "sorry", "my mistake"
- ✅ 實作 market metadata cache（TTL 60s）避免重複抓取
- ✅ dry_run 驗證：log_verdicts() 列印被納入/排除市場與原因（含 SKIP）

[Step 4-2] ✅ 完成 — 決策引擎（成本後淨邊際）
目標：由價格門檻改為淨利門檻，避免只看毛利。
- ✅ 新增參數：direction_mode（NoFirst/YesFirst/NoOnly/YesOnly）、entry_no_min_price、take_profit_no_price、entry_yes_max_price、take_profit_yes_price、stop_loss_delta
- ✅ 新增成本參數：taker_fee_bps、slippage_buffer_bps、execution_risk_bps、min_net_edge_bps
- ✅ 實作 NO / YES 雙路徑邊際模型（mention_decision.rs）：
  - NO：edge_no = (1 - p_no_entry) - cost
  - YES：edge_yes = (1 - p_yes_entry) - cost
- ✅ 交易條件：edge >= min_net_edge 且 spread/depth 合格；`direction_mode=no_first` 時優先執行 NO
- ✅ Unit tests 涵蓋所有路徑（邊際計算、閾值、模式切換）

[Step 4-3] ✅ 完成 — dry_run 日誌與回測
目標：讓 mention 策略可追溯、可重放、可回測。
- ✅ 新增表：mention_dry_run_trades（db/writer.rs，欄位依第十二節 12.2）
- ✅ 寫入 reason_code：EDGE_TOO_LOW / SPREAD_TOO_WIDE / DEPTH_TOO_THIN / TIME_EXIT / TAKE_PROFIT / STOP_LOSS
- ✅ Python 回測新增 mention 報表（mention_report.py）：
  - 觸發率、成交率、平均持有秒數
  - 成本前後 PnL、最大回撤、連續虧損

[Step 4-4] ⬜ 未開始 — 小資金 live 灰度
目標：先驗證執行品質，再逐步擴張。
前置條件：
- [ ] 先通過第十二節 12.3 全部檢核
- [ ] mention dry_run 累積 ≥ 7 天，勝率 ≥ 60%，單輪最大虧損 < 20%

灰度計畫：
- 第 1 週：只啟用 mention_no_gradual_w1（keyword=crypto，bet_size=5%，資金分配 5%）
- 第 2 週：連續 7 天成本後為正才啟用 mention_no_standard 或 mention_yes_auxiliary
- 進階（≥ 14 天正收益）：才考慮啟用 mention_no_first_combined

風控熔斷：
- 任一觸發立即退回 dry_run：日虧損超過 20 USDC / 延遲超過 2000ms
- auto_rollback_on_risk = true（見 config/settings.toml [mention_live_gradual]）

settings.toml 對應參數（見下方 Mention Market 系列）：
- mention_no_gradual_w1   ← Week 1 灰度（預設 enabled=false）
- mention_no_standard     ← Week 2+ NO 主策略（enabled=false）
- mention_yes_auxiliary   ← Week 2+ YES 輔策略（enabled=false）
- mention_no_first_combined ← 進階雙向（enabled=false）

---

---

# 區塊三：你前期需要準備的清單

### 第一步：帳號與錢包（必須，免費）

- [ ] 安裝 **MetaMask** 瀏覽器擴充，建立錢包並安全備份助記詞（離線保存）
- [ ] MetaMask 加入 Polygon 網路（Chain ID: 137, RPC: https://polygon-rpc.com）
- [ ] 到 **Polymarket.com** 用 MetaMask 連線並註冊
- [ ] 進入 Polymarket 帳號設定 → Private Key → 匯出私鑰（給 CLOB API 簽名用）
- [ ] 申請 **Alchemy 免費帳號**（https://alchemy.com）→ 建立 Polygon 主網 App → 取得 RPC URL（備援用）

### 第二步：Telegram 告警（建議，免費）

- [ ] Telegram 搜尋 **@BotFather** → `/newbot` → 取得 Bot Token
- [ ] 建立私人 Telegram 頻道，把 Bot 加為管理員
- [ ] 用 **@userinfobot** 取得你的 Chat ID

### 第三步：資金（Dry Run 階段不需要）

- [ ] **Phase 1-2（Dry Run）**：只需要錢包地址，零資金
- [ ] **Phase 3 前**：準備 $200–500 USDC（Polygon 網路）+ $5–10 POL（gas）
- [ ] 資金路徑：CEX（Binance / OKX）→ 提幣 USDC → Polygon 網路 → 你的 MetaMask 錢包地址
- [ ] 進入 Polymarket 網站 → 存入 USDC → 確認餘額顯示正確

### 第四步：開發環境

- [ ] **Rust**：`curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`
- [ ] **Python 3.11+**：推薦用 pyenv
- [ ] **Python venv（本地測試）**：`python3 -m venv .venv && source .venv/bin/activate && python -m pip install -r python-analytics/requirements.txt`
- [ ] **Node.js 20+**：儀表板前端
- [ ] **Claude Code**：按官方文件安裝
- [ ] **Git**
- [ ] **SQLite Browser**（可選，方便查看資料：https://sqlitebrowser.org）

### 第五步：VPS（Phase 3 前才需要）

- [ ] **Hetzner**（https://hetzner.com）或 AWS eu-west-1
- [ ] 位置：**愛爾蘭（Dublin）** 或英國（London）
- [ ] 規格：4 vCPU / 8 GB RAM / 80 GB SSD / Ubuntu 22.04 LTS
- [ ] 設定 SSH 金鑰登入，關閉密碼登入
- [ ] 月費約 $20–$40 USD

### 第六步：Claude Code 設定

- [ ] Claude Code 已安裝並登入
- [ ] 建議啟用的 MCP：`filesystem`（文件讀寫）、`fetch`（HTTP 測試）
- [ ] 在空資料夾中開啟 Claude Code，準備好「區塊一」直接貼入

### 重要提醒

| 事項 | 說明 |
|------|------|
| 私鑰保管 | 只填入 .env，不截圖、不分享、不貼給任何 AI，hooks 會阻擋 git add .env |
| 從 Dry Run 開始 | 系統預設 dry_run，不會動真實資金，至少跑 7 天再考慮 Live |
| 從小額開始 | Phase 3 首週 bet_size = $5–10，確認正常再調高 |
| 台灣用戶 | 可直接使用 Polymarket 國際版，無地理限制 |
| 歷史資料限制 | BTC 15m 市場的歷史 CLOB 資料通常為空，需要靠 Dry Run 自己累積 |
