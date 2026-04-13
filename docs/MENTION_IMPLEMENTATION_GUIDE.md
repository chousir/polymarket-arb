# Mention 策略完整實現指南

## 概述

當前 Mention 策略在 `main.rs` 中只是一個 **stub**（佔位符），返回空的 `CycleResult`。 
要進入 Phase 4，需要完成兩個核心步驟：

1. **Step 1：替換 main.rs stub → 調用 mention_decision 邏輯**
2. **Step 2：實現 fetch_trump_mention_markets() API 調用**

---

## 現狀：當前的 Stub 實現

### [rust-engine/src/main.rs](rust-engine/src/main.rs#L346-L359)

```rust
StrategyType::Mention => {
    tracing::info!("[Mention] 川普提及詞策略已啟用 (id={})", sc.id);
    tracing::warn!("[Mention] Phase 4 完整實現尚未準備好，目前跳過執行");
    let mode = if global.is_dry_run() { "dry_run" } else { "live" };
    Ok(crate::db::writer::CycleResult {
        strategy_id: sc.id,
        market_slug: info.slug.clone(),
        mode: mode.to_string(),
        leg1_side: None,
        leg1_price: None,
        leg2_price: None,
        resolved_winner: None,
        pnl_usdc: None,  // ← 永遠是 None（沒有實際交易）
    })
}
```

**問題**：
- ✗ 不獲取任何市場數據
- ✗ 不評估交易邊際（edge）
- ✗ 不下訂單
- ✗ 總是返回 PnL = None（dry_run 也看不到評估結果）

---

## Step 1：替換 Stub → 調用 mention_decision 邏輯

### 目標

在 `main.rs` 中實現完整的市場循環：
1. 獲取所有 Trump mention 市場
2. 過濾市場（keyword matching）
3. 對每個市場的訂單簿評估交易邊際
4. 決策並下訂單
5. 返回 `CycleResult`（dry_run 時寫入 dry_run_trades 表，live 時提交實單）

### 架構流程圖

```
main.rs (Mention branch)
    ↓
fetch_trump_mention_markets()  [Step 2 實現]
    ↓ returns Vec<MentionMarket>
mention_filter::filter_markets()  [已存在]
    ↓ returns Vec<MarketVerdict> (NO/YES/SKIP)
    ├→ for each qualifed market:
    │   ├→ fetch order-book (CLOB API)
    │   ├→ build MentionBookSnapshot
    │   ├→ mention_decision::evaluate()  [已存在]
    │   ├→ decide: BUY_NO / BUY_YES / HOLD
    │   └→ if BUY: executor.place_order()
    │
    ↓
記錄 CycleResult
    ├→ dry_run: 寫入 mention_dry_run_trades 表
    └→ live:   實際下單到 CLOB API
```

### 實現代碼模板

創建新檔案 `rust-engine/src/strategy/mention_executor.rs`（或直接在 main.rs 擴展）：

