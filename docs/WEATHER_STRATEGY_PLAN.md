# Phase 5：天氣預測自動化策略規劃書

> 狀態：規劃中（Phase 4 完成後啟動）  
> 核心邏輯：**利用氣象科學數據打敗散戶直覺定價**  
> 更新日期：2026-04

---

## 一、策略原理

Polymarket 天氣市場的定價主要來自「散戶對氣候的主觀感受」，而非專業氣象模型。
這形成了系統性的套利機會：

```
氣象局預測：紐約 6/15 最高氣溫落在 28–33°C 的機率 = 82%
Polymarket YES 成交價 = $0.55（隱含市場認為機率只有 55%）
→ 邊際（edge）= 82% - 55% - ~3% 費用 = 24%
→ 機器人自動買入 YES
```

關鍵優勢：
- 氣象預測更新頻率（每 6 小時）遠高於散戶重新定價的頻率
- 大城市天氣市場流動性足夠（bid/ask spread 通常 < 5%）
- 官方模型（ECMWF/GFS）在 3–7 日預測準確率 > 75%

---

## 二、目標市場

### 2.1 目標城市（高流動性，按優先順序）

| 城市 | 緯度 | 經度 | ICAO代碼 | NWS辦公室 | 備註 |
|------|------|------|---------|-----------|------|
| 紐約 | 40.7128 | -74.0060 | KJFK | OKX | 流動性最高 |
| 邁阿密 | 25.7617 | -80.1918 | KMIA | MFL | 颶風季高波動 |
| 芝加哥 | 41.8781 | -87.6298 | KORD | LOT | 溫差大，套利空間多 |
| 洛杉磯 | 34.0522 | -118.2437 | KLAX | LOX | 氣候穩定，NWS準確率高 |
| 倫敦 | 51.5074 | -0.1278 | EGLL | N/A（ECMWF/GFS）| ECMWF 總部所在地 |
| 巴黎 | 48.8566 | 2.3522 | LFPG | N/A | ECMWF 覆蓋 |
| 東京 | 35.6762 | 139.6503 | RJTT | N/A | JMA + GFS |
| 首爾 | 37.5665 | 126.9780 | RKSS | N/A | KMA + GFS |
| 杜拜 | 25.2048 | 55.2708 | OMDB | N/A | 高溫穩定，易預測 |
| 雪梨 | -33.8688 | 151.2093 | YSSY | N/A | 南半球季節反轉 |

### 2.2 市場類型

Polymarket 天氣市場通常為以下幾種：
- **溫度範圍**：`Will NYC max temp be 25–30°C on June 15?`
- **極端溫度**：`Will London exceed 35°C in July?`
- **降水事件**：`Will Miami see >2 inches of rain this week?`
- **颶風/極端天氣**：`Will Category 3+ hurricane hit Florida in June?`

市場發現方式：Gamma API `GET /markets?tag=weather&active=true&closed=false`

---

## 三、資料來源

### 3.1 資料源清單

| 資料源 | API 基底 | 費用 | 覆蓋 | 更新頻率 | 用途 |
|--------|---------|------|------|---------|------|
| **NWS/NOAA** | `https://api.weather.gov` | 免費，無需 key | 僅美國 | 每 1h | 美國城市精確機率預測 |
| **Open-Meteo (GFS)** | `https://api.open-meteo.com` | 免費，無需 key | 全球 | 每 6h | GFS 模型，全球覆蓋 |
| **Open-Meteo (ECMWF)** | `https://api.open-meteo.com` | 免費，無需 key | 全球 | 每 6h | ECMWF 模型，精度最高 |
| **METAR** | `https://aviationweather.gov/api` | 免費，無需 key | 全球 | 每 1h | 即時地面觀測，短線用 |
| **Open-Meteo (ENSEMBLE)** | `https://ensemble-api.open-meteo.com` | 免費 | 全球 | 每 6h | 多模型集成概率分布 |

> Open-Meteo 是本策略的核心 API：  
> 單一端點同時返回 GFS / ECMWF / ICON 等多個模型資料，免費且不需 API key。  
> ECMWF IFS 模型可免費獲取，商業 ECMWF API 不需要。

### 3.2 關鍵 API 呼叫範例

