# IPC 機制說明

## 三種方案比較

| 方案 | 延遲 | 依賴 | 適用場景 | 啟用階段 |
|------|------|------|---------|---------|
| SQLite polling | ~500ms | 無額外依賴 | Phase 1-2，Python 分析結果不需即時傳遞 | **目前使用** |
| Unix Domain Socket | < 1ms | 無額外依賴 | Phase 3，本機單機，Python 訊號需 5s 內到達 | Phase 3 切換 |
| Redis Pub/Sub | ~1-5ms | Redis 服務 | Phase 4+，VPS 多進程 / 跨機器部署 | Phase 4+ |

## 切換條件

### SQLite → Unix Socket（Phase 3）
- 觸發條件：Python 策略訊號需在 5 秒內傳遞給 Rust 引擎
- 操作：在 `ipc/mod.rs` 取消 `unix_socket` 的注釋，停用 `sqlite_poller`

### Unix Socket → Redis（Phase 4+）
- 觸發條件：Rust 引擎與 Python 分析層部署在不同機器或 Docker 容器
- 操作：在 `ipc/mod.rs` 取消 `redis_pubsub` 的注釋，安裝 Redis 服務

## 目前使用（Phase 1-2）

`sqlite_poller.rs` 每 500ms 輪詢 SQLite `signals` 表，讀取 Python 分析層寫入的策略訊號。
對於 15 分鐘市場策略，500ms 延遲完全可接受。
