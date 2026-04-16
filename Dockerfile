# 多階段構建：Rust 引擎 + Python 分析層
# Rust 最低需求：1.86（icu_* crates 需要）
# 若升級依賴後出現 "rustc X.Y is not supported" 錯誤，請同步更新此版本號
FROM rust:1.86 AS rust-builder

WORKDIR /build

# 複製 Rust 工程及配置
COPY rust-engine/ ./rust-engine/
COPY config/ ./config/

# 構築發佈二進制
RUN cd rust-engine && \
    cargo build --release && \
    cp target/release/polymarket-engine /tmp/polymarket-engine

# ═══════════════════════════════════════════════════════════════════════════

# Python + Node.js runtime 階段
FROM python:3.11-slim

WORKDIR /app

# 安裝系統依賴
RUN apt-get update && apt-get install -y \
    curl \
    nodejs \
    npm \
    && rm -rf /var/lib/apt/lists/*

# 複製配置与工具
COPY config/ ./config/
COPY tools/ ./tools/

# ── 設置 Rust 二進制 ──────────────────────────────────────────────────────
COPY --from=rust-builder /tmp/polymarket-engine /app/bin/polymarket-engine
RUN chmod +x /app/bin/polymarket-engine

# ── 設置 Python 環境 ──────────────────────────────────────────────────────
COPY python-analytics/ ./python-analytics/
RUN pip install --no-cache-dir -r python-analytics/requirements.txt

# ── 建置前端 ────────────────────────────────────────────────────────────────
RUN cd python-analytics/src/dashboard/frontend && \
    npm install && \
    npm run build && \
    rm -rf node_modules

# ── 建立數據目錄 ────────────────────────────────────────────────────────────
RUN mkdir -p /app/data

# ── 環境變數預設（來自 .env 覆蓋） ──────────────────────────────────────────
ENV TRADING_MODE=dry_run \
    DB_PATH=/app/data/market_snapshots.db \
    RUST_LOG=info,polymarket_engine=debug \
    CONFIG_PATH=/app/config/settings

# ── 健康檢查 ────────────────────────────────────────────────────────────────
HEALTHCHECK --interval=30s --timeout=10s --start-period=20s --retries=3 \
    CMD curl -f http://localhost:8080/health || exit 1

# ── 啟動腳本 ────────────────────────────────────────────────────────────────
COPY docker-entrypoint.sh /app/
RUN chmod +x /app/docker-entrypoint.sh

EXPOSE 8080

# 預設啟動 dry_run 模式（可透過環境變數 TRADING_MODE 覆蓋）
ENTRYPOINT ["/app/docker-entrypoint.sh"]