```rust
// rust-engine/src/strategy/mention_executor.rs

use crate::api::mention_market;
use crate::strategy::{mention_filter, mention_decision};
use crate::db::writer::CycleResult;
use crate::config::StrategyConfig;
use crate::execution::executor::Executor;

/// 完整的 Mention 市場循環實現
pub async fn run_mention_cycle(
    sc: &StrategyConfig,
    executor: &Executor,
    db: &DbWriter,
) -> Result<CycleResult, AppError> {
    // ────────────────────────────────────────────────────────────────────────
    // Phase 1: 獲取市場列表
    // ────────────────────────────────────────────────────────────────────────
    
    let raw_markets = match mention_market::fetch_trump_mention_markets().await {
        Ok(markets) => {
            tracing::info!("[Mention:{}] 獲取 {} 個市場", sc.id, markets.len());
            markets
        }
        Err(e) => {
            tracing::warn!("[Mention:{}] 獲取市場失敗: {e}", sc.id);
            return Ok(CycleResult {
                strategy_id: sc.id.clone(),
                market_slug: "error".into(),
                mode: if executor.is_dry_run() { "dry_run" } else { "live" },
                leg1_side: None,
                leg1_price: None,
                leg2_price: None,
                resolved_winner: None,
                pnl_usdc: None,
            });
        }
    };

    // ────────────────────────────────────────────────────────────────────────
    // Phase 2: 關鍵詞過濾
    // ────────────────────────────────────────────────────────────────────────
    
    let verdicts = mention_filter::filter_markets(&raw_markets);
    
    let mut total_pnl = 0.0;
    let mut entry_count = 0;
    let mut hold_count = 0;
    
    for verdict in verdicts {
        // SKIP 市場不處理
        if verdict.decision == mention_filter::Decision::Skip {
            if executor.is_dry_run() {
                tracing::debug!("[Mention] SKIP: {} ({})", verdict.market.slug, verdict.reason);
            }
            continue;
        }

        // ────────────────────────────────────────────────────────────────────
        // Phase 3: 獲取訂單簿 + 評估邊際
        // ────────────────────────────────────────────────────────────────────
        
        // 🔧 TODO: 從 CLOB API 獲取實時訂單簿
        // let book = clob::get_order_book(&market.token_id_yes, &market.token_id_no).await?;
        
        let snapshot = mention_decision::MentionBookSnapshot {
            yes_best_ask: 0.35,  // TODO: 從 book.yes 提取
            yes_best_bid: 0.34,
            no_best_ask: 0.65,
            no_best_bid: 0.64,
            yes_depth_usdc: 500.0,
            no_depth_usdc: 600.0,
        };

        // ────────────────────────────────────────────────────────────────────
        // Phase 4: 發起決策評估
        // ────────────────────────────────────────────────────────────────────
        
        // 將 StrategyConfig 轉換為 MentionDecisionConfig
        let decision_cfg = mention_decision::MentionDecisionConfig {
            direction_mode: match sc.direction_mode.as_str() {
                "yes_first" => mention_decision::DirectionMode::YesFirst,
                "yes_only" => mention_decision::DirectionMode::YesOnly,
                "no_only" => mention_decision::DirectionMode::NoOnly,
                _ => mention_decision::DirectionMode::NoFirst,
            },
            entry_no_min_price: sc.entry_no_min_price,
            entry_yes_max_price: sc.entry_yes_max_price,
            take_profit_no_price: sc.take_profit_no_price,
            take_profit_yes_price: sc.take_profit_yes_price,
            stop_loss_delta: sc.stop_loss_delta,
            taker_fee_bps: sc.taker_fee_bps,
            slippage_buffer_bps: sc.slippage_buffer_bps,
            execution_risk_bps: sc.execution_risk_bps,
            min_net_edge_bps: sc.min_net_edge_bps,
            bet_size_usdc: executor.available_capital() * sc.trade_size_pct,
            max_spread: sc.max_spread,
        };

        let signal = mention_decision::evaluate(&snapshot, &decision_cfg);

        tracing::info!(
            "[Mention:{}] {} | {}: {} bps | {}",
            sc.id,
            verdict.market.slug,
            signal.direction,
            signal.edge_bps,
            signal.reason
        );

        // ────────────────────────────────────────────────────────────────────
        // Phase 5: 根據信號下單或記錄
        // ────────────────────────────────────────────────────────────────────
        
        match signal.direction {
            mention_decision::TradeDirection::Hold => {
                hold_count += 1;
                if executor.is_dry_run() {
                    // dry_run: 寫入原因
                    db.write_mention_dry_run_trade(MentionDryRunTrade {
                        strategy_id: sc.id.clone(),
                        market_slug: verdict.market.slug.clone(),
                        keyword: "TODO_extract",
                        side: "HOLD".into(),
                        action: "SKIP".into(),
                        reason_code: signal.reason_code,
                        // ... 其他欄位 ...
                    }).await?;
                }
            }
            mention_decision::TradeDirection::BuyNo => {
                entry_count += 1;
                // 🔧 TODO: 實現訂單提交
                // let order = executor.place_order(
                //     side: "BUY",
                //     token_id: verdict.market.token_id_no,
                //     size: decision_cfg.bet_size_usdc / signal.entry_price,
                //     price: signal.entry_price,
                // ).await?;
                
                if !executor.is_dry_run() {
                    tracing::info!(
                        "[Mention LIVE] {} | BUY NO @ {:.4} | {} USDC",
                        verdict.market.slug,
                        signal.entry_price,
                        decision_cfg.bet_size_usdc
                    );
                }
            }
            mention_decision::TradeDirection::BuyYes => {
                entry_count += 1;
                // 🔧 TODO: 類似 BuyNo 實現
            }
        }
    }

    // ────────────────────────────────────────────────────────────────────
    // Phase 6: 返回結果
    // ────────────────────────────────────────────────────────────────────
    
    tracing::info!(
        "[Mention:{}] 循環結束 | 入場: {} | 持有: {}",
        sc.id,
        entry_count,
        hold_count
    );

    Ok(CycleResult {
        strategy_id: sc.id.clone(),
        market_slug: if entry_count > 0 { "multi_entry" } else { "no_entry" }.into(),
        mode: if executor.is_dry_run() { "dry_run" } else { "live" },
        leg1_side: if entry_count > 0 { Some("NO/YES".into()) } else { None },
        leg1_price: None,  // 🔧 TODO: 追蹤第一筆入場價
        leg2_price: None,
        resolved_winner: None,
        pnl_usdc: if entry_count > 0 { Some(total_pnl) } else { None },
    })
}
```

