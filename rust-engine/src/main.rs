mod api;
mod config;
mod db;
mod error;
mod execution;
mod ipc;
mod notify;
mod rate_limit;
mod risk;
mod strategy;
mod ws;

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc;
use tracing::Level;

use crate::config::{BotConfig, StrategyConfig, StrategyType};
use crate::db::writer::DbWriter;
use crate::error::AppError;
use crate::notify::telegram::Notifier;
use crate::risk::capital::{self, SharedCapital};
use crate::risk::circuit_breaker::{self, SharedBreaker};
use crate::ws::market_feed::PriceSnapshot;

// ── Entry point ───────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<(), AppError> {
    tracing_subscriber::fmt()
        .with_max_level(Level::INFO)
        .init();

    // ── 1. Load config + apply CLI overrides ──────────────────────────────────
    let args: Vec<String> = std::env::args().collect();
    let mut config = BotConfig::load()?;

    if let Some(pos) = args.iter().position(|a| a == "--mode") {
        if let Some(mode_str) = args.get(pos + 1) {
            match mode_str.as_str() {
                "dry_run" => {
                    config.mode = config::TradingMode::DryRun;
                    config.dry_run = true;
                }
                "live" => {
                    config.mode = config::TradingMode::Live;
                    config.dry_run = false;
                }
                other => {
                    tracing::warn!("[Main] 未知模式 '{other}'，繼續使用設定檔預設值");
                }
            }
        }
    }

    // ── 2. Live-mode guard ────────────────────────────────────────────────────
    if config.mode == config::TradingMode::Live {
        if !BotConfig::is_live_confirmed(&args) {
            tracing::error!("[Main] live 模式需加 --confirm-live 旗標才能啟動，退出。");
            return Ok(());
        }
        tracing::warn!("[Main] ⚠  LIVE MODE 已確認啟動");
    } else {
        tracing::info!("[Main] DRY_RUN 模式 — 訂單不會提交至 CLOB");
    }

    // ── 3. DB init ────────────────────────────────────────────────────────────
    if config.db_path != ":memory:" {
        if let Some(parent) = std::path::Path::new(&config.db_path).parent() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let db = DbWriter::open(&config.db_path)?;
    tracing::info!("[DB] 已開啟資料庫: {}", config.db_path);

    // ── 4. Per-strategy capital trackers ──────────────────────────────────────
    let mut capitals: HashMap<String, SharedCapital> = HashMap::new();
    for sc in config.strategies.iter().filter(|s| s.enabled) {
        let cap = capital::new_shared(sc);
        {
            let c = cap.lock().expect("capital mutex poisoned");
            tracing::info!(
                "[Capital:{}] 初始資金={:.2} USDC  drawdown_stop={:.0}%  bet_size={:.2} USDC",
                sc.id,
                c.current_capital,
                sc.max_drawdown_pct * 100.0,
                c.current_bet_size(),
            );
        }
        capitals.insert(sc.id.clone(), cap);
    }

    // ── 5. Live: override capital with on-chain USDC balance ─────────────────
    if config.mode == config::TradingMode::Live {
        let wallet = std::env::var("POLYGON_PUBLIC_KEY").unwrap_or_default();
        if wallet.is_empty() {
            tracing::warn!("[Main] POLYGON_PUBLIC_KEY 未設定，使用設定檔初始資金");
        } else {
            match api::polygon::fetch_usdc_balance(&config.polygon_rpc_url, &wallet).await {
                Ok(balance) => {
                    // Distribute on-chain balance proportionally across strategies
                    let total_alloc: f64 = config
                        .strategies
                        .iter()
                        .filter(|s| s.enabled)
                        .map(|s| s.capital_allocation_pct)
                        .sum();
                    for (id, cap) in &capitals {
                        if let Some(sc) = config.strategies.iter().find(|s| &s.id == id) {
                            let allocated = if total_alloc > 0.0 {
                                balance * (sc.capital_allocation_pct / total_alloc)
                            } else {
                                0.0
                            };
                            cap.lock()
                                .expect("capital mutex poisoned")
                                .override_from_onchain(allocated);
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        "[Main] Polygon USDC 查詢失敗: {e}，使用設定檔初始資金"
                    );
                }
            }
        }
    }

    // ── 6. Log active strategies ──────────────────────────────────────────────
    let active_count = config.strategies.iter().filter(|s| s.enabled).count();
    tracing::info!(
        "[Main] 已載入 {} 個策略，其中 {} 個啟用",
        config.strategies.len(),
        active_count
    );
    for s in &config.strategies {
        if s.enabled {
            tracing::info!(
                "[Main]   ✓ {}  type={:?}  sum_threshold={}  alloc={:.0}%",
                s.id,
                s.strategy_type,
                s.hedge_threshold_sum,
                s.capital_allocation_pct * 100.0,
            );
        } else {
            tracing::info!("[Main]   ✗ {} (disabled)", s.id);
        }
    }

    if active_count == 0 {
        tracing::error!("[Main] 沒有啟用的策略，請檢查 config/settings.toml，退出。");
        return Ok(());
    }

    // ── 7. Telegram notifier (optional) ──────────────────────────────────────
    let telegram = Notifier::new(
        config.telegram_bot_token.clone(),
        std::env::var("TELEGRAM_CHAT_ID").unwrap_or_default(),
    );
    if telegram.is_some() {
        tracing::info!("[TG] Telegram 告警已啟用");
    } else {
        tracing::info!("[TG] Telegram 未設定（TELEGRAM_BOT_TOKEN / TELEGRAM_CHAT_ID 為空）");
    }

    // ── 8. Circuit breaker ────────────────────────────────────────────────────
    let breaker: SharedBreaker = circuit_breaker::new_shared(config.max_daily_loss_usdc);
    tracing::info!(
        "[CB] Circuit breaker 初始化  max_daily_loss={:.2} USDC",
        config.max_daily_loss_usdc
    );

    // ── 9. Binance WS → shared atomic BTC last price ─────────────────────────
    let btc_price = Arc::new(AtomicU64::new(0));
    {
        let btc_price = Arc::clone(&btc_price);
        let tg = telegram.clone();
        let mut btc_rx = api::binance::connect_binance_ws();
        tokio::spawn(async move {
            let mut last_ok = true;
            while let Some(tick) = btc_rx.recv().await {
                if tick.last_price > 0.0 {
                    if !last_ok {
                        last_ok = true;
                        if let Some(t) = &tg {
                            t.notify_alert("🔌", "Binance WS 重新連線", "BTC 報價已恢復").await;
                        }
                    }
                    btc_price.store(tick.last_price.to_bits(), Ordering::Relaxed);
                    tracing::debug!("[BTC] last={:.2}", tick.last_price);
                }
            }
            tracing::error!("[BTC] Binance WS 已斷線且不再重連");
            if let Some(t) = &tg {
                t.notify_alert(
                    "🚨",
                    "Binance WS 斷線",
                    "超過最大重連次數，BTC 報價停止更新",
                )
                .await;
            }
        });
    }

    // ── 10. Stats task: log every 30 s ────────────────────────────────────────
    {
        let db_stats = db.clone();
        let breaker_stats = Arc::clone(&breaker);
        let capitals_stats = capitals
            .iter()
            .map(|(id, cap)| (id.clone(), Arc::clone(cap)))
            .collect::<HashMap<_, _>>();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(30));
            loop {
                interval.tick().await;
                let trades = db_stats.count_dry_run_trades().await.unwrap_or(-1);
                let cycles = db_stats.count_cycle_results().await.unwrap_or(-1);
                let daily_pnl = breaker_stats.lock().unwrap().daily_pnl();
                tracing::info!(
                    "[Stats] 已完成週期={cycles}  dry_run_trades={trades}  \
                     今日PnL={daily_pnl:+.4} USDC"
                );
                for (id, cap) in &capitals_stats {
                    let c = cap.lock().unwrap();
                    tracing::info!(
                        "[Capital:{}] current={:.4}  drawdown={:.1}%  fees_paid={:.4}  \
                         est_pnl={:.4}{}",
                        id,
                        c.current_capital,
                        c.drawdown_pct() * 100.0,
                        c.total_fees_paid,
                        c.total_estimated_pnl,
                        if c.is_stopped() { "  ⛔ STOPPED" } else { "" },
                    );
                }
            }
        });
    }

    // ── 11. Spawn Mention strategies as independent background tasks ──────────
    {
        let mention_strategies: Vec<_> = config
            .strategies
            .iter()
            .filter(|s| s.enabled && s.strategy_type == StrategyType::Mention)
            .collect();

        for sc in mention_strategies {
            let global = config.clone();
            let sc = sc.clone();
            let cap = capitals
                .get(&sc.id)
                .map(Arc::clone)
                .expect("capital tracker missing for mention strategy");
            let db = db.clone();

            tracing::info!("[Mention:{}] 啟動背景掃描任務", sc.id);
            tokio::spawn(async move {
                strategy::mention_executor::MentionStrategy::new(global, sc, cap)
                    .run_loop(&db)
                    .await;
            });
        }
    }

    // ── 11b. Spawn Weather strategies as independent background tasks ─────────
    {
        let weather_strategies: Vec<_> = config
            .strategies
            .iter()
            .filter(|s| s.enabled && s.strategy_type == StrategyType::Weather)
            .collect();

        for sc in weather_strategies {
            let global = config.clone();
            let sc = sc.clone();
            let cap = capitals
                .get(&sc.id)
                .map(Arc::clone)
                .expect("capital tracker missing for weather strategy");
            let db = db.clone();

            tracing::info!("[Weather:{}] 啟動背景掃描任務", sc.id);
            tokio::spawn(async move {
                strategy::weather_executor::WeatherStrategy::new(global, sc, cap)
                    .run_loop(&db)
                    .await;
            });
        }
    }

    // ── 11c. Spawn WeatherLadder strategies as independent background tasks ────
    {
        let ladder_strategies: Vec<_> = config
            .strategies
            .iter()
            .filter(|s| s.enabled && s.strategy_type == StrategyType::WeatherLadder)
            .collect();

        for sc in ladder_strategies {
            let global = config.clone();
            let sc = sc.clone();
            let cap = capitals
                .get(&sc.id)
                .map(Arc::clone)
                .expect("capital tracker missing for weather ladder strategy");
            let db = db.clone();

            tracing::info!("[Ladder:{}] 啟動背景掃描任務", sc.id);
            tokio::spawn(async move {
                strategy::weather_ladder_executor::WeatherLadderStrategy::new(global, sc, cap)
                    .run_loop(&db)
                    .await;
            });
        }
    }

    // ── 12. Main cycle loop (BTC strategies only) ─────────────────────────────
    tracing::info!("[Main] 進入主循環");
    if let Some(t) = &telegram {
        t.notify_alert(
            "🚀",
            "引擎啟動",
            &format!(
                "模式: {}  策略數: {}  每日虧損上限: {:.2} USDC",
                if config.is_dry_run() { "DRY_RUN" } else { "LIVE" },
                active_count,
                config.max_daily_loss_usdc
            ),
        )
        .await;
    }

    loop {
        // ── Circuit breaker gate ──────────────────────────────────────────────
        {
            let cb = breaker.lock().unwrap();
            if cb.is_tripped() {
                tracing::warn!("[Main] Circuit breaker 已觸發，跳過本循環（30s 後重檢）");
                drop(cb);
                tokio::time::sleep(Duration::from_secs(30)).await;
                continue;
            }
        }

        // ── Resolve current market window ─────────────────────────────────────
        let slug = api::gamma::current_slug();
        let next_open = api::gamma::next_open_ts();
        tracing::info!("[Main] 目前 slug={slug}  下一視窗={next_open}");

        let info = match api::gamma::fetch_market(&slug).await {
            Ok(i) => {
                tracing::info!(
                    "[Gamma] slug={}  up={}  down={}  close_ts={}",
                    i.slug,
                    i.up_token_id,
                    i.down_token_id,
                    i.close_ts,
                );
                i
            }
            Err(e) => {
                tracing::error!("[Gamma] 市場查詢失敗: {e}，30 秒後重試");
                if let Some(t) = &telegram {
                    t.notify_alert("⚠️", "Gamma API 失敗", &e.to_string()).await;
                }
                tokio::time::sleep(Duration::from_secs(30)).await;
                continue;
            }
        };

        // ── Skip expired windows ──────────────────────────────────────────────
        let now_ts = chrono::Utc::now().timestamp() as u64;
        if info.close_ts <= now_ts {
            let wait_secs = next_open.saturating_sub(now_ts) + 2;
            tracing::info!("[Main] 市場已關閉，等待 {wait_secs}s 至下一視窗...");
            tokio::time::sleep(Duration::from_secs(wait_secs)).await;
            continue;
        }

        // ── Subscribe + build combined feed, then fan out ─────────────────────
        // Exclude Mention, Weather, WeatherLadder — they run in their own background tasks.
        let active: Vec<(StrategyConfig, SharedCapital)> = config
            .strategies
            .iter()
            .filter(|s| {
                s.enabled
                    && s.strategy_type != StrategyType::Mention
                    && s.strategy_type != StrategyType::Weather
                    && s.strategy_type != StrategyType::WeatherLadder
            })
            .filter_map(|s| {
                capitals.get(&s.id).map(|cap| (s.clone(), Arc::clone(cap)))
            })
            .collect();

        let pm_rx = ws::market_feed::MarketFeed::new(
            info.up_token_id.clone(),
            info.down_token_id.clone(),
        )
        .subscribe();
        let combined_rx = make_combined_feed(pm_rx, Arc::clone(&btc_price));
        let strategy_rxs = fanout_feed(combined_rx, active.len());

        // ── Spawn all strategies in parallel ──────────────────────────────────
        let handles: Vec<_> = active
            .into_iter()
            .zip(strategy_rxs.into_iter())
            .map(|((sc, cap), rx)| {
                let global = config.clone();
                let info = info.clone();
                let db = db.clone();
                tokio::spawn(async move {
                    match sc.strategy_type {
                        StrategyType::DumpHedge => {
                            strategy::dump_hedge::DumpHedgeStrategy::new(global, sc, cap)
                                .run_market_cycle(&info, &db, rx)
                                .await
                        }
                        StrategyType::PureArb => {
                            strategy::pure_arb::PureArbStrategy::new(global, sc, cap)
                                .run_market_cycle(&info, &db, rx)
                                .await
                        }
                        // Mention, Weather, WeatherLadder are spawned as persistent
                        // background tasks above and never reach this BTC cycle fanout.
                        StrategyType::Mention
                        | StrategyType::Weather
                        | StrategyType::WeatherLadder => unreachable!(
                            "Mention/Weather/WeatherLadder strategy should not appear in BTC cycle fanout"
                        ),
                    }
                })
            })
            .collect();

        // ── Collect results ────────────────────────────────────────────────────
        for handle in handles {
            match handle.await {
                Ok(Ok(result)) => {
                    tracing::info!(
                        "[Main] 策略完成  id={}  leg1={:?}  leg2={:?}  pnl={:?}",
                        result.strategy_id,
                        result.leg1_price,
                        result.leg2_price,
                        result.pnl_usdc,
                    );

                    let events = breaker.lock().unwrap().record_cycle(&result);
                    let daily_pnl = breaker.lock().unwrap().daily_pnl();

                    if let Some(t) = &telegram {
                        t.notify_cycle_end(&result, daily_pnl).await;
                        for event in &events {
                            t.notify_breaker(event).await;
                        }
                    }
                    for event in &events {
                        tracing::warn!("[CB] 事件: {event:?}");
                    }
                }
                Ok(Err(e)) => {
                    tracing::error!("[Main] 策略執行錯誤: {e}");
                    if let Some(t) = &telegram {
                        t.notify_alert("❌", "策略執行錯誤", &e.to_string()).await;
                    }
                }
                Err(e) => {
                    tracing::error!("[Main] 策略 task panic: {e}");
                }
            }
        }

        // ── Wait for next window ──────────────────────────────────────────────
        let now_ts = chrono::Utc::now().timestamp() as u64;
        let wait_secs = next_open.saturating_sub(now_ts) + 2;
        if wait_secs > 0 {
            tracing::info!("[Main] 等待 {wait_secs}s 至下一視窗...");
            tokio::time::sleep(Duration::from_secs(wait_secs)).await;
        }
    }
}

