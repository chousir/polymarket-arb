// 核心策略邏輯：Dump-Hedge 兩腿策略
// Leg 1：急跌觸發買入（dump_threshold_pct）
// Leg 2：Up+Down 總和 <= hedge_threshold_sum 時對沖
