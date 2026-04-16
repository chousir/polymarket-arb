.PHONY: help build-image build-image-no-cache docker docker-run run-dry-run run-live run-dashboard stop logs ps status status-week status-month clean db-init db-clean build-rust build-python test docker-clean push setup check-env credentials

# ═══════════════════════════════════════════════════════════════════════════
# Polymarket 自動交易系統 Makefile
# ═══════════════════════════════════════════════════════════════════════════

# 顏色定義
RED     := \033[0;31m
GREEN   := \033[0;32m
YELLOW  := \033[0;33m
BLUE    := \033[0;34m
NC      := \033[0m

# 自動偵測 Docker Compose 版本（v2 plugin: "docker compose"，v1 standalone: "docker-compose"）
DOCKER_COMPOSE := $(shell docker compose version >/dev/null 2>&1 && echo "docker compose" || echo "docker-compose")

# 默認值
DOCKER_REGISTRY ?= polymarket
IMAGE_NAME ?= polymarket-arb
GIT_SHA    := $(shell git rev-parse --short HEAD 2>/dev/null || echo "unknown")
BUILD_DATE := $(shell date +%Y%m%d)
IMAGE_TAG  ?= $(BUILD_DATE)-$(GIT_SHA)
COMPOSE_PROFILE ?= all
TRADING_MODE ?= dry_run
CONFIRM_LIVE ?=

# ═══════════════════════════════════════════════════════════════════════════
# 默認目標（執行 make 時不指定目標）
# ═══════════════════════════════════════════════════════════════════════════

.DEFAULT_GOAL := build-image

# ═══════════════════════════════════════════════════════════════════════════
# HELP
# ═══════════════════════════════════════════════════════════════════════════

help:
	@echo "$(BLUE)╔════════════════════════════════════════════════════════════════╗$(NC)"
	@echo "$(BLUE)║          Polymarket 自動交易系統 — Makefile 指令              ║$(NC)"
	@echo "$(BLUE)╚════════════════════════════════════════════════════════════════╝$(NC)"
	@echo ""
	@echo "$(YELLOW)⚡ 快速開始$(NC)"
	@echo "  $(GREEN)make setup$(NC)                首次 clone 後執行：複製 .env、建立目錄"
	@echo "  $(GREEN)make docker$(NC)               構建 Docker 映像（= make build-image）"
	@echo "  $(GREEN)make credentials$(NC)          在 Docker 內衍生 CLOB API 憑證（不需本機 Python）"
	@echo "  $(GREEN)make docker-run$(NC)           啟動 Docker 容器（dry_run + 儀表板）"
	@echo ""
	@echo "$(YELLOW)📦 Docker 映像$(NC)"
	@echo "  $(GREEN)make build-image$(NC)           [預設] 構建 Docker 映像"
	@echo "  $(GREEN)make build-image-no-cache$(NC)  重新構建（不使用快取）"
	@echo "  $(GREEN)make push$(NC)                  推送映像到 Registry（需設置 DOCKER_REGISTRY）"
	@echo ""
	@echo "$(YELLOW)🚀 運行服務$(NC)"
	@echo "  $(GREEN)make docker-run$(NC)           啟動 Docker（dry_run + 儀表板）[推薦入口]"
	@echo "  $(GREEN)make run-dry-run$(NC)          同上（舊別名）"
	@echo "  $(GREEN)make run-live$(NC)             啟動 Live 交易模式（需確認 CONFIRM_LIVE=1）"
	@echo "  $(GREEN)make run-dashboard$(NC)        僅啟動儀表板"
	@echo "  $(GREEN)make run-engine$(NC)           僅啟動 Rust 引擎"
	@echo ""
	@echo "$(YELLOW)🛑 停止服務$(NC)"
	@echo "  $(GREEN)make stop$(NC)                 停止所有容器"
	@echo "  $(GREEN)make down$(NC)                 停止並移除容器"
	@echo ""
	@echo "$(YELLOW)📊 監控與報告$(NC)"
	@echo "  $(GREEN)make logs$(NC)                 查看即時日誌（可指定 SERVICE=app)"
	@echo "  $(GREEN)make ps$(NC)                   列出執行中的容器"
	@echo "  $(GREEN)make status$(NC)               策略狀態總覽（資產、收益、交易紀錄）"
	@echo "  $(GREEN)make status-week$(NC)          最近 7 天報告"
	@echo "  $(GREEN)make status-month$(NC)         最近 30 天報告"
	@echo "  $(GREEN)make status ARGS='--strategy dump_hedge_conservative'$(NC)"
	@echo "  $(GREEN)make status ARGS='--days 14 --trades 50'$(NC)"
	@echo ""
	@echo "$(YELLOW)🧪 本地開發$(NC)"
	@echo "  $(GREEN)make build-rust$(NC)           構建 Rust 引擎（release 版本）"
	@echo "  $(GREEN)make build-python$(NC)         安裝 Python 依賴"
	@echo "  $(GREEN)make test$(NC)                 執行 Rust 單元測試"
	@echo "  $(GREEN)make lint$(NC)                 運行 Rust linter (clippy)"
	@echo ""
	@echo "$(YELLOW)💾 數據庫$(NC)"
	@echo "  $(GREEN)make db-init$(NC)              初始化數據庫（建立 schema）"
	@echo "  $(GREEN)make db-clean$(NC)             清除所有數據庫記錄"
	@echo ""
	@echo "$(YELLOW)🧹 清理$(NC)"
	@echo "  $(GREEN)make clean$(NC)                清除本機構建文件"
	@echo "  $(GREEN)make docker-clean$(NC)         移除所有 Docker 鏡像和容器"
	@echo "  $(GREEN)make distclean$(NC)            完全清理（本機 + Docker）"
	@echo ""
	@echo "$(YELLOW)📝 參數$(NC)"
	@echo "  TRADING_MODE=dry_run|live              交易模式 (預設: dry_run)"
	@echo "  CONFIRM_LIVE=1                         Live 模式確認（防誤操作）"
	@echo "  SERVICE=app|engine|dashboard           指定服務（預設: app）"
	@echo "  DOCKER_REGISTRY=<registry>             Docker Registry（預設: polymarket)"
	@echo ""
	@echo "$(BLUE)快速開始（Docker，首次安裝）:$(NC)"
	@echo "  1. $(GREEN)make setup$(NC)             # 複製 .env、建立 data/ 目錄"
	@echo "  2. 編輯 .env，填入 POLYGON_PRIVATE_KEY 和 POLYGON_PUBLIC_KEY"
	@echo "  3. $(GREEN)make docker$(NC)            # 構建 Docker 映像"
	@echo "  4. $(GREEN)make credentials$(NC)       # 衍生 CLOB 憑證（不需本機 Python）"
	@echo "  5. 將憑證填回 .env"
	@echo "  6. $(GREEN)make docker-run$(NC)        # 啟動 dry_run + 儀表板"
	@echo "  7. 訪問 http://localhost:8080"
	@echo ""

# ═══════════════════════════════════════════════════════════════════════════
# 首次安裝
# ═══════════════════════════════════════════════════════════════════════════

# 首次 clone 後執行此目標：複製 .env、建立 data/ 目錄
setup:
	@echo "$(BLUE)═══ Polymarket 首次安裝設定 ═══$(NC)"
	@if [ -f ".env" ]; then \
		echo "$(YELLOW)⚠️  .env 已存在，跳過複製$(NC)"; \
	else \
		cp .env.docker .env; \
		echo "$(GREEN)✓ .env 已建立（從 .env.docker）$(NC)"; \
		echo "$(YELLOW)⚠️  請開啟 .env，填入 POLYGON_PRIVATE_KEY 與 POLYGON_PUBLIC_KEY$(NC)"; \
	fi
	@mkdir -p data
	@echo "$(GREEN)✓ data/ 目錄已就緒$(NC)"
	@echo ""
	@echo "$(YELLOW)下一步:$(NC)"
	@echo "  1. 編輯 .env，填入私鑰"
	@echo "  2. $(GREEN)make docker$(NC)      — 構建 Docker 映像"
	@echo "  3. $(GREEN)make docker-run$(NC)  — 啟動服務（dry_run + 儀表板）"
	@echo ""

# 在 Docker 容器內衍生 CLOB API 憑證（不需本機安裝 Python）
# 前置：.env 已有 POLYGON_PRIVATE_KEY，且映像已構建（make docker）
credentials: check-env
	@echo "$(BLUE)🔑 衍生 CLOB API 憑證（在 Docker 容器內執行）...$(NC)"
	$(DOCKER_COMPOSE) run --rm --entrypoint python app /app/tools/setup_credentials.py
	@echo ""
	@echo "$(YELLOW)請將上方輸出的 Key / Secret / Passphrase 填入 .env$(NC)"

# 內部用：確認 .env 存在，否則提前中止
check-env:
	@if [ ! -f ".env" ]; then \
		echo "$(RED)❌ 找不到 .env 檔案$(NC)"; \
		echo "   請先執行: $(GREEN)make setup$(NC)（或 cp .env.docker .env）"; \
		echo "   然後填入 POLYGON_PRIVATE_KEY 與 POLYGON_PUBLIC_KEY"; \
		exit 1; \
	fi

# ═══════════════════════════════════════════════════════════════════════════
# DOCKER 映像相關
# ═══════════════════════════════════════════════════════════════════════════

# 語義化別名：make docker = 構建映像
docker: build-image

build-image:
	@echo "$(BLUE)🔨 構建 Docker 映像: $(IMAGE_NAME):$(IMAGE_TAG)$(NC)"
	$(DOCKER_COMPOSE) --profile all build
	@docker tag $(IMAGE_NAME)-app $(IMAGE_NAME)-app:$(IMAGE_TAG) 2>/dev/null || true
	@docker tag $(IMAGE_NAME)-app $(IMAGE_NAME)-app:latest 2>/dev/null || true
	@echo "$(GREEN)✓ 映像構建完成: $(IMAGE_NAME)-app:$(IMAGE_TAG)$(NC)"

build-image-no-cache:
	@echo "$(BLUE)🔨 構建 Docker 映像（不使用快取）$(NC)"
	$(DOCKER_COMPOSE) --profile all build --no-cache
	@echo "$(GREEN)✓ 映像構建完成$(NC)"

push:
	@echo "$(BLUE)📤 推送映像到 $(DOCKER_REGISTRY)$(NC)"
	docker tag $(IMAGE_NAME):$(IMAGE_TAG) $(DOCKER_REGISTRY)/$(IMAGE_NAME):$(IMAGE_TAG)
	docker push $(DOCKER_REGISTRY)/$(IMAGE_NAME):$(IMAGE_TAG)
	@echo "$(GREEN)✓ 映像推送完成$(NC)"

# ═══════════════════════════════════════════════════════════════════════════
# 啟動服務
# ═══════════════════════════════════════════════════════════════════════════

# 語義化別名：make docker-run = 啟動 Docker 容器（dry_run 模式）
docker-run: check-env
	@mkdir -p data
	@echo "$(BLUE)🚀 啟動 Docker（dry_run 模式）...$(NC)"
	TRADING_MODE=dry_run $(DOCKER_COMPOSE) --profile all up -d app
	@echo "$(GREEN)✓ Docker 容器已啟動$(NC)"
	@echo "$(YELLOW)📊 儀表板: http://localhost:8080$(NC)"
	@echo "$(YELLOW)📋 查看日誌: $(GREEN)make logs$(NC)"
	@echo "$(YELLOW)🛑 停止:     $(GREEN)make down$(NC)"

run-dry-run: check-env
	@mkdir -p data
	@echo "$(BLUE)🚀 啟動 Dry Run 模式...$(NC)"
	TRADING_MODE=dry_run $(DOCKER_COMPOSE) --profile all up -d app
	@echo "$(GREEN)✓ Dry Run 已啟動$(NC)"
	@echo "$(YELLOW)📊 訪問儀表板: http://localhost:8080$(NC)"
	@echo "$(YELLOW)📋 查看日誌: $(GREEN)make logs$(NC)"

run-live: check-env
	@mkdir -p data
	@if [ -z "$(CONFIRM_LIVE)" ] || [ "$(CONFIRM_LIVE)" != "1" ]; then \
		echo "$(RED)❌ Live 模式需要設置 CONFIRM_LIVE=1 (防誤操作)$(NC)"; \
		echo "   執行: $(GREEN)make run-live CONFIRM_LIVE=1$(NC)"; \
		exit 1; \
	fi
	@echo "$(RED)⚠️ 啟動 LIVE 交易模式！確保已通過 7 天 dry_run 測試！$(NC)"
	@sleep 3
	TRADING_MODE=live CONFIRM_LIVE=1 $(DOCKER_COMPOSE) --profile engine up -d engine
	@echo "$(GREEN)✓ Live 引擎已啟動$(NC)"
	@echo "$(YELLOW)📋 查看日誌: $(GREEN)make logs SERVICE=engine$(NC)"

run-dashboard:
	@echo "$(BLUE)🚀 啟動儀表板...$(NC)"
	$(DOCKER_COMPOSE) --profile dashboard up -d dashboard
	@echo "$(GREEN)✓ 儀表板已啟動$(NC)"
	@echo "$(YELLOW)📊 訪問: http://localhost:8080$(NC)"

run-engine:
	@echo "$(BLUE)🚀 啟動 Rust 引擎（僅引擎，無儀表板）...$(NC)"
	TRADING_MODE=$(TRADING_MODE) $(DOCKER_COMPOSE) --profile engine up -d engine
	@echo "$(GREEN)✓ 引擎已啟動$(NC)"

# ═══════════════════════════════════════════════════════════════════════════
# 停止服務
# ═══════════════════════════════════════════════════════════════════════════

stop:
	@echo "$(BLUE)🛑 停止所有容器...$(NC)"
	$(DOCKER_COMPOSE) stop 2>/dev/null || true
	@docker stop polymarket-app polymarket-engine polymarket-dashboard 2>/dev/null || true
	@echo "$(GREEN)✓ 容器已停止$(NC)"

down:
	@echo "$(BLUE)🛑 停止並移除容器...$(NC)"
	$(DOCKER_COMPOSE) down 2>/dev/null || true
	@docker rm -f polymarket-app polymarket-engine polymarket-dashboard 2>/dev/null || true
	@echo "$(GREEN)✓ 容器已移除$(NC)"

restart:
	@echo "$(BLUE)🔄 重啟容器...$(NC)"
	$(DOCKER_COMPOSE) restart $(SERVICE)
	@echo "$(GREEN)✓ 容器已重啟$(NC)"

# ═══════════════════════════════════════════════════════════════════════════
# 監控和日誌
# ═══════════════════════════════════════════════════════════════════════════

logs:
	@SERVICE_NAME=$(SERVICE); \
	if [ -z "$$SERVICE_NAME" ]; then SERVICE_NAME="app"; fi; \
	echo "$(BLUE)📋 查看日誌: $$SERVICE_NAME$(NC)"; \
	$(DOCKER_COMPOSE) logs -f $$SERVICE_NAME

ps:
	@echo "$(BLUE)📦 執行中的容器$(NC)"
	$(DOCKER_COMPOSE) ps

status:
	@$(DOCKER_COMPOSE) run --rm --no-deps --entrypoint python app /app/tools/status.py $(ARGS)

status-week:
	@$(DOCKER_COMPOSE) run --rm --no-deps --entrypoint python app /app/tools/status.py --days 7

status-month:
	@$(DOCKER_COMPOSE) run --rm --no-deps --entrypoint python app /app/tools/status.py --days 30

shell:
	@SERVICE_NAME=$(SERVICE); \
	if [ -z "$$SERVICE_NAME" ]; then SERVICE_NAME="app"; fi; \
	echo "$(BLUE)🐚 進入容器 shell: $$SERVICE_NAME$(NC)"; \
	$(DOCKER_COMPOSE) exec $$SERVICE_NAME bash

# ═══════════════════════════════════════════════════════════════════════════
# 本地開發
# ═══════════════════════════════════════════════════════════════════════════

build-rust:
	@echo "$(BLUE)🔨 構建 Rust 引擎（release 版本）...$(NC)"
	cd rust-engine && cargo build --release
	@echo "$(GREEN)✓ Rust 引擎構建完成$(NC)"
	@echo "   二進制: rust-engine/target/release/polymarket-engine"

build-python:
	@echo "$(BLUE)🔨 安裝 Python 依賴...$(NC)"
	pip install -q -r python-analytics/requirements.txt
	@echo "$(GREEN)✓ Python 依賴安裝完成$(NC)"

test:
	@echo "$(BLUE)🧪 運行 Rust 單元測試...$(NC)"
	cd rust-engine && cargo test --release
	@echo "$(GREEN)✓ 測試完成$(NC)"

lint:
	@echo "$(BLUE)🔍 運行 Rust linter (clippy)...$(NC)"
	cd rust-engine && cargo clippy --release -- -D warnings
	@echo "$(GREEN)✓ Linter 檢查完成$(NC)"

fmt:
	@echo "$(BLUE)📝 格式化 Rust 代碼...$(NC)"
	cd rust-engine && cargo fmt
	@echo "$(GREEN)✓ 代碼格式化完成$(NC)"

# ═══════════════════════════════════════════════════════════════════════════
# 數據庫
# ═══════════════════════════════════════════════════════════════════════════

db-init:
	@echo "$(BLUE)💾 建立數據庫目錄...$(NC)"
	@mkdir -p data
	@mkdir -p rust-engine/data
	@echo "$(GREEN)✓ data/ 與 rust-engine/data/ 目錄已就緒$(NC)"
	@echo "$(YELLOW)ℹ️  SQLite schema 由 Rust 引擎首次啟動時自動建立$(NC)"

db-clean:
	@echo "$(RED)🗑️ 清除所有數據庫記錄（不可恢復！）$(NC)"
	@read -p "確認刪除? (yes/no) " confirm; \
	if [ "$$confirm" = "yes" ]; then \
		rm -f rust-engine/data/market_snapshots.db; \
		echo "$(GREEN)✓ 數據庫已清除$(NC)"; \
	else \
		echo "$(YELLOW)已取消$(NC)"; \
	fi

db-stats:
	@echo "$(BLUE)📊 數據庫統計$(NC)"
	@sqlite3 rust-engine/data/market_snapshots.db ".mode column" \
		"SELECT (SELECT COUNT(*) FROM dry_run_trades) AS dry_run_trades, \
		       (SELECT COUNT(*) FROM live_trades) AS live_trades, \
		       (SELECT COUNT(*) FROM cycle_results) AS cycle_results;"
	@echo ""
	@sqlite3 rust-engine/data/market_snapshots.db \
		"SELECT COUNT(DISTINCT DATE(ts)) FROM dry_run_trades;" | \
		xargs -I {} echo "$(GREEN)✓ Dry run 活躍天數: {} 天$(NC)"

# ═══════════════════════════════════════════════════════════════════════════
# 清理
# ═══════════════════════════════════════════════════════════════════════════

clean:
	@echo "$(BLUE)🧹 清除本機構建文件...$(NC)"
	cd rust-engine && cargo clean
	rm -rf python-analytics/src/dashboard/frontend/dist
	rm -rf python-analytics/src/dashboard/frontend/node_modules
	find . -type d -name __pycache__ -exec rm -rf {} + 2>/dev/null || true
	@echo "$(GREEN)✓ 清理完成$(NC)"

docker-clean:
	@echo "$(BLUE)🐳 移除所有 Docker 映像和容器...$(NC)"
	$(DOCKER_COMPOSE) down --rmi all -v || true
	@echo "$(GREEN)✓ Docker 清理完成$(NC)"

distclean: clean docker-clean
	@echo "$(BLUE)🌀 完全清理...$(NC)"
	@echo "$(GREEN)✓ 本機和 Docker 已清理$(NC)"

# ═══════════════════════════════════════════════════════════════════════════
# 開發工作流
# ═══════════════════════════════════════════════════════════════════════════

init: setup build-image db-init
	@echo "$(GREEN)✓ 初始化完成$(NC)"
	@echo "$(YELLOW)請確認 .env 已填入私鑰，再執行 $(GREEN)make docker-run$(NC)"

dev: build-rust build-python
	@echo "$(GREEN)✓ 開發環境已構建$(NC)"

watch:
	@echo "$(BLUE)👀 啟動 cargo watch（代碼自動重編）...$(NC)"
	cd rust-engine && cargo watch -x "build --release"

# ═══════════════════════════════════════════════════════════════════════════
# 實用工具
# ═══════════════════════════════════════════════════════════════════════════

env-check:
	@echo "$(BLUE)🔍 檢查環境配置...$(NC)"
	@echo ""
	@echo "Docker:"
	@docker --version || echo "$(RED)✗ Docker 未安裝$(NC)"
	@$(DOCKER_COMPOSE) --version || echo "$(RED)✗ Docker Compose 未安裝$(NC)"
	@echo ""
	@echo "Rust:"
	@rustc --version || echo "$(RED)✗ Rust 未安裝$(NC)"
	@cargo --version || echo "$(RED)✗ Cargo 未安裝$(NC)"
	@echo ""
	@echo "Python:"
	@python3 --version || echo "$(RED)✗ Python 未安裝$(NC)"
	@echo ""
	@if [ -f ".env" ]; then \
		echo "$(GREEN)✓ .env 已設置$(NC)"; \
	else \
		echo "$(RED)✗ .env 未設置（執行: cp .env.docker .env）$(NC)"; \
	fi
	@echo ""

version:
	@echo "Polymarket Engine v0.1.0"
	@echo "  Rust: $$(rustc --version | cut -d' ' -f2)"
	@echo "  Python: $$(python3 --version | cut -d' ' -f2)"
	@echo "  Docker: $$(docker --version | cut -d' ' -f3 | sed 's/,//')"

# ═══════════════════════════════════════════════════════════════════════════
# 備份和恢復
# ═══════════════════════════════════════════════════════════════════════════

backup:
	@echo "$(BLUE)💾 備份數據庫...$(NC)"
	@BACKUP_FILE="backups/market_snapshots.$$(date +%Y%m%d_%H%M%S).db"; \
	mkdir -p backups; \
	cp rust-engine/data/market_snapshots.db "$$BACKUP_FILE"; \
	echo "$(GREEN)✓ 備份完成: $$BACKUP_FILE$(NC)"

restore:
	@echo "$(BLUE)🔄 恢復數據庫...$(NC)"
	@ls -1 backups/*.db 2>/dev/null | sort -r | head -5 | nl
	@read -p "選擇備份編號 (1-5): " choice; \
	BACKUP_FILE=$$(ls -1 backups/*.db 2>/dev/null | sort -r | head -5 | sed -n "$${choice}p"); \
	if [ -z "$$BACKUP_FILE" ]; then echo "$(RED)無效選擇$(NC)"; exit 1; fi; \
	cp "$$BACKUP_FILE" rust-engine/data/market_snapshots.db; \
	echo "$(GREEN)✓ 已恢復: $$BACKUP_FILE$(NC)"
