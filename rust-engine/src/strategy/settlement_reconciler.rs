// Independent settlement reconciliation task.
//
// Background: Polymarket's UMA oracle finalises hours (sometimes days) after a
// market closes, so the 3-min in-cycle settlement poller in settlement.rs and
// the engine-restart fallback in weather_executor.rs miss most resolutions.
// As of phase-0 audit, 0/14 exits had a `SETTLEMENT` record on file.
//
// This reconciler runs as its own tokio task:
//   1. Every `interval_sec` seconds, scan weather_dry_run_trades for ENTRY rows
//      whose market has closed (close_ts <= now) and have no exit record.
//   2. For each, call gamma::fetch_token_settlement_price() to ask UMA.
//   3. If we get a settled price, write a SETTLEMENT row with realized_pnl.
//   4. If still unsettled and grace period exceeded, write a TIME_DECAY_EXIT
//      row using the entry-time model probability (avoid records lingering
//      forever in dashboards).
//
// DRY_RUN: writes go to weather_dry_run_trades (the dry_run table) — this is
// the same table the executors write to, so dashboards see consistent state.
// We do NOT touch live_trades — those are settled separately by the executors.

use std::time::Duration;

use crate::api::gamma;
use crate::config::BotConfig;
use crate::db::writer::{DbWriter, WeatherDryRunTrade};

/// Number of seconds after market close before we give up waiting for UMA
/// and write a TIME_DECAY_EXIT row using the model-estimated PnL.
/// 7 days is long enough for normal UMA disputes/resolution; beyond that the
/// market is effectively dead and we record an estimate to clear the queue.
const GRACE_PERIOD_SEC: i64 = 7 * 24 * 3600;

pub struct SettlementReconciler {
    config: BotConfig,
    db: DbWriter,
    interval_sec: u64,
}

impl SettlementReconciler {
    pub fn new(config: BotConfig, db: DbWriter) -> Self {
        Self {
            config,
            db,
            interval_sec: 30 * 60, // every 30 minutes
        }
    }

    pub async fn run_loop(self) {
        tracing::info!(
            "[SettlementReconciler] 啟動，每 {}s 掃描一次未結算 ENTRY",
            self.interval_sec
        );
        let mut interval = tokio::time::interval(Duration::from_secs(self.interval_sec));
        // First tick fires immediately; skip it so we don't double-run on startup
        // right after weather_executor's own startup recovery.
        interval.tick().await;
        loop {
            interval.tick().await;
            self.reconcile_once().await;
        }
    }

    async fn reconcile_once(&self) {
        let unresolved = self.db.load_unresolved_closed_weather_entries().await;
        if unresolved.is_empty() {
            tracing::debug!("[SettlementReconciler] 本輪無未結算 ENTRY");
            return;
        }
        tracing::info!(
            "[SettlementReconciler] 本輪掃描到 {} 筆未結算 ENTRY",
            unresolved.len()
        );

        let now_ts = chrono::Utc::now().timestamp();
        let mut settled = 0usize;
        let mut still_pending = 0usize;
        let mut force_closed = 0usize;

        for entry in unresolved {
            let secs_since_close = now_ts - entry.close_ts;

            match gamma::fetch_token_settlement_price(&entry.market_slug, &entry.token_id).await {
                Ok(Some(settled_price)) => {
                    let hold_sec = now_ts - entry.entry_ts;
                    let realized_pnl = (settled_price - entry.entry_price) * entry.size_usdc
                        - self.config.compute_fee(entry.size_usdc) * 2.0;
                    tracing::info!(
                        "[SettlementReconciler] SETTLEMENT {} side={} entry={:.4} settled={:.4} pnl={:+.4}",
                        entry.market_slug, entry.side, entry.entry_price, settled_price, realized_pnl
                    );
                    self.write_exit(&entry, "SETTLEMENT", settled_price, hold_sec, realized_pnl,
                        "settled by reconciler").await;
                    settled += 1;
                }
                Ok(None) if secs_since_close > GRACE_PERIOD_SEC => {
                    // Past grace period — record TIME_DECAY_EXIT with model estimate
                    // so dashboard's "unresolved" count drains.
                    let hold_sec = now_ts - entry.entry_ts;
                    let model_settle = if entry.side == "YES" {
                        entry.p_yes_at_entry
                    } else {
                        1.0 - entry.p_yes_at_entry
                    };
                    let realized_pnl = (model_settle - entry.entry_price) * entry.size_usdc
                        - self.config.compute_fee(entry.size_usdc) * 2.0;
                    tracing::warn!(
                        "[SettlementReconciler] FORCE_CLOSE {} (UMA 未結算 >{}d) 用模型估算 pnl={:+.4}",
                        entry.market_slug, GRACE_PERIOD_SEC / 86_400, realized_pnl
                    );
                    self.write_exit(&entry, "TIME_DECAY_EXIT", entry.entry_price, hold_sec,
                        realized_pnl, "force-closed past grace period").await;
                    force_closed += 1;
                }
                Ok(None) => {
                    tracing::debug!(
                        "[SettlementReconciler] {} 仍待 UMA 結算（已過 {}h）",
                        entry.market_slug, secs_since_close / 3600
                    );
                    still_pending += 1;
                }
                Err(e) => {
                    tracing::warn!(
                        "[SettlementReconciler] {} Gamma 查詢失敗: {e}",
                        entry.market_slug
                    );
                    still_pending += 1;
                }
            }
        }

        if settled + force_closed > 0 {
            tracing::info!(
                "[SettlementReconciler] 補結算 {} 筆，強制結束 {} 筆，仍等待 {} 筆",
                settled, force_closed, still_pending
            );
        }
    }

    async fn write_exit(
        &self,
        entry: &crate::db::writer::OpenWeatherEntry,
        action: &str,
        exit_price: f64,
        hold_sec: i64,
        realized_pnl: f64,
        note: &str,
    ) {
        let trade = WeatherDryRunTrade {
            strategy_id:            entry.strategy_id.clone(),
            event_id:               entry.event_id.clone(),
            market_slug:            entry.market_slug.clone(),
            city:                   entry.city.clone(),
            market_type:            entry.market_type.clone(),
            side:                   entry.side.clone(),
            action:                 action.into(),
            price:                  exit_price,
            size_usdc:              entry.size_usdc,
            spread_at_decision:     None,
            depth_usdc_at_decision: None,
            entry_price:            Some(entry.entry_price),
            exit_price:             Some(exit_price),
            hold_sec:               Some(hold_sec),
            model:                  entry.model.clone(),
            p_yes_at_entry:         Some(entry.p_yes_at_entry),
            p_yes_at_exit:          None,
            lead_days:              Some(entry.lead_days),
            taker_fee_bps:          None,
            slippage_buffer_bps:    None,
            expected_net_edge_bps:  Some(entry.expected_net_edge_bps),
            realized_pnl_usdc:      Some(realized_pnl),
            reason_code:            action.into(),
            note:                   Some(note.into()),
            token_id:               entry.token_id.clone(),
            close_ts:               entry.close_ts,
        };
        if let Err(e) = self.db.write_weather_dry_run_trade(&trade).await {
            tracing::error!(
                "[SettlementReconciler] 寫入 {} {} 失敗: {e}",
                action, entry.market_slug
            );
        }
    }
}
