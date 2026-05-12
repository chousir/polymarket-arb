# Phase 2：結算追蹤 Cron（修復 77% 未結算問題）

## 完成項目

### 2.1 SettlementReconciler Rust 任務
- 新建 [rust-engine/src/strategy/settlement_reconciler.rs](../../rust-engine/src/strategy/settlement_reconciler.rs)
- 每 30 分鐘掃描一次所有策略的未結算 ENTRY（市場已關閉）
- 邏輯：
  - 對每筆 ENTRY 呼叫 `gamma::fetch_token_settlement_price(slug, token_id)`
  - 拿到結算價 → 寫 `SETTLEMENT` 紀錄，含真實 realized_pnl
  - 拿不到且超過 7 天 grace period → 寫 `TIME_DECAY_EXIT` 用模型機率估算
  - 仍在 grace 期內 → 記 log 等下次再試
- 在 mod.rs 註冊 `pub mod settlement_reconciler`
- 在 main.rs 第 10b 區段 spawn 此任務（不論啟用哪些策略都啟動）

### 2.2 DbWriter 新方法
- [rust-engine/src/db/writer.rs](../../rust-engine/src/db/writer.rs)：
  - `load_unresolved_closed_weather_entries()`：跨策略查詢已關閉但未結算的 ENTRY
  - 查詢條件：`action='ENTRY' AND close_ts > 0 AND close_ts <= now AND NOT EXISTS(任何 exit 紀錄)`

### 2.3 一次性回補工具
- 新建 [tools/backfill_settlements.py](../../tools/backfill_settlements.py)
- Python 版本的 reconciler，用 httpx 直接呼叫 Polymarket Gamma API
- 預設 `--dry-run` 模式僅預覽，加 `--apply` 才寫入
- 自動偵測 DB schema 版本，若缺 `token_id`/`close_ts` 欄位會明確提示

## 驗證結果

### Rust 編譯與測試
```
$ cargo build --release
    Finished `release` profile [optimized] target(s) in 18.95s

$ cargo test --release --bins
test result: ok. 160 passed; 0 failed; 0 ignored; 0 measured
```

### Backfill 工具
```
$ python tools/backfill_settlements.py --db rust-engine/data/market_snapshots.db --days 90
[ERROR] DB schema 太舊：缺欄位 token_id close_ts
  Backfill 需要 token_id 才能呼叫 Polymarket Gamma API。
  請先讓引擎跑一段時間自動 migration，或在 VPS 上的新 DB 執行此腳本。
```
（預期行為。本地快照 DB 4-19 是舊 schema；新版引擎啟動會自動 ALTER TABLE 加欄位，
詳見 [writer.rs:758-763](../../rust-engine/src/db/writer.rs#L758-L763) 的 migration 區段。）

## VPS 部署步驟

```bash
# 1. 重編譯
cargo build --release

# 2. 重新啟動引擎（會自動 migrate schema，並啟動 reconciler）
cargo run --release -- --mode dry_run

# 3. 一次性回補歷史未結算紀錄
python tools/backfill_settlements.py --db data/market_snapshots.db --days 90  # 預覽
python tools/backfill_settlements.py --db data/market_snapshots.db --days 90 --apply  # 套用

# 4. 24h 後檢查覆蓋率提升
curl 'http://localhost:8090/api/weather/unresolved?days=30'
# 期望：settlement_official_pct 從 0 開始增長
```

## 後續批次
- Phase 3：閘門敏感度分析（已知 LOW_DEPTH 占 89.8%，是流動性問題不是閘門）
- Phase 4：分群分析 + 策略改進建議
