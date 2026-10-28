pub mod auth;
pub mod bootstrap;
pub mod config;
pub mod db;
pub mod ec2;
pub mod error;
mod http;

use std::{sync::Arc, time::Duration};

use axum::{
    Router,
    extract::Request,
    http::{HeaderName, StatusCode},
};
use sea_orm::DatabaseConnection;
use tokio::sync::{Mutex, OnceCell};
use tower_http::{
    request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer},
    timeout::TimeoutLayer,
    trace::{DefaultOnResponse, TraceLayer},
};
use tracing::Level;

/// Upper bound for any request. Answers 503, not 408: 408 invites client retries.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// Shared by the layers that set and propagate the id and by the trace span.
const REQUEST_ID: HeaderName = HeaderName::from_static("x-request-id");

#[derive(Clone)]
pub struct AppState {
    pub db: DatabaseConnection,
    pub api_keys: Arc<auth::ApiKeys>,
    /// Where each tenant of `api_keys` may call from.
    pub api_sources: Arc<auth::ApiSources>,
    pub vm: Arc<config::VmConfig>,
    pub ec2: Arc<dyn ec2::Ec2>,
    pub bootstrap: Arc<bootstrap::Store>,
    /// vCPUs and memory of the allowed instance types, cached after the first success.
    pub types: Arc<OnceCell<Vec<ec2::TypeSpec>>>,
    /// Held by `POST /v1/vms` from the name check to the launch.
    pub create_lock: Arc<Mutex<()>>,
}

impl AppState {
    pub fn new(
        db: DatabaseConnection,
        api_keys: auth::ApiKeys,
        api_sources: auth::ApiSources,
        vm: config::VmConfig,
        ec2: Arc<dyn ec2::Ec2>,
    ) -> Self {
        Self {
            db,
            api_keys: Arc::new(api_keys),
            api_sources: Arc::new(api_sources),
            vm: Arc::new(vm),
            ec2,
            bootstrap: Arc::new(bootstrap::Store::new(bootstrap::TTL)),
            types: Arc::default(),
            create_lock: Arc::default(),
        }
    }
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
                    let id = req.headers().get(REQUEST_ID).and_then(|v| v.to_str().ok());
                    tracing::info_span!(
                        "request",
                        method = %req.method(),
                        path = req.uri().path(),
                        request_id = id.unwrap_or("-"),
                    )
                })
                .on_response(DefaultOnResponse::new().level(Level::INFO)),
        )
        .layer(PropagateRequestIdLayer::new(REQUEST_ID))
        .layer(SetRequestIdLayer::new(REQUEST_ID, MakeRequestUuid))
}
