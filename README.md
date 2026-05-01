# Polymarket 自動交易系統

科學氣象模型 vs 群眾直覺的套利系統。Rust 引擎負責即時氣象預報拉取、訂單簿監控與訂單執行，Python 負責回測分析與儀表板。

---

## 策略說明

### 交易模式
| 模式 | 說明 |
|------|------|
| `dry_run` | 讀真實行情，走完整決策流程，**不送出真實訂單**，結果寫入 `dry_run_trades` |
| `live` | 真實下單，啟動時必須帶 `--confirm-live` |

---

## 關鍵參數（`config/settings.toml`）

| 參數 | 說明 |
|------|------|
| `capital_allocation_pct` | 該策略使用總資金比例 |
| `trade_size_pct` | 每次下注比例（動態隨資金調整） |
| `max_drawdown_pct` | 達到此回撤上限時停止入場 |
| `min_net_edge_bps` | 最低入場邊際（扣除雙邊 taker fee 後） |
| `stop_loss_delta` | 止損距離（lead_days ≥ 1 時自動 ×2） |
| `forecast_shift_threshold` | 預測偏移強制退出門檻（lead_days ≥ 1 時自動 ×2） |
| `enabled` | 是否啟用此策略 |

費用提醒：`min_net_edge_bps` 必須扣除雙邊 taker fee（最高 1.8%）才能確保獲利空間。

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
| `consensus_max_divergence` | Consensus 策略 CDF 模型間最大分歧（GFS vs ECMWF，σ 校正後互比，超過則不進場） |
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

#### P1-A：相鄰溫度桶相關性風險（2026-04-27 改善）

**根因**：同一城市同一天的所有溫度桶只有一個會 YES 結算，其餘全部 NO。
當策略同時在相鄰兩個桶（如 20°C、21°C）各持 NO 部位時，
兩個賭注是強相關的——若實際溫度恰好落在其中一個桶，必定一輸一贏：

```
實際溫度 = 21°C：20°C NO 贏（+5.99），21°C NO 輸（-44.01），淨值 = -38.02
```

**改善**：新增**峰值桶保護閘門（Gate 6）**。
進場前對每個城市+日期組，找出 CLOB 流動性最高的市場（「峰值桶」），
禁止在該桶押 NO。流動性最高的桶 ≈ 市場資金最集中的桶 ≈ 群眾最確信的溫度，
逆此方向下 NO 的風險/報酬最差。其餘桶的 NO 仍可正常進場。

#### P1-B：逆高 YES 市場信心門檻不足（2026-04-27 改善）

**根因**：BuyNo 的模型信心門檻固定為 70%，不論市場 YES 定價高低。
當市場已將某桶 YES 定為 50%（代表大量資金押注此結果），
模型只需 75% 確信即可進場，未能充分考慮市場的信息量。

**改善**：新增**高 YES 動態信心閘門（Gate 7）**。
當 `yes_best_ask >= high_yes_ask_threshold`（預設 0.35），
BuyNo 所需的最低 (1-p_yes) 提升至 `high_yes_min_confidence_no`（預設 0.85）。

```
21°C YES=50% 案例：
  市場：YES ask = 0.50 ≥ 0.35（觸發高門檻）
  模型：p_yes = 0.25，(1-0.25) = 0.75 < 0.85 → 閘門阻擋，不進場
```

#### P1-C：Consensus 策略三模型 p_yes 不可直接比較（2026-04-28 修復）

**根因**：舊邏輯直接比較 GFS CDF 機率、ECMWF CDF 機率、Ensemble 直接計票機率，
以 `max - min > consensus_max_divergence` 決定是否阻擋。三者的計算機制根本不同：

- **GFS / ECMWF**：單點預測 + 固定 σ 表（lead=2-4天 σ=3.0/2.5°C）→ 正態 CDF
- **Ensemble**：30 個成員直接計票 → 落在 1°C 桶的比例

固定 σ 不隨天氣狀況調整，導致兩種系統性錯誤：

