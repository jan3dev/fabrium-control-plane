//! Dispatcher against one stub server that plays both GitHub and Matrix.

use std::{
    collections::HashSet,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};

use axum::{
    Json, Router,
    body::Body,
    extract::{Path, State},
    http::{Request, StatusCode, header},
    routing::{delete, get, post, put},
};
use fabrium_control_plane::{
    AppState, build_app,
    config::{Config, FactoryConfig, GithubConfig, MatrixConfig},
    db,
    ec2::FakeEc2,
    entity::{factory_runs, github_deliveries},
    factory::{FactoryRuntime, dispatch},
    github::{GithubApi, events::FactoryEvent},
};
use hmac::{Hmac, KeyInit, Mac};
use sea_orm::{ActiveModelTrait, DatabaseConnection, EntityTrait, Set};
use serde_json::{Value, json};
use sha2::Sha256;
use tokio::net::TcpListener;
use tower::ServiceExt;

const SECRET: &str = "whsec-test";
const STAGE_HASH: &str = "6007fed9aaab6d871e6d973f04c07424f0e7799e33ac7f65fa5b1f29bdb34ffb";
const SHA: &str = "abc123";

/// Test-only RSA key (not a real GitHub App secret).
const TEST_PEM: &str = include_str!("fixtures/factory-test-key.pem");

#[derive(Clone, Default)]
struct Stub {
    /// Body of `GET .../commits/{sha}/check-suites`.
    suites: Arc<Mutex<Value>>,
    aliases: Arc<Mutex<HashSet<String>>>,
    room_creates: Arc<AtomicUsize>,
    wakes: Arc<AtomicUsize>,
    fail_wakes: Arc<AtomicBool>,
}

impl Stub {
    fn set_suites(&self, suites: &[(&str, Option<&str>)]) {
        let suites: Vec<Value> = suites
            .iter()
            .map(|(status, conclusion)| {
                json!({ "status": status, "conclusion": conclusion, "latest_check_runs_count": 1 })
            })
            .collect();
        *self.suites.lock().unwrap() =
            json!({ "total_count": suites.len(), "check_suites": suites });
    }
}

async fn create_room(State(s): State<Stub>, Json(body): Json<Value>) -> (StatusCode, Json<Value>) {
    s.room_creates.fetch_add(1, Ordering::SeqCst);
    let alias = body["room_alias_name"].as_str().unwrap().to_owned();
    if !s.aliases.lock().unwrap().insert(alias.clone()) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "errcode": "M_ROOM_IN_USE", "error": "Room alias already taken" })),
        );
    }
    (
        StatusCode::OK,
        Json(json!({ "room_id": format!("!{alias}:example.org") })),
    )
}

async fn resolve_alias(Path(alias): Path<String>) -> Json<Value> {
    let local = alias
        .trim_start_matches('#')
        .split(':')
        .next()
        .unwrap()
        .to_owned();
    Json(json!({ "room_id": format!("!{local}:example.org") }))
}

async fn send_message(State(s): State<Stub>) -> (StatusCode, Json<Value>) {
    if s.fail_wakes.load(Ordering::SeqCst) {
        return (
            StatusCode::BAD_GATEWAY,
            Json(json!({ "errcode": "M_UNKNOWN" })),
        );
    }
    s.wakes.fetch_add(1, Ordering::SeqCst);
    (StatusCode::OK, Json(json!({ "event_id": "$e" })))
}

