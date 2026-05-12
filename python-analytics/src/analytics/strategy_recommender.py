# Strategy recommender — turns strategy_diagnosis output into actionable
# parameter-tuning suggestions. NEVER writes to settings.toml — only emits
# a Markdown report for human review.
#
# Heuristics:
#   - Group with n>=3 and win_rate < 25% → propose removing/restricting it
#   - Group with n>=3 and avg_pnl_per_usd < -0.10 → propose risk reduction
#   - Group with win_rate >= 60% AND n>=3 → propose increasing allocation
#   - lead_days bucket with 0% win → propose tightening weather_max_lead_days
#   - p_yes bucket with high n but 0 wins → flag model calibration issue
#
# Usage:
#   cd python-analytics
#   python -m src.analytics.strategy_recommender --days 90
#   python -m src.analytics.strategy_recommender --days 90 \
#       > ../docs/improvement-log/phase4-recommendations.md

from __future__ import annotations

import argparse
import io
import os
import sys
from dataclasses import dataclass

_HERE = os.path.dirname(__file__)
_ANALYTICS_ROOT = os.path.normpath(os.path.join(_HERE, "../.."))
if _ANALYTICS_ROOT not in sys.path:
    sys.path.insert(0, _ANALYTICS_ROOT)

from src.analytics.strategy_diagnosis import (  # noqa: E402
    DiagnosisReport, GroupRow, run as run_diagnosis,
)


@dataclass
class Recommendation:
    severity: str       # "critical" | "warning" | "info"
    target: str         # parameter or strategy_id being suggested
    rationale: str      # why this recommendation
    proposed_action: str
    evidence: str       # n, win_rate, pnl numbers


MIN_SAMPLE = 3   # require at least 3 resolved trades before recommending


def _hot_losses(group: list[GroupRow]) -> list[GroupRow]:
    return [g for g in group
            if g.n_resolved >= MIN_SAMPLE
            and (g.win_rate < 0.25 or g.avg_pnl_per_usd < -0.10)]


def _winners(group: list[GroupRow]) -> list[GroupRow]:
    return [g for g in group
            if g.n_resolved >= MIN_SAMPLE and g.win_rate >= 0.60
            and g.avg_pnl_per_usd > 0]


