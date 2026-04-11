# Polymarket 自動交易系統

BTC 15 分鐘 Up/Down 市場，Dump-Hedge 兩腿策略。  
架構：Rust 核心引擎 + Python 分析層，透過 SQLite 共享資料。

## 初始設定（第一次必做，之後不需重複）

### Step 1：安裝系統依賴

```bash
# Rust
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh && source ~/.cargo/env

# Python（建議 Python 3.11+）
pip install py-clob-client python-dotenv
```

### Step 2：建立 .env

```bash
cp config/.env.example .env
# 用編輯器填入：
# POLYGON_PRIVATE_KEY=你的私鑰
# POLYGON_PUBLIC_KEY=你的錢包地址
```

### Step 3：一次性衍生 CLOB API 憑證

```bash
python tools/setup_credentials.py
```

程式印出三行，貼入 .env 對應欄位：
- `CLOB_API_KEY`
- `CLOB_API_SECRET`
- `CLOB_API_PASSPHRASE`

完成後 Rust 引擎直接從 .env 讀取，不需再執行此工具。

### Step 4：建置並啟動（Dry Run 模式）

```bash
cd rust-engine
cargo build --release
cargo run --release -- --mode dry_run
```

Dry run 模式讀取真實市場資料、執行完整策略決策，但不提交任何訂單。
所有「會執行」的操作記錄到 `data/market_snapshots.db` 的 `dry_run_trades` 表。

### Step 5：啟動儀表板

```bash
uvicorn python-analytics.src.dashboard.app:app --reload --port 8080
# 打開 http://localhost:8080
```

### 進入 Live 模式的前置條件

- Dry run 資料 ≥ 7 天
- 回測勝率 ≥ 60%
- 單輪最大虧損 < 20%

確認後執行：
```bash
cargo run --release -- --mode live --confirm-live
```
