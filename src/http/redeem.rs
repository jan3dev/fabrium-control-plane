use std::{sync::Arc, time::Duration};

use axum::{
    Json,
    extract::State,
    http::{HeaderMap, header::AUTHORIZATION},
};
use tracing::Instrument;

use crate::{AppState, auth::bearer_token, bootstrap::Payload, ec2::Ec2, error::AppError};

/// The longest that the tag call runs. It runs after the answer, so a VM never waits for it.
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

    // The entry is gone, so the payload must reach the VM whatever the tag does. The tag
    // call runs in a task of its own: the answer does not wait for it, and a VM that closes
    // its connection cannot cancel it.
    tracing::info!(vm = %instance, "bootstrap payload delivered");
    tokio::spawn(mark_delivered(state.ec2.clone(), instance).in_current_span());
    Ok(Json(payload))
}

/// Sets the tag `fabrium:setup=delivered`. It only logs: the VM has its payload already.
///
/// ponytail: a failed tag leaves a delivered VM showing `expired`. Retry the tag from the
/// list if that matters.
async fn mark_delivered(ec2: Arc<dyn Ec2>, instance: String) {
    match tokio::time::timeout(TAG_TIMEOUT, ec2.mark_setup_delivered(&instance)).await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => {
            tracing::error!(vm = %instance, error = %format!("{e:#}"), "tagging the VM as delivered failed")
        }
        Err(_) => tracing::error!(vm = %instance, "tagging the VM as delivered timed out"),
    }
}