async fn start_stub() -> (String, Stub) {
    let stub = Stub::default();
    stub.set_suites(&[]);
    let app = Router::new()
        .route(
            "/repos/{owner}/{repo}/installation",
            get(|| async { Json(json!({ "id": 1 })) }),
        )
        .route(
            "/app/installations/{id}/access_tokens",
            post(|| async {
                Json(json!({ "token": "tok", "expires_at": "2099-01-01T00:00:00Z" }))
            }),
        )
        .route(
            "/repos/{owner}/{repo}/commits/{sha}/check-suites",
            get(|State(s): State<Stub>| async move { Json(s.suites.lock().unwrap().clone()) }),
        )
        .route(
            "/repos/{owner}/{repo}/issues/{n}/labels",
            post(|| async { Json(json!([])) }),
        )
        .route(
            "/repos/{owner}/{repo}/issues/{n}/labels/{name}",
            delete(|| async { StatusCode::NO_CONTENT }),
        )
        .route(
            "/repos/{owner}/{repo}/issues/{n}/comments",
            post(|| async { Json(json!({})) }),
        )
        .route("/_matrix/client/v3/createRoom", post(create_room))
        .route(
            "/_matrix/client/v3/account/whoami",
            get(|| async { Json(json!({ "user_id": "@bot:example.org" })) }),
        )
        .route(
            "/_matrix/client/v3/directory/room/{alias}",
            get(resolve_alias),
        )
        .route(
            "/_matrix/client/v3/rooms/{room}/send/m.room.message/{txn}",
            put(send_message),
        )
        .with_state(stub.clone());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{addr}"), stub)
}

/// A runtime whose GitHub API and Matrix homeserver are both the stub at `base`.
fn runtime(base: &str) -> FactoryRuntime {
    let config = FactoryConfig {
        github: GithubConfig {
            app_id: 1,
            private_key_pem: TEST_PEM.to_owned(),
            webhook_secret: SECRET.into(),
            factory_repos: vec!["acme/sandbox".into()],
        },
        matrix: MatrixConfig {
            homeserver: base.to_owned(),
            access_token: "syt_test".into(),
            implementer: "@impl:example.org".into(),
            reviewer: "@rev:example.org".into(),
        },
    };
    FactoryRuntime::with_github_api(config, GithubApi::with_base(base)).unwrap()
}

async fn harness() -> (Arc<FactoryRuntime>, DatabaseConnection, Stub) {
    let (base, stub) = start_stub().await;
    let db = db::connect("sqlite::memory:").await.unwrap();
    (Arc::new(runtime(&base)), db, stub)
}

async fn seed_implementing(db: &DatabaseConnection) {
    factory_runs::ActiveModel {
        repo: Set("acme/sandbox".into()),
        issue: Set(3),
        state: Set("implementing".into()),
        branch: Set(Some("factory/3".into())),
        task_room: Set(Some("!task:example.org".into())),
        ci_attempts: Set(0),
        review_attempts: Set(0),
        budget_ends_at: Set(Some(i64::MAX)),
        created_at: Set(0),
        updated_at: Set(0),
        ..Default::default()
    }
    .insert(db)
    .await
    .unwrap();
}

async fn run(db: &DatabaseConnection) -> factory_runs::Model {
    factory_runs::Entity::find().one(db).await.unwrap().unwrap()
}

fn green_suite() -> FactoryEvent {
    FactoryEvent::CheckSuite {
        repo: "acme/sandbox".into(),
        branch: "factory/3".into(),
        sha: SHA.into(),
        conclusion: "success".into(),
        all_green: true,
    }
}