```
穩定天氣（ens_std=0.7°C）：Ensemble 計票 80%，CDF 固定 σ=3.0 算出 13% → 差 67% → 誤擋
混亂天氣（ens_std=3.5°C）：Ensemble 計票 7%，CDF 算出 13% → 差僅 6% → 誤放行
```

穩定天氣（預測最可靠）反而被阻擋；混亂天氣反而被放行——邏輯倒置。

**修復**：三步驟分層判斷，不再直接比較 p_yes 數值：

1. **均值一致性檢查**（`MODEL_MEAN_DIVERGENCE`）  
   `|μ_gfs - ens_mean| > 2.0°C` 或 `|μ_ecmwf - ens_mean| > 2.0°C` → 阻擋  
   原理：模型對「今天溫度大概幾度」的估計本身有根本分歧 → 真正不確定

2. **大氣混亂度檢查**（`HIGH_ATM_UNCERTAINTY`）  
   `ens_std > 3.5°C` → 阻擋  
   原理：Ensemble 30 個成員標準差 > 3.5°C = 大氣確實混亂不可預測

3. **σ 校正後的 CDF-to-CDF 比較**（`MODEL_DIVERGENCE`）  
   `σ_calibrated = max(ens_std, 1.2°C) × city_sigma_mult`  
   GFS 和 ECMWF 均改用此 σ 計算 p_yes，再互相比較  
   原理：兩個 CDF 值用同一 σ → 殘差只剩 μ 差異 → 才是真正可比的「預測分歧」

4. **Ensemble 方向驗證**（`ENS_DIRECTION_CONFLICT`）  
   CDF 平均 p_yes 與 Ensemble 計票 p_yes 落在 0.5 的不同側 → 阻擋  
   原理：方向相反代表兩種方法對「YES 是否比 NO 更可能」結論相反  
   不做數值比較（兩者單位不同），只做方向比對

```
穩定天氣修復後（ens_std=0.7°C → σ_cal=1.2°C）：
  CDF_gfs = CDF(μ=20.0, σ=1.2) for [19.5,20.5] ≈ 38%
  CDF_ecmwf = CDF(μ=20.1, σ=1.2) for [19.5,20.5] ≈ 36%
  divergence = 2% < 10% → 允許進場 ✓
  Ensemble 方向=BuyNo(計票100%)，CDF 方向=BuyNo(38%<50%) → 方向一致 ✓
```

**新增 API**：`WeatherForecast::ensemble_mean_std() -> Option<(f64, f64)>`、
`weather_decision::probability_yes_sigma(forecast, market, sigma, bias) -> Option<f64>`

#### P1：STOP_LOSS 被薄流動性市場單 tick 噪音誤觸

**根因**：氣象市場流動性低，一筆 $200-500 訂單即可使 best_ask 短暫跳動 20-30¢，
觸發 STOP_LOSS，但下一個 tick 價格即恢復，例如 London Apr26 18°C 被錯殺，
實際溫度最終並非 18°C（NO 本應結算 $1.00）。

**修復**：`WeatherPosition` 新增 `consecutive_sl_ticks: u32`；
每個 tick 若 `best_ask ≤ effective_sl` 才遞增計數，
需達 `min_sl_ticks`（預設 2，約 30 分鐘間隔 × 2 = 1 小時確認）才實際平倉。
價格回升則重置計數為 0。

---

## weather_custom_verify 詳細流程

`weather_custom_verify` 是 `weather_customized` 類型的驗證實例（灰度，5% 資金）。
每 15 分鐘掃描一次倫敦、NYC、東京、邁阿密、芝加哥的溫度市場，
使用 GFS Ensemble（30 成員直接計票）評估機率，通過 7 道閘門後才入場。

---

### 資料來源

| 資料 | API | 時機 |
|---|---|---|
| 活躍市場清單 | Polymarket Gamma API | 每次 loop 開頭 |
| YES / NO 訂單簿 | Polymarket CLOB API | 每個候選市場各拉一次 |
| GFS Ensemble 預報（30 成員） | Open-Meteo Ensemble API | 每城市一次，60 秒快取 |
| 結算價 | Polymarket Gamma API | 持倉到期後查詢，最多重試 3 次 |
| METAR 實況觀測 | aviationweather.gov | 目標日當天監控時取代 Ensemble |