def generate(report: DiagnosisReport) -> list[Recommendation]:
    recs: list[Recommendation] = []

    if report.total_resolved < 5:
        recs.append(Recommendation(
            severity="info",
            target="sampling",
            rationale=f"已結算交易僅 {report.total_resolved} 筆，樣本太小，建議至少累積 30 筆再做正式調參。",
            proposed_action="繼續跑 dry_run 累積樣本；其他建議僅作參考",
            evidence=f"n_resolved={report.total_resolved}",
        ))

    # ── strategy_id ───────────────────────────────────────────────────────────
    for g in _hot_losses(report.groups.get("strategy_id", [])):
        recs.append(Recommendation(
            severity="critical" if g.avg_pnl_per_usd < -0.10 else "warning",
            target=f"strategy:{g.bucket}",
            rationale=f"策略 {g.bucket} 勝率 {g.win_rate*100:.0f}% 且平均虧損 {g.avg_pnl_per_usd*100:.1f}%/USDC。",
            proposed_action=f"在 config/settings.toml 將 [strategies.{g.bucket}] 設 enabled=false 直到樣本充足後重評",
            evidence=f"n={g.n_resolved} wins={g.n_wins} pnl={g.total_pnl_usdc:+.2f}",
        ))
    for g in _winners(report.groups.get("strategy_id", [])):
        recs.append(Recommendation(
            severity="info",
            target=f"strategy:{g.bucket}",
            rationale=f"策略 {g.bucket} 勝率 {g.win_rate*100:.0f}%，平均 +{g.avg_pnl_per_usd*100:.1f}%/USDC。",
            proposed_action=f"考慮提高 capital_allocation_pct（目前須人工查 settings.toml）",
            evidence=f"n={g.n_resolved} wins={g.n_wins} pnl={g.total_pnl_usdc:+.2f}",
        ))

    # ── model ────────────────────────────────────────────────────────────────
    for g in _hot_losses(report.groups.get("model", [])):
        recs.append(Recommendation(
            severity="warning",
            target=f"model:{g.bucket}",
            rationale=f"模型 {g.bucket} 連敗（勝率 {g.win_rate*100:.0f}%）。",
            proposed_action=f"檢查該模型策略的 min_model_confidence 是否設太低，或停用使用 {g.bucket} 的策略",
            evidence=f"n={g.n_resolved} pnl={g.total_pnl_usdc:+.2f}",
        ))

    # ── city ─────────────────────────────────────────────────────────────────
    for g in _hot_losses(report.groups.get("city", [])):
        recs.append(Recommendation(
            severity="warning",
            target=f"city:{g.bucket}",
            rationale=f"城市 {g.bucket} 勝率 {g.win_rate*100:.0f}%，可能預報精度差或市場結算規則特殊。",
            proposed_action=f"從受影響策略的 city_whitelist 移除 {g.bucket}，或縮小其 size_usdc",
            evidence=f"n={g.n_resolved} pnl={g.total_pnl_usdc:+.2f}",
        ))

    # ── side ─────────────────────────────────────────────────────────────────
    sides = report.groups.get("side", [])
    for g in _hot_losses(sides):
        recs.append(Recommendation(
            severity="warning",
            target=f"side:{g.bucket}",
            rationale=f"買 {g.bucket} 平均虧損 {g.avg_pnl_per_usd*100:.1f}%/USDC，方向性問題。",
            proposed_action=(
                f"YES 端虧損可能源於模型對 p_yes 過度樂觀，"
                f"NO 端虧損可能源於模型對極端事件估算不足；"
                f"建議在 weather_decision.rs 中對應方向加入額外安全邊際"
            ),
            evidence=f"n={g.n_resolved} pnl={g.total_pnl_usdc:+.2f}",
        ))

    # ── lead_days_bucket ─────────────────────────────────────────────────────
    for g in _hot_losses(report.groups.get("lead_days_bucket", [])):
        recs.append(Recommendation(
            severity="warning",
            target=f"lead_days:{g.bucket}",
            rationale=f"提前 {g.bucket} 進場勝率 {g.win_rate*100:.0f}%。",
            proposed_action=(
                f"如為高 lead_days，降低相關策略的 weather_max_lead_days；"
                f"如為 same-day 而虧損，可能源自 TIME_DECAY_EXIT 過晚平倉，"
                f"建議檢查 abort_before_close_sec 設定"
            ),
            evidence=f"n={g.n_resolved} pnl={g.total_pnl_usdc:+.2f}",
        ))

    # ── p_yes_at_entry_bucket ────────────────────────────────────────────────
    p_buckets = report.groups.get("p_yes_at_entry_bucket", [])
    overcalibrated = [g for g in p_buckets
                      if g.n_resolved >= MIN_SAMPLE
                      and g.bucket in ("0.80-1.00", "0.00-0.20")
                      and g.win_rate < 0.30]
    for g in overcalibrated:
        recs.append(Recommendation(
            severity="warning",
            target=f"model_calibration:{g.bucket}",
            rationale=(
                f"進場時模型機率落在極端區段 ({g.bucket}) 但實際勝率僅 "
                f"{g.win_rate*100:.0f}% — 模型對極端事件過度自信。"
            ),
            proposed_action=(
                "提高 sigma（模型誤差假設），或對極端 p_yes 區段加入 "
                "額外的「實際歷史命中率」校正係數"
            ),
            evidence=f"n={g.n_resolved} pnl={g.total_pnl_usdc:+.2f}",
        ))

    # ── action breakdown observations ────────────────────────────────────────
    actions = report.groups.get("action", [])
    sl_row = next((a for a in actions if a.bucket == "STOP_LOSS"), None)
    td_row = next((a for a in actions if a.bucket == "TIME_DECAY_EXIT"), None)
    if sl_row and sl_row.n_resolved >= MIN_SAMPLE and sl_row.avg_pnl_per_usd < -0.15:
        recs.append(Recommendation(
            severity="warning",
            target="stop_loss_delta",
            rationale=(
                f"STOP_LOSS 觸發 {sl_row.n_resolved} 次，平均虧 "
                f"{sl_row.avg_pnl_per_usd*100:.1f}%/USDC，"
                f"代表 stop_loss_delta 可能設太寬。"
            ),
            proposed_action="把相關策略的 stop_loss_delta 從 0.12 試降至 0.08",
            evidence=f"n={sl_row.n_resolved} pnl={sl_row.total_pnl_usdc:+.2f}",
        ))
    if td_row and td_row.n_resolved >= MIN_SAMPLE and td_row.win_rate < 0.20:
        recs.append(Recommendation(
            severity="info",
            target="time_decay_exit",
            rationale=(
                f"TIME_DECAY_EXIT 觸發 {td_row.n_resolved} 次但勝率低，"
                f"可能源於用模型機率估算的 PnL 不夠保守。"
            ),
            proposed_action=(
                "等批次 2 的 SettlementReconciler 跑滿 24h 後，"
                "用 SETTLEMENT 真實結算價重新計算這些紀錄"
            ),
            evidence=f"n={td_row.n_resolved} win%={td_row.win_rate*100:.0f}",
        ))

    return recs


