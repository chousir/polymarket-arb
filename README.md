# Polymarket 自動交易系統

本專案是針對 Polymarket BTC 15 分鐘 Up/Down 市場的自動交易系統，
採用 Rust 核心引擎執行即時交易決策，並由 Python 提供回測、參數優化與儀表板。

核心目標：
- 在真實市場資料下持續監控機會
- 以風控優先的策略執行方式降低單輪風險
- 透過 dry_run 先驗證策略，再進入 live

## 專案在做什麼

- Rust 引擎（`rust-engine/`）
	- 連線 Polymarket/Gamma/CLOB/Binance
	- 監控市場、計算訊號、執行訂單流程
	- 寫入交易週期資料到 SQLite
	- 支援 dry_run 攔截與 live 下單保護
- Python 分析層（`python-analytics/`）
	- 讀取 SQLite 進行回測統計
	- 網格優化策略參數
	- FastAPI 儀表板可視化策略結果

資料中樞：
- SQLite（預設 `./data/market_snapshots.db`）
- Rust 寫入、Python 讀取，共用同一份資料

## 策略與策略簡介

目前在 `config/settings.toml` 內可配置兩類策略：

1. Dump-Hedge（兩腿）
- Leg 1：偵測 BTC 急跌（`dump_threshold_pct`）後先入場
- Leg 2：等待 Up+Down 價格總和低於 `hedge_threshold_sum` 時對沖
- 目的：在波動期鎖定價差、降低單腿暴露時間

2. Pure-Arb（即時雙腿）
- 不等待急跌訊號，直接監控 `hedge_threshold_sum`
- 當總和出現折價時同時買入兩腿
- 目的：抓取市場短暫錯價

每個策略都可獨立設定：
- `capital_allocation_pct`：該策略使用總資金比例
- `trade_size_pct`：每次下注比例（動態隨資金變化）
- `max_drawdown_pct`：達到回撤上限時停止入場
- `enabled`：是否啟用

## 目前策略啟用狀態（2026-04）

目前預設已將 Mention 與 BTC 15 分鐘 Up/Down 相關策略全部設為停用（`enabled = false`）。

原因如下：
- Mention 策略目前若只用 `tag=trump` 抓市場，會混入大量非「Trump mention」事件，導致樣本噪音高、誤判率高。
- Mention 策略需要先做更精準的事件層篩選（例如先鎖定「What will Trump say ...」事件家族，再進市場層關鍵詞過濾），否則不適合直接投入資金。
- BTC 15 分鐘 Up/Down（Dump-Hedge / Pure-Arb）在現行成本與流動性條件下效益偏弱，實盤可行性低，暫不建議投入開發與資金。

目前專案以保守模式運行：先保留基礎設施與資料管線，策略預設不自動入場，待後續完成更嚴格篩選與驗證後再評估重啟。

## 交易模式

- `dry_run`
	- 讀真實行情、走完整決策流程
	- 不送出真實訂單
	- 模擬交易寫入 `dry_run_trades`
- `live`
	- 真實下單
	- 啟動時必須帶 `--confirm-live`

## Step-by-Step 安裝與設定

以下以 Linux/macOS shell 為例。

### Step 1：安裝依賴

```bash
# Rust
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source ~/.cargo/env

# Python（建議 3.11+）
python3 --version
```

### Step 2：建立 Python 虛擬環境（建議）

```bash
python3 -m venv .venv
source .venv/bin/activate
python -m pip install --upgrade pip
python -m pip install -r python-analytics/requirements.txt
```

### Step 3：建立 `.env`

本機開發可用 `config/.env.example`：

```bash
cp config/.env.example .env
```

Docker 部署可用 `.env.docker`：

```bash
cp .env.docker .env
```

至少先填：
- `POLYGON_PRIVATE_KEY`
- `POLYGON_PUBLIC_KEY`

### Step 4：一次性衍生 CLOB API 憑證

```bash
python tools/setup_credentials.py
```

