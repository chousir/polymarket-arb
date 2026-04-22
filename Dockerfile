# 多階段構建：Rust 引擎 + 前端 + Python 分析層
# Rust 最低需求：1.86（icu_* crates 需要）
# 若升級依賴後出現 "rustc X.Y is not supported" 錯誤，請同步更新此版本號
FROM rust:1.86 AS rust-builder

WORKDIR /build

COPY rust-engine/ ./rust-engine/
COPY config/ ./config/

RUN cd rust-engine && \
    cargo build --release && \
    cp target/release/polymarket-engine /tmp/polymarket-engine

# ═══════════════════════════════════════════════════════════════════════════

# 前端構建階段（Node.js 不進最終 image）
FROM node:20-slim AS frontend-builder

WORKDIR /frontend

COPY python-analytics/src/dashboard/frontend/ ./

RUN npm install && npm run build

# ═══════════════════════════════════════════════════════════════════════════

# Python runtime 階段（無 Node.js / npm）
FROM python:3.11-slim

WORKDIR /app

RUN apt-get update && apt-get install -y \
    curl \
    && rm -rf /var/lib/apt/lists/*

COPY config/ ./config/
COPY tools/ ./tools/

# ── 設置 Rust 二進制 ──────────────────────────────────────────────────────
COPY --from=rust-builder /tmp/polymarket-engine /app/bin/polymarket-engine
RUN chmod +x /app/bin/polymarket-engine

# ── 設置 Python 環境 ──────────────────────────────────────────────────────
COPY python-analytics/ ./python-analytics/
RUN pip install --no-cache-dir -r python-analytics/requirements.txt

# ── 注入已構建的前端靜態檔（不含 node_modules / npm）─────────────────────
COPY --from=frontend-builder /frontend/dist ./python-analytics/src/dashboard/frontend/dist

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

ENTRYPOINT ["/app/docker-entrypoint.sh"]
