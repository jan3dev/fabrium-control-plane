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
        "project_id",
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
        // serde reads a struct from a positional array too: this one has the right shape.
        json!([
            "reviewer",
            "@ana:example.org",
            "glm-5-3",
            "",
            null,
            null,
            null,
            null
        ]),
    ];
    for body in &cases {
        let (status, error) = post(&h, STAGE, body).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert!(error["error"].is_string(), "{body}");
    }
    assert_eq!(list(&h, STAGE).await, json!([]));
    // A serde error never reaches the body.
    for body in [&cases[2], &cases[15], &cases[16], &cases[17]] {
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
    let agent = created(&h, STAGE, &agent_body("reviewer")).await;
    let uri = format!("/v1/agents/{}", agent["id"].as_str().unwrap());
    let create = serde_json::to_vec(&agent_body("other")).unwrap();
    let change = br#"{"description":"d2"}"#.to_vec();
    let routes = [
        ("GET", "/v1/agents", Vec::new()),
        ("POST", "/v1/agents", create),
        ("PUT", uri.as_str(), change),
        ("DELETE", uri.as_str(), Vec::new()),
    ];
    for (method, uri, body) in &routes {
        let sent = send_from(&h.app, method, uri, None, Some(STAGE_SOURCE), body.clone()).await;
        assert_eq!(sent.0, StatusCode::UNAUTHORIZED, "{method} {uri}");
        // A key works only from a source of its tenant.
        let from = Some(PROD_SOURCE);
        let sent = send_from(&h.app, method, uri, Some(STAGE), from, body.clone()).await;
        assert_eq!(sent.0, StatusCode::FORBIDDEN, "{method} {uri}");
    }
    assert_eq!(list(&h, STAGE).await, json!([agent]));
}

/// `PUT /v1/agents/{id}` with a raw body.
async fn put_raw(h: &Harness, auth: &str, id: &str, body: Vec<u8>) -> (StatusCode, Value) {
    let uri = format!("/v1/agents/{id}");
    let (status, bytes) = send(&h.app, "PUT", &uri, Some(auth), body).await;
    let json = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json)
}

async fn put(h: &Harness, auth: &str, agent: &Value, body: &Value) -> (StatusCode, Value) {
    let id = agent["id"].as_str().unwrap();
    put_raw(h, auth, id, serde_json::to_vec(body).unwrap()).await
}

async fn delete(h: &Harness, auth: &str, id: &str) -> (StatusCode, Vec<u8>) {
    call_raw(&h.app, "DELETE", &format!("/v1/agents/{id}"), Some(auth)).await
}

/// The agent as the list of its tenant shows it.
async fn stored(h: &Harness, auth: &str, agent: &Value) -> Value {
    let agents = list(h, auth).await;
    let found = agents
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["id"] == agent["id"]);
    found.expect("the agent is listed").clone()
}

/// Moves `created_at` and `updated_at` of a row to the year 2000, so that a change shows.
async fn backdate(h: &Harness, agent: &Value) -> Value {
    set_created_at(h, agent, "2000-01-01T00:00:00Z").await;
    let id = agent["id"].as_str().unwrap();
    let sql = format!("UPDATE agents SET updated_at = '2000-01-01T00:00:00Z' WHERE id = '{id}'");
    h.state.db.execute_unprepared(&sql).await.unwrap();
    stored(h, STAGE, agent).await
}

fn placement(vm: &str, rooms: Value) -> Value {
    json!({ "vm": vm, "rooms": rooms })
}

