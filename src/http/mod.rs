mod health;
mod ping;

use axum::{Router, middleware, routing::get};

use crate::{AppState, auth};

pub fn router(state: AppState) -> Router {
    let protected = Router::new()
        .route("/v1/ping", get(ping::ping))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            auth::require_key,
        ));
    Router::new()
        .route("/health", get(health::health))
        .merge(protected)
        .with_state(state)
}
