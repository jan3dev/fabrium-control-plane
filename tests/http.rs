use std::sync::Arc;

use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode, header},
};
use fabrium_control_plane::{AppState, auth::ApiKeys, build_app, db};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

// SHA-256 of "secret".
const HASH: &str = "2bb80d537b1da3e38bd30361aa855686bde0eacd7162fef6a25fe97bf527a25b";

async fn state() -> AppState {
    AppState {
        db: db::connect("sqlite::memory:").await.unwrap(),
        api_keys: Arc::new(ApiKeys::parse(&format!("zooid:{HASH}")).unwrap()),
    }
}

async fn app() -> Router {
    build_app(state().await)
}

async fn get(uri: &str, auth: Option<&str>) -> (StatusCode, Option<String>, Value) {
    get_from(app().await, uri, auth).await
}

async fn get_from(
    app: Router,
    uri: &str,
    auth: Option<&str>,
) -> (StatusCode, Option<String>, Value) {
    let mut req = Request::get(uri);
    if let Some(a) = auth {
        req = req.header(header::AUTHORIZATION, a);
    }
    let res = app.oneshot(req.body(Body::empty()).unwrap()).await.unwrap();
    let status = res.status();
    let www = res
        .headers()
        .get(header::WWW_AUTHENTICATE)
        .map(|v| v.to_str().unwrap().to_owned());
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (status, www, serde_json::from_slice(&bytes).unwrap())
}

#[tokio::test]
async fn health_is_public() {
    let (status, _, body) = get("/health", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!({ "status": "ok" }));
}

#[tokio::test]
async fn health_is_503_when_db_is_down() {
    let state = state().await;
    state.db.close_by_ref().await.unwrap();
    let (status, _, body) = get_from(build_app(state), "/health", None).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body, json!({ "status": "unavailable" }));
}

#[tokio::test]
async fn ping_without_header_is_401() {
    let (status, www, body) = get("/v1/ping", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(www.as_deref(), Some("Bearer"));
    assert!(body["error"].is_string());
}

#[tokio::test]
async fn ping_with_bad_key_or_scheme_is_401() {
    for auth in ["Bearer wrong", "Basic secret", "secret"] {
        let (status, www, _) = get("/v1/ping", Some(auth)).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{auth}");
        assert_eq!(www.as_deref(), Some("Bearer"));
    }
}

#[tokio::test]
async fn bearer_scheme_is_case_insensitive() {
    for auth in ["bearer secret", "BEARER secret", "Bearer   secret"] {
        let (status, _, _) = get("/v1/ping", Some(auth)).await;
        assert_eq!(status, StatusCode::OK, "{auth}");
    }
    let (status, _, _) = get("/v1/ping", Some("Bearer ")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn ping_with_valid_key_returns_client() {
    let (status, _, body) = get("/v1/ping", Some("Bearer secret")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!({ "pong": true, "client": "zooid" }));
}