### 在 main.rs 中調用

將 main.rs 的 Mention 分支改為：

```rust
StrategyType::Mention => {
    tracing::info!("[Mention] 川普提及詞策略啟動 (id={})", sc.id);
    
    // ✅ 調用完整實現
    crate::strategy::mention_executor::run_mention_cycle(
        &sc,
        &executor,
        &db,
    )
    .await
}
```

---

## Step 2：實現 fetch_trump_mention_markets() API 調用

### 現狀

[rust-engine/src/api/mention_market.rs](rust-engine/src/api/mention_market.rs) 已有 80% 的實現：

```rust
pub async fn fetch_trump_mention_markets()
    -> Result<Vec<MentionMarket>, crate::error::AppError>
{
    // ✅ 已實現：
    // - 60秒 TTL 緩存
    // - Gamma API 端點拼接
    // - HTTP 請求 + 錯誤處理
    
    // ❌ 但是沒完成的部分：
    // - 將 GammaMentionMarket 轉換為 MentionMarket
    // - 解析 YES/NO token ID
    // - RFC 3339 時間戳轉換
}
```

### 完整實現

在 [rust-engine/src/api/mention_market.rs](rust-engine/src/api/mention_market.rs) 中，完成缺失的部分：

```rust
// ❌ 當前停留在這裡（行 ~85）：

let raw: Vec<GammaMentionMarket> = serde_json::from_str(&text)
    .map_err(|e| crate::error::AppError::Json(e))?;

// ❌ 需要補完：

    // Parse + keep only markets with a valid YES/NO token pair
    let markets: Vec<MentionMarket> = raw
        .into_iter()
        .filter_map(|m| match m.into_mention_market() {
            Ok(mm) => Some(mm),
            Err(e) => {
                tracing::debug!("[MentionMkt] 略過市場 (parse error): {e}");
                None
            }
        })
        .collect();

    tracing::info!("[MentionMkt] 獲取 {} 個 Trump mention 市場", markets.len());

    // ── Cache write ───────────────────────────────────────────────────────────
    {
        let mut cache = MARKET_CACHE.lock().unwrap();
        *cache = Some((Instant::now(), markets.clone()));
    }

    Ok(markets)
}
```

### 核心三個子功能

#### A. `into_mention_market()` — 解析單個市場

