use std::{net::IpAddr, sync::Arc, time::Duration};

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
    ec2::{Ec2, Instance},
    error::AppError,
};

/// The longest that the tag call runs.
const TAG_TIMEOUT: Duration = Duration::from_secs(5);

/// `POST /v1/bootstrap`: a new VM redeems its token. No tenant key. Only the VM can: the caller
/// must come from the public IP of the instance that owns the token.
pub async fn redeem(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Payload>, AppError> {
    // Every refusal is the same 404: no header, an unknown or ended token, no address or another
    // one, a foreign repeat. A refused attempt does not use the token up. A failed AWS lookup is a
    // 502, and only the holder of a live token can reach it.
    let token = (headers.get(AUTHORIZATION))
        .and_then(|v| v.to_str().ok())
        .and_then(bearer_token)
        .ok_or(AppError::NotFound)?;
    // A read: the token stays as it is. A token that the store does not know costs no AWS call.
    let vm = (state.bootstrap.instance_of(token)).ok_or(AppError::NotFound)?;
    let from = vm_address(&state, &headers, &vm).await?.to_string();
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

/// The address of the caller, if it is the VM: the public IP that EC2 shows for the instance.
/// Otherwise it logs a warning, because the token may have reached someone else, and gives a
/// 404. The token stays valid.
async fn vm_address(state: &AppState, headers: &HeaderMap, vm: &str) -> Result<IpAddr, AppError> {
    let Some(from) = client_address(headers) else {
        tracing::warn!(vm = %vm, "refused a redeem with no client address");
        return Err(AppError::NotFound);
    };
    let instance = state.ec2.get(vm).await?;
    let Some(vm_ip) = instance.as_ref().and_then(public_ip) else {
        tracing::warn!(vm = %vm, from = %from, "refused a redeem: EC2 shows no public IP for the VM");
        return Err(AppError::NotFound);
    };
    if from != vm_ip {
        tracing::warn!(vm = %vm, from = %from, vm_ip = %vm_ip, "refused a redeem from an address that is not the VM's");
        return Err(AppError::NotFound);
    }
    Ok(from)
}

/// The public IP of the instance, with an IPv4-mapped IPv6 address as IPv4. `None` if it has
/// none or EC2 gave something else than an IP address.
fn public_ip(instance: &Instance) -> Option<IpAddr> {
    let ip: IpAddr = instance.public_ip.as_deref()?.parse().ok()?;
    Some(ip.to_canonical())
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
