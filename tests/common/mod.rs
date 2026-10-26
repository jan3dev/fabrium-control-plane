//! Helpers that the API test files share. Each file uses a part of them.
#![allow(dead_code)]

use std::sync::Arc;

use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode, header},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use fabrium_control_plane::{
    AppState,
    bootstrap::Payload,
    build_app,
    config::Config,
    db,
    ec2::{FakeEc2, Instance, fake},
};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

// SHA-256 of "stage-secret" and of "prod-secret". The tenants are `stage` and `prod-sim`.
pub const STAGE_HASH: &str = "6007fed9aaab6d871e6d973f04c07424f0e7799e33ac7f65fa5b1f29bdb34ffb";
pub const PROD_HASH: &str = "e2d097e6ec0d559bc076f6e49643bee69a8bd5ae0333158434cdd58abd263c60";
pub const STAGE: &str = "Bearer stage-secret";
pub const PROD: &str = "Bearer prod-secret";

pub struct Harness {
    pub app: Router,
    pub fake: Arc<FakeEc2>,
    pub state: AppState,
}

pub async fn harness() -> Harness {
    harness_with(|_| {}).await
}

/// A harness whose state the test can change before the router is built.
pub async fn harness_with(edit: impl FnOnce(&mut AppState)) -> Harness {
    let config = Config::from_lookup(|k| match k {
        "API_KEYS" => Some(format!("stage:{STAGE_HASH},prod-sim:{PROD_HASH}")),
        "PUBLIC_URL" => Some("https://cp.example.org".to_owned()),
        _ => None,
    })
    .unwrap();
    let fake = Arc::new(FakeEc2::default());
    let mut state = AppState::new(
        db::connect("sqlite::memory:").await.unwrap(),
        config.api_keys,
        config.vm,
        fake.clone(),
    );
    edit(&mut state);
    Harness {
        app: build_app(state.clone()),
        fake,
        state,
    }
}

/// The status and the raw body of a request with a body.
pub async fn send(
    app: &Router,
    method: &str,
    uri: &str,
    auth: Option<&str>,
    body: Vec<u8>,
) -> (StatusCode, Vec<u8>) {
    let mut req = Request::builder().method(method).uri(uri);
    if let Some(auth) = auth {
        req = req.header(header::AUTHORIZATION, auth);
    }
    let res = app
        .clone()
        .oneshot(req.body(Body::from(body)).unwrap())
        .await
        .unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (status, bytes.to_vec())
}

/// The status and the raw body of a request without a body.
pub async fn call_raw(
    app: &Router,
    method: &str,
    uri: &str,
    auth: Option<&str>,
) -> (StatusCode, Vec<u8>) {
    send(app, method, uri, auth, Vec::new()).await
}

/// The status and the JSON body. The body is `Null` if it is empty or not JSON.
pub async fn call(
    app: &Router,
    method: &str,
    uri: &str,
    auth: Option<&str>,
) -> (StatusCode, Value) {
    let (status, bytes) = call_raw(app, method, uri, auth).await;
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

pub fn aws_failed() -> Value {
    json!({ "error": "aws request failed" })
}

pub fn empty_payload() -> Payload {
    Payload {
        files: vec![],
        script: "true".to_owned(),
    }
}

/// A valid create request.
pub fn create_body(name: &str) -> Value {
    json!({
        "name": name,
        "bootstrap": {
            "files": [{
                "path": "/home/ubuntu/zooid/.env", "mode": "600", "owner": "ubuntu",
                "content": "SECRET=1\n"
            }],
            "script": "#!/bin/bash\ntrue\n"
        }
    })
}

/// The status and the JSON body of `POST /v1/vms`.
pub async fn create(app: &Router, auth: &str, body: &Value) -> (StatusCode, Value) {
    let bytes = serde_json::to_vec(body).unwrap();
    let (status, bytes) = send(app, "POST", "/v1/vms", Some(auth), bytes).await;
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

/// Lets the tasks that a handler left behind run. The runtime of a test has one thread.
pub async fn settle() {
    for _ in 0..10 {
        tokio::task::yield_now().await;
    }
}

/// The token in the user data of a launch.
pub fn token_in(user_data: &str) -> String {
    let script = String::from_utf8(STANDARD.decode(user_data).unwrap()).unwrap();
    (script.lines())
        .find_map(|l| l.strip_prefix("TOKEN='")?.strip_suffix('\''))
        .expect("a TOKEN= line")
        .to_owned()
}

/// `POST /v1/bootstrap` with the token of the first launch.
pub async fn redeem(h: &Harness, token: &str) -> (StatusCode, Value) {
    let auth = format!("Bearer {token}");
    call(&h.app, "POST", "/v1/bootstrap", Some(&auth)).await
}

/// An instance of tenant `stage` with a known launch time.
pub fn stage_vm(id: &str, name: &str, state: &str, instance_type: &str) -> Instance {
    fake::instance(id, name, Some("stage"), state, instance_type)
}

pub fn not_found() -> Value {
    json!({ "error": "not found" })
}