**NWS（美國城市）：**
```
GET https://api.weather.gov/points/{lat},{lon}
→ 取得 gridId, gridX, gridY

GET https://api.weather.gov/gridpoints/{gridId}/{gridX},{gridY}/forecast/hourly
→ 返回逐時預測，含 temperature, probabilityOfPrecipitation
```

**Open-Meteo（全球，多模型）：**
```
GET https://api.open-meteo.com/v1/forecast
  ?latitude=40.71&longitude=-74.01
  &daily=temperature_2m_max,temperature_2m_min,precipitation_sum
  &models=gfs_seamless,ecmwf_ifs04,icon_seamless
  &temperature_unit=celsius
  &timezone=America/New_York
  &forecast_days=14
→ 返回每個模型的逐日預測值
```

**Open-Meteo Ensemble（概率分布）：**
```
GET https://ensemble-api.open-meteo.com/v1/ensemble
  ?latitude=40.71&longitude=-74.01
  &daily=temperature_2m_max
  &models=gfs_seamless
  &forecast_days=7
→ 返回 50+ 個集成成員，可直接計算概率分布
```

**METAR（即時觀測）：**
```
GET https://aviationweather.gov/api/data/metar
  ?ids=KJFK&format=json&hours=3
→ 返回最近 3 小時的地面觀測（氣溫、露點、風速、能見度）
```

---

## 四、策略變種

每個策略用不同的資料源組合，未來可回測比較哪個最優：

### Strategy 1：`weather_nws_direct`
- **資料源**：NWS/NOAA（美國專屬）
- **目標城市**：NYC、邁阿密、芝加哥、LA
- **方法**：直接使用 NWS 每小時概率預測 vs Polymarket 盤口
- **優勢**：NWS 預測含官方概率值，不需推導
- **劣勢**：僅美國城市；NWS API 偶爾不穩定

NWS 策略的運行方式是：系統每約 15 分鐘掃描一次天氣市場，先過濾可交易標的（美國 12 座城市白名單、到期天數 1–7 天、最小深度 75 USDC 與最大價差 0.05），再用 NWS 預報把事件換成模型機率 Pyes​，並和盤口價格比較計算淨邊際（已扣 (taker_fee_bps + slippage_buffer_bps) / 10_000 × 2 緩衝）；只有當模型信心夠高（例如 Pyes≥0.65 或 ≤0.35）且淨邊際超過門檻（如 800 bps）才進場買 YES 或 NO，進場後會持續監控四種出場條件：到達止盈價就獲利了結（TAKE_PROFIT：entry_price + ((2taker_fee_bps + 2slippage_buffer_bps + 0.5*min_net_edge_bps)/10000)）、價格反向跌破止損帶就停損（STOP_LOSS=0.06）、距離收盤太近就時間止盈/止損退出（TIME_DECAY_EXIT），以及新一輪 NWS 預測使機率偏移超過閾值時提前平倉（FORECAST_SHIFT=0.15），用來控制回撤並鎖定有效邊際。

### Strategy 2：`weather_gfs_global`
- **資料源**：Open-Meteo GFS 模型
- **目標城市**：所有 10 個城市
- **方法**：GFS 點預測 → 結合歷史誤差建立概率分布
- **優勢**：全球覆蓋；每 6 小時更新
- **劣勢**：需要自建概率轉換邏輯（點預測 → 概率）

GFS 策略的運行方式是：系統每約 15 分鐘掃描一次天氣市場，先過濾可交易標的（城市白名單、到期天數 1–7 天、最小深度 75 USDC 與最大價差 0.06），再用 GFS 預報把事件換成模型機率 Pyes​，並和盤口價格比較計算淨邊際（已扣 (taker_fee_bps + slippage_buffer_bps) / 10_000 × 2 緩衝）；只有當模型信心夠高（例如 Pyes≥0.65 或 ≤0.35）且淨邊際超過門檻（如 1000 bps）才進場買 YES 或 NO，進場後會持續監控四種出場條件：到達止盈價就獲利了結（TAKE_PROFIT：entry_price + ((2*taker_fee_bps + 2*slippage_buffer_bps + 0.5*min_net_edge_bps)/10000)）、價格反向跌破止損帶就停損（STOP_LOSS=0.06）、距離收盤太近就時間止盈/止損退出（TIME_DECAY_EXIT），以及新一輪 GFS 預測使機率偏移超過閾值時提前平倉（FORECAST_SHIFT=0.15），用來控制回撤並鎖定有效邊際。


