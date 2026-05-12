# Phase 5：整合與最終驗證

## 完成項目

### 5.1 儀表板端點整合
新增端點全部 200 OK：

| 端點 | 用途 | 狀態 |
|---|---|---|
| `/api/stats?days=N` | 真實觸發率、未結算指標 | ✅ |
| `/api/strategies?days=N` | 每策略指標（含 weather 觸發率/未結算） | ✅ |
| `/api/weather/rejection-breakdown` | 拒絕原因排名 | ✅ |
| `/api/weather/unresolved` | 未結算 ENTRY 統計 | ✅ |
| `/api/weather/gate-sensitivity` | 閘門敏感度分析 | ✅ |
| `/api/weather/diagnosis?dimension=X` | 分群診斷 | ✅ |
| `/api/weather/recommendations` | 改進建議清單 | ✅ |

### 5.2 端到端驗證

```
$ cargo build --release
    Finished `release` profile [optimized] target(s)

$ cargo test --release --bins
test result: ok. 160 passed; 0 failed

$ python -m src.analytics.gate_sensitivity --days 60   # ✅
$ python -m src.analytics.strategy_diagnosis --days 90 # ✅
$ python -m src.analytics.strategy_recommender --days 90 # ✅
$ python tools/backfill_settlements.py --days 90       # ✅ schema 偵測正常
```

### 5.3 文件更新
- README.md：新增「診斷與改進分析」、「結算補齊」、「拉取錢包真實交易」、「儀表板新端點」四節
- CLAUDE.md：新增「診斷工具」、「結算回補」兩節

---

## 完成標準對照（規劃書）

| 標準 | 結果 |
|---|---|
| 儀表板 trigger_rate 不再顯示 100% | ✅ 從 100% 修正為 0.40%（真實值） |
| weather_dry_run_trades 表中 SETTLEMENT 筆數 > 0 | ⏳ 待 VPS 部署後驗證（本地快照無 token_id 欄位） |
| 未結算 ENTRY 數量 < 5% | ⏳ 同上 |
| 拒絕原因儀表板可見 Top-N 排名 | ✅ LOW_DEPTH 89.8% 等資料正確輸出 |
| phase4-diagnosis.md 與 phase4-recommendations.md 產出 | ✅ |
| 所有 cargo test 通過 | ✅ 160 passed |
| DRY_RUN 攔截邏輯保持完整 | ✅ 未變更執行路徑 |

---

## 給使用者的回答（對應四個原始問題）

### 1. 為何觸發率都是 100%？
**Bug**：[app.py:149](../../python-analytics/src/dashboard/app.py#L149) 原本用 `cycle_results.leg1_price` 算分母，但 cycle_results 表本身只記錄已觸發循環，分母失真。
**修正**：weather/mention 策略改用 `ENTRY / (ENTRY + NO_TRADE)`，真實觸發率 **0.40%**。

### 2. 為何很多交易明明到期都沒結算？
**根因**：Polymarket UMA Oracle 通常市場關閉後**數小時甚至數天**才結算，但原本只有 3 分鐘 in-cycle 輪詢，超時即放棄（pnl=NULL）。
**修正**：新增 `SettlementReconciler` 背景任務每 30 分鐘自動補結算，並提供 `backfill_settlements.py` 一次性回補歷史紀錄。

### 3. 為何程式好像沒運作？觸發條件太嚴格？
**答案：不是。** 拒絕原因分布：
- **LOW_DEPTH 89.77%**（市場深度<50 USDC）— Polymarket 天氣市場結構性流動性不足
- FORECAST_UNAVAILABLE 8.31%（預報拉不到）
- 其他閘門 <2%（不是瓶頸）

即使 min_depth_usdc 降到 5，也只多解鎖 16% 訊號。**不該放寬閘門**，應改為換目標市場或暫不交易此類市場。

### 4. 賠錢策略怎麼改進？
分群診斷 14 筆已結算發現：
- weather_metar_short_aggressive **10 筆全敗**（停用）
- city Toronto **10 筆全敗**（移出白名單）
- p_yes 0.80-1.00 區段 10 筆全敗 → **模型對極端事件 over-confident**（需校正）
- STOP_LOSS 平均虧 21%/USDC → **stop_loss_delta 太寬**（0.12→0.08）

詳見 [phase4-recommendations.md](phase4-recommendations.md)。

樣本警示：只有 14 筆已結算，需先讓 SettlementReconciler 跑滿 24h 補齊 26 筆 unresolved，並累積到 30+ 筆後再正式調參。

---

## VPS 部署順序

```bash
# 1. 重編譯
cd rust-engine && cargo build --release

# 2. 重啟引擎（自動 migrate schema + 啟動 SettlementReconciler）
cargo run --release -- --mode dry_run

# 3. 回補歷史紀錄
python tools/backfill_settlements.py --db data/market_snapshots.db --days 90       # 預覽
python tools/backfill_settlements.py --db data/market_snapshots.db --days 90 --apply

# 4. 啟動儀表板
cd python-analytics && uvicorn src.dashboard.app:app --port 8080

# 5. 等 24-48h 後檢視
curl 'http://VPS:8080/api/weather/unresolved?days=30'      # settlement_official_pct 應 > 0
curl 'http://VPS:8080/api/weather/recommendations?days=30' # 看更新後的建議
```

---

## 不在本次範圍

- 修改 `config/settings.toml` 中的任何閾值（必須使用者人工審閱建議後手動改）
- 修改 live 模式邏輯（DRY_RUN 攔截完整保留）
- 樣本不足下的激進結論（建議至少 30 筆才下定論）
