# Docker 部署指南

## 快速開始

### 1. 準備環境變數

```bash
cp .env.docker .env
# 用編輯器填入私鑰、CLOB API 憑證等
# 見 .env.docker 的註解
```

> ⚠️ **重要**：`.env` 包含私鑰，切勿提交到 Git

### 2. 構建映像

```bash
# 第一次構建會下載依賴並編譯 Rust，約 5~10 分鐘
docker-compose build
```

### 3. 啟動服務

#### Dry Run 模式（推薦先用，安全測試）

```bash
# 同時啟動 Rust 引擎 + Python 儀表板
docker-compose --profile all up -d app

# 查看日誌
docker-compose logs -f app

# 訪問儀表板
# http://localhost:8080 / http://your-vps-ip:8080
```

#### 僅運行儀表板（查看既有數據）

```bash
docker-compose --profile dashboard up -d dashboard
docker-compose logs -f dashboard
```

#### Live 模式（生產環境，謹慎！）

```bash
# 須設置 CONFIRM_LIVE 防止誤觸
TRADING_MODE=live CONFIRM_LIVE=1 \
  docker-compose --profile engine up -d engine

docker-compose logs -f engine
```

### 常用命令

```bash
# 查看所有服務狀態
docker-compose ps

# 查看實時日誌
docker-compose logs -f [app|engine|dashboard]

# 停止所有服務
docker-compose down

# 停止並清除數據
docker-compose down -v

# 重新啟動特定服務
docker-compose restart app

# 進入容器 shell
docker-compose exec app bash
```

## 架構

```
┌─────────────────────────────────────────────────────┐
│                Docker Container                     │
├─────────────────────────────────────────────────────┤
│  ┌──────────────────┐     ┌──────────────────────┐  │
│  │  Rust Engine     │     │  Python Dashboard    │  │
│  │  (polymarket-    │────▶│  (uvicorn 8080)      │  │
│  │   engine)        │     │  + Vite frontend     │  │
│  │                  │     │                      │  │
│  │ 監控市場 + 下單  │     │  分析 + 參數優化      │  │
│  └──────────────────┘     └──────────────────────┘  │
│            │                       ▲                │
│            ▼                       │                │
│  ┌─────────────────────────────────────────┐        │
│  │      SQLite (market_snapshots.db)       │        │
│  │  - dry_run_trades                       │        │
│  │  - live_trades                          │        │
│  │  - cycle_results                        │        │
│  └─────────────────────────────────────────┘        │
└─────────────────────────────────────────────────────┘
           │
           ▼
┌─────────────────────────────────────────────────────┐
│         Volume Mount: ./data:/app/data              │
│      (持久化 SQLite & 日誌)                         │
└─────────────────────────────────────────────────────┘
```

## 環境設置

### 單引擎模式（Rust 只）

```yaml
SERVICE_MODE: 0  # 只運行 Rust engine
TRADING_MODE: dry_run
```

### 單儀表板模式（Python 只）

```yaml
SERVICE_MODE: 1  # 只運行 Python dashboard
```

### 整合模式（推薦，Rust + Python）

```yaml
SERVICE_MODE: 2  # 同時運行引擎 + 儀表板
TRADING_MODE: dry_run
```

## VPS 部署（AWS/Linode/Vultr）

1. **SSH 登入 VPS**
   ```bash
   ssh -i ~/.ssh/id_rsa user@your-vps-ip
   ```

2. **安裝 Docker 和 Docker Compose**
   ```bash
   curl -sSL https://get.docker.com | sh
   sudo usermod -aG docker $USER
   ```

3. **複製專案到 VPS**
   ```bash
   git clone https://github.com/your-repo/polymarket-arb.git
   cd polymarket-arb
   ```

4. **設置 .env 和啟動**
   ```bash
   cp .env.docker .env
   # 編輯 .env，填入私鑰等敏感資訊
   vi .env

   # 啟動（建議用 screen 或 nohup 保持後台執行）
   screen -S polymarket
   docker-compose --profile all up app
   # Ctrl+A, 再按 D 分離 screen 會話
   ```

5. **監控日誌**
   ```bash
   # 重新連接 screen
   screen -r polymarket
   
   # 或遠端查看日誌
   docker-compose logs -f
   ```

6. **停止並重啟**
   ```bash
   # 出於某種原因需要重新啟動
   docker-compose down
   docker-compose up -d --profile all app
   ```

## 故障排除

### 容器無法啟動

```bash
# 查看完整日誌
docker-compose logs app

# 檢查環境變數
docker-compose exec app env | grep TRADING_MODE

# 檢查 DB 是否遺失
docker-compose exec app ls -la /app/data/
```

### 私鑰問題 (Live 模式)

```bash
# 確保私鑰格式正確（64 字符十六進制）
echo $POLYGON_PRIVATE_KEY | wc -c  # 應該是 65 (含換行)

# 檢查 CLOB API 憑證是否填完整
docker-compose exec app env | grep CLOB_API
```

### 儀表板無法訪問

```bash
# 檢查埠是否開放
netstat -tlnp | grep 8080

# 檢查防火牆
sudo ufw allow 8080

# 測試本機連線
curl -v http://localhost:8080
```

## 清換鏡像與重新構建

```bash
# 移除既有鏡像（清空佈署）
docker-compose down --rmi all

# 重新構建（不使用快取）
docker-compose build --no-cache

# 重新啟動
docker-compose --profile all up -d app
```

## 更新代碼

```bash
# 拉取最新代碼
git pull origin main

# 重新構建並重啟
docker-compose build
docker-compose --profile all up -d app
```

## 備份數據

```bash
# 備份 SQLite 數據庫
docker run --rm -v polymarket-arb_polymarket-data:/data \
  -v ./backups:/backup alpine \
  cp /data/market_snapshots.db /backup/market_snapshots.db.$(date +%Y%m%d)

# 或直接複製 volume
docker cp $(docker-compose ps -q app):/app/data/market_snapshots.db ./backups/
```

## 資源需求

- **CPU**：最低 2 核（dry_run），4 核推薦（支援 live + 儀表板）
- **記憶體**：最低 1 GB（dry_run），2~4 GB 推薦
- **磁盤**：100 MB（初始），每月 ~10~50 MB （取決於交易量）
- **網絡**：需要穩定的 WebSocket 連線到 Polymarket + Binance

最佳部署位置：**AWS eu-west-2 (倫敦)** 或 **愛爾蘭** VPS