### Strategy 3：`weather_ecmwf_global`
- **資料源**：Open-Meteo ECMWF IFS 模型
- **目標城市**：所有 10 個城市
- **方法**：同 GFS，但用 ECMWF 模型（全球公認最準）
- **優勢**：準確率最高（中期 3–10 日預測）
- **劣勢**：解析度較低（0.4°）；概率需推導

ECMWF 策略的運行方式是：系統每約 15 分鐘掃描一次天氣市場，先過濾可交易標的（全球 13 座城市白名單、到期天數 2–5 天、最小深度 50 USDC 與最大價差 0.07），再用 ECMWF 預報把事件換成模型機率 Pyes​，並和盤口價格比較計算淨邊際（已扣 (taker_fee_bps + slippage_buffer_bps) / 10_000 × 2 緩衝）；只有當模型信心夠高（例如 Pyes≥0.62 或 ≤0.38）且淨邊際超過門檻（如 800 bps）才進場買 YES 或 NO，進場後會持續監控四種出場條件：到達止盈價就獲利了結（TAKE_PROFIT：entry_price + ((2taker_fee_bps + 2slippage_buffer_bps + 0.5*min_net_edge_bps)/10000)）、價格反向跌破止損帶就停損（STOP_LOSS=0.07）、距離收盤太近就時間止盈/止損退出（TIME_DECAY_EXIT），以及新一輪 ECMWF 預測使機率偏移超過閾值時提前平倉（FORECAST_SHIFT=0.12），用來控制回撤並鎖定有效邊際。

### Strategy 4：`weather_ensemble_prob`
- **資料源**：Open-Meteo Ensemble API（50 個集成成員）
- **目標城市**：NYC、倫敦、東京（高流動性優先）
- **方法**：直接統計集成成員落在目標溫度範圍的比例 → 直接輸出概率
- **優勢**：最直接的概率計算，不需推導
- **劣勢**：API 呼叫較重；每 6 小時更新

Ensemble 策略的運行方式是：系統每約 15 分鐘掃描一次天氣市場，先過濾可交易標的（全球 10 座高流動性城市白名單、到期天數 1–7 天、最小深度 50 USDC 與最大價差 0.06），再用 Ensemble 預報（統計 50+ 個集成成員分布）把事件換成模型機率 Pyes​，並和盤口價格比較計算淨邊際（已扣 (taker_fee_bps + slippage_buffer_bps) / 10_000 × 2 緩衝）；只有當模型信心夠高（例如 Pyes≥0.70 或 ≤0.30）且淨邊際超過門檻（如 1000 bps）才進場買 YES 或 NO，進場後會持續監控四種出場條件：到達止盈價就獲利了結（TAKE_PROFIT：entry_price + ((2taker_fee_bps + 2slippage_buffer_bps + 0.5*min_net_edge_bps)/10000)）、價格反向跌破止損帶就停損（STOP_LOSS=0.12）、距離收盤太近就時間止盈/止損退出（TIME_DECAY_EXIT），以及新一輪 Ensemble 預測使機率偏移超過閾值時提前平倉（FORECAST_SHIFT=0.12），用來控制回撤並鎖定有效邊際。

### Strategy 5：`weather_multi_consensus`
- **資料源**：GFS + ECMWF + Ensemble 三者交叉比對
- **目標城市**：NYC、倫敦（最高流動性）
- **方法**：只在三個來源都同意（概率偏差 < 10%）時才進場
- **優勢**：最高信心度；假陽性最少
- **劣勢**：進場頻率最低

