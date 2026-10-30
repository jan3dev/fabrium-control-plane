//! `POST /v1/github/webhook`: HMAC verify, delivery dedupe, hand off to the dispatcher.

use std::time::{SystemTime, UNIX_EPOCH};

use axum::{
    Json,
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode},
};
use hmac::{Hmac, KeyInit, Mac};
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set};
use serde_json::{Value, json};
use sha2::Sha256;
use subtle::ConstantTimeEq;

use crate::{
    AppState, entity::github_deliveries, error::AppError, factory::dispatch, github::events,
};

type HmacSha256 = Hmac<Sha256>;

/// Maximum webhook body. GitHub payloads are usually far smaller.
pub const MAX_BODY: usize = 1024 * 1024;

pub async fn webhook(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<(StatusCode, Json<Value>), AppError> {
    let Some(factory) = state.factory.clone() else {
        return Err(AppError::NotFound);
    };

    let signature = header_str(&headers, "x-hub-signature-256").ok_or(AppError::Forbidden)?;
    if !verify_signature(&factory.config.github.webhook_secret, &body, signature) {
        return Err(AppError::Forbidden);
    }

    let delivery = header_str(&headers, "x-github-delivery")
        .ok_or_else(|| AppError::BadRequest("missing X-GitHub-Delivery".into()))?;
    let event_name = header_str(&headers, "x-github-event")
        .ok_or_else(|| AppError::BadRequest("missing X-GitHub-Event".into()))?;

    if !record_delivery(&state.db, delivery).await? {
        tracing::info!(%delivery, "duplicate GitHub delivery");
        return Ok((
            StatusCode::OK,
            Json(json!({ "ok": true, "duplicate": true })),
        ));
    }

    let payload: Value = serde_json::from_slice(&body)
        .map_err(|e| AppError::BadRequest(format!("invalid JSON: {e}")))?;

    if let Some(repo) = repo_full_name(&payload)
        && !factory.config.allows_repo(repo)
    {
        tracing::info!(%repo, %delivery, "ignored repository outside FACTORY_REPOS");
        return Ok((
            StatusCode::OK,
            Json(json!({ "ok": true, "ignored": "repo" })),
        ));
    }

    let Some(event) = events::normalize(event_name, &payload) else {
        return Ok((
            StatusCode::OK,
            Json(json!({ "ok": true, "ignored": "event" })),
        ));
    };

    // Answer before the work. GitHub retries on a slow 200; the 10 s layer would also cut us.
    tokio::spawn(async move {
        if let Err(e) = dispatch::handle(factory, state.db.clone(), event).await {
            tracing::error!(error = %format!("{e:#}"), "factory dispatch failed");
        }
    });

    Ok((StatusCode::OK, Json(json!({ "ok": true }))))
}

fn header_str<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).and_then(|v| v.to_str().ok())
}

/// Constant-time compare of `sha256=<hex>` against HMAC-SHA256(secret, body).
pub fn verify_signature(secret: &str, body: &[u8], signature_header: &str) -> bool {
    let Some(hex_sig) = signature_header.strip_prefix("sha256=") else {
        return false;
    };
    let Ok(mut mac) = HmacSha256::new_from_slice(secret.as_bytes()) else {
        return false;
    };
    mac.update(body);
    let expected = mac.finalize().into_bytes();
    let mut got = [0u8; 32];
    if hex::decode_to_slice(hex_sig, &mut got).is_err() {
        return false;
    }
    bool::from(expected.as_slice().ct_eq(&got))
}

/// Insert the delivery id. Returns `false` when it was already present.
async fn record_delivery(db: &sea_orm::DatabaseConnection, id: &str) -> Result<bool, AppError> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_secs() as i64;
    let existing = github_deliveries::Entity::find()
        .filter(github_deliveries::Column::Id.eq(id))
        .one(db)
        .await
        .map_err(|e| AppError::Aws(format!("delivery lookup: {e}")))?;
    if existing.is_some() {
        return Ok(false);
    }
    let row = github_deliveries::ActiveModel {
        id: Set(id.to_owned()),
        received_at: Set(now),
    };
    match row.insert(db).await {
        Ok(_) => Ok(true),
        Err(e) if is_unique_violation(&e) => Ok(false),
        Err(e) => Err(AppError::Aws(format!("delivery insert: {e}"))),
    }
}

fn is_unique_violation(err: &sea_orm::DbErr) -> bool {
    let s = err.to_string();
    s.contains("UNIQUE") || s.contains("unique") || s.contains("2067")
}

fn repo_full_name(payload: &Value) -> Option<&str> {
    payload
        .get("repository")
        .and_then(|r| r.get("full_name"))
        .and_then(|v| v.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_signature_passes() {
        let secret = "test-secret";
        let body = br#"{"zen":"ok"}"#;
        let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(body);
        let hex = hex::encode(mac.finalize().into_bytes());
        let header = format!("sha256={hex}");
        assert!(verify_signature(secret, body, &header));
        assert!(!verify_signature(secret, body, "sha256=00"));
        assert!(!verify_signature(secret, body, &hex));
        assert!(!verify_signature("other", body, &header));
    }
}
