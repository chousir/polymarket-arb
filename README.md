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
深入檢視是否有以下問題，並提出建議，我確定沒問題後再修改

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

### 退出冷卻機制（exited_slugs）

某個 slug 因任何原因（STOP_LOSS、FORECAST_SHIFT、TIME_DECAY_EXIT、TAKE_PROFIT）被平倉後，
該 slug 會進入 **24 小時冷卻期**，期間不允許重入，目的是防止連續虧損的 death-loop。

| 冷卻期行為 | 說明 |
|---|---|
| 冷卻中 | scan_for_entries 跳過該 slug，記錄 `[Weather] 冷卻中（24h），跳過重入` |
| 冷卻結束後 | 自動解除封鎖，若市場仍在且有 edge 則正常進場 |
| 進程重啟 | 冷卻記錄清空（`HashMap<String, Instant>` 存於記憶體） |

> 技術實作：`exited_slugs: HashMap<String, Instant>`，插入時記錄退出時間點，
> 檢查時以 `elapsed() < SLUG_COOLDOWN`（24h）判斷是否仍在冷卻。

### Weather 策略關鍵參數

| 參數 | 說明 |
|---|---|
| `profit_target` | Trailing TP 啟動門檻（bid_best 需超過 entry + profit_target 才開始追蹤） |
| `min_ensemble_members` | Ensemble 策略最低有效成員數。Open-Meteo GFS Ensemble 標準 30 員；部分 null 後低於此值視為資料不完整，不進場（預設 10） |
| `stop_loss_delta` | 距入場價的止損距離。lead_days ≥ 1 時自動 ×2 |
| `min_sl_ticks` | STOP_LOSS 需連續觸發幾個監控 tick 才實際平倉（預設 2）。過濾薄流動性市場單一 tick 噪音，防止假止損 |
| `forecast_shift_threshold` | 預測 p_yes 偏移此幅度即強制平倉（trailing-best）。lead_days ≥ 1 時自動 ×2 |
| `min_model_confidence` | Extreme / Ensemble TempRange BUY_YES 最低信心門檻 |
| `min_temprange_p_yes` | 確定性模型 TempRange BUY_YES 門檻（1°C 帶上限 ≈ 0.26） |
| `min_model_confidence_temprange` | TempRange BUY_NO 最低信心門檻（1-p_yes 需達此值） |
| `forecast_temp_bias_celsius` | 模型溫度預測的加法修正量（預設 0.0）。ECMWF/GFS 系統性低估時設正值（例 +2.0）；只影響 CDF 型模型，Ensemble 直接計票不受影響 |
| `consensus_max_divergence` | Consensus 策略三模型最大分歧（超過則不進場） |
| `weather_min_lead_days` / `weather_max_lead_days` | 允許交易的預測天數範圍 |

### 已知問題與修復記錄

#### P0-A：TIME_DECAY_EXIT 誤取代結算（2026-04-26 修復）

**根因**：`settlement.rs` 的輪詢窗口僅 3 分鐘。Polymarket 天氣市場走 UMA oracle，
從「市場關閉」到「API 報告 resolved」通常需要數小時，導致結算被遺漏，
所有到期持倉以市場中間價（而非 $0/$1 結算價）記帳為 TIME_DECAY_EXIT。

**修復**：在 `weather_executor.rs` 的 TIME_DECAY_EXIT 分支中，
先呼叫 `gamma::fetch_token_settlement_price(slug, token_id)`；
若取得結算價（1.0 或 0.0），直接以 **SETTLEMENT** 記帳（正確 PnL），
不再以市場中間價離場。取不到才降級為 TIME_DECAY_EXIT。

#### P0-B：ECMWF/Ensemble 溫度系統性低估（2026-04 春季暖化異常）

**根因**：2026 年春季歐洲出現異常高溫（倫敦 4 月創 80 年紀錄），
ECMWF/GFS 中期預報（2-4 天前）以氣候常態校準，系統性低估實際溫度 2-4°C，
導致策略大量買入「溫度不會那麼高」的 NO，實際卻全部踩雷。

