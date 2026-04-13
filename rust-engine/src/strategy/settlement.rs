use std::time::Duration;

use crate::api::gamma::{self, ResolvedOutcome};
use crate::config::BotConfig;

pub struct SettlementResult {
    pub resolved_winner: Option<String>,
    pub pnl_usdc: Option<f64>,
}

pub async fn resolve_updown_pnl(
    global: &BotConfig,
    market_slug: &str,
    close_ts: u64,
    leg1_price: Option<f64>,
    leg2_price: Option<f64>,
    leg_bet_size: f64,
) -> Result<SettlementResult, crate::error::AppError> {
    if leg1_price.is_none() {
        return Ok(SettlementResult {
            resolved_winner: None,
            pnl_usdc: None,
        });
    }

    let now_ts = chrono::Utc::now().timestamp() as u64;
    if close_ts > now_ts {
        tokio::time::sleep(Duration::from_secs(close_ts - now_ts)).await;
    }

    // Gamma resolution can lag market close; poll for up to 3 minutes.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(180);
    let mut winner: Option<ResolvedOutcome> = None;

    while tokio::time::Instant::now() < deadline {
        match gamma::fetch_resolved_outcome(market_slug).await {
            Ok(Some(outcome)) => {
                winner = Some(outcome);
                break;
            }
            Ok(None) => {}
            Err(e) => {
                tracing::warn!(
                    "[Settlement] 讀取結算結果失敗 slug={} err={}",
                    market_slug,
                    e
                );
            }
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
    }

    let Some(winner) = winner else {
        tracing::info!(
            "[Settlement] 結算超時未取得 winner slug={}（保留未結算）",
            market_slug
        );
        return Ok(SettlementResult {
            resolved_winner: None,
            pnl_usdc: None,
        });
    };

    let fee_per_leg = global.compute_fee(leg_bet_size);
    let pnl = compute_pnl_for_updown(winner.clone(), leg1_price, leg2_price, leg_bet_size, fee_per_leg);

    let resolved_winner = Some(match winner {
        ResolvedOutcome::Up => "Up".to_string(),
        ResolvedOutcome::Down => "Down".to_string(),
    });

    Ok(SettlementResult {
        resolved_winner,
        pnl_usdc: Some(pnl),
    })
}

fn compute_pnl_for_updown(
    winner: ResolvedOutcome,
    leg1_price: Option<f64>,
    leg2_price: Option<f64>,
    leg_bet_size: f64,
    fee_per_leg: f64,
) -> f64 {
    let mut payout = 0.0;
    let mut total_cost = 0.0;
    let mut total_fees = 0.0;

    if let Some(p1) = leg1_price {
        total_cost += leg_bet_size;
        total_fees += fee_per_leg;
        if winner == ResolvedOutcome::Up {
            payout += leg_bet_size / p1;
        }
    }

    if let Some(p2) = leg2_price {
        total_cost += leg_bet_size;
        total_fees += fee_per_leg;
        if winner == ResolvedOutcome::Down {
            payout += leg_bet_size / p2;
        }
    }

    payout - total_cost - total_fees
}
