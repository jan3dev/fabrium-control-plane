mod agents;
mod health;
mod ping;
mod redeem;
mod vms;

use axum::{
    Router,
    extract::DefaultBodyLimit,
    middleware,
    routing::{delete, get, post, put},
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
        .route("/v1/vms/{id}/auto-stop", put(vms::set_auto_stop))
        .route(
            "/v1/agents",
            get(agents::list)
                .merge(post(agents::create).layer(DefaultBodyLimit::max(agents::MAX_BODY))),
        )
        .route(
            "/v1/agents/{id}",
            put(agents::change)
                .layer(DefaultBodyLimit::max(agents::MAX_BODY))
                .merge(delete(agents::delete)),
        )
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            auth::require_key,
        ));
    Router::new()
        .route("/health", get(health::health))
        .route("/v1/bootstrap", post(redeem::redeem))
        .route("/v1/vm/auto-stop", get(redeem::auto_stop))
        .merge(protected)
        .with_state(state)
}
