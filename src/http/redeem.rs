use std::{net::IpAddr, sync::Arc, time::Duration};

use axum::{
    Json,
    extract::State,
    http::{HeaderMap, header::AUTHORIZATION},
};
use tracing::Instrument;

use crate::{
    AppState,
    auth::bearer_token,
    bootstrap::{Payload, Redeemed},
    ec2::Ec2,
    error::AppError,
};

/// The longest that the tag call runs. It runs after the answer, so a VM never waits for it.
const TAG_TIMEOUT: Duration = Duration::from_secs(5);

/// `POST /v1/bootstrap`: a new VM redeems its token. No tenant key. A repeat from the same
/// address within the grace time of the store gets the same payload, for a VM whose first
/// answer got lost.
pub async fn redeem(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Payload>, AppError> {
    // Every failure is the same 404: a missing header, an unknown, used, or expired token, or
    // a repeat from another address.
    let token = (headers.get(AUTHORIZATION))
        .and_then(|v| v.to_str().ok())
        .and_then(bearer_token)
        .ok_or(AppError::NotFound)?;
    let from = caller_address(&headers);
    match state
        .bootstrap
        .redeem(token, &from)
        .ok_or(AppError::NotFound)?
    {
        Redeemed::Payload(payload, instance) => {
            // The payload must reach the VM whatever the tag does. The tag call runs in a task
            // of its own: the answer does not wait for it, and a VM that closes its connection
            // cannot cancel it.
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

/// The caller: the last `X-Forwarded-For` entry, which Caddy sets. The service listens on
/// localhost only (compose.yaml), so every outside request comes through Caddy. `unknown`
/// without a valid entry, as for a local call.
fn caller_address(headers: &HeaderMap) -> String {
    (headers.get("x-forwarded-for"))
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.rsplit(',').next())
        .and_then(|a| a.trim().parse::<IpAddr>().ok())
        .map_or_else(|| "unknown".to_owned(), |ip| ip.to_string())
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
