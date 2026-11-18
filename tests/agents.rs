//! The agent records of a tenant.

mod common;

use axum::http::StatusCode;
use common::*;
use sea_orm::ConnectionTrait;
use serde_json::{Value, json};

fn agent_body(name: &str) -> Value {
    json!({ "name": name, "owner": "@ana:example.org", "model": "glm-5-3" })
}

fn with(name: &str, edit: impl FnOnce(&mut Value)) -> Value {
    let mut body = agent_body(name);
    edit(&mut body);
    body
}

/// `POST /v1/agents` with a raw body.
async fn post_raw(h: &Harness, auth: &str, body: Vec<u8>) -> (StatusCode, Value) {
    let (status, bytes) = send(&h.app, "POST", "/v1/agents", Some(auth), body).await;
    let json = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json)
}

async fn post(h: &Harness, auth: &str, body: &Value) -> (StatusCode, Value) {
    post_raw(h, auth, serde_json::to_vec(body).unwrap()).await
}

/// Creates an agent and returns it.
async fn created(h: &Harness, auth: &str, body: &Value) -> Value {
    let (status, agent) = post(h, auth, body).await;
    assert_eq!(status, StatusCode::CREATED, "{agent}");
    agent
}

async fn list(h: &Harness, auth: &str) -> Value {
    let (status, agents) = call(&h.app, "GET", "/v1/agents", Some(auth)).await;
    assert_eq!(status, StatusCode::OK);
    agents
}

fn names(agents: &Value) -> Vec<&str> {
    let agents = agents.as_array().unwrap().iter();
    agents.map(|a| a["name"].as_str().unwrap()).collect()
}

/// Sets `created_at` of a row. The column has whole seconds, so a test that needs an order
/// sets it.
async fn set_created_at(h: &Harness, agent: &Value, at: &str) {
    let id = agent["id"].as_str().unwrap();
    let sql = format!("UPDATE agents SET created_at = '{at}' WHERE id = '{id}'");
    h.state.db.execute_unprepared(&sql).await.unwrap();
}