```rust
impl GammaMentionMarket {
    fn into_mention_market(self) -> Result<MentionMarket, crate::error::AppError> {
        // 1️⃣ 解析 close_ts
        let close_ts = parse_iso_to_ts(&self.end_date)?;

        // 2️⃣ 解析 outcomes（應該是 ["Yes", "No"] 或類似）
        let outcomes: Vec<String> = serde_json::from_str(&self.outcomes)
            .map_err(|e| crate::error::AppError::Json(e))?;
        
        // 3️⃣ 解析 token IDs（對應 outcomes 的順序）
        let token_ids: Vec<String> = serde_json::from_str(&self.clob_token_ids)
            .map_err(|e| crate::error::AppError::Json(e))?;

        // 4️⃣ 基本長度檢查
        if outcomes.len() < 2 || token_ids.len() < 2 {
            return Err(crate::error::AppError::ApiError(
                format!(
                    "Invalid market outcomes (got {}), expected >= 2",
                    outcomes.len()
                ),
            ));
        }

        // 5️⃣ 找出 YES / NO token（大小寫不敏感匹配）
        let mut token_yes: Option<String> = None;
        let mut token_no: Option<String> = None;
        
        for (i, outcome) in outcomes.iter().enumerate() {
            let lo = outcome.to_lowercase();
            if lo == "yes" || lo.contains("yes") {
                token_yes = Some(token_ids[i].clone());
            } else if lo == "no" || lo.contains("no") {
                token_no = Some(token_ids[i].clone());
            }
        }

        // 6️⃣ 驗證 YES 和 NO 都存在
        let token_id_yes = token_yes
            .ok_or_else(|| crate::error::AppError::ApiError(
                "沒有找到 YES token".into(),
            ))?;
        let token_id_no = token_no
            .ok_or_else(|| crate::error::AppError::ApiError(
                "沒有找到 NO token".into(),
            ))?;

        // 7️⃣ 解析 tags
        let tags: Vec<String> = self.tags
            .iter()
            .filter_map(|v| v.as_str().map(|s| s.to_string()))
            .collect();

        // 8️⃣ 返回結構化市場
        Ok(MentionMarket {
            slug: self.slug,
            question: self.question.unwrap_or_default(),
            token_id_yes,
            token_id_no,
            close_ts,
            tags,
        })
    }
}
```

#### B. `parse_iso_to_ts()` — RFC 3339 → Unix 時間戳

```rust
fn parse_iso_to_ts(iso: &str) -> Result<u64, crate::error::AppError> {
    // 輸入例: "2026-05-01T20:00:00Z"
    // 輸出: Unix timestamp (秒)
    
    use chrono::DateTime;
    
    let dt: DateTime<chrono::Utc> = iso
        .parse()
        .map_err(|e| crate::error::AppError::ApiError(
            format!("Failed to parse ISO datetime '{}': {}", iso, e),
        ))?;
    
    Ok(dt.timestamp() as u64)
}
```

#### C. 完整函數流程

