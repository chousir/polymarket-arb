# Phase 4：策略改進建議書（自動產出）

> 本文件由 `strategy_recommender.py` 自動生成。**所有建議都需人工審閱後再修改 config/settings.toml**。

## 全局指標

- 樣本：最近 **90** 天的 **14** 筆已結算交易
- 總 PnL：**-24.4739 USDC**
- 整體勝率：**7.1%**
- 平均 PnL/USDC：**-0.05609**

## ⚠️ Warning（建議審視）

### `strategy:weather_metar_short_aggressive`

- **原因**：策略 weather_metar_short_aggressive 勝率 0% 且平均虧損 -4.5%/USDC。
- **建議**：在 config/settings.toml 將 [strategies.weather_metar_short_aggressive] 設 enabled=false 直到樣本充足後重評
- **證據**：n=10 wins=0 pnl=-15.51

### `model:metar_short`

- **原因**：模型 metar_short 連敗（勝率 8%）。
- **建議**：檢查該模型策略的 min_model_confidence 是否設太低，或停用使用 metar_short 的策略
- **證據**：n=12 pnl=-13.40

### `city:NYC`

- **原因**：城市 NYC 勝率 25%，可能預報精度差或市場結算規則特殊。
- **建議**：從受影響策略的 city_whitelist 移除 NYC，或縮小其 size_usdc
- **證據**：n=4 pnl=-12.67

### `city:Toronto`

- **原因**：城市 Toronto 勝率 0%，可能預報精度差或市場結算規則特殊。
- **建議**：從受影響策略的 city_whitelist 移除 Toronto，或縮小其 size_usdc
- **證據**：n=10 pnl=-11.80

### `side:YES`

- **原因**：買 YES 平均虧損 -2.9%/USDC，方向性問題。
- **建議**：YES 端虧損可能源於模型對 p_yes 過度樂觀，NO 端虧損可能源於模型對極端事件估算不足；建議在 weather_decision.rs 中對應方向加入額外安全邊際
- **證據**：n=12 pnl=-13.40

### `lead_days:1-2d`

- **原因**：提前 1-2d 進場勝率 20%。
- **建議**：如為高 lead_days，降低相關策略的 weather_max_lead_days；如為 same-day 而虧損，可能源自 TIME_DECAY_EXIT 過晚平倉，建議檢查 abort_before_close_sec 設定
- **證據**：n=5 pnl=-19.17

### `lead_days:same-day`

- **原因**：提前 same-day 進場勝率 0%。
- **建議**：如為高 lead_days，降低相關策略的 weather_max_lead_days；如為 same-day 而虧損，可能源自 TIME_DECAY_EXIT 過晚平倉，建議檢查 abort_before_close_sec 設定
- **證據**：n=9 pnl=-5.30

### `model_calibration:0.80-1.00`

- **原因**：進場時模型機率落在極端區段 (0.80-1.00) 但實際勝率僅 0% — 模型對極端事件過度自信。
- **建議**：提高 sigma（模型誤差假設），或對極端 p_yes 區段加入 額外的「實際歷史命中率」校正係數
- **證據**：n=10 pnl=-11.80

### `stop_loss_delta`

- **原因**：STOP_LOSS 觸發 3 次，平均虧 -21.0%/USDC，代表 stop_loss_delta 可能設太寬。
- **建議**：把相關策略的 stop_loss_delta 從 0.12 試降至 0.08
- **證據**：n=3 pnl=-17.57

## ℹ️ Info（觀察提示）

### `time_decay_exit`

- **原因**：TIME_DECAY_EXIT 觸發 9 次但勝率低，可能源於用模型機率估算的 PnL 不夠保守。
- **建議**：等批次 2 的 SettlementReconciler 跑滿 24h 後，用 SETTLEMENT 真實結算價重新計算這些紀錄
- **證據**：n=9 win%=0


