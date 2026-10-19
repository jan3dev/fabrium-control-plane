pub mod auth;
pub mod config;
pub mod db;
pub mod error;
mod http;

use std::{sync::Arc, time::Duration};

use axum::{Router, http::StatusCode};
use sea_orm::DatabaseConnection;
use tower_http::{
    request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer},
    timeout::TimeoutLayer,
    trace::TraceLayer,
};

#[derive(Clone)]
pub struct AppState {
    pub db: DatabaseConnection,
    pub api_keys: Arc<auth::ApiKeys>,
}

pub fn build_app(state: AppState) -> Router {
    http::router(state)
        .layer(TimeoutLayer::with_status_code(
            StatusCode::REQUEST_TIMEOUT,
            Duration::from_secs(10),
        ))
        .layer(TraceLayer::new_for_http())
        .layer(PropagateRequestIdLayer::x_request_id())
        .layer(SetRequestIdLayer::x_request_id(MakeRequestUuid))
}