把輸出的三個值貼回 `.env`：
- `CLOB_API_KEY`
- `CLOB_API_SECRET`
- `CLOB_API_PASSPHRASE`

這個步驟只需做一次；後續引擎會直接從 `.env` 讀取。

### Step 5：檢查策略設定檔

編輯 `config/settings.toml`，重點區塊：

- `[global]`：模式、監控時間窗、接近收盤停止入場秒數
- `[api]`：Gamma/CLOB/WebSocket/Binance 端點
- `[rate_limits]`：API 速率限制
- `[risk]`：每日最大虧損與斷路器
- `[capital]`：初始資金、taker fee、gas 成本
- `[[strategies]]`：策略清單與每個策略的參數

## 環境變數怎麼用

主要由 Rust 引擎在啟動時讀取：

- `TRADING_MODE`：`dry_run` 或 `live`
- `DB_PATH`：SQLite 路徑（預設 `./data/market_snapshots.db`）
- `POLYGON_PRIVATE_KEY`：Polygon 私鑰（live 必填）
- `POLYGON_PUBLIC_KEY`：錢包地址（live 會查 USDC 餘額）
- `CLOB_API_KEY` / `CLOB_API_SECRET` / `CLOB_API_PASSPHRASE`：CLOB 憑證
- `TELEGRAM_BOT_TOKEN`：通知用（可選）
- `CONFIRM_LIVE`：Docker live 模式防呆開關

注意：
- `.env` 含敏感資訊，不要提交到 Git
- 建議 dry_run 穩定跑滿至少 7 天再開 live

## 如何執行（本機）

### 1) 啟動 Rust 引擎（dry_run）

```bash
cd rust-engine
cargo build --release
cargo run --release -- --mode dry_run
```

### 2) 啟動儀表板

另開一個終端機：

```bash
uvicorn src.dashboard.app:app --reload --port 8080
```

開啟：`http://localhost:8080`

如果你想明確切到 Python 專案目錄，也可以先 `cd python-analytics` 再啟動，兩種方式都可以。

### 3) 回測與優化

```bash
cd python-analytics
python -m src.backtest.engine --days 7 --db ../data/market_snapshots.db
python -m src.strategy.optimizer --db ../data/market_snapshots.db --days 7
```

### 4) 進入 Live（高風險）

僅在滿足門檻後執行：
- dry_run 資料 >= 7 天
- 回測勝率 >= 60%
- 單輪最大虧損 < 20%

```bash
cd rust-engine
cargo run --release -- --mode live --confirm-live
```

## 如何執行（Docker / Makefile）

### Makefile 快速流程

```bash
# 建置映像（預設目標）
make

# 啟動 dry_run（引擎 + 儀表板）
make run-dry-run

# 看日誌
make logs SERVICE=app

# 停止
make down
```

常用：
- `make help`
- `make run-dashboard`
- `make run-live CONFIRM_LIVE=1`

### 直接用 Docker Compose

```bash
docker-compose build
docker-compose --profile all up -d app
docker-compose logs -f app
docker-compose down
```

## 驗證系統是否正常

可用以下檢查 dry_run 是否持續產生資料：

```bash
sqlite3 rust-engine/data/market_snapshots.db "SELECT COUNT(*) FROM dry_run_trades;"
sqlite3 rust-engine/data/market_snapshots.db "SELECT COUNT(DISTINCT DATE(ts)) FROM dry_run_trades;"
```

## 安全與風控重點

- 新增任何下單路徑時，必須保留 dry_run 攔截
- live 模式一定要有顯式確認旗標
- 私鑰只從環境變數讀取，不要硬編碼
- 上線前先完成一段可驗證的 dry_run 樣本

## 專案結構（摘要）

```text
polymarket-arb/
|- rust-engine/        # 交易引擎（Rust）
|- python-analytics/   # 回測、優化、儀表板（Python）
|- config/             # settings.toml, .env.example
|- data/               # SQLite DB（預設輸出）
|- tools/              # setup_credentials.py 等工具
|- docker-compose.yml
|- Makefile
```
