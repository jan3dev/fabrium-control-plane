//! Installation token caching against a stub GitHub API.

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use axum::{
    Json, Router,
    extract::State,
    routing::{get, post},
};
use fabrium_control_plane::github::{GithubApi, GithubApp};
use serde_json::json;
use tokio::net::TcpListener;

/// Test-only RSA key (not a real GitHub App secret).
const TEST_PEM: &str = include_str!("fixtures/factory-test-key.pem");

#[derive(Clone)]
struct Stub {
    install_lookups: Arc<AtomicUsize>,
    token_mints: Arc<AtomicUsize>,
}

async fn start_stub() -> (String, Stub) {
    let stub = Stub {
        install_lookups: Arc::new(AtomicUsize::new(0)),
        token_mints: Arc::new(AtomicUsize::new(0)),
    };
    let app = Router::new()
        .route(
            "/repos/{owner}/{repo}/installation",
            get(|State(s): State<Stub>| async move {
                s.install_lookups.fetch_add(1, Ordering::SeqCst);
                Json(json!({ "id": 42 }))
            }),
        )
        .route(
            "/app/installations/{id}/access_tokens",
            post(|State(s): State<Stub>| async move {
                let n = s.token_mints.fetch_add(1, Ordering::SeqCst);
                Json(json!({ "token": format!("tok-{n}"), "expires_at": "2099-01-01T00:00:00Z" }))
            }),
        )
        .with_state(stub.clone());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{addr}"), stub)
}

#[tokio::test]
async fn repeated_calls_reuse_one_installation_token() {
    let (base, stub) = start_stub().await;
    let app = GithubApp::new(1, TEST_PEM, GithubApi::with_base(&base)).unwrap();
    let first = app.token_for_repo("acme/sandbox").await.unwrap();
    let second = app.token_for_repo("acme/sandbox").await.unwrap();
    assert_eq!(first, second);
    assert_eq!(stub.install_lookups.load(Ordering::SeqCst), 1);
    assert_eq!(stub.token_mints.load(Ordering::SeqCst), 1);
}