#[tokio::test]
async fn a_new_profile_has_an_id_and_no_vm() {
    let h = harness().await;
    let agent = created(&h, STAGE, &agent_body("reviewer")).await;
    let id = agent["id"].as_str().unwrap();
    assert_eq!(id.len(), 32);
    assert!(id.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')));
    let mut keys: Vec<_> = agent.as_object().unwrap().keys().cloned().collect();
    keys.sort();
    let want = [
        "created_at",
        "description",
        "id",
        "model",
        "name",
        "owner",
        "project",
        "rooms",
        "tag",
        "updated_at",
        "vm",
    ];
    assert_eq!(keys, want);
    assert_eq!(
        (&agent["name"], &agent["owner"], &agent["model"]),
        (
            &json!("reviewer"),
            &json!("@ana:example.org"),
            &json!("glm-5-3")
        )
    );
    assert_eq!((&agent["vm"], &agent["rooms"]), (&Value::Null, &json!([])));
    assert_eq!(agent["description"], "");
    assert_eq!(
        (&agent["tag"], &agent["project"]),
        (&Value::Null, &Value::Null)
    );
    let created_at = agent["created_at"].as_str().unwrap();
    assert_eq!(created_at.len(), "2026-11-18T19:00:00Z".len());
    assert!(created_at.ends_with('Z'));
    assert_eq!(agent["updated_at"], created_at);
}

#[tokio::test]
async fn the_list_has_the_agents_of_the_tenant_in_creation_order() {
    let h = harness().await;
    let b = created(&h, STAGE, &agent_body("b")).await;
    // Both creates can fall in the same second: move "b" back one, so the order shows.
    set_created_at(&h, &b, "2000-01-01T00:00:00Z").await;
    let a = created(&h, STAGE, &agent_body("a")).await;
    created(&h, PROD, &agent_body("c")).await;
    assert_eq!(names(&list(&h, STAGE).await), ["b", "a"]);
    assert_eq!(names(&list(&h, PROD).await), ["c"]);
    // In the same second, the id decides.
    set_created_at(&h, &a, "2000-01-01T00:00:00Z").await;
    let mut by_id = [&a, &b];
    by_id.sort_by_key(|x| x["id"].as_str().unwrap().to_owned());
    let want: Vec<_> = by_id.iter().map(|x| x["name"].as_str().unwrap()).collect();
    assert_eq!(names(&list(&h, STAGE).await), want);
}

#[tokio::test]
async fn two_profiles_can_have_the_same_name() {
    let h = harness().await;
    let one = created(&h, STAGE, &agent_body("reviewer")).await;
    let two = created(&h, STAGE, &agent_body("reviewer")).await;
    assert_ne!(one["id"], two["id"]);
    assert_eq!(names(&list(&h, STAGE).await), ["reviewer", "reviewer"]);
}

#[tokio::test]
async fn a_name_is_unique_on_a_vm() {
    let h = harness().await;
    let on = |vm: &str| {
        with("reviewer", |b| {
            b["vm"] = json!(vm);
            b["rooms"] = json!(["#general:example.org"]);
        })
    };
    let agent = created(&h, STAGE, &on("lima")).await;
    assert_eq!(
        (&agent["vm"], &agent["rooms"]),
        (&json!("lima"), &json!(["#general:example.org"]))
    );
    assert_eq!(
        post(&h, STAGE, &on("lima")).await,
        (StatusCode::CONFLICT, json!({ "error": "name in use" }))
    );
    created(&h, STAGE, &on("cusco")).await;
    // The VM name of another tenant is another VM.
    created(&h, PROD, &on("lima")).await;
    assert_eq!(list(&h, STAGE).await.as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn create_refuses_bad_fields() {
    let h = harness().await;
    let placed = |vm: Value, rooms: Value| {
        with("reviewer", |b| {
            b["vm"] = vm;
            b["rooms"] = rooms;
        })
    };
    let rooms_21: Vec<_> = (0..21).map(|n| format!("#room{n}:example.org")).collect();
    let long_description = format!("{}a", "é".repeat(8192));
    assert_eq!(long_description.len(), 16385);
    // Each body breaks one rule.
    let cases = [
        with("Reviewer", |_| {}),
        with(&"a".repeat(33), |_| {}),
        with("reviewer", |b| {
            b.as_object_mut().unwrap().remove("owner");
        }),
        with("reviewer", |b| b["owner"] = json!("ana")),
        with("reviewer", |b| {
            b.as_object_mut().unwrap().remove("model");
        }),
        with("reviewer", |b| b["model"] = json!("a b")),
        with("reviewer", |b| b["tag"] = json!("a".repeat(201))),
        with("reviewer", |b| b["tag"] = json!("a\u{7}")),
        with("reviewer", |b| b["vm"] = json!("lima")),
        with("reviewer", |b| b["rooms"] = json!([])),
        placed(json!(""), json!([])),
        placed(json!("Lima"), json!([])),
        placed(json!("lima"), json!(rooms_21)),
        placed(json!("lima"), json!(["general"])),
        with("reviewer", |b| b["description"] = json!(long_description)),
        with("reviewer", |b| b["id"] = json!("0".repeat(32))),
        json!([agent_body("reviewer")]),
    ];
    for body in &cases {
        let (status, error) = post(&h, STAGE, body).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert!(error["error"].is_string(), "{body}");
    }
    assert_eq!(list(&h, STAGE).await, json!([]));
    // A serde error never reaches the body.
    for body in [&cases[2], &cases[15], &cases[16]] {
        let invalid = json!({ "error": "invalid request body" });
        assert_eq!(post(&h, STAGE, body).await.1, invalid, "{body}");
    }
    // The tag counts characters, not bytes, and 20 rooms are allowed.
    let rooms_20 = &rooms_21[..20];
    created(
        &h,
        STAGE,
        &with("reviewer", |b| b["tag"] = json!("é".repeat(200))),
    )
    .await;
    created(&h, STAGE, &placed(json!("lima"), json!(rooms_20))).await;
}

#[tokio::test]
async fn a_description_of_the_limit_is_stored() {
    let h = harness().await;
    let description = "é".repeat(8192);
    assert_eq!(description.len(), 16384);
    let body = with("reviewer", |b| b["description"] = json!(description));
    created(&h, STAGE, &body).await;
    assert_eq!(list(&h, STAGE).await[0]["description"], description);
}

#[tokio::test]
async fn the_agent_routes_need_the_tenant_key() {
    let h = harness().await;
    let (status, _) = call(&h.app, "GET", "/v1/agents", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    // A key works only from a source of its tenant.
    let body = serde_json::to_vec(&agent_body("reviewer")).unwrap();
    for method in ["GET", "POST"] {
        let from = Some(PROD_SOURCE);
        let sent = send_from(
            &h.app,
            method,
            "/v1/agents",
            Some(STAGE),
            from,
            body.clone(),
        )
        .await;
        assert_eq!(sent.0, StatusCode::FORBIDDEN, "{method}");
    }
    assert_eq!(list(&h, STAGE).await, json!([]));
}
