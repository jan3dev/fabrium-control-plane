pub mod auth;
pub mod config;
pub mod db;
pub mod error;
mod http;

use std::{sync::Arc, time::Duration};

use axum::{Router, extract::Request, http::StatusCode};
use sea_orm::DatabaseConnection;
use tower_http::{
    request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer},
    timeout::TimeoutLayer,
    trace::{DefaultOnResponse, TraceLayer},
};
use tracing::Level;

/// Upper bound for any request. Answers 503, not 408: 408 invites client retries.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone)]
pub struct AppState {
    pub db: DatabaseConnection,
    pub api_keys: Arc<auth::ApiKeys>,
}

// Layers run outermost first: request id, then trace (so its span has the id), then timeout.
pub fn build_app(state: AppState) -> Router {
    http::router(state)
        .layer(TimeoutLayer::with_status_code(
            StatusCode::SERVICE_UNAVAILABLE,
            REQUEST_TIMEOUT,
        ))
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(|req: &Request| {
                    let id = req
                        .headers()
                        .get("x-request-id")
                        .and_then(|v| v.to_str().ok());
                    tracing::info_span!(
                        "request",
                        method = %req.method(),
                        path = req.uri().path(),
                        request_id = id.unwrap_or("-"),
                    )
                })
                .on_response(DefaultOnResponse::new().level(Level::INFO)),
        )
        .layer(PropagateRequestIdLayer::x_request_id())
        .layer(SetRequestIdLayer::x_request_id(MakeRequestUuid))
}