**修復**：`WeatherDecisionConfig` 新增 `forecast_temp_bias_celsius`（預設 0.0）；
在 `evaluate_temp_range`、`evaluate_extreme`、`probability_yes` 中，
CDF 計算時 `mu = forecast.max_temp_c + forecast_temp_bias_celsius`。
**建議值**：異常暖春設 `+2.0`，ECMWF 冬季偏暖時設 `-1.0`。
Ensemble 直接計票不受此參數影響。

#### P0-E：TAKE_PROFIT 記帳虧損（2026-04-26 修復）

**根因**：trailing floor 是「觸發條件」而非「成交保證」。

```
entry = 0.78,  profit_target ≈ 0.051
bid_best 達到 ~0.85（TP 啟動）
trailing_floor = max(0.85 - 0.051, 0.831) = 0.831

15 分鐘後 bid 跳空至 0.80：
  0.80 < 0.831 → TP 觸發，exit_price = book.best_bid = 0.80
  PnL = (0.80-0.78)×427 - 2×fee = +8.5 - 15.4 = -6.8 USDC
```

根本問題：15 分鐘輪詢間隔讓市場有足夠時間跳空，繞過 trailing floor 保護。
TP 只能保證「bid 低於 floor 時點火」，無法保證「填單價 ≥ floor」。

**修復**：在 TAKE_PROFIT 點火後，加防線：
`if exit_price < entry_price + 2 × taker_fee_pct → 暫不平倉，交給下一個 tick 的 STOP_LOSS`。
避免以虧損出場卻標記為 TAKE_PROFIT，同時避免提前鎖定本可回升的損失。

#### P1：STOP_LOSS 被薄流動性市場單 tick 噪音誤觸

**根因**：氣象市場流動性低，一筆 $200-500 訂單即可使 best_ask 短暫跳動 20-30¢，
觸發 STOP_LOSS，但下一個 tick 價格即恢復，例如 London Apr26 18°C 被錯殺，
實際溫度最終並非 18°C（NO 本應結算 $1.00）。

**修復**：`WeatherPosition` 新增 `consecutive_sl_ticks: u32`；
每個 tick 若 `best_ask ≤ effective_sl` 才遞增計數，
需達 `min_sl_ticks`（預設 2，約 30 分鐘間隔 × 2 = 1 小時確認）才實際平倉。
價格回升則重置計數為 0。

---

## Weather Customized 策略說明

`WeatherCustomized` 是在 `WeatherStrategy`（Ensemble 模型）基礎上加裝五道額外進場閘門的穩健版天氣策略。
它使用與 `weather_ensemble_prob` 完全相同的 Ensemble 計票邏輯決定方向，但在入場前必須通過所有閘門。

### 交易流程

```
每隔 loop_interval_sec（預設 900 秒）
│
├─ 1. 拉取所有活躍天氣市場（fetch_weather_markets）
│
├─ 2. 更新每個市場的報價歷史（price_history: HashMap<slug, VecDeque<PriceTick>>）
│     - 保留最近 customized_lookback_ticks 筆（預設 4 tick ≈ 60 分鐘）
│     - YES ask / NO ask 分別記錄
│
├─ 3. 監控現有持倉（若有）
│     ├─ SETTLEMENT：市場已到期時先查 UMA oracle 結算價
│     │    - 取得 1.0 / 0.0 → 以真實結算 PnL 記帳，移除持倉
│     │    - 未結算 → 降級 TIME_DECAY_EXIT（以市場中間價估算）
│     ├─ TAKE_PROFIT：bid ≥ trailing_floor，且 exit > entry + 2×fee
│     ├─ STOP_LOSS：ask ≤ effective_sl 連續 min_sl_ticks tick 才執行
│     └─ FORECAST_SHIFT：Ensemble 預測轉向，p_yes 偏移超過門檻
│
└─ 4. 掃描新進場（scan_for_entries）
      ├─ [基礎閘門] Ensemble 信心 / edge / 價差 / 深度
      │    （與 WeatherStrategy 相同）
      │
      ├─ [閘門 1] LOOKBACK_SLOPE
      │    - 取最近 customized_lookback_ticks 筆歷史，計算線性斜率（上升/tick）
      │    - 斜率 ≥ customized_min_slope（預設 0.0 = 不下跌即可）才通過
      │    - 歷史不足 customized_min_history_ticks（預設 3）時略過此閘門
      │
      ├─ [閘門 2] MIN_ENTRY_PRICE
      │    - token ask ≥ customized_min_entry_price（預設 0.30）
      │    - 過濾接近 0 的廉價 token（流動性差、風險/報酬失衡）
      │
      ├─ [閘門 3] MAX_ENTRY_PRICE
      │    - token ask ≤ customized_max_entry_price（預設 0.85）
      │    - 避免入場高價 token（費用侵蝕全部獲利空間）
      │
      ├─ [閘門 4] ENSEMBLE_SPREAD
      │    - 所有 Ensemble 成員的溫度標準差 ≤ customized_max_ensemble_spread_celsius（預設 4.0 °C）
      │    - 模型分歧過大代表預報不確定性高 → 跳過
      │
      └─ [閘門 5] CITY_EXPOSURE
           - 同一城市的現有持倉數 < customized_max_positions_per_city（預設 3）
           - 避免單一城市集中風險
```

