//! The projects of a tenant.

mod common;

use axum::http::StatusCode;
use common::*;
use serde_json::{Value, json};

fn json_of(bytes: &[u8]) -> Value {
    serde_json::from_slice(bytes).unwrap_or(Value::Null)
}

/// `POST /v1/projects` with a raw body.
async fn post_raw(h: &Harness, auth: &str, body: &[u8]) -> (StatusCode, Value) {
    let (status, bytes) = send(&h.app, "POST", "/v1/projects", Some(auth), body.to_vec()).await;
    (status, json_of(&bytes))
}

async fn post_project(h: &Harness, auth: &str, name: &str) -> (StatusCode, Value) {
    post_raw(
        h,
        auth,
        &serde_json::to_vec(&json!({ "name": name })).unwrap(),
    )
    .await
}

/// Creates a project and returns it.
async fn created(h: &Harness, auth: &str, name: &str) -> Value {
    let (status, project) = post_project(h, auth, name).await;
    assert_eq!(status, StatusCode::CREATED, "{project}");
    project
}

async fn put_project(h: &Harness, auth: &str, id: &str, body: &[u8]) -> (StatusCode, Value) {
    let uri = format!("/v1/projects/{id}");
    let (status, bytes) = send(&h.app, "PUT", &uri, Some(auth), body.to_vec()).await;
    (status, json_of(&bytes))
}

async fn rename(h: &Harness, auth: &str, project: &Value, name: &str) -> (StatusCode, Value) {
    let body = serde_json::to_vec(&json!({ "name": name })).unwrap();
    put_project(h, auth, project["id"].as_str().unwrap(), &body).await
}

async fn list_projects(h: &Harness, auth: &str) -> Value {
    let (status, projects) = call(&h.app, "GET", "/v1/projects", Some(auth)).await;
    assert_eq!(status, StatusCode::OK);
    projects
}

fn names(projects: &Value) -> Vec<&str> {
    let projects = projects.as_array().unwrap().iter();
    projects.map(|p| p["name"].as_str().unwrap()).collect()
}

fn error(message: &str) -> Value {
    json!({ "error": message })
}

