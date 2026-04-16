# Polymarket 自動交易系統

針對 Polymarket BTC 15 分鐘 Up/Down 市場的自動套利系統。Rust 引擎負責即時監控與訂單執行，Python 負責回測分析與儀表板。

---

## 策略說明

### Dump-Hedge（兩腿）
1. **Leg 1**：偵測 BTC 急跌（`dump_threshold_pct`），先入場 Down 單
2. **Leg 2**：等待 Up + Down 總價格 < `hedge_threshold_sum` 時對沖另一腿
3. 目的：鎖定價差、限制單腿暴露時間

### Pure-Arb（即時雙腿）
- 不等急跌訊號，直接監控 `hedge_threshold_sum`
- 出現折價時同時買入兩腿

### 交易模式
| 模式 | 說明 |
|------|------|
| `dry_run` | 讀真實行情，走完整決策流程，**不送出真實訂單**，結果寫入 `dry_run_trades` |
| `live` | 真實下單，啟動時必須帶 `--confirm-live` |

---

## 關鍵參數（`config/settings.toml`）

| 參數 | 說明 |
|------|------|
| `dump_threshold_pct` | BTC 急跌觸發門檻（%） |
| `hedge_threshold_sum` | Up+Down 總和折價門檻，低於此值才對沖 |
| `capital_allocation_pct` | 該策略使用總資金比例 |
| `trade_size_pct` | 每次下注比例（動態隨資金調整） |
| `max_drawdown_pct` | 達到此回撤上限時停止入場 |
| `enabled` | 是否啟用此策略 |

費用提醒：`hedge_threshold_sum` 必須扣除雙邊 taker fee（最高 1.8%）才能確保獲利空間。

---

## Docker 執行流程（推薦）

**環境需求**：Docker 20.10+、Docker Compose（v1 或 v2）。不需本機安裝 Rust / Python / Node.js。

```bash
# 1. 複製環境變數模版
make setup
#    → 建立 .env 與 data/ 目錄
#    → 編輯 .env，填入 POLYGON_PRIVATE_KEY 和 POLYGON_PUBLIC_KEY

# 2. 構建 Docker 映像（含 Rust 編譯，約 5~10 分鐘）
make docker

# 3. 衍生 CLOB API 憑證（一次性）
make credentials
#    → 輸出 CLOB_API_KEY / SECRET / PASSPHRASE，貼回 .env

# 4. 啟動（dry_run 模式）
make docker-run
#    → 引擎開始監控，儀表板：http://localhost:8080
```

常用指令：

```bash
make logs                    # 查看即時日誌
make logs SERVICE=engine     # 只看引擎日誌
make down                    # 停止並移除容器
make help                    # 查看所有可用指令
```

---

## 本機執行流程

**環境需求**：Rust 1.86+、Python 3.11+、Node.js 18+

```bash
# 安裝 Rust
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# 安裝 Python 套件
python3 -m venv .venv && source .venv/bin/activate
pip install -r python-analytics/requirements.txt

# 複製 .env
cp config/.env.example .env
# 填入 POLYGON_PRIVATE_KEY、POLYGON_PUBLIC_KEY，執行 credentials 後補上 CLOB_API_*

# 衍生 CLOB 憑證
python tools/setup_credentials.py
```

啟動引擎（dry_run）：

```bash
cd rust-engine
cargo build --release
cargo run --release -- --mode dry_run
```

啟動儀表板（另開終端機）：

```bash
cd python-analytics
uvicorn src.dashboard.app:app --reload --port 8080
```

進入 live（滿足門檻後才執行）：

```bash
cargo run --release -- --mode live --confirm-live
```

> 上 live 最低條件：dry_run ≥ 7 天、勝率 ≥ 60%、單輪最大虧損 < 20%

---

## 查看交易紀錄與日誌

### status 工具（推薦）

```bash
# Docker 環境
make status ARGS="--days 14 --trades 50"

# 本機環境
python3 tools/status.py --days 14 --trades 50
```

### SQLite 直查

```bash
# Docker 執行（DB 掛載在 ./data/）
sqlite3 data/market_snapshots.db \
  "SELECT ts, action, side, price, realized_pnl_usdc FROM dry_run_trades
   WHERE action != 'NO_TRADE' ORDER BY ts DESC LIMIT 20;"

# 本機執行（DB 在 rust-engine/data/）
sqlite3 rust-engine/data/market_snapshots.db \
  "SELECT ts, action, side, price, realized_pnl_usdc FROM dry_run_trades
   WHERE action != 'NO_TRADE' ORDER BY ts DESC LIMIT 20;"
```

### Docker 日誌

```bash
make logs                  # 所有服務
make logs SERVICE=engine   # 只看引擎
docker compose logs -f --tail=100 engine
```

### 回測

```bash
cd python-analytics
python -m src.backtest.engine --days 7 --db ../data/market_snapshots.db
python -m src.strategy.optimizer --db ../data/market_snapshots.db --days 7
```

---

## 專案結構

```
polymarket-arb/
├── rust-engine/        # 交易引擎（Rust）
├── python-analytics/   # 回測、優化、儀表板（Python）
├── config/             # settings.toml, .env.example
├── data/               # SQLite DB（Docker volume 掛載點）
├── tools/              # setup_credentials.py, status.py
├── docker-compose.yml
└── Makefile
```

---

---

## Weather 策略說明

### 市場類型

