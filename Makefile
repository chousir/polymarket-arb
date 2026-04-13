.PHONY: help build-image build-image-no-cache run-dry-run run-live run-dashboard stop logs ps clean db-init db-clean build-rust build-python test docker-clean push

# ═══════════════════════════════════════════════════════════════════════════
# Polymarket 自動交易系統 Makefile
# ═══════════════════════════════════════════════════════════════════════════

# 顏色定義
RED     := \033[0;31m
GREEN   := \033[0;32m
YELLOW  := \033[0;33m
BLUE    := \033[0;34m
NC      := \033[0m

# 默認值
DOCKER_REGISTRY ?= polymarket
IMAGE_NAME ?= polymarket-arb
IMAGE_TAG ?= latest
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
	@echo "$(YELLOW)📦 Docker 映像$(NC)"
	@echo "  $(GREEN)make build-image$(NC)           [預設] 構建 Docker 映像"
	@echo "  $(GREEN)make build-image-no-cache$(NC)  重新構建（不使用快取）"
	@echo "  $(GREEN)make push$(NC)                  推送映像到 Registry（需設置 DOCKER_REGISTRY）"
	@echo ""
	@echo "$(YELLOW)🚀 運行服務$(NC)"
	@echo "  $(GREEN)make run-dry-run$(NC)          啟動 Dry Run 模式（推薦先用）"
	@echo "  $(GREEN)make run-live$(NC)             啟動 Live 交易模式（需確認 CONFIRM_LIVE=1）"
	@echo "  $(GREEN)make run-dashboard$(NC)        僅啟動儀表板"
	@echo "  $(GREEN)make run-engine$(NC)           僅啟動 Rust 引擎"
	@echo ""
	@echo "$(YELLOW)🛑 停止服務$(NC)"
	@echo "  $(GREEN)make stop$(NC)                 停止所有容器"
	@echo "  $(GREEN)make down$(NC)                 停止並移除容器"
	@echo ""
	@echo "$(YELLOW)📊 監控$(NC)"
	@echo "  $(GREEN)make logs$(NC)                 查看即時日誌（可指定 SERVICE=app)"
	@echo "  $(GREEN)make ps$(NC)                   列出執行中的容器"
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
	@echo "$(BLUE)快速開始:$(NC)"
	@echo "  1. $(GREEN)make$(NC)                   # 構建映像（預設目標）"
	@echo "  2. $(GREEN)make run-dry-run$(NC)       # 啟動 Dry Run"
	@echo "  3. 訪問 http://localhost:8080 查看儀表板"
	@echo ""

# ═══════════════════════════════════════════════════════════════════════════
# DOCKER 映像相關
# ═══════════════════════════════════════════════════════════════════════════

build-image:
	@echo "$(BLUE)🔨 構建 Docker 映像: $(IMAGE_NAME):$(IMAGE_TAG)$(NC)"
	docker-compose build
	@echo "$(GREEN)✓ 映像構建完成$(NC)"

build-image-no-cache:
	@echo "$(BLUE)🔨 構建 Docker 映像（不使用快取）$(NC)"
	docker-compose build --no-cache
	@echo "$(GREEN)✓ 映像構建完成$(NC)"

push:
	@echo "$(BLUE)📤 推送映像到 $(DOCKER_REGISTRY)$(NC)"
	docker tag $(IMAGE_NAME):$(IMAGE_TAG) $(DOCKER_REGISTRY)/$(IMAGE_NAME):$(IMAGE_TAG)
	docker push $(DOCKER_REGISTRY)/$(IMAGE_NAME):$(IMAGE_TAG)
	@echo "$(GREEN)✓ 映像推送完成$(NC)"

# ═══════════════════════════════════════════════════════════════════════════
# 啟動服務
# ═══════════════════════════════════════════════════════════════════════════

run-dry-run:
	@echo "$(BLUE)🚀 啟動 Dry Run 模式...$(NC)"
	TRADING_MODE=dry_run docker-compose --profile all up -d app
	@echo "$(GREEN)✓ Dry Run 已啟動$(NC)"
	@echo "$(YELLOW)📊 訪問儀表板: http://localhost:8080$(NC)"
	@echo "$(YELLOW)📋 查看日誌: $(GREEN)make logs$(NC)"

run-live:
	@if [ -z "$(CONFIRM_LIVE)" ] || [ "$(CONFIRM_LIVE)" != "1" ]; then \
		echo "$(RED)❌ Live 模式需要設置 CONFIRM_LIVE=1 (防誤操作)$(NC)"; \
		echo "   執行: $(GREEN)make run-live CONFIRM_LIVE=1$(NC)"; \
		exit 1; \
	fi
	@echo "$(RED)⚠️ 啟動 LIVE 交易模式！確保已通過 7 天 dry_run 測試！$(NC)"
	@sleep 3
	TRADING_MODE=live CONFIRM_LIVE=1 docker-compose --profile engine up -d engine
	@echo "$(GREEN)✓ Live 引擎已啟動$(NC)"
	@echo "$(YELLOW)📋 查看日誌: $(GREEN)make logs SERVICE=engine$(NC)"

run-dashboard:
	@echo "$(BLUE)🚀 啟動儀表板...$(NC)"
	docker-compose --profile dashboard up -d dashboard
	@echo "$(GREEN)✓ 儀表板已啟動$(NC)"
	@echo "$(YELLOW)📊 訪問: http://localhost:8080$(NC)"

run-engine:
	@echo "$(BLUE)🚀 啟動 Rust 引擎（僅引擎，無儀表板）...$(NC)"
	TRADING_MODE=$(TRADING_MODE) docker-compose --profile engine up -d engine
	@echo "$(GREEN)✓ 引擎已啟動$(NC)"

# ═══════════════════════════════════════════════════════════════════════════
# 停止服務
# ═══════════════════════════════════════════════════════════════════════════

stop:
	@echo "$(BLUE)🛑 停止所有容器...$(NC)"
	docker-compose stop
	@echo "$(GREEN)✓ 容器已停止$(NC)"

down:
	@echo "$(BLUE)🛑 停止並移除容器...$(NC)"
	docker-compose down
	@echo "$(GREEN)✓ 容器已移除$(NC)"

restart:
	@echo "$(BLUE)🔄 重啟容器...$(NC)"
	docker-compose restart $(SERVICE)
	@echo "$(GREEN)✓ 容器已重啟$(NC)"

# ═══════════════════════════════════════════════════════════════════════════
# 監控和日誌
# ═══════════════════════════════════════════════════════════════════════════

logs:
	@SERVICE_NAME=$(SERVICE); \
	if [ -z "$$SERVICE_NAME" ]; then SERVICE_NAME="app"; fi; \
	echo "$(BLUE)📋 查看日誌: $$SERVICE_NAME$(NC)"; \
	docker-compose logs -f $$SERVICE_NAME

ps:
	@echo "$(BLUE)📦 執行中的容器$(NC)"
	docker-compose ps

shell:
	@SERVICE_NAME=$(SERVICE); \
	if [ -z "$$SERVICE_NAME" ]; then SERVICE_NAME="app"; fi; \
	echo "$(BLUE)🐚 進入容器 shell: $$SERVICE_NAME$(NC)"; \
	docker-compose exec $$SERVICE_NAME bash

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
	@echo "$(BLUE)💾 初始化數據庫...$(NC)"
	@if [ -f "rust-engine/data/market_snapshots.db" ]; then \
		echo "$(YELLOW)⚠️ 相同數據庫已存在$(NC)"; \
	else \
		sqlite3 rust-engine/data/market_snapshots.db < /dev/null; \
		echo "$(GREEN)✓ 數據庫已建立$(NC)"; \
	fi

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
	docker-compose down --rmi all -v || true
	@echo "$(GREEN)✓ Docker 清理完成$(NC)"

distclean: clean docker-clean
	@echo "$(BLUE)🌀 完全清理...$(NC)"
	@echo "$(GREEN)✓ 本機和 Docker 已清理$(NC)"

# ═══════════════════════════════════════════════════════════════════════════
# 開發工作流
# ═══════════════════════════════════════════════════════════════════════════

init: build-image build-python db-init
	@echo "$(GREEN)✓ 初始化完成，可執行 $(GREEN)make run-dry-run$(NC)"

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
	@docker-compose --version || echo "$(RED)✗ Docker Compose 未安裝$(NC)"
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
