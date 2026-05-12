# Phase 4：分群分析 + 策略改進建議

## 完成項目

### 4.1 Polymarket 官網交易拉取
- 新建 [python-analytics/src/data/polymarket_history.py](../../python-analytics/src/data/polymarket_history.py)
- 從 `https://data-api.polymarket.com/trades?user=<wallet>` 拉取自有錢包歷史交易
- 自動建立 `polymarket_trades_external` 表（PRIMARY KEY tx_hash 防重複）
- dry_run 模式下錢包沒有真實交易，回傳空清單（預期行為）

### 4.2 分群診斷
- 新建 [python-analytics/src/analytics/strategy_diagnosis.py](../../python-analytics/src/analytics/strategy_diagnosis.py)
- 分群維度：strategy_id / model / city / market_type / side / action / lead_days_bucket / p_yes_at_entry_bucket
- 輸出三種格式：table / json / markdown
- 儀表板端點：`GET /api/weather/diagnosis?days=N&dimension=X`

### 4.3 改進建議生成
- 新建 [python-analytics/src/analytics/strategy_recommender.py](../../python-analytics/src/analytics/strategy_recommender.py)
- 啟發式規則：
  - 樣本不足（<5 筆）→ info 提示
  - 勝率 <25% 或平均 PnL <-10%/USD → warning/critical
  - 勝率 ≥60% → 加碼建議
  - 極端 p_yes 區段勝率低 → 模型校正建議
  - STOP_LOSS 平均虧 >15%/USD → 縮窄 stop_loss_delta
- 儀表板端點：`GET /api/weather/recommendations?days=N`

## 驗證結果（本地 DB，days=90）

### 分群診斷重點
| 維度 | 觀察 |
|---|---|
| **strategy** | weather_metar_short_aggressive 10 筆全敗（-15.5 USDC） |
| **strategy** | weather_metar_short_conservative 唯一賺錢（2 筆，勝率 50%，+2.1 USDC） |
| **city** | Toronto 10 筆全敗，NYC 4 筆勝率 25% |
| **side** | YES 12 筆勝率 8.3%；NO 2 筆勝率 0% |
| **lead_days** | same-day 9 筆全敗（全部 TIME_DECAY_EXIT） |
| **p_yes_at_entry** | 0.80-1.00 區段 10 筆**全敗** ← 模型對極端事件 over-confident |
| **action** | STOP_LOSS 平均虧 -21%/USDC，stop_loss_delta 可能太寬 |

### 自動產出的關鍵建議
1. **停用 weather_metar_short_aggressive**（n=10 wins=0）
2. **檢視 metar_short 模型**（n=12 win=8%）
3. **city_whitelist 移除 Toronto / NYC**
4. **模型校正 0.80-1.00 區段**：實際勝率 0% 顯示模型對「肯定不會發生」的判斷不可靠
5. **stop_loss_delta 從 0.12 降至 0.08**

詳細：
- [phase4-diagnosis.md](phase4-diagnosis.md)
- [phase4-recommendations.md](phase4-recommendations.md)

## 樣本警示

只有 **14 筆**已結算交易，統計顯著性不足。建議：
1. 先執行 Phase 2 的 SettlementReconciler 跑滿 24h，把 26 筆 unresolved ENTRY 補上 SETTLEMENT
2. 累積到至少 30 筆再正式調參
3. 中期目標：50-100 筆/月 以建立穩定的勝率估計

## 後續批次
- Phase 5：儀表板整合 + 文件更新