#[tokio::test]
async fn a_change_keeps_the_fields_that_it_does_not_name() {
    let h = harness().await;
    let body = with("reviewer", |b| {
        b["tag"] = json!("Reviewer");
        b["project"] = json!("Payments");
    });
    let agent = created(&h, STAGE, &body).await;
    let before = backdate(&h, &agent).await;
    let (status, changed) = put(&h, STAGE, &agent, &json!({ "description": "d2" })).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(changed["description"], "d2");
    for key in [
        "id",
        "name",
        "owner",
        "model",
        "tag",
        "project",
        "vm",
        "rooms",
        "created_at",
    ] {
        assert_eq!(changed[key], before[key], "{key}");
    }
    assert_ne!(changed["updated_at"], before["updated_at"]);
    assert_eq!(stored(&h, STAGE, &agent).await, changed);
    // The other fields change too.
    let body = json!({ "owner": "@luis:example.org", "model": "glm-5-4", "tag": "Lead", "project": "Ops" });
    let (status, changed) = put(&h, STAGE, &agent, &body).await;
    assert_eq!(status, StatusCode::OK);
    for key in ["owner", "model", "tag", "project"] {
        assert_eq!(changed[key], body[key], "{key}");
    }
}

#[tokio::test]
async fn an_empty_tag_clears_it() {
    let h = harness().await;
    let body = with("reviewer", |b| {
        b["tag"] = json!("Reviewer");
        b["project"] = json!("Payments");
    });
    let agent = created(&h, STAGE, &body).await;
    let (status, changed) = put(&h, STAGE, &agent, &json!({ "tag": "", "project": "" })).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        (&changed["tag"], &changed["project"]),
        (&Value::Null, &Value::Null)
    );
    assert_eq!(stored(&h, STAGE, &agent).await["tag"], Value::Null);
}

#[tokio::test]
async fn an_assign_sets_the_vm_and_the_rooms_once() {
    let h = harness().await;
    let agent = created(&h, STAGE, &agent_body("reviewer")).await;
    let first = placement("lima", json!(["#general:example.org"]));
    let (status, changed) = put(&h, STAGE, &agent, &first).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        (&changed["vm"], &changed["rooms"]),
        (&first["vm"], &first["rooms"])
    );
    // A second assign changes nothing, the other fields of its body included: it is one statement.
    let vm_already_set = (StatusCode::CONFLICT, json!({ "error": "vm already set" }));
    let mut other = placement("cusco", json!([]));
    other["description"] = json!("d2");
    for second in [other, first.clone()] {
        assert_eq!(
            put(&h, STAGE, &agent, &second).await,
            vm_already_set,
            "{second}"
        );
    }
    let now = stored(&h, STAGE, &agent).await;
    assert_eq!((&now["vm"], &now["rooms"]), (&first["vm"], &first["rooms"]));
    assert_eq!(now["description"], agent["description"]);
}

#[tokio::test]
async fn an_assign_to_a_vm_that_has_the_name_is_refused() {
    let h = harness().await;
    let on_lima = with("reviewer", |b| {
        b["vm"] = json!("lima");
        b["rooms"] = json!([]);
    });
    created(&h, STAGE, &on_lima).await;
    let profile = created(&h, STAGE, &agent_body("reviewer")).await;
    assert_eq!(
        put(&h, STAGE, &profile, &placement("lima", json!([]))).await,
        (StatusCode::CONFLICT, json!({ "error": "name in use" }))
    );
    assert_eq!(stored(&h, STAGE, &profile).await, profile);
}

#[tokio::test]
async fn a_change_refuses_bad_fields() {
    let h = harness().await;
    let agent = created(&h, STAGE, &agent_body("reviewer")).await;
    let cases = [
        json!({ "name": "x" }),
        json!({ "vm": "lima" }),
        json!({ "rooms": [] }),
        json!({ "model": "" }),
        json!({ "owner": "ana" }),
        json!({ "tag": "a\u{7}" }),
        json!({ "description": format!("{}a", "é".repeat(8192)) }),
        placement("", json!([])),
        placement("Lima", json!([])),
        placement("lima", json!(["general"])),
        json!([]),
        // serde reads a struct from a positional array too: this one has the right shape.
        json!(["@luis:example.org", null, null, null, null, null, null]),
    ];
    for body in &cases {
        let (status, error) = put(&h, STAGE, &agent, body).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert!(error["error"].is_string(), "{body}");
    }
    assert_eq!(stored(&h, STAGE, &agent).await, agent);
    for body in [&cases[10], &cases[11]] {
        let invalid = json!({ "error": "invalid request body" });
        assert_eq!(put(&h, STAGE, &agent, body).await.1, invalid, "{body}");
    }
    // An empty change is no change.
    assert_eq!(
        put(&h, STAGE, &agent, &json!({})).await,
        (StatusCode::OK, agent.clone())
    );
    // A refused "vm": "" left the agent a profile, so a real assign still works.
    let (status, changed) = put(&h, STAGE, &agent, &placement("lima", json!([]))).await;
    assert_eq!((status, &changed["vm"]), (StatusCode::OK, &json!("lima")));
}

