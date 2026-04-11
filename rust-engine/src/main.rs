mod api;
mod config;
mod db;
mod error;
mod execution;
mod ipc;
mod rate_limit;
mod risk;
mod strategy;
mod ws;

#[tokio::main]
async fn main() -> Result<(), error::AppError> {
    tracing_subscriber::fmt::init();
    tracing::info!("Polymarket engine starting...");
    Ok(())
}
