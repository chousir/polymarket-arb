#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("API error: {0}")]
    ApiError(String),

    #[error("WebSocket error: {0}")]
    WsError(String),

    #[error("Database error: {0}")]
    DbError(#[from] rusqlite::Error),

    #[error("Config error: {0}")]
    ConfigError(String),

    #[error("not implemented")]
    NotImplemented,

    // ── Low-level From impls ──────────────────────────────────────────────────
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),

    /// Catch-all for errors without a dedicated variant (e.g. task JoinError).
    #[error("{0}")]
    Other(String),
}