Consensus（多模型共識）策略的運行方式是：系統每約 15 分鐘掃描一次天氣市場，先過濾可交易標的（NYC 與倫敦等最高流動性城市白名單、到期天數 1–7 天、最小深度 50 USDC 與最大價差 0.06），再交叉比對 GFS、ECMWF 與 Ensemble 三大模型預報，僅當三者機率分歧小於 10%（共識極高）時，才把事件換成共識模型機率 Pyes​，並和盤口價格比較計算淨邊際（已扣 (taker_fee_bps + slippage_buffer_bps) / 10_000 × 2 緩衝）；只有當模型信心夠高（例如 Pyes≥0.60 或 ≤0.40）且淨邊際超過門檻（如 600 bps，因具備多模型共識，故允許較低進場門檻）才進場買 YES 或 NO，進場後會持續監控五種出場條件：到達止盈價就獲利了結（TAKE_PROFIT：entry_price + ((2taker_fee_bps + 2slippage_buffer_bps + 0.5*min_net_edge_bps)/10000)）、價格反向跌破止損帶就停損（STOP_LOSS=0.15）、距離收盤太近就時間止盈/止損退出（TIME_DECAY_EXIT）、新一輪預測使機率偏移超過閾值時提前平倉（FORECAST_SHIFT=0.15），**以及當模型間的預測分歧擴大破壞共識時（MODEL_DIVERGENCE）**提前減倉或平倉，用來控制回撤並鎖定有效邊際。

### Strategy 6：`weather_metar_short`
- **資料源**：METAR 即時觀測 + NWS/GFS 短期預測
- **目標城市**：所有有 ICAO 代碼的城市
- **方法**：結合當前實測溫度趨勢，只做 24 小時內到期的市場
- **優勢**：短期預測準確率最高（>85%）
- **劣勢**：市場到期太快，流動性可能不足

METAR 短線策略的運行方式是：系統每約 5 分鐘（loop_interval_sec=300，因短線天氣變化極快，掃描頻率高於其他策略的 15 分鐘）掃描一次天氣市場，先過濾可交易標的（全球所有支援且具備 ICAO 代碼的城市、到期天數 0–1 天的極短線、最小深度 60 USDC 與最大價差 0.07），再用 METAR 即時地面觀測結合 NWS/GFS 短期預測把事件換成模型機率 Pyes​，並和盤口價格比較計算淨邊際（已扣 (taker_fee_bps + slippage_buffer_bps) / 10_000 × 2 緩衝）；只有當模型信心夠高（例如 Pyes≥0.62 或 ≤0.38）且淨邊際超過門檻（如 700 bps）才進場買 YES 或 NO，進場後會持續監控四種出場條件：到達止盈價就獲利了結（TAKE_PROFIT：entry_price + ((2taker_fee_bps + 2slippage_buffer_bps + 0.5*min_net_edge_bps)/10000)）、價格反向跌破止損帶就停損（STOP_LOSS=0.10）、距離收盤太近就時間止盈/止損退出（TIME_DECAY_EXIT），以及新一輪觀測預測使機率偏移超過閾值時提前平倉（FORECAST_SHIFT=0.10），用來控制回撤並鎖定有效邊際。

---

## 五、核心演算法：邊際計算模型

### 5.1 溫度範圍市場（主要類型）

```
市場問題：NYC 最高氣溫是否在 X–Y°C 之間？

輸入：
  model_temp = GFS/ECMWF 預測最高氣溫（點估計）
  model_spread = 歷史誤差標準差（或 Ensemble 標準差）
  market_range = [X, Y]
  market_yes_ask = Polymarket YES 賣盤價

計算：
  # 假設預測誤差服從常態分布
  p_yes = Φ((Y - model_temp) / model_spread) - Φ((X - model_temp) / model_spread)
  taker_fee_frac = taker_fee_bps / 10000.0
  edge = p_yes - market_yes_ask - taker_fee_frac * 2.0

決策：
  if edge >= min_net_edge_bps / 10000.0:
    side = "YES"  → 買入 YES token
  elif (1 - p_yes) - (1 - market_yes_ask) - taker_fee_frac * 2.0 >= threshold:
    side = "NO"   → 買入 NO token（即空 YES）
  else:
    hold
```

### 5.2 極端事件市場（二元事件）

```
市場問題：London 溫度是否超過 35°C？

輸入：
  p_exceed = Ensemble 成員中超過 35°C 的比例
  market_yes_ask = Polymarket YES 賣盤價

計算：
  edge = p_exceed - market_yes_ask - cost_frac

決策：
  if edge >= min_edge:
    買 YES
```

### 5.3 歷史誤差標準差（需預先建立）

