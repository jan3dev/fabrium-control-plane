use std::time::Duration;

use axum::{Json, extract::State, http::StatusCode};
use serde_json::{Value, json};

use crate::AppState;

/// Shorter than the request timeout, so a stalled database yields the JSON 503.
const PING_TIMEOUT: Duration = Duration::from_secs(2);

pub async fn health(State(state): State<AppState>) -> (StatusCode, Json<Value>) {
    match tokio::time::timeout(PING_TIMEOUT, state.db.ping()).await {
        Ok(Ok(())) => (StatusCode::OK, Json(json!({ "status": "ok" }))),
        failure => {
            match failure {
                Ok(Err(e)) => tracing::error!(error = %e, "health check failed"),
                _ => tracing::error!("health check timed out"),
            }
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({ "status": "unavailable" })),
            )
        }
    }
}