#[tokio::test]
async fn a_delete_removes_the_record() {
    let h = harness().await;
    let agent = created(&h, STAGE, &agent_body("reviewer")).await;
    let id = agent["id"].as_str().unwrap();
    assert_eq!(
        delete(&h, STAGE, id).await,
        (StatusCode::NO_CONTENT, Vec::new())
    );
    assert_eq!(list(&h, STAGE).await, json!([]));
    let (status, body) = delete(&h, STAGE, id).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(serde_json::from_slice::<Value>(&body).unwrap(), not_found());
}

#[tokio::test]
async fn another_tenant_gets_not_found() {
    let h = harness().await;
    let agent = created(&h, STAGE, &agent_body("reviewer")).await;
    let id = agent["id"].as_str().unwrap();
    let body = json!({ "description": "d2" });
    assert_eq!(
        put(&h, PROD, &agent, &body).await,
        (StatusCode::NOT_FOUND, not_found())
    );
    assert_eq!(delete(&h, PROD, id).await.0, StatusCode::NOT_FOUND);
    assert_eq!(list(&h, PROD).await, json!([]));
    assert_eq!(list(&h, STAGE).await, json!([agent]));
}

#[tokio::test]
async fn an_id_of_another_form_is_not_found() {
    let h = harness().await;
    created(&h, STAGE, &agent_body("reviewer")).await;
    let upper = "A".repeat(32);
    for id in ["abc", "%FF", upper.as_str(), &"0".repeat(33)] {
        // The id is checked before the body.
        let (status, body) = put_raw(&h, STAGE, id, b"not json".to_vec()).await;
        assert_eq!(
            (status, body),
            (StatusCode::NOT_FOUND, not_found()),
            "PUT {id}"
        );
        let (status, body) = delete(&h, STAGE, id).await;
        let body = serde_json::from_slice::<Value>(&body).unwrap();
        assert_eq!(
            (status, body),
            (StatusCode::NOT_FOUND, not_found()),
            "DELETE {id}"
        );
    }
    // An unknown id of the right form is not found too.
    let unknown = "0".repeat(32);
    let (status, body) = put_raw(&h, STAGE, &unknown, b"{}".to_vec()).await;
    assert_eq!((status, body), (StatusCode::NOT_FOUND, not_found()));
    assert_eq!(list(&h, STAGE).await.as_array().unwrap().len(), 1);
}

/// A valid agent write of exactly `len` bytes: the JSON of `body`, then spaces.
fn padded(body: &Value, len: usize) -> Vec<u8> {
    let mut bytes = serde_json::to_vec(body).unwrap();
    bytes.resize(len, b' ');
    bytes
}