```python
# 每個城市、每個模型、每個預測天數，建立誤差分布
# city = "NYC", model = "gfs", lead_days = 3
# historical_errors = [observed_max_temp - predicted_max_temp, ...]
# model_spread = std(historical_errors)
```

> Phase 5.1 先使用**固定誤差假設**啟動（2°C for 1–3 day, 4°C for 4–7 day, 6°C for 7–14 day）  
> Phase 5.2 再建立歷史誤差資料庫做動態校準

---

## 六、動態平倉策略（避免資金卡死）

天氣市場到期日通常是 1–30 天後，長時間持倉會鎖死資金。動態平倉邏輯：

### 6.1 平倉觸發條件

```
每 15 分鐘更新一次，檢查以下條件：

1. TAKE_PROFIT（提前獲利）
   條件：bid_price >= take_profit_threshold（例如入場價 × 1.3）
   動作：市價賣出，結算利潤

2. FORECAST_SHIFT（預測劇烈改變）
   條件：最新模型概率 vs 入場時概率差 > shift_threshold（例如 15%）
   動作：重新評估邊際；若邊際轉負則立即平倉

3. MODEL_DIVERGENCE（模型分歧擴大）
   條件：僅適用 consensus 策略；若兩個以上模型產生分歧
   動作：減倉 50% 或全部平倉

4. TIME_DECAY_EXIT（時間衰減平倉）
   條件：已過市場生命週期 80% 且持倉仍有利潤
   動作：提前鎖定利潤，釋放資金

5. STOP_LOSS（停損）
   條件：ask_price >= entry_price + stop_loss_delta（例如 0.15）
   動作：市價平倉，限制最大虧損
```

### 6.2 持倉限制（避免單一市場過度集中）

```toml
# 每個城市最大同時持倉數
max_positions_per_city = 2

# 天氣策略整體最大持倉 USDC
max_total_weather_exposure_usdc = 500.0

# 單筆下注上限
max_single_bet_usdc = 50.0

# 持倉時間上限（天）；超過則強制平倉
max_hold_days = 14
```

---

## 七、新增模組架構

```
rust-engine/src/
├── api/
│   ├── weather/
│   │   ├── mod.rs          ← 統一 WeatherForecast struct + 城市映射
│   │   ├── nws.rs          ← NWS/NOAA API（美國）
│   │   ├── openmeteo.rs    ← Open-Meteo API（GFS + ECMWF + Ensemble）
│   │   └── metar.rs        ← METAR 即時觀測
│   └── weather_market.rs   ← Gamma API 天氣市場發現 + 解析
│
├── strategy/
│   ├── weather_decision.rs ← 邊際計算模型（概率 vs 盤口）
│   ├── weather_filter.rs   ← 市場篩選（城市白名單、市場類型辨識）
│   └── weather_executor.rs ← 執行循環（掃描 + 部位監控 + 動態平倉）
│
└── db/
    └── writer.rs           ← 新增 write_weather_dry_run_trade()

python-analytics/src/
├── backtest/
│   └── weather_report.py   ← 天氣策略回測 + 績效報告
└── dashboard/
    └── frontend/src/        ← Weather Tab（同 Mention Tab 格式）
```

### 新增 SQLite Table：`weather_dry_run_trades`

```sql
CREATE TABLE IF NOT EXISTS weather_dry_run_trades (
    id                    INTEGER PRIMARY KEY,
    ts                    DATETIME DEFAULT CURRENT_TIMESTAMP,
    strategy_id           TEXT NOT NULL,
    event_id              TEXT NOT NULL,      -- 唯一識別此筆交易
    market_slug           TEXT NOT NULL,
    city                  TEXT NOT NULL,      -- "NYC" | "London" | ...
    market_type           TEXT NOT NULL,      -- "temp_range" | "extreme" | "precip"
    side                  TEXT NOT NULL,      -- "YES" | "NO"
    action                TEXT NOT NULL,      -- "ENTRY" | "TAKE_PROFIT" | "STOP_LOSS" |
                                             --  "FORECAST_SHIFT" | "TIME_DECAY_EXIT" | "CANCEL"
    price                 REAL NOT NULL,
    size_usdc             REAL NOT NULL,
    -- 入場時快照
    model_source          TEXT,              -- "nws" | "gfs" | "ecmwf" | "ensemble" | "consensus"
    model_prob_yes        REAL,              -- 模型計算的 YES 概率（0.0–1.0）
    market_yes_ask        REAL,              -- 入場時盤口 YES ask
    edge_bps              REAL,             -- 邊際（bps）
    model_temp_c          REAL,              -- 模型預測溫度（°C）
    temp_range_low_c      REAL,              -- 市場溫度範圍下限
    temp_range_high_c     REAL,              -- 市場溫度範圍上限
    -- 出場資訊
    entry_price           REAL,
    exit_price            REAL,
    hold_sec              INTEGER,
    realized_pnl_usdc     REAL,
    reason_code           TEXT NOT NULL,
    note                  TEXT
);
```

