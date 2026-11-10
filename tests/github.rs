//! Webhook HMAC, delivery dedupe, and FACTORY_REPOS allowlist.

use std::sync::Arc;

use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode, header},
};
use fabrium_control_plane::{
    AppState, build_app,
    config::{Config, FactoryConfig, GithubConfig, MatrixConfig},
    db,
    ec2::FakeEc2,
    factory::FactoryRuntime,
    github::webhook::verify_signature,
};
use hmac::{Hmac, KeyInit, Mac};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sha2::Sha256;
use tower::ServiceExt;

const SECRET: &str = "whsec-test";
const STAGE_HASH: &str = "6007fed9aaab6d871e6d973f04c07424f0e7799e33ac7f65fa5b1f29bdb34ffb";

/// Test-only RSA key (not a real GitHub App secret).
const TEST_PEM: &str = include_str!("fixtures/factory-test-key.pem");

fn sign(body: &[u8]) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(SECRET.as_bytes()).unwrap();
    mac.update(body);
    format!("sha256={}", hex::encode(mac.finalize().into_bytes()))
}

fn factory_config(repos: &str) -> FactoryConfig {
    FactoryConfig {
        github: GithubConfig {
            app_id: 1,
            private_key_pem: TEST_PEM.to_owned(),
            webhook_secret: SECRET.into(),
            factory_repos: repos.split(',').map(str::trim).map(str::to_owned).collect(),
        },
        matrix: MatrixConfig {
            homeserver: "https://matrix.example.org".into(),
            access_token: "syt_test".into(),
            implementer: "@impl:example.org".into(),
            reviewer: "@rev:example.org".into(),
        },
    }
}

async fn harness(repos: &str) -> Router {
    let config = Config::from_lookup(|k| match k {
        "API_KEYS" => Some(format!("stage:{STAGE_HASH}")),
        "PUBLIC_URL" => Some("https://cp.example.org".to_owned()),
        _ => None,
    })
    .unwrap();
    let factory = FactoryRuntime::new(factory_config(repos)).expect("factory runtime");
    let state = AppState::new(
        db::connect("sqlite::memory:").await.unwrap(),
        config.api_keys,
        config.vm,
        Arc::new(FakeEc2::default()),
    )
    .with_factory(factory);
    build_app(state)
}

async fn post_webhook(
    app: &Router,
    delivery: &str,
    event: &str,
    body: &[u8],
    signature: Option<&str>,
) -> (StatusCode, Value) {
    let mut req = Request::builder()
        .method("POST")
        .uri("/v1/github/webhook")
        .header("x-github-delivery", delivery)
        .header("x-github-event", event)
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(sig) = signature {
        req = req.header("x-hub-signature-256", sig);
    }
    let res = app
        .clone()
        .oneshot(req.body(Body::from(body.to_vec())).unwrap())
        .await
        .unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

#[tokio::test]
async fn valid_signature_is_accepted() {
    let app = harness("acme/sandbox").await;
    let body = serde_json::to_vec(&json!({
        "action": "labeled",
        "issue": {
            "number": 1,
            "title": "t",
            "body": "## Goal\n\nx\n\n## Acceptance criteria\n\n- y",
            "labels": [{"name": "factory:ready"}]
        },
        "label": {"name": "factory:ready"},
        "repository": {"full_name": "acme/sandbox"},
        "sender": {"type": "User", "login": "alice"}
    }))
    .unwrap();
    let sig = sign(&body);
    let (status, json) = post_webhook(&app, "del-1", "issues", &body, Some(&sig)).await;
    assert_eq!(status, StatusCode::OK, "{json}");
    assert_eq!(json["ok"], true);
}

#[tokio::test]
async fn bad_signature_is_forbidden() {
    let app = harness("acme/sandbox").await;
    let body = br#"{"zen":"x"}"#;
    let (status, _) = post_webhook(&app, "del-bad", "ping", body, Some("sha256=00")).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn duplicate_delivery_returns_ok() {
    let app = harness("acme/sandbox").await;
    let body = br#"{"zen":"duplicate"}"#;
    let sig = sign(body);
    let (s1, _) = post_webhook(&app, "del-dup", "ping", body, Some(&sig)).await;
    let (s2, j2) = post_webhook(&app, "del-dup", "ping", body, Some(&sig)).await;
    assert_eq!(s1, StatusCode::OK);
    assert_eq!(s2, StatusCode::OK);
    assert_eq!(j2["duplicate"], true);
}

#[tokio::test]
async fn ignored_repository_is_ok() {
    let app = harness("acme/sandbox").await;
    let body = serde_json::to_vec(&json!({
        "action": "labeled",
        "issue": {"number": 1, "title": "t", "body": "", "labels": []},
        "label": {"name": "factory:ready"},
        "repository": {"full_name": "other/repo"},
        "sender": {"type": "User"}
    }))
    .unwrap();
    let sig = sign(&body);
    let (status, json) = post_webhook(&app, "del-other", "issues", &body, Some(&sig)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["ignored"], "repo");
}

#[tokio::test]
async fn webhook_without_factory_is_404() {
    let config = Config::from_lookup(|k| match k {
        "API_KEYS" => Some(format!("stage:{STAGE_HASH}")),
        "PUBLIC_URL" => Some("https://cp.example.org".to_owned()),
        _ => None,
    })
    .unwrap();
    let state = AppState::new(
        db::connect("sqlite::memory:").await.unwrap(),
        config.api_keys,
        config.vm,
        Arc::new(FakeEc2::default()),
    );
    let app = build_app(state);
    let body = br#"{}"#;
    let (status, _) = post_webhook(&app, "x", "ping", body, Some("sha256=00")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[test]
fn signature_helper_matches_github() {
    assert!(verify_signature(SECRET, b"abc", &sign(b"abc")));
}