### weather_customized 專屬參數

| 參數 | 預設值 | 說明 |
|---|---|---|
| `customized_lookback_ticks` | 4 | 斜率計算使用的歷史 tick 數（每 tick ≈ loop_interval_sec 秒） |
| `customized_min_slope` | 0.0 | 每 tick 最低上升幅度。設 `0.0` 只排除下跌；設 `0.002` 要求每 tick 漲 0.2¢ |
| `customized_min_history_ticks` | 3 | 歷史 tick 不足時跳過斜率閘門，允許在市場剛開盤時入場 |
| `customized_min_entry_price` | 0.30 | 進場最低 token ask（USDC）。低於此值跳過（廉價 token 流動性不足） |
| `customized_max_entry_price` | 0.85 | 進場最高 token ask（USDC）。高於此值跳過（費用無法被抵銷） |
| `customized_max_ensemble_spread_celsius` | 4.0 | Ensemble 成員最大溫度標準差（°C）。超過代表模型高度不確定 |
| `customized_max_positions_per_city` | 3 | 同一城市最多持倉數 |

繼承自 `WeatherStrategy` 的參數（全部適用）：`forecast_temp_bias_celsius`、`min_sl_ticks`、`stop_loss_delta`、`profit_target`、`forecast_shift_threshold`、`min_model_confidence`、`min_ensemble_members` 等。

### 範例設定（`config/settings.toml`）

```toml
[[strategies]]
id                   = "weather_custom_london"
strategy_type        = "WeatherCustomized"
enabled              = true
capital_allocation_pct = 0.15
trade_size_pct       = 0.05
max_drawdown_pct     = 0.20

# Ensemble / decision thresholds
min_model_confidence = 0.65
min_ensemble_members = 10
forecast_temp_bias_celsius = 2.0  # 2026 春季倫敦暖化修正

# Stop-loss & TP
stop_loss_delta      = 0.12
profit_target        = 0.10
min_sl_ticks         = 2
forecast_shift_threshold = 0.15

# Customized gates
customized_lookback_ticks             = 4
customized_min_slope                  = 0.001   # 每 tick 至少漲 0.1¢
customized_min_history_ticks          = 3
customized_min_entry_price            = 0.30
customized_max_entry_price            = 0.80
customized_max_ensemble_spread_celsius = 3.0
customized_max_positions_per_city     = 2
```

### 與 weather_ensemble_prob 的差異

| 項目 | weather_ensemble_prob | weather_customized |
|---|---|---|
| 預測模型 | Ensemble（計票） | Ensemble（計票，相同邏輯） |
| 斜率確認 | 無 | 必須通過 Lookback Slope 閘門 |
| 進場價格範圍 | 無限制 | 30%–85%（可調） |
| 模型分歧過濾 | 無 | Ensemble 標準差 ≤ 4°C（可調） |
| 城市集中度 | 無 | 同城市最多 3 倉（可調） |
| 目的 | 最大化交易機會 | 只在高信心、低雜訊時入場 |

---

## 安全規則

- `.env` 含私鑰，**不可 git add、不可截圖、不可分享**
- live 模式必須明確帶 `--confirm-live`
- 所有新增下單路徑必須有 dry_run 攔截（見 `CLAUDE.md`）
