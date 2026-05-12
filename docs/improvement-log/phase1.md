# Phase 1：診斷指標修正

## 完成項目

### 1.1 觸發率計算邏輯修正
- 修改 `_build_stats()`、`_build_strategy_table()` 改用 `weather_dry_run_trades` 表的 `ENTRY / (ENTRY + NO_TRADE)` 公式
- 對 dump_hedge / pure_arb 策略保留 `cycle_results.leg1_price` 邏輯
- 新增 `trigger_rate_source` 欄位指出資料來源（"weather_dry_run_trades" / "cycle_results" / "n/a"）

### 1.2 未結算指標
- `_build_stats` 回傳新增：
  - `weather_unresolved_entries`：ENTRY 但未出場的市場數
  - `settlement_coverage_pct`：已出場市場 / 總 ENTRY 市場
  - `settlement_official_pct`：拿到 Polymarket UMA 官方結算價的比例

### 1.3 拒絕原因端點
- 新增 `GET /api/weather/rejection-breakdown?days=N&strategy_id=X`
- 新增 `GET /api/weather/unresolved?days=N&strategy_id=X`
- 新增 DbReader 方法：`get_weather_action_counts()`、`get_weather_rejection_breakdown()`、`get_weather_unresolved_entries()`

## 驗證結果（本地 DB 快照，days=60）

### `/api/stats?days=60`
```json
{
  "trigger_rate": 0.003995,
  "trigger_rate_source": "weather_dry_run_trades",
  "weather_entries": 69,
  "weather_no_trade": 17202,
  "weather_exits": 14,
  "weather_settled_official": 0,
  "weather_unresolved_entries": 26,
  "settlement_coverage_pct": 0.1034,
  "settlement_official_pct": 0.0
}
```

### `/api/weather/rejection-breakdown?days=60` Top 6
| reason_code | count | pct |
|---|---|---|
| **LOW_DEPTH** | **15,442** | **89.77%** |
| FORECAST_UNAVAILABLE | 1,429 | 8.31% |
| SPREAD_WIDE | 152 | 0.88% |
| LOW_EDGE | 135 | 0.78% |
| MODEL_DIVERGENCE | 32 | 0.19% |
| LOW_CONFIDENCE | 12 | 0.07% |

## 重要結論（顛覆原假設）

使用者問題 #3「程式都沒運作，觸發條件太嚴格？」**答案：不是條件嚴，是市場流動性不夠。**

- **LOW_DEPTH 占 89.8%**：絕大多數 Polymarket 天氣市場深度 < 50 USDC（`min_depth_usdc` 預設值）
- **LOW_EDGE 只占 0.78%**：邊際門檻幾乎沒擋住任何訊號
- **LOW_CONFIDENCE 只占 0.07%**：模型置信度門檻也不是瓶頸

### 對策建議（提交批次 4 強化）
1. **調降 min_depth_usdc**（如從 50 → 20）後重新觀察是否有更多深度合理的訊號通過
2. **追蹤 FORECAST_UNAVAILABLE**：8.3% 是預報拉取失敗，可能是 NWS 城市不在覆蓋範圍或 API 429
3. 不要動 LOW_EDGE / LOW_CONFIDENCE 閘門（它們本來就在保護你）

## 後續批次
- Phase 2：建立結算追蹤 reconciler，把 settlement_official_pct 從 0% 拉起來
- Phase 3：補齊 reason_code 列舉、做敏感度分析
- Phase 4：分群分析 + 策略改進建議
