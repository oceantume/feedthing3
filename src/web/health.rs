//! Health check for the server's slot switching and watchdog.

use std::time::Duration;

use axum::{extract::State, http::StatusCode};
use sqlx::Connection;

use super::AppState;

/// `GET /healthz`
///
/// 200 when the database answers within a second, 503 otherwise. Readiness
/// comes from startup: migrations run before the server listens, so nothing
/// answers here until they're done. Checked every few seconds, so cheap.
pub async fn check(State(app): State<AppState>) -> StatusCode {
    let ping = async { app.db.acquire().await?.ping().await };
    match tokio::time::timeout(Duration::from_secs(1), ping).await {
        Ok(Ok(())) => StatusCode::OK,
        _ => StatusCode::SERVICE_UNAVAILABLE,
    }
}
