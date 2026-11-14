//! The VM registry: creator, auto-stop hours, and the VM token.

mod common;

use axum::http::StatusCode;
use common::*;
use fabrium_control_plane::ec2::{Action, Instance};
use serde_json::{Value, json};

const LEGACY: &str = "i-0000000000000001";

fn body_with(edit: impl FnOnce(&mut Value)) -> Value {
    let mut body = create_body("lima");
    edit(&mut body);
    body
}

/// Creates `lima` for `@ann:example.org` with 6 hours. Returns the VM and its bootstrap token.
async fn created(h: &Harness) -> (Value, String) {
    let body = body_with(|b| {
        b["creator"] = json!("@ann:example.org");
        b["auto_stop_hours"] = json!(6);
    });
    let (status, vm) = create(&h.app, STAGE, &body).await;
    assert_eq!(status, StatusCode::CREATED);
    (vm, token_in(&h.fake.launches()[0].user_data))
}

/// The VM token from the payload that the VM redeems.
async fn vm_token(h: &Harness, bootstrap: &str) -> String {
    let (status, payload) = redeem(h, bootstrap).await;
    assert_eq!(status, StatusCode::OK);
    file(&payload, "/etc/fabrium/vm-token")["content"]
        .as_str()
        .unwrap()
        .to_owned()
}

fn file<'a>(payload: &'a Value, path: &str) -> &'a Value {
    (payload["files"].as_array().unwrap().iter())
        .find(|f| f["path"] == path)
        .unwrap_or_else(|| panic!("no file {path}"))
}

async fn hours_by_token(h: &Harness, token: &str) -> (StatusCode, Value) {
    let auth = format!("Bearer {token}");
    call(&h.app, "GET", "/v1/vm/auto-stop", Some(&auth)).await
}

async fn set_hours(h: &Harness, auth: &str, id: &str, body: &str) -> (StatusCode, Value) {
    let uri = format!("/v1/vms/{id}/auto-stop");
    let (status, bytes) = send(&h.app, "PUT", &uri, Some(auth), body.as_bytes().to_vec()).await;
    let json = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json)
}

#[tokio::test]
async fn create_registers_the_creator_and_the_hours() {
    let h = harness().await;
    let (vm, _) = created(&h).await;
    assert_eq!(vm["creator"], "@ann:example.org");
    assert_eq!(vm["registered"], true);
    assert_eq!(vm["auto_stop_hours"], 6);
    // The launch tag keeps the first value for the watcher of an older image.
    assert_eq!(h.fake.launches()[0].auto_stop_hours, 6);

    let (_, listed) = call(&h.app, "GET", "/v1/vms", Some(STAGE)).await;
    assert_eq!(listed[0]["creator"], "@ann:example.org");
    assert_eq!(listed[0]["registered"], true);
    assert_eq!(listed[0]["auto_stop_hours"], 6);
}

#[tokio::test]
async fn create_without_a_creator_still_registers_the_vm() {
    let h = harness().await;
    let (status, vm) = create(&h.app, STAGE, &create_body("lima")).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(vm["creator"], Value::Null);
    assert_eq!(vm["registered"], true);
    assert_eq!(vm["auto_stop_hours"], 12);
}

#[tokio::test]
async fn create_rejects_an_invalid_creator() {
    let h = harness().await;
    let long = format!("@{}:example.org", "a".repeat(250));
    for creator in [
        json!("ann"),
        json!("@ann"),
        json!("@a nn:x"),
        json!(long),
        json!(7),
    ] {
        let body = body_with(|b| b["creator"] = creator.clone());
        let (status, _) = create(&h.app, STAGE, &body).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{creator}");
    }
    assert!(h.fake.launches().is_empty());
}

#[tokio::test]
async fn the_payload_carries_the_vm_token_and_the_control_plane_url() {
    let h = harness().await;
    let (_, bootstrap) = created(&h).await;
    let (status, payload) = redeem(&h, &bootstrap).await;
    assert_eq!(status, StatusCode::OK);
    // The tenant's own file is still there.
    assert_eq!(file(&payload, "/home/ubuntu/zooid/.env")["owner"], "ubuntu");
    let token = file(&payload, "/etc/fabrium/vm-token");
    assert_eq!(
        (&token["owner"], &token["mode"]),
        (&json!("root"), &json!("600"))
    );
    assert_ne!(token["content"], bootstrap.as_str());
    assert_eq!(token["content"].as_str().unwrap().len(), 43);
    let url = file(&payload, "/etc/fabrium/control-plane-url");
    assert_eq!(
        (&url["owner"], &url["mode"]),
        (&json!("root"), &json!("644"))
    );
    assert_eq!(url["content"], "https://cp.example.org");
}