---

### 每次 loop（每 15 分鐘）詳細步驟

#### 第一階段：前置檢查

1. **到期冷卻清理**：清除 `exited_slugs` 中超過 24 小時的記錄，解鎖可重入
2. **熔斷器檢查**：若本策略資金觸及 `max_drawdown_pct`（30%），跳過本輪掃描

---

#### 第二階段：監控現有持倉

對每個持倉依序檢查四個退出條件，**最先觸發的優先執行**：

**① TIME_DECAY_EXIT（到期強制退出）**

觸發條件：`Instant::now() >= abort_at`（距市場關閉時間 < `abort_before_close_sec`）

```
1. 查 UMA oracle 結算價（gamma::fetch_token_settlement_price）
   ├─ 取得 1.0 / 0.0 → SETTLEMENT，以真實 PnL 記帳
   └─ 未就緒 → 每 60 秒重試，最多 3 次
       ├─ 重試成功 → SETTLEMENT
       └─ 3 次失敗 → TIME_DECAY_EXIT，以 entry_price 估算 PnL（dry_run 模式）
                     live 模式：拉訂單簿最新 bid 離場
```

**② TAKE_PROFIT（Trailing 止盈）**

每 tick 更新最高水位 `bid_best = max(bid_best, current_bid)`：

```
effective_gap = min(profit_target, (1 - bid_best) × 2)，最小 2¢
trailing_floor = max(bid_best - effective_gap, entry + profit_target)

觸發條件：
  bid_best ≥ entry + profit_target（獲利已啟動）
  AND current_bid < trailing_floor（回落超過 gap）
  AND exit_price > entry + 2 × taker_fee_pct（防虧損出場）
```

| bid_best | profit_target=0.10 | effective_gap | trailing_floor |
|---|---|---|---|
| 0.60 | 0.10 | 0.10 | 0.50 |
| 0.90 | 0.10 | 0.10 | 0.80 |
| 0.95 | 0.08 | 0.08 | 0.87 |
| 0.99 | 0.02 | 0.02 | 0.97 |

**③ STOP_LOSS（Trailing 止損）**

```
current_lead_days = target_date - today

sl_delta = stop_loss_delta × 2   （lead_days ≥ 1，隔日以上，給更大緩衝）
         = stop_loss_delta        （lead_days = 0，當日市場）

effective_sl = max(entry - sl_delta, bid_best - sl_delta)

觸發條件：
  current_ask ≤ effective_sl（連續 min_sl_ticks=2 tick 確認）
```

`min_sl_ticks=2` 的目的：過濾薄流動性市場單筆大單造成的瞬間噪音，需兩個 15 分鐘 tick 持續低於止損線才出場。

**④ FORECAST_SHIFT（預測偏移強制退出）**

```
shift_threshold = forecast_shift_threshold × 2   （lead_days ≥ 1）
                = forecast_shift_threshold         （lead_days = 0）

預報來源：
  lead_days ≥ 1 → GFS Ensemble 重新計票
  lead_days = 0 → METAR 實況觀測（MetarShort，σ=2.5°C CDF）
                  失敗時回退 Ensemble

觸發條件：
  |new_p_yes - entry_p_yes| ≥ shift_threshold
  AND 方向已翻轉（持 NO 但新信號為 BuyYes 或 Hold）
  AND new_p_yes > 0.02（排除 API 異常導致的假信號）
```

**倫敦範例（目標日當天 lead_days=0）**：
上午 10 點入場 19°C NO（entry_p_yes=0.12），  
下午 3 點 METAR 顯示倫敦 EGLC 氣溫 21°C。  
MetarShort CDF（σ=2.5°C，μ=21°C）：P(temp=19°C) = Φ((19.5-21)/2.5) - Φ((18.5-21)/2.5) ≈ 0.27  
shift = |0.27 - 0.12| = 0.15 ≥ 0.15，方向翻轉 → **FORECAST_SHIFT 強制退出**

