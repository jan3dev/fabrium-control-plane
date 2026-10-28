use std::sync::Arc;

use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode, header},
};
use fabrium_control_plane::{AppState, build_app, config::Config, db, ec2::FakeEc2};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

// SHA-256 of "secret".
const HASH: &str = "2bb80d537b1da3e38bd30361aa855686bde0eacd7162fef6a25fe97bf527a25b";
// Where the tenant `zooid` calls from. Documentation ranges (RFC 5737 and RFC 3849).
const SOURCES: &str = "zooid:192.0.2.0/24,zooid:2001:db8:a::/48";
/// An address in `SOURCES`.
const SOURCE: &str = "192.0.2.10";

async fn state() -> AppState {
    let config = Config::from_lookup(|k| match k {
        "API_KEYS" => Some(format!("zooid:{HASH}")),
        "API_SOURCES" => Some(SOURCES.to_owned()),
        "PUBLIC_URL" => Some("https://cp.example.org".to_owned()),
        _ => None,
    })
    .unwrap();
    AppState::new(
        db::connect("sqlite::memory:").await.unwrap(),
        config.api_keys,
        config.api_sources,
        config.vm,
        Arc::new(FakeEc2::default()),
    )
}

async fn app() -> Router {
    build_app(state().await)
}

/// A request from `SOURCE`.
async fn get(uri: &str, auth: Option<&str>) -> (StatusCode, Option<String>, Value) {
    get_from(app().await, uri, auth, Some(SOURCE)).await
}

/// A request as Caddy forwards it, with `X-Forwarded-For: <source>` if there is one.
async fn get_from(
    app: Router,
    uri: &str,
    auth: Option<&str>,
    source: Option<&str>,
) -> (StatusCode, Option<String>, Value) {
    let mut req = Request::get(uri);
    if let Some(a) = auth {
        req = req.header(header::AUTHORIZATION, a);
    }
    if let Some(s) = source {
        req = req.header("x-forwarded-for", s);
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
async fn health_needs_no_source() {
    let app = app().await;
    for source in [None, Some("198.51.100.10"), Some(SOURCE)] {
        let (status, _, body) = get_from(app.clone(), "/health", None, source).await;
        assert_eq!(status, StatusCode::OK, "{source:?}");
        assert_eq!(body, json!({ "status": "ok" }), "{source:?}");
    }
}

#[tokio::test]
async fn health_is_503_when_db_is_down() {
    let state = state().await;
    state.db.close_by_ref().await.unwrap();
    let (status, _, body) = get_from(build_app(state), "/health", None, None).await;
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

#[tokio::test]
async fn ping_from_a_source_of_the_tenant_is_200() {
    let app = app().await;
    for source in [
        "192.0.2.10",
        "192.0.2.255",
        "2001:db8:a::1",
        "2001:db8:a:ffff::1",
        " 192.0.2.10 ",
        // IPv4-mapped IPv6 is the IPv4 address.
        "::ffff:192.0.2.10",
        // Caddy appends the real address. What the caller sent before it does not count.
        "198.51.100.10, 192.0.2.10",
    ] {
        let (status, _, body) =
            get_from(app.clone(), "/v1/ping", Some("Bearer secret"), Some(source)).await;
        assert_eq!(status, StatusCode::OK, "{source}");
        assert_eq!(body, json!({ "pong": true, "client": "zooid" }), "{source}");
    }
}

#[tokio::test]
async fn ping_from_another_source_is_403() {
    let app = app().await;
    for source in [
        "198.51.100.10",
        "203.0.113.10",
        "2001:db8:b::1",
        "::ffff:198.51.100.10",
        // The caller sends an allowed address first, and Caddy appends the real one.
        "192.0.2.10, 198.51.100.10",
    ] {
        let (status, www, body) =
            get_from(app.clone(), "/v1/ping", Some("Bearer secret"), Some(source)).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{source}");
        assert_eq!(body, json!({ "error": "forbidden" }), "{source}");
        assert_eq!(www, None, "{source}");
    }
}

#[tokio::test]
async fn ping_without_a_usable_address_is_403() {
    let app = app().await;
    for source in [
        None,
        Some(""),
        Some(" "),
        Some("unknown"),
        Some("192.0.2.10:443"),
        Some("192.0.2.10,"),
        Some("192.0.2.256"),
    ] {
        let (status, _, body) =
            get_from(app.clone(), "/v1/ping", Some("Bearer secret"), source).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{source:?}");
        assert_eq!(body, json!({ "error": "forbidden" }), "{source:?}");
    }
}

#[tokio::test]
async fn a_bad_key_is_401_from_any_source() {
    let app = app().await;
    for source in [Some(SOURCE), Some("198.51.100.10"), None] {
        for auth in [None, Some("Bearer wrong"), Some("Basic secret")] {
            let (status, www, _) = get_from(app.clone(), "/v1/ping", auth, source).await;
            assert_eq!(status, StatusCode::UNAUTHORIZED, "{auth:?} {source:?}");
            assert_eq!(www.as_deref(), Some("Bearer"), "{auth:?} {source:?}");
        }
    }
}