// ── Combined feed helper ──────────────────────────────────────────────────────

fn make_combined_feed(
    mut pm_rx: mpsc::Receiver<PriceSnapshot>,
    btc_price: Arc<AtomicU64>,
) -> mpsc::Receiver<PriceSnapshot> {
    let (tx, rx) = mpsc::channel(64);
    tokio::spawn(async move {
        while let Some(mut snap) = pm_rx.recv().await {
            let bits = btc_price.load(Ordering::Relaxed);
            snap.btc_last = f64::from_bits(bits);
            if tx.send(snap).await.is_err() {
                break;
            }
        }
    });
    rx
}

// ── Fanout: one source → N independent receivers ──────────────────────────────
///
/// 每個啟用的策略都會收到完全相同的 PriceSnapshot 副本，互不干擾。
fn fanout_feed(
    mut source: mpsc::Receiver<PriceSnapshot>,
    count: usize,
) -> Vec<mpsc::Receiver<PriceSnapshot>> {
    if count == 0 {
        return vec![];
    }
    if count == 1 {
        // Optimise: no copying needed for a single consumer
        return vec![{
            let (tx, rx) = mpsc::channel(64);
            tokio::spawn(async move {
                while let Some(snap) = source.recv().await {
                    if tx.send(snap).await.is_err() {
                        break;
                    }
                }
            });
            rx
        }];
    }

    let mut txs = Vec::with_capacity(count);
    let mut rxs = Vec::with_capacity(count);
    for _ in 0..count {
        let (tx, rx) = mpsc::channel::<PriceSnapshot>(64);
        txs.push(tx);
        rxs.push(rx);
    }

    tokio::spawn(async move {
        while let Some(snap) = source.recv().await {
            for tx in &txs {
                // best-effort: if a strategy's channel is full, drop that tick for it
                let _ = tx.try_send(snap.clone());
            }
        }
        // source closed → txs drop here, closing all downstream receivers
    });

    rxs
}