---

#### 第三階段：掃描新進場（scan_for_entries）

**前置過濾**（拉訂單簿之前，零 API 成本）：

```
✗ 已有該 slug 的持倉
✗ 該 slug 在 24 小時冷卻期內
✗ 城市不在 city_whitelist（NYC/London/Tokyo/Miami/Chicago）
✗ lead_days < 1 或 > 5（不做同日/超長期市場）
✗ 訂單簿深度 < weather_min_depth_usdc（$60）
✗ 距市場關閉 < effective_abort_sec
```

**峰值桶預計算**（整批拉完市場後，進入逐市場迴圈前）：

```
對每個城市+日期組：
  找出 liquidity_clob 最高的市場 → 記入 peak_no_slugs
  （此步為純記憶體操作，不拉 API）
```

**逐市場評估**（每個候選市場依序執行）：

```
[閘門 5] CITY_EXPOSURE
  同城市持倉數 ≥ 2 → 跳過整個城市（不拉任何訂單簿）

[閘門 4] ENSEMBLE_SPREAD（拉訂單簿之前）
  Ensemble 成員溫度 std_dev > 3.0°C → 跳過
  原理：spread 大 = 30 個成員預測值分散，模型本身不確定

  → 拉 YES 訂單簿 + NO 訂單簿（兩次 CLOB API call）

[基礎閘門 A] SPREAD_WIDE
  YES spread（ask-bid）> max_spread（0.05）→ Hold（流動性不足）

[基礎閘門 B] LOW_DEPTH
  YES 訂單簿深度 < 60 USDC → Hold

[更新報價歷史]
  price_history[slug].push_back(PriceTick { no_ask, yes_ask })
  保留最近 5 筆（lookback_ticks=4 + 1）

[基礎閘門 C] 信心 + Edge（weather_decision::evaluate）
  p_yes = Ensemble 成員中落在 [lo, hi] 溫度範圍的比例
          （需 ≥ min_ensemble_members=20 個有效成員）

  cost_frac = (taker_fee_bps=180 + slippage_buffer_bps=45) / 10000 = 0.0225

  edge_yes = p_yes - yes_ask - cost_frac
  edge_no  = (1 - p_yes) - no_ask - cost_frac

  優先 BuyYes：edge_yes ≥ 450bps 且 edge_yes ≥ edge_no
    → 再查：p_yes ≥ min_temprange_p_yes（0.26，Ensemble 則用 min_model_confidence）
  否則 BuyNo：edge_no ≥ 450bps
    → 再查下方閘門 7
  否則 → Hold，跳過

[閘門 2] MIN_ENTRY_PRICE
  entry_ask < 0.30 → 跳過（廉價 token，流動性差）

[閘門 3] MAX_ENTRY_PRICE
  entry_ask > 0.90 → 跳過（費用侵蝕獲利）

[閘門 1] LOOKBACK_SLOPE（歷史 tick ≥ 3 時才啟用）
  slope = (price[-1] - price[0]) / (n-1)
  slope < customized_min_slope（0.001）→ 跳過（價格趨勢下行）

[閘門 6] PEAK_BUCKET_BLOCK（僅 BuyNo）
  market.slug ∈ peak_no_slugs → 跳過
  原理：同城市+日期中流動性最高的桶 = 市場最確信的溫度，不逆押 NO

[閘門 7] HIGH_YES_CONFIDENCE（僅 BuyNo）
  yes_best_ask ≥ 0.35 時：
    需 (1 - p_yes) ≥ 0.85（即 p_yes ≤ 0.15）
  yes_best_ask < 0.35 時：
    需 (1 - p_yes) ≥ 0.70（正常門檻）
  不符合 → 跳過

  ↓ 全部通過 ↓

[下單]
  side = BuyYes / BuyNo
  entry_ask = 當前 best_ask
  bet_size = 由 SharedCapital::current_bet_size() 決定（trade_size_pct × 可用資金）
  profit_target = (180×2 + 45×2 + 450×0.5) / 10000 = 0.0675（約 6.75¢）
  stop_loss_price = entry_ask - sl_delta（lead_days≥1 時 sl_delta=0.24，=0 時=0.12）

  dry_run：寫入 weather_dry_run_trades 表，不送出訂單
  live：呼叫 CLOB API 下限價單
```