#[tokio::test]
async fn a_new_project_has_an_id_and_two_equal_times() {
    let h = harness().await;
    let project = created(&h, STAGE, "Payments").await;
    let id = project["id"].as_str().unwrap();
    assert_eq!(id.len(), 32);
    assert!(id.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')));
    assert_eq!(project["name"], "Payments");
    let created_at = project["created_at"].as_str().unwrap();
    assert_eq!(created_at.len(), "2026-11-19T19:00:00Z".len());
    assert!(created_at.ends_with('Z'));
    assert_eq!(project["updated_at"], created_at);
    let mut keys: Vec<_> = project.as_object().unwrap().keys().cloned().collect();
    keys.sort();
    assert_eq!(keys, ["created_at", "id", "name", "updated_at"]);
    assert_eq!(list_projects(&h, STAGE).await, json!([project]));
}

#[tokio::test]
async fn the_list_is_sorted_by_name_without_regard_to_case_then_id() {
    let h = harness().await;
    for name in ["beta", "Alpha", "alpha2"] {
        created(&h, STAGE, name).await;
    }
    created(&h, PROD, "Gamma").await;
    assert_eq!(
        names(&list_projects(&h, STAGE).await),
        ["Alpha", "alpha2", "beta"]
    );
    assert_eq!(names(&list_projects(&h, PROD).await), ["Gamma"]);
}

#[tokio::test]
async fn a_name_is_unique_in_the_tenant_without_regard_to_ascii_case() {
    let h = harness().await;
    created(&h, STAGE, "Payments").await;
    let in_use = (StatusCode::CONFLICT, error("name in use"));
    assert_eq!(post_project(&h, STAGE, "payments").await, in_use);
    assert_eq!(post_project(&h, STAGE, "PAYMENTS").await, in_use);
    // Another tenant has its own names.
    created(&h, PROD, "Payments").await;
    // Only ASCII letters fold.
    created(&h, STAGE, "Café").await;
    created(&h, STAGE, "CAFÉ").await;
    assert_eq!(list_projects(&h, STAGE).await.as_array().unwrap().len(), 3);
}

#[tokio::test]
async fn create_refuses_bad_names() {
    let h = harness().await;
    let long = "a".repeat(65);
    for name in [
        "",
        long.as_str(),
        " Payments",
        "Payments ",
        "Pay\u{7}ments",
        "Pay\nments",
    ] {
        let refused = (StatusCode::BAD_REQUEST, error("invalid project name"));
        assert_eq!(post_project(&h, STAGE, name).await, refused, "{name:?}");
    }
    assert_eq!(list_projects(&h, STAGE).await, json!([]));
    // The limit counts characters, not bytes.
    created(&h, STAGE, &"a".repeat(64)).await;
    created(&h, STAGE, &"é".repeat(64)).await;
}

#[tokio::test]
async fn create_refuses_bad_bodies() {
    let h = harness().await;
    for body in [
        r#"{}"#,
        r#"{"name":1}"#,
        r#"{"name":"a","id":"x"}"#,
        r#"[{"name":"a"}]"#,
        r#"["a"]"#,
        r#""nope""#,
        "nope",
    ] {
        let refused = (StatusCode::BAD_REQUEST, error("invalid request body"));
        assert_eq!(
            post_raw(&h, STAGE, body.as_bytes()).await,
            refused,
            "{body}"
        );
    }
    assert_eq!(list_projects(&h, STAGE).await, json!([]));
}

/// Moves `created_at` and `updated_at` of a project to the year 2000, so that a change shows.
async fn backdate(h: &Harness, project: &Value) -> Value {
    use sea_orm::ConnectionTrait;
    let id = project["id"].as_str().unwrap();
    let sql = format!(
        "UPDATE projects SET created_at = '2000-01-01T00:00:00Z', \
         updated_at = '2000-01-01T00:00:00Z' WHERE id = '{id}'"
    );
    h.state.db.execute_unprepared(&sql).await.unwrap();
    let projects = list_projects(h, STAGE).await;
    let found = projects.as_array().unwrap().iter().find(|p| p["id"] == id);
    found.expect("the project is listed").clone()
}

#[tokio::test]
async fn a_rename_that_changes_only_the_case_is_allowed() {
    let h = harness().await;
    let project = created(&h, STAGE, "Payments").await;
    let before = backdate(&h, &project).await;
    let (status, renamed) = rename(&h, STAGE, &project, "payments").await;
    assert_eq!(status, StatusCode::OK, "{renamed}");
    assert_eq!(renamed["name"], "payments");
    assert_eq!(renamed["id"], before["id"]);
    assert_eq!(renamed["created_at"], before["created_at"]);
    assert_ne!(renamed["updated_at"], before["updated_at"]);
    assert_eq!(list_projects(&h, STAGE).await, json!([renamed]));
}

#[tokio::test]
async fn a_rename_to_the_name_of_another_project_is_a_conflict() {
    let h = harness().await;
    created(&h, STAGE, "a").await;
    let b = created(&h, STAGE, "b").await;
    assert_eq!(
        rename(&h, STAGE, &b, "A").await,
        (StatusCode::CONFLICT, error("name in use"))
    );
    assert_eq!(names(&list_projects(&h, STAGE).await), ["a", "b"]);
}

#[tokio::test]
async fn rename_checks_the_body_the_tenant_and_the_id() {
    let h = harness().await;
    let project = created(&h, STAGE, "Payments").await;
    let id = project["id"].as_str().unwrap();
    assert_eq!(
        rename(&h, STAGE, &project, "").await,
        (StatusCode::BAD_REQUEST, error("invalid project name"))
    );
    for body in [r#"{}"#, r#"{"name":"a","x":1}"#, "[]", "nope"] {
        assert_eq!(
            put_project(&h, STAGE, id, body.as_bytes()).await,
            (StatusCode::BAD_REQUEST, error("invalid request body")),
            "{body}"
        );
    }
    assert_eq!(
        rename(&h, PROD, &project, "Billing").await,
        (StatusCode::NOT_FOUND, not_found())
    );
    let body = br#"{"name":"Billing"}"#;
    let upper = "A".repeat(32);
    let unknown = "0".repeat(32);
    for id in ["abc", "%FF", upper.as_str(), unknown.as_str()] {
        assert_eq!(
            put_project(&h, STAGE, id, body).await,
            (StatusCode::NOT_FOUND, not_found()),
            "{id}"
        );
    }
    assert_eq!(list_projects(&h, STAGE).await, json!([project]));
}

#[tokio::test]
async fn the_project_routes_need_the_tenant_key() {
    let h = harness().await;
    let project = created(&h, STAGE, "Payments").await;
    let uri = format!("/v1/projects/{}", project["id"].as_str().unwrap());
    let body = br#"{"name":"Billing"}"#.to_vec();
    let routes = [
        ("GET", "/v1/projects", Vec::new()),
        ("POST", "/v1/projects", body.clone()),
        ("PUT", uri.as_str(), body),
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
    assert_eq!(list_projects(&h, STAGE).await, json!([project]));
}

async fn delete(h: &Harness, auth: &str, id: &str) -> (StatusCode, Vec<u8>) {
    call_raw(&h.app, "DELETE", &format!("/v1/projects/{id}"), Some(auth)).await
}

fn in_use() -> (StatusCode, Vec<u8>) {
    let body = serde_json::to_vec(&error("project in use")).unwrap();
    (StatusCode::CONFLICT, body)
}

fn deleted() -> (StatusCode, Vec<u8>) {
    (StatusCode::NO_CONTENT, Vec::new())
}

fn id_of(value: &Value) -> &str {
    value["id"].as_str().unwrap()
}

/// Creates an agent of `stage` in the project. Returns the agent.
async fn agent_in(h: &Harness, project_id: &str) -> Value {
    let body = json!({
        "name": "reviewer", "owner": "@ana:example.org", "model": "glm-5-3",
        "project_id": project_id,
    });
    let bytes = serde_json::to_vec(&body).unwrap();
    let (status, bytes) = send(&h.app, "POST", "/v1/agents", Some(STAGE), bytes).await;
    assert_eq!(status, StatusCode::CREATED);
    json_of(&bytes)
}

/// Creates the VM `lima` of `stage` in the project. Returns its id.
async fn vm_in(h: &Harness, project_id: &str) -> String {
    let mut body = create_body("lima");
    body["project_id"] = json!(project_id);
    let (status, vm) = create(&h.app, STAGE, &body).await;
    assert_eq!(status, StatusCode::CREATED, "{vm}");
    id_of(&vm).to_owned()
}

/// Moves the registry row of a VM back to a day of the past.
async fn backdate_row(h: &Harness, vm: &str) {
    use sea_orm::ConnectionTrait;
    let sql =
        format!("UPDATE vms SET created_at = '2026-01-01T00:00:00Z' WHERE instance_id = '{vm}'");
    h.state.db.execute_unprepared(&sql).await.unwrap();
}

/// Whether the VM has a registry row.
async fn has_row(h: &Harness, vm: &str) -> bool {
    use sea_orm::{ConnectionTrait, DbBackend, Statement};
    let sql = "SELECT 1 FROM vms WHERE instance_id = ?";
    let query = Statement::from_sql_and_values(DbBackend::Sqlite, sql, [vm.into()]);
    h.state.db.query_one_raw(query).await.unwrap().is_some()
}

#[tokio::test]
async fn an_unused_project_is_deleted() {
    let h = harness().await;
    let project = created(&h, STAGE, "Payments").await;
    assert_eq!(delete(&h, STAGE, id_of(&project)).await, deleted());
    assert_eq!(list_projects(&h, STAGE).await, json!([]));
    let (status, body) = delete(&h, STAGE, id_of(&project)).await;
    assert_eq!(
        (status, json_of(&body)),
        (StatusCode::NOT_FOUND, not_found())
    );
    // The name is free again.
    created(&h, STAGE, "Payments").await;
}

#[tokio::test]
async fn a_project_of_an_agent_is_in_use() {
    let h = harness().await;
    let project = created(&h, STAGE, "Payments").await;
    let agent = agent_in(&h, id_of(&project)).await;
    assert_eq!(delete(&h, STAGE, id_of(&project)).await, in_use());
    assert_eq!(list_projects(&h, STAGE).await, json!([project]));
    let uri = format!("/v1/agents/{}", id_of(&agent));
    let (status, _) = call_raw(&h.app, "DELETE", &uri, Some(STAGE)).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(delete(&h, STAGE, id_of(&project)).await, deleted());
}

#[tokio::test]
async fn an_agent_that_leaves_the_project_frees_it() {
    let h = harness().await;
    let project = created(&h, STAGE, "Payments").await;
    let agent = agent_in(&h, id_of(&project)).await;
    let uri = format!("/v1/agents/{}", id_of(&agent));
    let body = br#"{"project_id":""}"#.to_vec();
    let (status, _) = send(&h.app, "PUT", &uri, Some(STAGE), body).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(delete(&h, STAGE, id_of(&project)).await, deleted());
}

#[tokio::test]
async fn an_agent_of_another_tenant_does_not_hold_the_project() {
    let h = harness().await;
    let project = created(&h, STAGE, "Payments").await;
    // A row of another tenant with the id, as no write of the API can make it.
    use sea_orm::ConnectionTrait;
    let sql = format!(
        "INSERT INTO agents (id, tenant, name, owner, model, project_id) VALUES \
         ('{}', 'prod-sim', 'x', '@ana:example.org', 'glm-5-3', '{}')",
        "0".repeat(32),
        id_of(&project)
    );
    h.state.db.execute_unprepared(&sql).await.unwrap();
    assert_eq!(delete(&h, STAGE, id_of(&project)).await, deleted());
}

#[tokio::test]
async fn a_project_of_a_vm_row_is_in_use() {
    let h = harness().await;
    let project = created(&h, STAGE, "Payments").await;
    let vm = vm_in(&h, id_of(&project)).await;
    assert_eq!(delete(&h, STAGE, id_of(&project)).await, in_use());
    let uri = format!("/v1/vms/{vm}/owner");
    let body = br#"{"project_id":""}"#.to_vec();
    let (status, _) = send(&h.app, "PUT", &uri, Some(STAGE), body).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(delete(&h, STAGE, id_of(&project)).await, deleted());
}

#[tokio::test]
async fn a_delete_of_another_tenant_is_not_found_and_changes_nothing() {
    let h = harness().await;
    let project = created(&h, STAGE, "Payments").await;
    let upper = "A".repeat(32);
    for (auth, id) in [
        (PROD, id_of(&project)),
        (STAGE, "abc"),
        (STAGE, "%FF"),
        (STAGE, upper.as_str()),
        (STAGE, &"0".repeat(32)),
    ] {
        let (status, body) = delete(&h, auth, id).await;
        assert_eq!(
            (status, json_of(&body)),
            (StatusCode::NOT_FOUND, not_found()),
            "{id}"
        );
    }
    assert_eq!(list_projects(&h, STAGE).await, json!([project]));
}

#[tokio::test]
async fn a_row_of_a_vm_that_ec2_no_longer_lists_does_not_keep_the_project() {
    let h = harness().await;
    let project = created(&h, STAGE, "Payments").await;
    let vm = vm_in(&h, id_of(&project)).await;
    // The VM died outside the control plane, long ago.
    h.fake.set_state(&vm, "terminated");
    backdate_row(&h, &vm).await;
    assert_eq!(delete(&h, STAGE, id_of(&project)).await, deleted());
    assert!(!has_row(&h, &vm).await);
}

#[tokio::test]
async fn a_new_row_that_ec2_does_not_list_yet_still_counts() {
    let h = harness().await;
    let project = created(&h, STAGE, "Payments").await;
    let vm = vm_in(&h, id_of(&project)).await;
    h.fake.hide_launches(true);
    assert_eq!(delete(&h, STAGE, id_of(&project)).await, in_use());
    assert!(has_row(&h, &vm).await);
}

#[tokio::test]
async fn a_row_of_a_listed_vm_still_counts_when_it_is_old() {
    let h = harness().await;
    let project = created(&h, STAGE, "Payments").await;
    let vm = vm_in(&h, id_of(&project)).await;
    h.fake.set_state(&vm, "running");
    backdate_row(&h, &vm).await;
    assert_eq!(delete(&h, STAGE, id_of(&project)).await, in_use());
    assert!(has_row(&h, &vm).await);
}

#[tokio::test]
async fn a_failing_ec2_list_does_not_fail_the_delete() {
    let h = harness().await;
    let empty = created(&h, STAGE, "Empty").await;
    let project = created(&h, STAGE, "Payments").await;
    let vm = vm_in(&h, id_of(&project)).await;
    h.fake.set_state(&vm, "terminated");
    backdate_row(&h, &vm).await;
    h.fake.fail(true);
    assert_eq!(delete(&h, STAGE, id_of(&empty)).await, deleted());
    // Without the list, a row that looks stale still counts.
    assert_eq!(delete(&h, STAGE, id_of(&project)).await, in_use());
    assert!(has_row(&h, &vm).await);
}
