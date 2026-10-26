use std::time::Duration;

use axum::{
    Json,
    extract::State,
    http::{HeaderMap, header::AUTHORIZATION},
};

use crate::{AppState, auth::bearer_token, bootstrap::Payload, error::AppError};

/// Shorter than the request timeout (10 s). If the timeout cancelled the handler after the
/// entry is gone, the VM would lose its payload for good.
const TAG_TIMEOUT: Duration = Duration::from_secs(5);

/// `POST /v1/bootstrap`: a new VM redeems its single-use token. No tenant key.
pub async fn redeem(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Payload>, AppError> {
    // Every failure is the same 404: a missing header, an unknown, used, or expired token.
    let token = (headers.get(AUTHORIZATION))
        .and_then(|v| v.to_str().ok())
        .and_then(bearer_token)
        .ok_or(AppError::NotFound)?;
    let (payload, instance) = state.bootstrap.redeem(token).ok_or(AppError::NotFound)?;

    // The entry is gone, so the payload must reach the VM whatever the tag does.
    // ponytail: a failed tag leaves a delivered VM showing `expired`. Retry the tag from
    // the list if that matters.
    match tokio::time::timeout(TAG_TIMEOUT, state.ec2.mark_setup_delivered(&instance)).await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => {
            tracing::error!(vm = %instance, error = %format!("{e:#}"), "tagging the VM as delivered failed")
        }
        Err(_) => tracing::error!(vm = %instance, "tagging the VM as delivered timed out"),
    }
    tracing::info!(vm = %instance, "bootstrap payload delivered");
    Ok(Json(payload))
}