---

### 進場後持倉欄位

| 欄位 | 說明 |
|---|---|
| `entry_price` | 入場 ask 價格 |
| `bid_best` | 持倉期間最高 bid 水位（trailing TP 基準） |
| `stop_loss_price` | 固定底線（entry - sl_delta） |
| `profit_target` | TP 啟動門檻（≈6.75¢） |
| `p_yes_at_entry` | 入場時模型 p_yes（用於 FORECAST_SHIFT 計算偏移量） |
| `abort_at` | 強制退出時間點（close_ts - effective_abort_sec） |
| `consecutive_sl_ticks` | 連續低於止損線的 tick 計數（需達 2 才真正出場） |

---

### weather_custom_verify 完整參數

| 參數 | 設定值 | 說明 |
|---|---|---|
| `capital_allocation_pct` | 0.05 | 使用總資金 5%（灰度驗證） |
| `trade_size_pct` | 0.05 | 每筆 5% 可用資金 |
| `max_drawdown_pct` | 0.30 | 回撤 30% 熔斷 |
| `loop_interval_sec` | 900 | 每 15 分鐘掃描一次 |
| `taker_fee_bps` | 180 | Taker 費率（1.8%） |
| `slippage_buffer_bps` | 45 | 滑點緩衝 |
| `min_net_edge_bps` | 450 | 最低邊際（4.5%） |
| `max_spread` | 0.05 | 訂單簿最大價差（5¢） |
| `min_model_confidence` | 0.70 | BuyNo 正常信心門檻 |
| `min_ensemble_members` | 20 | 最低有效 Ensemble 成員數 |
| `weather_min_depth_usdc` | 60 | 訂單簿最低深度 |
| `weather_min_lead_days` | 1 | 最短 1 天後結算（不做當日市場進場） |
| `weather_max_lead_days` | 5 | 最長 5 天後結算 |
| `city_whitelist` | NYC/London/Tokyo/Miami/Chicago | 交易城市 |
| `min_temprange_p_yes` | 0.26 | TempRange BuyYes 最低 p_yes |
| `stop_loss_delta` | 0.12（×2=0.24 for lead≥1） | 止損距離 |
| `profit_target` | 0.10（計算後實際 ≈ 0.0675） | TP 啟動門檻 |
| `min_sl_ticks` | 2 | 止損需連續 2 tick 確認 |
| `forecast_shift_threshold` | 0.15（×2=0.30 for lead≥1） | 預測偏移強制退出門檻 |
| `customized_lookback_ticks` | 4 | 斜率歷史視窗（≈60 分鐘） |
| `customized_min_slope` | 0.001 | 每 tick 最低漲幅（0.1¢） |
| `customized_min_history_ticks` | 3 | 歷史不足時略過斜率閘門 |
| `customized_min_entry_price` | 0.30 | 進場最低 ask |
| `customized_max_entry_price` | 0.90 | 進場最高 ask |
| `customized_max_ensemble_spread_celsius` | 3.0 | Ensemble 成員最大標準差（°C） |
| `customized_max_positions_per_city` | 2 | 同城市最多 2 倉 |
| `high_yes_ask_threshold` | 0.35 | 觸發高信心門檻的 YES ask 下限 |
| `high_yes_min_confidence_no` | 0.85 | 高 YES 市場的 BuyNo 信心要求 |

> `forecast_temp_bias_celsius` 對 Ensemble 直接計票**無效**，`weather_custom_verify` 不設此值。

---

## 安全規則

- `.env` 含私鑰，**不可 git add、不可截圖、不可分享**
- live 模式必須明確帶 `--confirm-live`
- 所有新增下單路徑必須有 dry_run 攔截（見 `CLAUDE.md`）
