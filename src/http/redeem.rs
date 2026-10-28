use std::{sync::Arc, time::Duration};

use axum::{
    Json,
    extract::State,
    http::{HeaderMap, header::AUTHORIZATION},
};
use tracing::Instrument;

use crate::{
    AppState,
    auth::{bearer_token, client_address},
    bootstrap::{Payload, Redeemed},
    ec2::Ec2,
    error::AppError,
};

/// The longest that the tag call runs.
const TAG_TIMEOUT: Duration = Duration::from_secs(5);

/// `POST /v1/bootstrap`: a new VM redeems its token. No tenant key.
pub async fn redeem(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Payload>, AppError> {
    // Every failure is the same 404: no header, an unknown or ended token, a foreign repeat.
    let token = (headers.get(AUTHORIZATION))
        .and_then(|v| v.to_str().ok())
        .and_then(bearer_token)
        .ok_or(AppError::NotFound)?;
    let from = client_address(&headers).map_or_else(|| "unknown".to_owned(), |ip| ip.to_string());
    match state
        .bootstrap
        .redeem(token, &from)
        .ok_or(AppError::NotFound)?
    {
        Redeemed::Payload(payload, instance) => {
            // Own task: the answer skips the wait, and a closed connection cannot cancel the tag.
            tracing::info!(vm = %instance, from = %from, "bootstrap payload delivered");
            tokio::spawn(mark_delivered(state.ec2.clone(), instance).in_current_span());
            Ok(Json(payload))
        }
        Redeemed::Refused { instance, first } => {
            // The token reached someone else than the VM. An operator must see it.
            tracing::warn!(vm = %instance, from = %from, first = %first, "refused a repeat redeem from another address");
            Err(AppError::NotFound)
        }
    }
}

/// Sets the tag `fabrium:setup=delivered`. It only logs: the VM has its payload already.
async fn mark_delivered(ec2: Arc<dyn Ec2>, instance: String) {
    match tokio::time::timeout(TAG_TIMEOUT, ec2.mark_setup_delivered(&instance)).await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => {
            tracing::error!(vm = %instance, error = %format!("{e:#}"), "tagging the VM as delivered failed")
        }
        Err(_) => tracing::error!(vm = %instance, "tagging the VM as delivered timed out"),
    }
}