### 新增 `StrategyType::Weather`

```rust
// config.rs
pub enum StrategyType {
    DumpHedge,
    PureArb,
    Mention,
    Weather,  // ← 新增
}
```

---

## 八、settings.toml 新增策略範例

```toml
# ── Weather Strategies ───────────────────────────────────────────────────────

[[strategy]]
id = "weather_nws_w1"
strategy_type = "weather"
enabled = true
capital_allocation_pct = 0.10
trade_size_pct = 0.08
max_drawdown_pct = 0.25
initial_allocated_usdc = 0.0

# 資料來源
weather_data_sources = ["nws"]           # 使用的資料源列表
weather_cities = ["NYC", "Miami", "Chicago", "LA"]
weather_market_types = ["temp_range", "extreme"]

# 決策門檻
min_net_edge_bps = 800                   # 最低 8% 邊際才進場（天氣市場費用高）
min_model_confidence = 0.65              # 模型概率至少 65% 才進場
max_spread = 0.08                        # 最大 bid-ask spread 8%

# 動態平倉
take_profit_multiplier = 1.35           # 入場價 ×1.35 觸發 TP
stop_loss_delta = 0.15                  # ask 上漲 0.15 觸發 SL
forecast_shift_threshold = 0.15         # 預測概率變化 >15% 重新評估
max_hold_days = 7                        # 最長持倉 7 天
time_decay_exit_pct = 0.80              # 已過 80% 生命週期且獲利則平倉

[[strategy]]
id = "weather_ensemble_prob"
strategy_type = "weather"
enabled = true
capital_allocation_pct = 0.10
trade_size_pct = 0.08
max_drawdown_pct = 0.25
initial_allocated_usdc = 0.0

weather_data_sources = ["ensemble"]
weather_cities = ["NYC", "London", "Tokyo"]
weather_market_types = ["temp_range", "extreme"]
min_net_edge_bps = 1000                  # 較高門檻（10%），Ensemble 資料更可靠
min_model_confidence = 0.70
max_spread = 0.06
take_profit_multiplier = 1.40
stop_loss_delta = 0.12
forecast_shift_threshold = 0.12
max_hold_days = 10
time_decay_exit_pct = 0.80

[[strategy]]
id = "weather_multi_consensus"
strategy_type = "weather"
enabled = false                          # Phase 5.2 才啟用
capital_allocation_pct = 0.10
trade_size_pct = 0.10
max_drawdown_pct = 0.20
initial_allocated_usdc = 0.0

weather_data_sources = ["gfs", "ecmwf", "ensemble"]
weather_cities = ["NYC", "London"]
weather_market_types = ["temp_range"]
min_net_edge_bps = 600                   # 共識策略門檻可低一點（高信心）
consensus_max_divergence = 0.10          # 三個來源概率差距 < 10%
min_model_confidence = 0.60
max_spread = 0.06
take_profit_multiplier = 1.30
stop_loss_delta = 0.15
forecast_shift_threshold = 0.15
max_hold_days = 7
time_decay_exit_pct = 0.80
```

---

## 九、啟動序列（逐步完成）

> 每個步驟完成後再告訴 Claude 進行下一步。
> 每個步驟都是可以獨立測試、獨立 `cargo test` 的單元。

---

### Step 1：Rust — 天氣資料 API 模組

**目標**：實現三個資料源的資料抓取，統一轉換為 `WeatherForecast` struct。