def to_markdown(report: DiagnosisReport, recs: list[Recommendation]) -> str:
    out = io.StringIO()
    out.write("# Phase 4：策略改進建議書（自動產出）\n\n")
    out.write("> 本文件由 `strategy_recommender.py` 自動生成。**所有建議都需人工審閱後再修改 config/settings.toml**。\n\n")
    out.write("## 全局指標\n\n")
    out.write(f"- 樣本：最近 **{report.days}** 天的 **{report.total_resolved}** 筆已結算交易\n")
    out.write(f"- 總 PnL：**{report.total_pnl_usdc:+.4f} USDC**\n")
    out.write(f"- 整體勝率：**{report.overall_win_rate * 100:.1f}%**\n")
    out.write(f"- 平均 PnL/USDC：**{report.overall_avg_pnl_per_usd:+.5f}**\n\n")

    if not recs:
        out.write("**目前無明顯異常需要調整。**\n")
        return out.getvalue()

    crit = [r for r in recs if r.severity == "critical"]
    warn = [r for r in recs if r.severity == "warning"]
    info = [r for r in recs if r.severity == "info"]

    def section(label: str, items: list[Recommendation]) -> None:
        if not items:
            return
        out.write(f"## {label}\n\n")
        for r in items:
            out.write(f"### `{r.target}`\n\n")
            out.write(f"- **原因**：{r.rationale}\n")
            out.write(f"- **建議**：{r.proposed_action}\n")
            out.write(f"- **證據**：{r.evidence}\n\n")

    section("🚨 Critical（需要立即處理）", crit)
    section("⚠️ Warning（建議審視）", warn)
    section("ℹ️ Info（觀察提示）", info)
    return out.getvalue()


def main() -> None:
    ap = argparse.ArgumentParser(description="Strategy improvement recommender")
    ap.add_argument("--db", default=None)
    ap.add_argument("--days", type=int, default=90)
    args = ap.parse_args()

    db = args.db
    if db is None:
        repo_root = os.path.normpath(os.path.join(_HERE, "../../.."))
        db = os.path.join(repo_root, "rust-engine", "data", "market_snapshots.db")
    if not os.path.exists(db):
        print(f"[ERROR] DB not found: {db}", file=sys.stderr)
        sys.exit(1)

    report = run_diagnosis(db, args.days)
    recs = generate(report)
    print(to_markdown(report, recs))


if __name__ == "__main__":
    main()