```rust
pub async fn fetch_trump_mention_markets()
    -> Result<Vec<MentionMarket>, crate::error::AppError>
{
    // ── 尝试从缓存读取 ────────────────────────────────────────────────
    {
        let cache = MARKET_CACHE.lock().unwrap();
        if let Some((fetched_at, ref markets)) = *cache {
            if fetched_at.elapsed() < CACHE_TTL {
                tracing::debug!("[MentionMkt] cache hit ({} markets)", markets.len());
                return Ok(markets.clone());
            }
        }
    }

    // ── 构造 API 请求 ─────────────────────────────────────────────────
    let url = format!(
        "{GAMMA_BASE}/markets?tag=trump&active=true&closed=false&limit=100"
    );
    tracing::debug!("[MentionMkt] GET {url}");

    // ── 执行 HTTP 请求 ────────────────────────────────────────────────
    let text = HTTP_CLIENT
        .get(&url)
        .send()
        .await
        .map_err(|e| crate::error::AppError::ApiError(
            format!("Failed to fetch markets: {}", e),
        ))?
        .error_for_status()
        .map_err(|e| crate::error::AppError::ApiError(
            format!("API error ({}): {}", e.status().unwrap_or_default(), e),
        ))?
        .text()
        .await
        .map_err(|e| crate::error::AppError::ApiError(
            format!("Failed to read response: {}", e),
        ))?;

    // ── 解析 JSON ─────────────────────────────────────────────────────
    let raw: Vec<GammaMentionMarket> = serde_json::from_str(&text)
        .map_err(|e| crate::error::AppError::Json(e))?;

    tracing::debug!("[MentionMkt] 原始接收 {} 個市場", raw.len());

    // ── 转换成 MentionMarket + 过滤无效项 ──────────────────────────────
    let markets: Vec<MentionMarket> = raw
        .into_iter()
        .filter_map(|m| {
            match m.into_mention_market() {
                Ok(mm) => {
                    tracing::trace!("[MentionMkt] ✓ {}: {}", mm.slug, mm.question);
                    Some(mm)
                }
                Err(e) => {
                    tracing::debug!("[MentionMkt] ✗ 略過市場 (parse error): {}", e);
                    None
                }
            }
        })
        .collect();

    tracing::info!("[MentionMkt] 獲取 {} 個有效 Trump mention 市場", markets.len());

    // ── 更新缓存 ──────────────────────────────────────────────────────
    {
        let mut cache = MARKET_CACHE.lock().unwrap();
        *cache = Some((Instant::now(), markets.clone()));
    }

    Ok(markets)
}
```

---

## 整合清單

### 完成 Step 1 需要修改的文件

| 文件 | 改動 | 優先級 |
|------|------|--------|
| `src/strategy/mention_executor.rs` | 新建（或直接在 main.rs 擴展） | 必須 |
| `src/main.rs` | 修改 Mention 分支調用 mention_executor | 必須 |
| `src/execution/executor.rs` | 添加 `place_order()` 方法（已存在？需確認） | 必須 |
| `src/db/writer.rs` | 添加 `write_mention_dry_run_trade()` 方法 | 必須 |

### 完成 Step 2 需要修改的文件

| 文件 | 改動 | 狀態 |
|------|------|------|
| `src/api/mention_market.rs` | 完成 `parse_iso_to_ts()` 函數 | 70% 完成 |
| `src/api/mention_market.rs` | 完成 `into_mention_market()` impl | 70% 完成 |
| `src/api/mention_market.rs` | 完成 fetch 函數的 cache + parse 邏輯 | 80% 完成 |

---

## 預期效果

### Dry-run 模式

執行後應看到日誌：

```
[Mention:mention_no_standard] 川普提及詞策略啟動
[MentionMkt] 獲取 12 個有效 Trump mention 市場
[Mention:mention_no_standard] Will Trump mention 'crypto'?: SKIP (already in a position)
[Mention:mention_no_standard] Will Trump mention 'rigged election'?: 
  decision = BuyYes | edge = 125 bps | reason = EDGE_OK
[Mention:mention_no_standard] dry_run entry: YES @ 0.32 | 25 USDC
[Mention:mention_no_standard] 循環結束 | 入場: 2 | 持有: 10
```

### Live 模式（需 --confirm-live flag）

執行後提交實單到 CLOB API：

```
[Mention LIVE] will-trump-mention-crypto: BUY NO @ 0.08 | 25 USDC
[Mention LIVE] Order ID: 0x...abc | Status: PLACED
```

---

## 參考資源

- **決策邏輯**：[src/strategy/mention_decision.rs](src/strategy/mention_decision.rs)  
  包含 `evaluate()` → `TradeSignal`，邊際計算已完成
  
- **市場過濾**：[src/strategy/mention_filter.rs](src/strategy/mention_filter.rs)  
  包含關鍵詞分類，NO/YES/SKIP 三路決策
  
- **參考實現（DumpHedge）**：[src/strategy/dump_hedge.rs](src/strategy/dump_hedge.rs)  
  完整的市場循環 pattern：fetch → evaluate → execute → record