#[tokio::test]
async fn the_agent_writes_take_at_most_128_kib() {
    let h = harness().await;
    let limit = 128 * 1024;
    let body = agent_body("reviewer");
    assert_eq!(
        post_raw(&h, STAGE, padded(&body, limit)).await.0,
        StatusCode::CREATED
    );
    let over = post_raw(&h, STAGE, padded(&body, limit + 1)).await.0;
    assert_eq!(over, StatusCode::PAYLOAD_TOO_LARGE);
    let agent = &list(&h, STAGE).await[0];
    let id = agent["id"].as_str().unwrap();
    let change = json!({ "description": "d2" });
    assert_eq!(
        put_raw(&h, STAGE, id, padded(&change, limit)).await.0,
        StatusCode::OK
    );
    let over = put_raw(&h, STAGE, id, padded(&change, limit + 1)).await.0;
    assert_eq!(over, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(list(&h, STAGE).await.as_array().unwrap().len(), 1);
}

/// Creates the VM `name` as `stage`, redeems its bootstrap token, and returns its VM token.
async fn vm_token_of(h: &Harness, name: &str) -> String {
    let (status, vm) = create(&h.app, STAGE, &create_body(name)).await;
    assert_eq!(status, StatusCode::CREATED, "{vm}");
    let launch = h.fake.launches().pop().expect("a launch");
    let (status, payload) = redeem(h, &token_in(&launch.user_data)).await;
    assert_eq!(status, StatusCode::OK);
    let files = payload["files"].as_array().unwrap();
    let file = files.iter().find(|f| f["path"] == "/etc/fabrium/vm-token");
    file.expect("the VM token file")["content"]
        .as_str()
        .unwrap()
        .to_owned()
}

async fn vm_agents(h: &Harness, token: &str) -> (StatusCode, Value) {
    let auth = format!("Bearer {token}");
    call(&h.app, "GET", "/v1/vm/agents", Some(&auth)).await
}

fn on(name: &str, vm: &str) -> Value {
    with(name, |b| {
        b["vm"] = json!(vm);
        b["rooms"] = json!(["#general:example.org"]);
        b["description"] = json!(format!("You are {name}."));
    })
}

#[tokio::test]
async fn a_vm_reads_its_own_agents() {
    let h = harness().await;
    let token = vm_token_of(&h, "lima").await;
    for (name, vm) in [("b", "lima"), ("a", "lima"), ("c", "cusco")] {
        created(&h, STAGE, &on(name, vm)).await;
    }
    created(&h, STAGE, &agent_body("d")).await;
    let entry = |name: &str| {
        json!({
            "name": name,
            "description": format!("You are {name}."),
            "model": "glm-5-3",
            "rooms": ["#general:example.org"],
        })
    };
    let want = json!({ "agents": [entry("a"), entry("b")] });
    assert_eq!(vm_agents(&h, &token).await, (StatusCode::OK, want));
}

#[tokio::test]
async fn a_vm_without_agents_gets_an_empty_list() {
    let h = harness().await;
    let token = vm_token_of(&h, "lima").await;
    created(&h, STAGE, &agent_body("profile")).await;
    let empty = (StatusCode::OK, json!({ "agents": [] }));
    assert_eq!(vm_agents(&h, &token).await, empty);
}

#[tokio::test]
async fn a_vm_does_not_read_the_agents_of_the_same_name_in_another_tenant() {
    let h = harness().await;
    let token = vm_token_of(&h, "lima").await;
    created(&h, PROD, &on("reviewer", "lima")).await;
    let empty = (StatusCode::OK, json!({ "agents": [] }));
    assert_eq!(vm_agents(&h, &token).await, empty);
}

#[tokio::test]
async fn a_missing_or_unknown_token_is_not_found() {
    let h = harness().await;
    vm_token_of(&h, "lima").await;
    created(&h, STAGE, &on("reviewer", "lima")).await;
    let bootstrap = format!("Bearer {}", token_in(&h.fake.launches()[0].user_data));
    for auth in [
        None,
        Some("Bearer nope"),
        Some(STAGE),
        Some(bootstrap.as_str()),
    ] {
        let (status, body) = call(&h.app, "GET", "/v1/vm/agents", auth).await;
        assert_eq!(
            (status, body),
            (StatusCode::NOT_FOUND, not_found()),
            "{auth:?}"
        );
    }
}

#[tokio::test]
async fn the_token_of_a_deleted_vm_is_not_found() {
    let h = harness().await;
    let token = vm_token_of(&h, "lima").await;
    let agent = created(&h, STAGE, &on("reviewer", "lima")).await;
    assert_eq!(vm_agents(&h, &token).await.0, StatusCode::OK);
    let id = h.fake.launched()[0].id.clone();
    let (status, _) = call(&h.app, "DELETE", &format!("/v1/vms/{id}"), Some(STAGE)).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(
        vm_agents(&h, &token).await,
        (StatusCode::NOT_FOUND, not_found())
    );
    // A delete of the VM keeps its agents.
    assert_eq!(list(&h, STAGE).await, json!([agent]));
}

#[tokio::test]
async fn the_route_needs_no_source_address() {
    let h = harness().await;
    let token = vm_token_of(&h, "lima").await;
    let auth = format!("Bearer {token}");
    let (status, body) = call_from(&h.app, "GET", "/v1/vm/agents", Some(&auth), None).await;
    assert_eq!((status, body), (StatusCode::OK, json!({ "agents": [] })));
}

/// Creates the project `name` through `POST /v1/projects` and returns its id.
async fn project(h: &Harness, auth: &str, name: &str) -> String {
    let body = serde_json::to_vec(&json!({ "name": name })).unwrap();
    let (status, bytes) = send(&h.app, "POST", "/v1/projects", Some(auth), body).await;
    assert_eq!(status, StatusCode::CREATED);
    let project: Value = serde_json::from_slice(&bytes).unwrap();
    project["id"].as_str().unwrap().to_owned()
}

fn unknown_project() -> (StatusCode, Value) {
    (
        StatusCode::BAD_REQUEST,
        json!({ "error": "unknown project" }),
    )
}

#[tokio::test]
async fn an_agent_has_a_null_project_id_by_default() {
    let h = harness().await;
    let agent = created(&h, STAGE, &agent_body("reviewer")).await;
    // The key is there, with null.
    assert_eq!(agent.get("project_id"), Some(&Value::Null));
    assert_eq!(agent["project"], Value::Null);
    assert_eq!(
        stored(&h, STAGE, &agent).await.get("project_id"),
        Some(&Value::Null)
    );
}

#[tokio::test]
async fn create_takes_a_project_id() {
    let h = harness().await;
    let id = project(&h, STAGE, "Payments").await;
    let agent = created(
        &h,
        STAGE,
        &with("reviewer", |b| b["project_id"] = json!(id)),
    )
    .await;
    assert_eq!(agent["project_id"], id);
    assert_eq!(stored(&h, STAGE, &agent).await["project_id"], id);
}

#[tokio::test]
async fn the_text_project_of_phase_1_is_still_stored_and_returned() {
    let h = harness().await;
    let agent = created(
        &h,
        STAGE,
        &with("reviewer", |b| b["project"] = json!("Legacy")),
    )
    .await;
    assert_eq!(agent["project"], "Legacy");
    assert_eq!(agent.get("project_id"), Some(&Value::Null));
    let (status, changed) = put(&h, STAGE, &agent, &json!({ "project": "" })).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(changed["project"], Value::Null);
}

#[tokio::test]
async fn an_unknown_project_id_is_refused_on_create_and_change() {
    let h = harness().await;
    let zeros = "0".repeat(32);
    for id in [zeros.as_str(), "nope"] {
        let body = with("reviewer", |b| b["project_id"] = json!(id));
        assert_eq!(post(&h, STAGE, &body).await, unknown_project(), "{id}");
    }
    assert_eq!(list(&h, STAGE).await, json!([]));
    let kept = project(&h, STAGE, "Payments").await;
    let agent = created(
        &h,
        STAGE,
        &with("reviewer", |b| b["project_id"] = json!(kept)),
    )
    .await;
    for id in [zeros.as_str(), "nope"] {
        let body = json!({ "project_id": id, "description": "d2" });
        assert_eq!(
            put(&h, STAGE, &agent, &body).await,
            unknown_project(),
            "{id}"
        );
    }
    assert_eq!(stored(&h, STAGE, &agent).await, agent);
}

#[tokio::test]
async fn a_project_of_another_tenant_is_unknown() {
    let h = harness().await;
    let theirs = project(&h, PROD, "Payments").await;
    let body = with("reviewer", |b| b["project_id"] = json!(theirs));
    assert_eq!(post(&h, STAGE, &body).await, unknown_project());
    let agent = created(&h, STAGE, &agent_body("reviewer")).await;
    let change = json!({ "project_id": theirs });
    assert_eq!(put(&h, STAGE, &agent, &change).await, unknown_project());
    assert_eq!(stored(&h, STAGE, &agent).await, agent);
}

#[tokio::test]
async fn an_empty_project_id_clears_it() {
    let h = harness().await;
    let id = project(&h, STAGE, "Payments").await;
    let agent = created(&h, STAGE, &agent_body("reviewer")).await;
    let (status, changed) = put(&h, STAGE, &agent, &json!({ "project_id": id })).await;
    assert_eq!(
        (status, &changed["project_id"]),
        (StatusCode::OK, &json!(id))
    );
    // A change without the field keeps it.
    let (status, changed) = put(&h, STAGE, &agent, &json!({ "description": "d2" })).await;
    assert_eq!(
        (status, &changed["project_id"]),
        (StatusCode::OK, &json!(id))
    );
    let (status, changed) = put(&h, STAGE, &agent, &json!({ "project_id": "" })).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(changed.get("project_id"), Some(&Value::Null));
    assert_eq!(stored(&h, STAGE, &agent).await["project_id"], Value::Null);
}

#[tokio::test]
async fn a_change_that_names_a_project_and_a_vm_is_one_step() {
    let h = harness().await;
    let id = project(&h, STAGE, "Payments").await;
    let agent = created(&h, STAGE, &agent_body("reviewer")).await;
    let rooms = json!(["#general:example.org"]);
    let first = json!({ "project_id": id, "vm": "lima", "rooms": rooms });
    let (status, changed) = put(&h, STAGE, &agent, &first).await;
    assert_eq!(status, StatusCode::OK);
    let both = (&changed["project_id"], &changed["vm"], &changed["rooms"]);
    assert_eq!(both, (&json!(id), &json!("lima"), &rooms));
    // A second assign changes nothing: the project of the body neither.
    let other = project(&h, STAGE, "Billing").await;
    let second = json!({ "project_id": other, "vm": "cusco", "rooms": [] });
    assert_eq!(
        put(&h, STAGE, &agent, &second).await,
        (StatusCode::CONFLICT, json!({ "error": "vm already set" }))
    );
    assert_eq!(stored(&h, STAGE, &agent).await["project_id"], id);
}

#[tokio::test]
async fn a_missing_agent_with_an_unknown_project_is_not_found() {
    let h = harness().await;
    let body = serde_json::to_vec(&json!({ "project_id": "f".repeat(32) })).unwrap();
    assert_eq!(
        put_raw(&h, STAGE, &"0".repeat(32), body).await,
        (StatusCode::NOT_FOUND, not_found())
    );
}

#[tokio::test]
#[ignore = "needs Task 4"]
async fn a_delete_of_a_project_cannot_leave_a_dangling_agent() {
    let h = harness().await;
    for n in 0..20 {
        let id = project(&h, STAGE, &format!("p{n}")).await;
        let body = with("reviewer", |b| b["project_id"] = json!(id));
        let uri = format!("/v1/projects/{id}");
        let (agent, deleted) = tokio::join!(
            post(&h, STAGE, &body),
            call(&h.app, "DELETE", &uri, Some(STAGE))
        );
        let in_list = list_has_project(&h, &id).await;
        match (agent.0, deleted.0) {
            (StatusCode::CREATED, StatusCode::CONFLICT) => {
                assert_eq!(deleted.1, json!({ "error": "project in use" }));
                assert!(in_list, "{n}");
            }
            (StatusCode::BAD_REQUEST, StatusCode::NO_CONTENT) => {
                assert_eq!(agent, unknown_project());
                assert!(!in_list, "{n}");
            }
            pair => panic!("{n}: {pair:?}"),
        }
    }
}

async fn list_has_project(h: &Harness, id: &str) -> bool {
    let (status, projects) = call(&h.app, "GET", "/v1/projects", Some(STAGE)).await;
    assert_eq!(status, StatusCode::OK);
    projects.as_array().unwrap().iter().any(|p| p["id"] == id)
}