新增檔案：
- `rust-engine/src/api/weather/mod.rs` — `WeatherForecast` struct + 城市座標映射表
- `rust-engine/src/api/weather/openmeteo.rs` — Open-Meteo API（GFS + ECMWF + Ensemble）
- `rust-engine/src/api/weather/nws.rs` — NWS API（美國城市）
- `rust-engine/src/api/weather/metar.rs` — METAR 即時觀測

**輸出**：
```rust
pub struct WeatherForecast {
    pub city: String,
    pub model: WeatherModel,         // Nws | Gfs | Ecmwf | Ensemble | Metar
    pub forecast_date: NaiveDate,    // 預測目標日期
    pub max_temp_c: f64,
    pub min_temp_c: f64,
    pub prob_precip: f64,            // 降水概率 0.0–1.0
    pub ensemble_members: Option<Vec<f64>>,  // Ensemble 時才有
    pub fetched_at: DateTime<Utc>,
    pub lead_days: u32,              // 提前幾天預測
}
```

單元測試：mock HTTP 回應，驗證解析邏輯。

---

### Step 2：Rust — 天氣市場發現模組

**目標**：從 Gamma API 抓取天氣市場，解析市場類型、城市、溫度範圍。

新增檔案：
- `rust-engine/src/api/weather_market.rs`

**功能**：
- `fetch_weather_markets()` — Gamma API `?tag=weather&active=true&closed=false`，60 秒 TTL 快取
- 解析市場 slug/question 判斷城市與市場類型：
  - `parse_city(question)` — 從問題文字辨識城市名
  - `parse_temp_range(question)` — 提取溫度範圍（X°C ~ Y°C）
  - `parse_market_type(question)` — 判斷類型：temp_range / extreme / precip

**輸出**：
```rust
pub struct WeatherMarket {
    pub slug: String,
    pub question: String,
    pub city: String,                // "NYC" | "London" | ...
    pub market_type: WeatherMarketType,
    pub target_date: NaiveDate,
    pub temp_range: Option<(f64, f64)>,  // (low_c, high_c)
    pub token_id_yes: String,
    pub token_id_no: String,
    pub close_ts: u64,
}
```

---

### Step 3：Rust — 邊際計算模型

**目標**：輸入氣象預測 + 盤口，輸出進場訊號。

新增檔案：
- `rust-engine/src/strategy/weather_decision.rs`

**功能**：
```rust
pub fn evaluate_temp_range(
    forecast: &WeatherForecast,
    market: &WeatherMarket,
    snapshot: &WeatherBookSnapshot,   // yes_best_ask, no_best_ask, depth
    cfg: &WeatherDecisionConfig,
) -> WeatherSignal {
    // 1. 計算概率（正態分布 CDF，或 Ensemble 直接計數）
    // 2. 計算 edge = p_yes - yes_ask - fee
    // 3. 返回 BuyYes / BuyNo / Hold + edge_bps + reason
}
```

歷史誤差假設（Phase 5.1 固定值）：
```rust
fn temp_model_sigma(model: WeatherModel, lead_days: u32) -> f64 {
    match (model, lead_days) {
        (_, 0..=1) => 1.5,
        (Ecmwf, 2..=4) => 2.5,
        (Gfs, 2..=4) => 3.0,
        (Ecmwf, 5..=7) => 3.5,
        (Gfs, 5..=7) => 4.5,
        (_, _) => 6.0,
    }
}
```

單元測試：驗證正態 CDF、邊際計算、決策邏輯。

---

### Step 4：Rust — 市場篩選模組

**目標**：過濾不適合入場的市場。

新增檔案：
- `rust-engine/src/strategy/weather_filter.rs`

**篩選條件**：
- 城市是否在白名單
- 市場類型是否支援
- 目標日期是否在有效預測範圍（1–14 天）
- 市場流動性是否足夠（depth_usdc >= min_depth）
- 距離收盤是否太近（< abort_before_close_sec）

---

### Step 5：Rust — 執行器主循環

**目標**：整合以上模組，實現完整的掃描 + 監控 + 動態平倉循環。

新增檔案：
- `rust-engine/src/strategy/weather_executor.rs`