#[tokio::test]
async fn one_green_suite_waits_for_the_others() {
    let (rt, db, stub) = harness().await;
    seed_implementing(&db).await;
    stub.set_suites(&[("completed", Some("success")), ("in_progress", None)]);

    dispatch::handle(rt, db.clone(), green_suite())
        .await
        .unwrap();

    let row = run(&db).await;
    assert_eq!(row.state, "implementing");
    assert_eq!(row.ci_attempts, 0);
    assert_eq!(stub.wakes.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn green_suite_with_red_sibling_rewakes_implementer() {
    let (rt, db, stub) = harness().await;
    seed_implementing(&db).await;
    stub.set_suites(&[
        ("completed", Some("success")),
        ("completed", Some("failure")),
    ]);

    dispatch::handle(rt, db.clone(), green_suite())
        .await
        .unwrap();

    let row = run(&db).await;
    assert_eq!(row.state, "implementing");
    assert_eq!(row.ci_attempts, 1);
    assert_eq!(stub.wakes.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn all_green_suites_start_review() {
    let (rt, db, stub) = harness().await;
    seed_implementing(&db).await;
    stub.set_suites(&[
        ("completed", Some("success")),
        ("completed", Some("skipped")),
    ]);

    dispatch::handle(rt, db.clone(), green_suite())
        .await
        .unwrap();

    let row = run(&db).await;
    assert_eq!(row.state, "review");
    assert_eq!(row.review_head.as_deref(), Some(SHA));
    assert!(row.review_room.is_some());
    assert_eq!(stub.wakes.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn failed_review_wake_keeps_run_retryable() {
    let (rt, db, stub) = harness().await;
    seed_implementing(&db).await;
    stub.set_suites(&[("completed", Some("success"))]);
    stub.fail_wakes.store(true, Ordering::SeqCst);

    assert!(
        dispatch::handle(rt.clone(), db.clone(), green_suite())
            .await
            .is_err()
    );
    let row = run(&db).await;
    assert_eq!(row.state, "implementing");
    assert_eq!(row.review_room, None);

    stub.fail_wakes.store(false, Ordering::SeqCst);
    dispatch::handle(rt, db.clone(), green_suite())
        .await
        .unwrap();

    let row = run(&db).await;
    assert_eq!(row.state, "review");
    assert_eq!(
        row.review_room.as_deref(),
        Some("!acme-sandbox-3-review:example.org")
    );
    assert_eq!(stub.room_creates.load(Ordering::SeqCst), 2);
    assert_eq!(stub.wakes.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn failed_dispatch_releases_delivery_for_redelivery() {
    let (base, stub) = start_stub().await;
    let config = Config::from_lookup(|k| match k {
        "API_KEYS" => Some(format!("stage:{STAGE_HASH}")),
        "PUBLIC_URL" => Some("https://cp.example.org".to_owned()),
        _ => None,
    })
    .unwrap();
    let db = db::connect("sqlite::memory:").await.unwrap();
    seed_implementing(&db).await;
    stub.set_suites(&[("completed", Some("success"))]);
    stub.fail_wakes.store(true, Ordering::SeqCst);
    let state = AppState::new(
        db.clone(),
        config.api_keys,
        config.vm,
        Arc::new(FakeEc2::default()),
    )
    .with_factory(runtime(&base));
    let app = build_app(state);

    let body = serde_json::to_vec(&json!({
        "action": "completed",
        "check_suite": {
            "conclusion": "success",
            "head_branch": "factory/3",
            "head_sha": SHA,
            "status": "completed"
        },
        "repository": {"full_name": "acme/sandbox"}
    }))
    .unwrap();
    let mut mac = Hmac::<Sha256>::new_from_slice(SECRET.as_bytes()).unwrap();
    mac.update(&body);
    let sig = format!("sha256={}", hex::encode(mac.finalize().into_bytes()));
    let post = || {
        Request::builder()
            .method("POST")
            .uri("/v1/github/webhook")
            .header("x-github-delivery", "del-retry")
            .header("x-github-event", "check_suite")
            .header("x-hub-signature-256", &sig)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.clone()))
            .unwrap()
    };

    let res = app.clone().oneshot(post()).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let released = async {
        while github_deliveries::Entity::find_by_id("del-retry")
            .one(&db)
            .await
            .unwrap()
            .is_some()
        {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    };
    tokio::time::timeout(Duration::from_secs(5), released)
        .await
        .expect("failed delivery was not released");

    stub.fail_wakes.store(false, Ordering::SeqCst);
    let res = app.oneshot(post()).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let advanced = async {
        while run(&db).await.state != "review" {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    };
    tokio::time::timeout(Duration::from_secs(5), advanced)
        .await
        .expect("redelivery did not advance the run");
}
