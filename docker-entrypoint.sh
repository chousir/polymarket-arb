#!/bin/bash
set -e

# Polymarket 多進程啟動腳本

# 確保配置已讀取
export RUST_LOG=${RUST_LOG:-info,polymarket_engine=debug}
export DB_PATH=${DB_PATH:-/app/data/market_snapshots.db}
export TRADING_MODE=${TRADING_MODE:-dry_run}
export TELEGRAM_BOT_TOKEN=${TELEGRAM_BOT_TOKEN:-}

# 檢查必要的環境變數（Live 模式）
if [ "$TRADING_MODE" = "live" ]; then
    if [ -z "$POLYGON_PRIVATE_KEY" ] || [ -z "$CLOB_API_KEY" ]; then
        echo "[ERROR] Live 模式需要設置 POLYGON_PRIVATE_KEY, CLOB_API_KEY 等環境變數"
        echo "        見 .env.example"
        exit 1
    fi
fi

echo "[docker-entrypoint] TRADING_MODE=$TRADING_MODE"
echo "[docker-entrypoint] DB_PATH=$DB_PATH"

# Mode: 0 = engine-only, 1 = dashboard-only, 2 = both (default)
SERVICE_MODE=${SERVICE_MODE:-2}

# ── Rust 引擎進程 ──────────────────────────────────────────────────────────
run_engine() {
    echo "[docker-entrypoint] 啟動 Rust 引擎..."
    
    case "$TRADING_MODE" in
        "dry_run")
            exec /app/bin/polymarket-engine --mode dry_run
            ;;
        "live")
            if [ -z "$CONFIRM_LIVE" ]; then
                echo "[ERROR] Live 模式必須設置 CONFIRM_LIVE=1"
                exit 1
            fi
            exec /app/bin/polymarket-engine --mode live --confirm-live
            ;;
        *)
            echo "[ERROR] 未知的 TRADING_MODE=$TRADING_MODE"
            exit 1
            ;;
    esac
}

# ── Python Uvicorn 儀表板進程 ────────────────────────────────────────────────
run_dashboard() {
    echo "[docker-entrypoint] 啟動 Python 儀表板..."
    cd /app/python-analytics
    exec uvicorn src.dashboard.app:app \
        --host 0.0.0.0 \
        --port 8080 \
        --log-level info
}

# ── 兩個進程並行（背景啟動一個，前景跑另一個） ───────────────────────────────
run_both() {
    echo "[docker-entrypoint] 啟動 Rust 引擎（背景）+ Python 儀表板（前景）..."
    
    /app/bin/polymarket-engine \
        $([ "$TRADING_MODE" = "live" ] && echo "--mode live --confirm-live" || echo "--mode dry_run") \
        &
    ENGINE_PID=$!
    
    # 等待引擎初始化
    sleep 3
    
    # 前景啟動 Dashboard
    cd /app/python-analytics
    trap "kill $ENGINE_PID" EXIT
    exec uvicorn src.dashboard.app:app \
        --host 0.0.0.0 \
        --port 8080 \
        --log-level info
}

# ── 啟動邏輯 ──────────────────────────────────────────────────────────────
case "$SERVICE_MODE" in
    0)
        run_engine
        ;;
    1)
        run_dashboard
        ;;
    2)
        run_both
        ;;
    *)
        echo "[ERROR] 未知的 SERVICE_MODE=$SERVICE_MODE (0=engine, 1=dashboard, 2=both)"
        exit 1
        ;;
esac