#[tokio::test]
async fn the_vm_token_reads_the_hours_and_nothing_else_does() {
    let h = harness().await;
    let (_, bootstrap) = created(&h).await;
    let token = vm_token(&h, &bootstrap).await;
    assert_eq!(
        hours_by_token(&h, &token).await,
        (StatusCode::OK, json!({ "auto_stop_hours": 6 }))
    );
    for auth in [
        None,
        Some("Bearer wrong"),
        Some(STAGE),
        Some(&*format!("Bearer {bootstrap}")),
    ] {
        let (status, body) = call(&h.app, "GET", "/v1/vm/auto-stop", auth).await;
        assert_eq!(
            (status, body),
            (StatusCode::NOT_FOUND, not_found()),
            "{auth:?}"
        );
    }
    // A VM token is not a tenant key.
    let auth = format!("Bearer {token}");
    assert_eq!(
        call(&h.app, "GET", "/v1/vms", Some(&auth)).await.0,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn set_auto_stop_updates_the_registry() {
    let h = harness().await;
    let (vm, bootstrap) = created(&h).await;
    let id = vm["id"].as_str().unwrap();
    let token = vm_token(&h, &bootstrap).await;

    let (status, vm) = set_hours(&h, STAGE, id, r#"{"auto_stop_hours": 3}"#).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        (&vm["auto_stop_hours"], &vm["creator"]),
        (&json!(3), &json!("@ann:example.org"))
    );
    assert_eq!(
        hours_by_token(&h, &token).await.1,
        json!({ "auto_stop_hours": 3 })
    );
    // No EC2 call changes a tag: the tag stays at the launch value.
    assert_eq!(h.fake.instance(id).unwrap().auto_stop_hours, Some(6));
}

#[tokio::test]
async fn set_auto_stop_checks_the_body_and_the_tenant() {
    let h = harness().await;
    let (vm, _) = created(&h).await;
    let id = vm["id"].as_str().unwrap();
    for body in [
        "",
        "{}",
        r#"{"auto_stop_hours": 0}"#,
        r#"{"auto_stop_hours": 169}"#,
        r#"{"auto_stop_hours": "3"}"#,
    ] {
        assert_eq!(
            set_hours(&h, STAGE, id, body).await.0,
            StatusCode::BAD_REQUEST,
            "{body}"
        );
    }
    let (status, body) = set_hours(&h, PROD, id, r#"{"auto_stop_hours": 3}"#).await;
    assert_eq!((status, body), (StatusCode::NOT_FOUND, not_found()));
    assert_eq!(
        call(&h.app, "GET", "/v1/vms", Some(STAGE)).await.1[0]["auto_stop_hours"],
        6
    );
}

#[tokio::test]
async fn set_auto_stop_on_an_unregistered_vm_is_409() {
    let h = harness().await;
    h.fake.seed(Instance {
        auto_stop_hours: Some(8),
        ..stage_vm(LEGACY, "old", "running", "t3a.medium")
    });
    let (status, body) = set_hours(&h, STAGE, LEGACY, r#"{"auto_stop_hours": 3}"#).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(
        body,
        json!({ "error": "recreate the vm to change its auto-stop" })
    );
}

#[tokio::test]
async fn start_with_hours_updates_the_registry() {
    let h = harness().await;
    let (vm, bootstrap) = created(&h).await;
    let id = vm["id"].as_str().unwrap();
    let token = vm_token(&h, &bootstrap).await;
    h.fake.set_state(id, "stopped");

    let uri = format!("/v1/vms/{id}/start");
    let body = br#"{"auto_stop_hours": 5}"#.to_vec();
    let (status, bytes) = send(&h.app, "POST", &uri, Some(STAGE), body).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let vm: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(vm["auto_stop_hours"], 5);
    assert_eq!(
        hours_by_token(&h, &token).await.1,
        json!({ "auto_stop_hours": 5 })
    );
    assert_eq!(h.fake.actions(), [(id.to_owned(), Action::Start)]);
}

#[tokio::test]
async fn an_unregistered_vm_lists_and_starts_with_its_tag() {
    let h = harness().await;
    h.fake.seed(Instance {
        auto_stop_hours: Some(8),
        ..stage_vm(LEGACY, "old", "stopped", "t3a.medium")
    });
    let (_, listed) = call(&h.app, "GET", "/v1/vms", Some(STAGE)).await;
    assert_eq!(
        (
            &listed[0]["registered"],
            &listed[0]["creator"],
            &listed[0]["auto_stop_hours"]
        ),
        (&json!(false), &Value::Null, &json!(8))
    );

    // The hours of a start are ignored: the watcher of this VM reads only its tag.
    let uri = format!("/v1/vms/{LEGACY}/start");
    let body = br#"{"auto_stop_hours": 5}"#.to_vec();
    let (status, bytes) = send(&h.app, "POST", &uri, Some(STAGE), body).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let vm: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(
        (&vm["auto_stop_hours"], &vm["registered"]),
        (&json!(8), &json!(false))
    );
    assert_eq!(h.fake.actions(), [(LEGACY.to_owned(), Action::Start)]);
}

#[tokio::test]
async fn delete_revokes_the_vm_token() {
    let h = harness().await;
    let (vm, bootstrap) = created(&h).await;
    let id = vm["id"].as_str().unwrap();
    let token = vm_token(&h, &bootstrap).await;
    let uri = format!("/v1/vms/{id}");
    assert_eq!(
        call(&h.app, "DELETE", &uri, Some(STAGE)).await.0,
        StatusCode::ACCEPTED
    );
    assert_eq!(
        hours_by_token(&h, &token).await,
        (StatusCode::NOT_FOUND, not_found())
    );
}