**架構**（同 mention_executor.rs 風格）：
```rust
pub struct WeatherStrategy {
    global: BotConfig,
    sc: StrategyConfig,
    capital: SharedCapital,
}

impl WeatherStrategy {
    pub async fn run_loop(&self, db: &DbWriter) {
        // 每 15 分鐘一次（天氣更新比加密貨幣慢）
        let mut interval = tokio::time::interval(Duration::from_secs(15 * 60));
        let mut open_positions: Vec<WeatherPosition> = Vec::new();
        loop {
            interval.tick().await;
            self.monitor_positions(&mut open_positions, db).await;
            self.scan_for_entries(&open_positions, db).await;
        }
    }
}
```

DRY_RUN 合規：所有執行路徑在提交前檢查 `config.is_dry_run()`。

---

### Step 6：Rust — config.rs + db/writer.rs 擴充

**目標**：新增 `StrategyType::Weather`、天氣策略專用設定欄位、DB 寫入方法。

修改：
- `config.rs` — `StrategyType` 加 `Weather`，`StrategyConfig` 加天氣專用欄位
- `db/writer.rs` — 新增 `create_weather_dry_run_trades_table()` + `write_weather_dry_run_trade()`
- `strategy/mod.rs` — 註冊 `pub mod weather_decision`, `weather_filter`, `weather_executor`
- `main.rs` — Weather 策略作為背景 `tokio::spawn`（同 Mention 架構）

---

### Step 7：Python — 回測引擎

**目標**：從 `weather_dry_run_trades` 讀取資料，計算策略績效。

新增檔案：
- `python-analytics/src/backtest/weather_report.py`

**功能**：
- 按 strategy_id 分組統計：進場次數、觸發率、填充率、淨 PnL、勝率
- 按城市、市場類型、資料源分組分析
- 計算 Sharpe、最大回撤
- 輸出 JSON 供儀表板使用

---

### Step 8：儀表板 — Weather Tab

**目標**：新增 Weather 分頁，格式同 BTC / Mention Tab。

修改：
- `python-analytics/src/dashboard/app.py` — 新增 `/api/weather/*` 端點
- `python-analytics/src/dashboard/frontend/src/App.tsx` — 新增 `WeatherTab` 元件

**Weather Tab 佈局**：
```
[ BTC | Mention | Weather ]

┌─────────────────────────────────────────────────────────┐
│ Entries  Trigger Rate  Fill Rate  Net PnL  Drawdown  Sharpe │  ← 6 stat cards
└─────────────────────────────────────────────────────────┘

┌─────────────────────────────────────────────────────────┐
│ 策略績效                                                  │
│ Strategy | Cities | Source | Entries | Edge | PnL | ...  │
└─────────────────────────────────────────────────────────┘

┌─────────────────────────────────────────────────────────┐
│ 策略詳情（點選後展開）                                     │
│ 左：最近交易  右：活躍城市 + 來源                           │
└─────────────────────────────────────────────────────────┘

┌─────────────────────────────────────────────────────────┐
│ 平倉原因分布（TAKE_PROFIT / STOP_LOSS / FORECAST_SHIFT /  │
│              TIME_DECAY / CANCEL）                        │
└─────────────────────────────────────────────────────────┘
```

---

## 十、進入 Live 的最低條件

| 指標 | 門檻 |
|------|------|
| Dry Run 天數 | ≥ 14 天（天氣市場週期較長） |
| 勝率 | ≥ 60% |
| 單策略最大回撤 | < 20% |
| 進場次數 | ≥ 30 次（足夠的統計樣本） |
| 最佳策略模型一致性 | 同一城市、同資料源，結果穩定 |

---

## 十一、潛在風險與應對

| 風險 | 說明 | 應對 |
|------|------|------|
| 市場流動性不足 | 天氣市場 bid-ask 可能很寬 | `max_spread = 0.08`，不足則 CANCEL |
| 模型誤差被市場定價 | 若大量機器人也在用 ECMWF，套利空間縮小 | 加入多模型交叉確認提高信心 |
| 極端事件 | 颶風/熱浪超出模型預測範圍 | `max_hold_days = 7`，不持長線 |
| API 不穩定 | NWS API 偶爾 5xx | 備用 Open-Meteo，60s 重試 |
| 市場發現失效 | Gamma API `tag=weather` 覆蓋不完整 | 補充關鍵字搜尋（`temperature`、`celsius`） |

---

*計畫書結束 — 等待啟動序列指令*