| 類型 | slug 特徵 | 問題範例 | p_yes 範圍 |
|---|---|---|---|
| **TempRange** | `-12c`（無修飾詞） | "最高溫是否為 12°C？" | 確定性模型 ≤ 0.26；Ensemble 可達 0-1 |
| **Extreme** | `-76forhigher`、`-55forbelow` | "最高溫是否達 76°F 以上？" | 0-1（門檻型機率） |
| **Precip** | `rain`、`snow` 等關鍵字 | "是否會下雨？" | 0-1 |

### 各市場類型 BUY_YES 進場閘門

| 市場類型 | 適用模型 | 閘門參數 | 預設值 |
|---|---|---|---|
| TempRange | GFS / NWS / ECMWF | `min_temprange_p_yes` | 0.26 |
| TempRange | **Ensemble** | `min_model_confidence` | 0.65（直接數成員，需 65% 一致） |
| Extreme | 所有模型 | `min_model_confidence` | 0.65 |
| BUY_NO（TempRange） | 所有模型 | `min_model_confidence_temprange` | 0.65 |

### 動態 Stop Loss 與 FORECAST_SHIFT（重要）

氣象市場在隔夜期間（預報每 6-12h 更新、流動性薄弱）存在大幅正常波動，
固定參數容易在方向正確時被暫時噪聲掃出。因此 **lead_days ≥ 1** 時自動擴大緩衝：

| 觸發類型 | lead_days = 0（當日） | lead_days ≥ 1（隔日以上） |
|---|---|---|
| **STOP_LOSS** | `stop_loss_delta` | `stop_loss_delta × 2` |
| **FORECAST_SHIFT** | `forecast_shift_threshold` | `forecast_shift_threshold × 2` |

> 例：`stop_loss_delta = 0.12`、`forecast_shift_threshold = 0.15`，
> lead_days ≥ 1 時實際為 **SL=0.24、FS=0.30**

**Trailing Stop Loss**：止損線會隨 `bid_best`（最高水位）往上移動，防止浮盈全部回吐：

```
effective_sl = max(entry - sl_delta, bid_best - sl_delta)
```

| bid_best | sl_delta=0.12 | effective_sl | 說明 |
|----------|---------------|--------------|------|
| 0.45（剛入場）| — | 0.33 | 固定保底 |
| 0.55 | — | 0.43 | SL 上移至接近 entry |
| 0.57 | — | **0.45** | SL = entry，進入保本區 |
| 0.70 | — | 0.58 | 鎖定 13¢ 獲利 |

Trailing SL 與 Trailing TP 分工：
- **Trailing SL**：寬間距（sl_delta）全程兜底，bid_best 上升後不會再虧超過 sl_delta
- **Trailing TP**：精確鎖利，越接近 100¢ trailing gap 越緊（細節見下方）

**額外保護**：若模型預測持倉方向機率 < 2% 但市場仍定價 > 10¢，
視為 API 瞬間異常，跳過本輪 FORECAST_SHIFT（不更新 trailing-best 基準）。

### Trailing Take-Profit 機制

持倉進入獲利後，`bid_best` 持續追蹤最高 bid 水位，trailing floor = `bid_best - effective_gap`。
只有在 `bid_best ≥ entry + profit_target`（獲利已啟動）且 `bid < trailing_floor` 時才出場。

**自適應 trailing gap**：越接近 100¢ 結算，gap 自動縮小，防止接近尾聲時回吐大段獲利：

```
effective_gap = min(profit_target, (1 - bid_best) × 2), 最小值 2¢
```

| bid_best | profit_target=0.10 時的 effective_gap | trailing_floor |
|----------|--------------------------------------|----------------|
| 0.60     | 0.10                                 | 0.50           |
| 0.90     | 0.10                                 | 0.80           |
| 0.95     | 0.08                                 | 0.87           |
| 0.99     | 0.02                                 | 0.97           |

> 不設固定 hard cap（如 99¢）：接近結算時流動性薄弱，hard cap 容易滑價；
> 自適應 gap 在 99¢ 只需回落 2¢ 即出場，效果等同於 hard cap 但更靈活。

### Weather 策略關鍵參數

| 參數 | 說明 |
|---|---|
| `profit_target` | Trailing TP 啟動門檻（bid_best 需超過 entry + profit_target 才開始追蹤） |
| `min_ensemble_members` | Ensemble 策略最低有效成員數。Open-Meteo GFS Ensemble 標準 30 員；部分 null 後低於此值視為資料不完整，不進場（預設 10） |
| `stop_loss_delta` | 距入場價的止損距離。lead_days ≥ 1 時自動 ×2 |
| `forecast_shift_threshold` | 預測 p_yes 偏移此幅度即強制平倉（trailing-best）。lead_days ≥ 1 時自動 ×2 |
| `min_model_confidence` | Extreme / Ensemble TempRange BUY_YES 最低信心門檻 |
| `min_temprange_p_yes` | 確定性模型 TempRange BUY_YES 門檻（1°C 帶上限 ≈ 0.26） |
| `min_model_confidence_temprange` | TempRange BUY_NO 最低信心門檻（1-p_yes 需達此值） |
| `consensus_max_divergence` | Consensus 策略三模型最大分歧（超過則不進場） |
| `weather_min_lead_days` / `weather_max_lead_days` | 允許交易的預測天數範圍 |

---

## 安全規則

- `.env` 含私鑰，**不可 git add、不可截圖、不可分享**
- live 模式必須明確帶 `--confirm-live`
- 所有新增下單路徑必須有 dry_run 攔截（見 `CLAUDE.md`）
