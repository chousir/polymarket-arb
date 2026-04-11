// BotConfig + dry_run flag
// 從 .env 和 config/settings.toml 讀取設定

#[derive(Debug)]
pub struct BotConfig {
    pub dry_run: bool,
    pub monitor_window_sec: u64,
    pub dump_threshold_pct: f64,
    pub hedge_threshold_sum: f64,
    pub bet_size_usdc: f64,
    pub max_position_usdc: f64,
    pub hedge_wait_limit_sec: u64,
    pub abort_before_close_sec: u64,
}
