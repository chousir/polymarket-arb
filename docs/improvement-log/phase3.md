# Phase 3：閘門敏感度分析

## 完成項目

### 3.1 reason_code logging 檢視
檢查 [weather_decision.rs](../../rust-engine/src/strategy/weather_decision.rs) 與 [weather_executor.rs](../../rust-engine/src/strategy/weather_executor.rs)，
現有 reason_code 已涵蓋：
`LOW_EDGE / LOW_CONFIDENCE / LOW_DEPTH / SPREAD_WIDE / MODEL_DIVERGENCE / FORECAST_UNAVAILABLE / MODEL_MEAN_DIVERGENCE / HIGH_ATM_UNCERTAINTY / ENS_DIRECTION_CONFLICT / BUY_YES / BUY_NO`

無需擴充。

### 3.2 敏感度分析腳本
- 新建 [python-analytics/src/analytics/gate_sensitivity.py](../../python-analytics/src/analytics/gate_sensitivity.py)
- 對每個閘門產生「若閾值放寬到 X，會多通過多少 NO_TRADE」的對比表
- 在儀表板新增端點：`GET /api/weather/gate-sensitivity?days=N&strategy_id=X`

## 驗證結果（本地 DB，days=60）

| Gate | Candidate | Δ entries | sample metric |
|---|---|---:|---:|
| LOW_DEPTH | min_depth_usdc → 25 | 381 | depth=36.6 |
| LOW_DEPTH | min_depth_usdc → 10 | 1,415 | depth=21.2 |
| LOW_DEPTH | min_depth_usdc → 5 | 2,805 | depth=14.4 |
| LOW_EDGE | min_net_edge_bps -30% | 135 | — |
| LOW_CONFIDENCE | min_model_confidence → 0.50 | 12 | p_yes=0.499 |
| SPREAD_WIDE | max_spread → 0.10 | 101 | spread=0.076 |
| SPREAD_WIDE | max_spread → 0.15 | 146 | spread=0.089 |

**基線：** 14 筆已結算交易，勝率 7.1%，平均 PnL/USD = **-0.056**（賠錢）。

## 關鍵結論

### 1. LOW_DEPTH 不是「閘門太嚴」
- 90% LOW_DEPTH 連 10 USDC 都沒有
- 即使 min_depth_usdc 從 50 降到 5，也只多解鎖 16% 訊號
- 真正問題：Polymarket 天氣市場結構性流動性不足

### 2. 不該放寬任何閘門
- **基線勝率 7.1%，平均虧損每 USDC -0.056**
- 即使閘門全部放寬讓更多訊號通過，更多進場 = 賠更多
- 策略本身有問題，需要從**選市場/選模型/選城市**改進（批次 4）

### 3. 真正的優化方向
- **換有深度的市場**：先找 Polymarket 上深度 > 50 USDC 的天氣市場清單
- **看為何勝率才 7.1%**：批次 4 的分群分析會說明哪個維度（city / model / lead_days / p_yes_at_entry）拖累勝率
- **暫不動閾值**：等批次 4 找出虧損集中特徵後再決定

## 後續批次
- Phase 4：Polymarket 官網交易紀錄拉取 + 分群分析 + 策略改進建議
