mod health;
mod ping;
mod vms;

use axum::{
    Router,
    extract::DefaultBodyLimit,
    middleware,
    routing::{delete, get, post},
};

use crate::{AppState, auth, ec2::Action};

pub fn router(state: AppState) -> Router {
    let protected = Router::new()
        .route("/v1/ping", get(ping::ping))
        .route("/v1/instance-types", get(vms::instance_types))
        .route(
            "/v1/vms",
            get(vms::list).merge(post(vms::create).layer(DefaultBodyLimit::max(vms::MAX_BODY))),
        )
        .route("/v1/vms/{id}", delete(vms::delete))
        .route("/v1/vms/{id}/start", vms::action(Action::Start))
        .route("/v1/vms/{id}/stop", vms::action(Action::Stop))
        .route("/v1/vms/{id}/reboot", vms::action(Action::Reboot))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            auth::require_key,
        ));
    Router::new()
        .route("/health", get(health::health))
        .merge(protected)
        .with_state(state)
}
