mod common;

use std::{sync::Arc, time::Duration};

use axum::http::StatusCode;
use base64::{Engine, engine::general_purpose::STANDARD};
use common::*;
use fabrium_control_plane::{
    bootstrap::{Redeemed, Store, TTL},
    ec2::{Action, Instance, fake},
};
use serde_json::{Value, json};

// SHA-256 of "stage-secret" and of "prod-secret". The tenants are `stage` and `prod-sim`.
#[tokio::test]
async fn instance_types_follow_config_order() {
    let h = harness().await;
    let (status, body) = call(&h.app, "GET", "/v1/instance-types", Some(STAGE)).await;
    assert_eq!(status, StatusCode::OK);
    let types = body.as_array().unwrap();
    let names: Vec<_> = types.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(
        names,
        [
            "t3a.medium",
            "t3a.large",
            "t3.medium",
            "t3.large",
            "m6a.large",
            "c6a.large",
            "r6a.large"
        ]
    );
    let flagged = |key: &str| -> Vec<&str> {
        let on = types.iter().filter(|t| t[key] == json!(true));
        on.map(|t| t["name"].as_str().unwrap()).collect()
    };
    assert_eq!(flagged("sev_snp"), ["m6a.large", "c6a.large", "r6a.large"]);
    assert_eq!(flagged("default"), ["t3a.medium"]);
    let first = json!({ "name": "t3a.medium", "vcpus": 2, "memory_mib": 4096, "sev_snp": false, "default": true });
    assert_eq!(types[0], first);
    assert_eq!(types[1]["memory_mib"], 8192);
}

#[tokio::test]
async fn instance_types_are_cached_after_the_first_success() {
    let h = harness().await;
    h.fake.fail(true);
    let (status, body) = call(&h.app, "GET", "/v1/instance-types", Some(STAGE)).await;
    assert_eq!((status, body), (StatusCode::BAD_GATEWAY, aws_failed()));
    // A failure is not cached.
    h.fake.fail(false);
    for _ in 0..2 {
        let (status, _) = call(&h.app, "GET", "/v1/instance-types", Some(STAGE)).await;
        assert_eq!(status, StatusCode::OK);
    }
    assert_eq!(h.fake.type_calls(), 2);
}

#[tokio::test]
async fn vm_routes_need_a_tenant_key() {
    let h = harness().await;
    for uri in ["/v1/instance-types", "/v1/vms"] {
        for auth in [
            None,
            Some("Bearer wrong"),
            Some("Basic stage-secret"),
            Some("stage-secret"),
        ] {
            let (status, body) = call(&h.app, "GET", uri, auth).await;
            assert_eq!(status, StatusCode::UNAUTHORIZED, "{uri} {auth:?}");
            assert!(body["error"].is_string());
        }
    }
}

#[tokio::test]
async fn a_key_works_only_from_the_sources_of_its_tenant() {
    let h = harness().await;
    let cases = [
        (STAGE, STAGE_SOURCE, StatusCode::OK),
        (STAGE, PROD_SOURCE, StatusCode::FORBIDDEN),
        (PROD, PROD_SOURCE, StatusCode::OK),
        (PROD, STAGE_SOURCE, StatusCode::FORBIDDEN),
    ];
    for (auth, source, want) in cases {
        for uri in ["/v1/instance-types", "/v1/vms"] {
            let (status, body) = call_from(&h.app, "GET", uri, Some(auth), Some(source)).await;
            assert_eq!(status, want, "{auth} {source} {uri}");
            if want == StatusCode::FORBIDDEN {
                assert_eq!(body, json!({ "error": "forbidden" }), "{auth} {source}");
            }
        }
    }
}

#[tokio::test]
async fn a_refused_source_changes_nothing() {
    let h = harness().await;
    h.fake.seed(stage_vm(ID, "lima", "running", "t3a.medium"));
    // The key is the tenant's. The source is not.
    let from = Some(PROD_SOURCE);
    let body = serde_json::to_vec(&create_body("new")).unwrap();
    let (status, _) = send_from(&h.app, "POST", "/v1/vms", Some(STAGE), from, body).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    for (method, suffix) in [("POST", "/stop"), ("POST", "/reboot"), ("DELETE", "")] {
        let uri = format!("/v1/vms/{ID}{suffix}");
        let (status, _) = call_from(&h.app, method, &uri, Some(STAGE), from).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{method} {uri}");
    }
    assert!(h.fake.launches().is_empty());
    assert!(h.fake.actions().is_empty());
    // From the right source the same request works, so the source caused the 403.
    let uri = format!("/v1/vms/{ID}/stop");
    let (status, _) = call_from(&h.app, "POST", &uri, Some(STAGE), Some(STAGE_SOURCE)).await;
    assert_eq!(status, StatusCode::ACCEPTED);
}

#[tokio::test]
async fn list_shows_only_live_vms_of_the_caller_sorted_by_name() {
    let h = harness().await;
    h.fake.seed(Instance {
        public_ip: Some("203.0.113.10".to_owned()),
        setup_delivered: true,
        ..stage_vm("i-0000000000000002", "b", "running", "t3a.medium")
    });
    h.fake
        .seed(stage_vm("i-0000000000000001", "a", "stopped", "m6a.large"));
    h.fake.seed(stage_vm(
        "i-0000000000000003",
        "gone",
        "terminated",
        "t3a.medium",
    ));
    h.fake.seed(fake::instance(
        "i-0000000000000004",
        "x",
        Some("prod-sim"),
        "running",
        "t3a.medium",
    ));
    // A legacy VM that Fabrium did not launch: no tenant tag.
    h.fake.seed(fake::instance(
        "i-0000000000000005",
        "y",
        None,
        "running",
        "t3a.medium",
    ));

    let (status, body) = call(&h.app, "GET", "/v1/vms", Some(STAGE)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body,
        json!([
            {
                "id": "i-0000000000000001", "name": "a", "state": "stopped",
                "instance_type": "m6a.large", "sev_snp": true, "public_ip": null,
                "launched_at": "2026-10-26T19:00:00Z", "setup": "expired"
            },
            {
                "id": "i-0000000000000002", "name": "b", "state": "running",
                "instance_type": "t3a.medium", "sev_snp": false, "public_ip": "203.0.113.10",
                "launched_at": "2026-10-26T19:00:00Z", "setup": "delivered"
            }
        ])
    );

    let (_, body) = call(&h.app, "GET", "/v1/vms", Some(PROD)).await;
    let names: Vec<_> = body
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["name"].clone())
        .collect();
    assert_eq!(names, [json!("x")]);
}

#[tokio::test]
async fn setup_comes_from_the_tag_then_the_store_then_expiry() {
    let h = harness().await;
    h.fake.seed(Instance {
        setup_delivered: true,
        ..stage_vm("i-0000000000000001", "delivered", "running", "t3a.medium")
    });
    h.fake.seed(stage_vm(
        "i-0000000000000002",
        "pending",
        "pending",
        "t3a.medium",
    ));
    h.fake.seed(stage_vm(
        "i-0000000000000003",
        "expired",
        "running",
        "t3a.medium",
    ));
    h.state.bootstrap.insert(
        "token",
        empty_payload(),
        "i-0000000000000002",
        "stage",
        "pending",
    );

    let (_, body) = call(&h.app, "GET", "/v1/vms", Some(STAGE)).await;
    let setup: Vec<_> = body
        .as_array()
        .unwrap()
        .iter()
        .map(|v| (v["name"].as_str().unwrap(), v["setup"].as_str().unwrap()))
        .collect();
    assert_eq!(
        setup,
        [
            ("delivered", "delivered"),
            ("expired", "expired"),
            ("pending", "pending")
        ]
    );
}

#[tokio::test]
async fn list_is_502_when_aws_fails() {
    let h = harness().await;
    h.fake.fail(true);
    let (status, body) = call(&h.app, "GET", "/v1/vms", Some(STAGE)).await;
    assert_eq!((status, body), (StatusCode::BAD_GATEWAY, aws_failed()));
}

const ID: &str = "i-0000000000000001";
const STATES: [&str; 5] = ["pending", "running", "stopping", "stopped", "shutting-down"];

#[tokio::test]
async fn actions_follow_the_state_table() {
    // The verb, the action, the one state that it accepts, and the state that it causes.
    let table = [
        ("start", Action::Start, "stopped", "pending"),
        ("stop", Action::Stop, "running", "stopping"),
        ("reboot", Action::Reboot, "running", "running"),
    ];
    for (verb, action, allowed, next) in table {
        for state in STATES {
            let h = harness().await;
            h.fake.seed(stage_vm(ID, "lima", state, "t3a.medium"));
            let uri = format!("/v1/vms/{ID}/{verb}");
            let (status, body) = call(&h.app, "POST", &uri, Some(STAGE)).await;
            if state == allowed {
                assert_eq!(status, StatusCode::ACCEPTED, "{verb} from {state}");
                let shown = (
                    body["id"].as_str(),
                    body["name"].as_str(),
                    body["state"].as_str(),
                );
                assert_eq!(shown, (Some(ID), Some("lima"), Some(next)), "{verb}");
                assert_eq!(h.fake.actions(), [(ID.to_owned(), action)], "{verb}");
                assert_eq!(h.fake.instance(ID).unwrap().state, next, "{verb}");
            } else {
                assert_eq!(status, StatusCode::CONFLICT, "{verb} from {state}");
                assert_eq!(body, json!({ "error": format!("vm is {state}") }));
                assert!(h.fake.actions().is_empty(), "{verb} from {state}");
            }
        }
    }
}

#[tokio::test]
async fn unknown_foreign_untagged_terminated_and_malformed_ids_are_404() {
    let h = harness().await;
    h.fake.seed(fake::instance(
        "i-00000000000000a1",
        "other",
        Some("prod-sim"),
        "running",
        "t3a.medium",
    ));
    h.fake.seed(fake::instance(
        "i-00000000000000a2",
        "legacy",
        None,
        "running",
        "t3a.medium",
    ));
    h.fake.seed(stage_vm(
        "i-00000000000000a3",
        "gone",
        "terminated",
        "t3a.medium",
    ));
    let malformed = [
        "i-xyz",
        "i-123",
        "i-123456789012345678",
        "i-ABCDEF12",
        "abc",
        "i-",
    ];
    for id in malformed {
        // The VM exists and the tenant owns it, so only the id rule can give the 404.
        h.fake.seed(stage_vm(id, "odd", "running", "t3a.medium"));
    }
    let ids = [
        "i-00000000000000a1", // another tenant
        "i-00000000000000a2", // no tenant tag
        "i-00000000000000a3", // terminated
        "i-00000000000000ff", // unknown
    ];
    for id in ids.into_iter().chain(malformed) {
        let verbs = [
            ("POST", "/start"),
            ("POST", "/stop"),
            ("POST", "/reboot"),
            ("DELETE", ""),
        ];
        for (method, suffix) in verbs {
            let uri = format!("/v1/vms/{id}{suffix}");
            let (status, body) = call(&h.app, method, &uri, Some(STAGE)).await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{method} {uri}");
            assert_eq!(body, json!({ "error": "not found" }), "{method} {uri}");
        }
    }
    assert!(h.fake.actions().is_empty());
    // The owner can still act on its own VM, so the 404 above is not a general failure.
    let (status, _) = call(
        &h.app,
        "POST",
        "/v1/vms/i-00000000000000a1/stop",
        Some(PROD),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
}

#[tokio::test]
async fn an_id_that_is_not_utf8_is_404_like_any_other_bad_id() {
    let h = harness().await;
    let calls = [
        ("POST", "/v1/vms/%FF/start"),
        ("POST", "/v1/vms/%FF/stop"),
        ("POST", "/v1/vms/%FF/reboot"),
        ("POST", "/v1/vms/i-%FFabc/start"),
        ("DELETE", "/v1/vms/%C3%28"),
    ];
    for (method, uri) in calls {
        let (status, body) = call(&h.app, method, uri, Some(STAGE)).await;
        assert_eq!(
            (status, body),
            (StatusCode::NOT_FOUND, not_found()),
            "{method} {uri}"
        );
    }
    assert!(h.fake.actions().is_empty());
}

#[tokio::test]
async fn delete_terminates_in_every_listed_state() {
    for state in STATES {
        let h = harness().await;
        h.fake.seed(stage_vm(ID, "lima", state, "t3a.medium"));
        let (status, body) =
            call_raw(&h.app, "DELETE", &format!("/v1/vms/{ID}"), Some(STAGE)).await;
        assert_eq!(status, StatusCode::ACCEPTED, "{state}");
        assert!(body.is_empty(), "{state}");
        assert_eq!(
            h.fake.actions(),
            [(ID.to_owned(), Action::Terminate)],
            "{state}"
        );
    }
}

#[tokio::test]
async fn delete_drops_the_pending_payload() {
    let h = harness().await;
    h.fake.seed(stage_vm(ID, "lima", "pending", "t3a.medium"));
    h.state
        .bootstrap
        .insert("token", empty_payload(), ID, "stage", "lima");
    let (status, _) = call_raw(&h.app, "DELETE", &format!("/v1/vms/{ID}"), Some(STAGE)).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert!(!h.state.bootstrap.has_live(ID));
    assert!(h.state.bootstrap.redeem("token", "unknown").is_none());
}

#[tokio::test]
async fn a_failed_terminate_keeps_the_pending_payload() {
    let h = harness().await;
    h.fake.seed(stage_vm(ID, "lima", "pending", "t3a.medium"));
    h.state
        .bootstrap
        .insert("token", empty_payload(), ID, "stage", "lima");
    h.fake.fail_writes(true);
    let (status, _) = call_raw(&h.app, "DELETE", &format!("/v1/vms/{ID}"), Some(STAGE)).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    // The VM still exists, so it can still redeem its token.
    assert!(h.state.bootstrap.has_live(ID));
}

#[tokio::test]
async fn actions_are_502_when_aws_fails() {
    for mode in ["lookup", "action"] {
        let h = harness().await;
        h.fake.seed(stage_vm(ID, "lima", "running", "t3a.medium"));
        h.fake.seed(stage_vm(
            "i-0000000000000002",
            "sleepy",
            "stopped",
            "t3a.medium",
        ));
        match mode {
            "lookup" => h.fake.fail(true),
            _ => h.fake.fail_writes(true),
        }
        let calls = [
            ("POST", format!("/v1/vms/{ID}/stop")),
            ("POST", format!("/v1/vms/{ID}/reboot")),
            ("POST", "/v1/vms/i-0000000000000002/start".to_owned()),
            ("DELETE", format!("/v1/vms/{ID}")),
        ];
        for (method, uri) in calls {
            let (status, body) = call(&h.app, method, &uri, Some(STAGE)).await;
            assert_eq!(
                (status, body),
                (StatusCode::BAD_GATEWAY, aws_failed()),
                "{mode} {method} {uri}"
            );
        }
        h.fake.fail(false);
        assert_eq!(h.fake.instance(ID).unwrap().state, "running", "{mode}");
        assert!(h.fake.actions().is_empty(), "{mode}");
    }
}

#[tokio::test]
async fn create_without_a_key_is_401() {
    let h = harness().await;
    let (status, _) = send(&h.app, "POST", "/v1/vms", None, b"{}".to_vec()).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(h.fake.launches().is_empty());
}

#[tokio::test]
async fn create_launches_a_tagged_vm_with_the_defaults() {
    let h = harness().await;
    let (status, vm) = create(&h.app, STAGE, &create_body("lima")).await;
    assert_eq!(status, StatusCode::CREATED);
    let launches = h.fake.launches();
    assert_eq!(launches.len(), 1);
    let launch = &launches[0];
    let got = (
        launch.tenant.as_str(),
        launch.name.as_str(),
        launch.instance_type.as_str(),
    );
    assert_eq!(got, ("stage", "lima", "t3a.medium"));
    assert_eq!(vm["name"], "lima");
    assert_eq!(vm["state"], "pending");
    assert_eq!(vm["setup"], "pending");
    assert_eq!(vm["instance_type"], "t3a.medium");
    assert_eq!(vm["sev_snp"], false);
    // The fake gives each VM that it launches a public IP.
    assert_eq!(vm["public_ip"], "203.0.113.1");
    // The 201 shows the same VM that the list shows.
    let (_, listed) = call(&h.app, "GET", "/v1/vms", Some(STAGE)).await;
    assert_eq!(listed, json!([vm]));
}

#[tokio::test]
async fn create_uses_the_requested_type() {
    let h = harness().await;
    let mut body = create_body("lima");
    body["instance_type"] = json!("m6a.large");
    let (status, vm) = create(&h.app, STAGE, &body).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(vm["instance_type"], "m6a.large");
    assert_eq!(vm["sev_snp"], true);
    assert_eq!(h.fake.launches()[0].instance_type, "m6a.large");
}

/// A valid request with one change.
fn with(edit: impl FnOnce(&mut Value)) -> Value {
    let mut body = create_body("lima");
    edit(&mut body);
    body
}

#[tokio::test]
async fn create_rejects_invalid_input_with_400() {
    let h = harness().await;
    let mut cases: Vec<(String, Value)> = Vec::new();
    let long = "a".repeat(64);
    for name in ["", "Lima", "-lima", "li_ma", "li ma", long.as_str()] {
        cases.push((format!("name {name:?}"), with(|b| b["name"] = json!(name))));
    }
    cases.push(("name is a number".into(), with(|b| b["name"] = json!(5))));
    for t in ["t3a.xlarge", "T3A.MEDIUM", "", " t3a.medium", "m5.large"] {
        cases.push((
            format!("type {t:?}"),
            with(|b| b["instance_type"] = json!(t)),
        ));
    }
    for path in [
        "home/x",
        "",
        "/a/../b",
        "/..",
        "/etc/trailing/",
        "/etc/a\nb",
        "/\u{0}../etc/x",
    ] {
        let edit = |b: &mut Value| b["bootstrap"]["files"][0]["path"] = json!(path);
        cases.push((format!("path {path:?}"), with(edit)));
    }
    for mode in ["8", "12345", "rwx", ""] {
        let edit = |b: &mut Value| b["bootstrap"]["files"][0]["mode"] = json!(mode);
        cases.push((format!("mode {mode:?}"), with(edit)));
    }
    for owner in ["Root", "1abc", ""] {
        let edit = |b: &mut Value| b["bootstrap"]["files"][0]["owner"] = json!(owner);
        cases.push((format!("owner {owner:?}"), with(edit)));
    }
    let edit = |b: &mut Value| b["bootstrap"]["script"] = json!("");
    cases.push(("empty script".into(), with(edit)));
    let edit = |b: &mut Value| drop(b["bootstrap"].as_object_mut().unwrap().remove("script"));
    cases.push(("missing script".into(), with(edit)));
    let edit = |b: &mut Value| drop(b.as_object_mut().unwrap().remove("bootstrap"));
    cases.push(("missing bootstrap".into(), with(edit)));
    let edit = |b: &mut Value| drop(b.as_object_mut().unwrap().remove("name"));
    cases.push(("missing name".into(), with(edit)));
    let edit = |b: &mut Value| b["bootstrap"]["files"] = json!("x");
    cases.push(("files is not an array".into(), with(edit)));
    let edit = |b: &mut Value| b["bootstrap"]["files"][0]["content"] = json!(7);
    cases.push(("content is not a string".into(), with(edit)));

    for (label, body) in &cases {
        let (status, resp) = create(&h.app, STAGE, body).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{label}");
        assert!(resp["error"].is_string(), "{label}");
    }
    for raw in [&b"nope"[..], b"", b"[]", b"null", b"{\"name\":"] {
        let (status, bytes) = send(&h.app, "POST", "/v1/vms", Some(STAGE), raw.to_vec()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{raw:?}");
        let resp: Value = serde_json::from_slice(&bytes).unwrap();
        assert!(resp["error"].is_string(), "{raw:?}");
    }
    assert!(h.fake.launches().is_empty());
}

#[tokio::test]
async fn create_accepts_the_edges_of_the_rules() {
    let h = harness().await;
    let long = "a".repeat(63);
    for name in [long.as_str(), "9", "a-", "0-0"] {
        let (status, vm) = create(&h.app, STAGE, &create_body(name)).await;
        assert_eq!(status, StatusCode::CREATED, "{name}");
        assert_eq!(vm["name"], name);
    }
    // `null` means the default type, and there may be no files at all.
    let edit = |b: &mut Value| {
        b["instance_type"] = Value::Null;
        b["bootstrap"]["files"] = json!([]);
    };
    let (status, vm) = create(&h.app, STAGE, &with(edit)).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(vm["instance_type"], "t3a.medium");
}

#[tokio::test]
async fn name_check_is_per_tenant_and_ignores_terminated_vms() {
    let h = harness().await;
    h.fake.seed(stage_vm(
        "i-0000000000000001",
        "lima",
        "running",
        "t3a.medium",
    ));
    h.fake.seed(stage_vm(
        "i-0000000000000002",
        "sleepy",
        "stopped",
        "t3a.medium",
    ));
    h.fake.seed(stage_vm(
        "i-0000000000000003",
        "leaving",
        "shutting-down",
        "t3a.medium",
    ));
    h.fake.seed(stage_vm(
        "i-0000000000000004",
        "old",
        "terminated",
        "t3a.medium",
    ));
    h.fake.seed(fake::instance(
        "i-0000000000000005",
        "shared",
        Some("prod-sim"),
        "running",
        "t3a.medium",
    ));
    h.fake.seed(fake::instance(
        "i-0000000000000006",
        "legacy",
        None,
        "running",
        "t3a.medium",
    ));
    for name in ["lima", "sleepy", "leaving"] {
        let (status, body) = create(&h.app, STAGE, &create_body(name)).await;
        assert_eq!(
            (status, body),
            (StatusCode::CONFLICT, json!({ "error": "name in use" })),
            "{name}"
        );
    }
    assert!(h.fake.launches().is_empty());
    // The name stays free for another tenant, and where only a terminated, foreign, or untagged VM has it.
    for (auth, name) in [
        (PROD, "lima"),
        (STAGE, "old"),
        (STAGE, "shared"),
        (STAGE, "legacy"),
    ] {
        let (status, _) = create(&h.app, auth, &create_body(name)).await;
        assert_eq!(status, StatusCode::CREATED, "{auth} {name}");
    }
}

#[tokio::test]
async fn a_pending_launch_holds_its_name() {
    let h = harness().await;
    // EC2 can miss a VM that RunInstances just accepted. The payload store knows it.
    h.fake.hide_launches(true);
    let (status, vm) = create(&h.app, STAGE, &create_body("lima")).await;
    assert_eq!(status, StatusCode::CREATED);
    let (status, body) = create(&h.app, STAGE, &create_body("lima")).await;
    assert_eq!(
        (status, body),
        (StatusCode::CONFLICT, json!({ "error": "name in use" }))
    );
    assert_eq!(h.fake.launches().len(), 1);
    // The name is per tenant, and a delete frees it.
    assert_eq!(
        create(&h.app, PROD, &create_body("lima")).await.0,
        StatusCode::CREATED
    );
    let uri = format!("/v1/vms/{}", vm["id"].as_str().unwrap());
    let (status, _) = call_raw(&h.app, "DELETE", &uri, Some(STAGE)).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(
        create(&h.app, STAGE, &create_body("lima")).await.0,
        StatusCode::CREATED
    );
}

#[tokio::test]
async fn concurrent_creates_of_one_name_launch_once() {
    let h = harness().await;
    let body = create_body("lima");
    let one = create(&h.app, STAGE, &body);
    let two = create(&h.app, STAGE, &body);
    let ((a, _), (b, _)) = tokio::join!(one, two);
    let mut statuses = [a, b];
    statuses.sort();
    assert_eq!(statuses, [StatusCode::CREATED, StatusCode::CONFLICT]);
    assert_eq!(h.fake.launches().len(), 1);
}

/// A valid create request of exactly `len` bytes.
fn body_of_len(name: &str, len: usize) -> Vec<u8> {
    let mut body = create_body(name);
    body["bootstrap"]["files"][0]["content"] = json!("");
    let base = serde_json::to_vec(&body).unwrap().len();
    body["bootstrap"]["files"][0]["content"] = json!("x".repeat(len - base));
    let bytes = serde_json::to_vec(&body).unwrap();
    assert_eq!(bytes.len(), len);
    bytes
}

#[tokio::test]
async fn body_limit_is_256_kib() {
    let h = harness().await;
    let exact = body_of_len("exact", 256 * 1024);
    let (status, _) = send(&h.app, "POST", "/v1/vms", Some(STAGE), exact).await;
    assert_eq!(status, StatusCode::CREATED);
    let over = body_of_len("over", 256 * 1024 + 1);
    let (status, _) = send(&h.app, "POST", "/v1/vms", Some(STAGE), over).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(h.fake.launches().len(), 1);
}

#[tokio::test]
async fn user_data_holds_the_stub_and_the_store_holds_the_payload() {
    let h = harness().await;
    let (_, vm) = create(&h.app, STAGE, &create_body("lima")).await;
    let user_data = &h.fake.launches()[0].user_data;
    let script = String::from_utf8(STANDARD.decode(user_data).unwrap()).unwrap();
    assert!(script.starts_with("#!/bin/bash\n"));
    assert!(script.contains("\nURL='https://cp.example.org/v1/bootstrap'\n"));
    let token = token_in(user_data);
    let Some(Redeemed::Payload(payload, instance)) = h.state.bootstrap.redeem(&token, "unknown")
    else {
        panic!("the token redeems");
    };
    assert_eq!(vm["id"], json!(instance));
    assert_eq!(
        serde_json::to_value(&payload).unwrap(),
        create_body("lima")["bootstrap"]
    );
}

#[tokio::test]
async fn create_is_502_and_launches_nothing_when_aws_fails() {
    for mode in ["lookup", "launch"] {
        let h = harness().await;
        match mode {
            "lookup" => h.fake.fail(true),
            _ => h.fake.fail_writes(true),
        }
        let (status, body) = create(&h.app, STAGE, &create_body("lima")).await;
        assert_eq!(
            (status, body),
            (StatusCode::BAD_GATEWAY, aws_failed()),
            "{mode}"
        );
        assert!(h.fake.launches().is_empty(), "{mode}");
        // Nothing is stuck: the same request works when AWS is back.
        h.fake.fail(false);
        h.fake.fail_writes(false);
        let (status, _) = create(&h.app, STAGE, &create_body("lima")).await;
        assert_eq!(status, StatusCode::CREATED, "{mode}");
    }
}

#[tokio::test]
async fn a_create_that_times_out_still_stores_its_payload() {
    let h = harness().await;
    // RunInstances answers in 6 s: the second create waits for the lock, then times out mid-launch.
    h.fake.delay_launch(Duration::from_secs(6));
    tokio::time::pause(); // after the database is up
    let (a, b) = (create_body("a"), create_body("b"));
    let (first, second) = tokio::join!(create(&h.app, STAGE, &a), create(&h.app, STAGE, &b));
    assert_eq!(first.0, StatusCode::CREATED);
    assert_eq!(
        second.0,
        StatusCode::SERVICE_UNAVAILABLE,
        "the premise: the request timed out"
    );
    // Let the work that the timeout left behind finish.
    tokio::time::sleep(Duration::from_secs(30)).await;

    // Every VM that EC2 accepted has its payload: else it boots with nothing and holds its name.
    let launched = h.fake.instances();
    assert_eq!(launched.len(), 2);
    for instance in &launched {
        let has_payload = h.state.bootstrap.has_live(&instance.id);
        assert!(has_payload, "the VM {:?} has no payload", instance.name);
    }
    let (_, listed) = call(&h.app, "GET", "/v1/vms", Some(STAGE)).await;
    let setup: Vec<_> = (listed.as_array().unwrap().iter())
        .map(|v| (v["name"].as_str().unwrap(), v["setup"].as_str().unwrap()))
        .collect();
    assert_eq!(setup, [("a", "pending"), ("b", "pending")]);
}

#[tokio::test]
async fn redeem_repeats_the_payload_only_within_the_grace_time() {
    let h = harness().await;
    let (_, vm) = create(&h.app, STAGE, &create_body("lima")).await;
    let token = token_in(&h.fake.launches()[0].user_data);

    let (status, body) = redeem(&h, &token).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, create_body("lima")["bootstrap"]);
    settle().await; // the tag is set after the answer
    assert!(
        h.fake
            .instance(vm["id"].as_str().unwrap())
            .unwrap()
            .setup_delivered
    );
    let (_, listed) = call(&h.app, "GET", "/v1/vms", Some(STAGE)).await;
    assert_eq!(listed[0]["setup"], "delivered");

    // A VM whose first answer got lost asks again and gets the same payload.
    let (status, body) = redeem(&h, &token).await;
    assert_eq!(
        (status, body),
        (StatusCode::OK, create_body("lima")["bootstrap"].clone())
    );

    // After the grace time, the token is used up.
    let h =
        harness_with(|s| s.bootstrap = Arc::new(Store::new(TTL).with_grace(Duration::ZERO))).await;
    create(&h.app, STAGE, &create_body("lima")).await;
    let token = token_in(&h.fake.launches()[0].user_data);
    assert_eq!(redeem(&h, &token).await.0, StatusCode::OK);
    let (status, body) = redeem(&h, &token).await;
    assert_eq!((status, body), (StatusCode::NOT_FOUND, not_found()));
}

#[tokio::test]
async fn redeem_rejects_missing_unknown_and_expired_tokens() {
    let h = harness().await;
    create(&h.app, STAGE, &create_body("lima")).await;
    for auth in [
        None,
        Some("Basic abc"),
        Some("Bearer nope"),
        Some("Bearer "),
        Some("abc"),
    ] {
        let (status, body) = call(&h.app, "POST", "/v1/bootstrap", auth).await;
        assert_eq!(
            (status, body),
            (StatusCode::NOT_FOUND, not_found()),
            "{auth:?}"
        );
    }
    // The wrong guesses did not burn the real token.
    let token = token_in(&h.fake.launches()[0].user_data);
    assert_eq!(redeem(&h, &token).await.0, StatusCode::OK);

    // A token that nobody redeems in time.
    let h = harness_with(|s| s.bootstrap = Arc::new(Store::new(Duration::ZERO))).await;
    create(&h.app, STAGE, &create_body("lima")).await;
    let token = token_in(&h.fake.launches()[0].user_data);
    let (status, body) = redeem(&h, &token).await;
    assert_eq!((status, body), (StatusCode::NOT_FOUND, not_found()));
    let (_, listed) = call(&h.app, "GET", "/v1/vms", Some(STAGE)).await;
    assert_eq!(listed[0]["setup"], "expired");
}

#[tokio::test]
async fn keys_and_tokens_do_not_cross() {
    let h = harness().await;
    create(&h.app, STAGE, &create_body("lima")).await;
    let token = token_in(&h.fake.launches()[0].user_data);
    // A tenant key is not a bootstrap token.
    let (status, body) = call(&h.app, "POST", "/v1/bootstrap", Some(STAGE)).await;
    assert_eq!((status, body), (StatusCode::NOT_FOUND, not_found()));
    // A bootstrap token is not a tenant key.
    let auth = format!("Bearer {token}");
    for uri in ["/v1/vms", "/v1/instance-types"] {
        let (status, _) = call(&h.app, "GET", uri, Some(&auth)).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{uri}");
    }
    let (status, _) = call(&h.app, "DELETE", "/v1/vms/i-fa000000000000001", Some(&auth)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    // Neither mix-up used the token up.
    assert_eq!(redeem(&h, &token).await.0, StatusCode::OK);
}

#[tokio::test]
async fn payload_round_trips_awkward_content() {
    let h = harness().await;
    let contents = [
        "it's \"quoted\"",
        "back\\slash $HOME `cmd` $(cmd) ${x}",
        "line1\nline2\n",
        "crlf\r\nline\r\n",
        "héllo wörld, 日本語, 🙂",
        "",
        "trailing space ",
        "\ttab",
        "'; rm -rf / #",
        "<TOKEN> and <PUBLIC_URL>",
        "no newline at the end",
    ];
    for (i, content) in contents.iter().enumerate() {
        let mut body = create_body(&format!("vm-{i}"));
        body["bootstrap"]["files"][0]["content"] = json!(content);
        body["bootstrap"]["script"] = json!(format!("#!/bin/bash\necho '{content}'\n"));
        let (status, _) = create(&h.app, STAGE, &body).await;
        assert_eq!(status, StatusCode::CREATED, "{content:?}");
        let token = token_in(&h.fake.launches()[i].user_data);
        let (status, got) = redeem(&h, &token).await;
        assert_eq!(status, StatusCode::OK, "{content:?}");
        assert_eq!(got, body["bootstrap"], "{content:?}");
    }
    // No files at all, and several files with different owners and modes.
    let mut body = create_body("none");
    body["bootstrap"]["files"] = json!([]);
    create(&h.app, STAGE, &body).await;
    let token = token_in(&h.fake.launches()[contents.len()].user_data);
    assert_eq!(redeem(&h, &token).await.1, body["bootstrap"]);

    let files = json!([
        { "path": "/etc/a", "mode": "644", "owner": "root", "content": "a" },
        { "path": "/home/ubuntu/b", "mode": "0600", "owner": "ubuntu", "content": "b" },
    ]);
    let mut body = create_body("many");
    body["bootstrap"]["files"] = files;
    create(&h.app, STAGE, &body).await;
    let token = token_in(&h.fake.launches()[contents.len() + 1].user_data);
    assert_eq!(redeem(&h, &token).await.1, body["bootstrap"]);
}

#[tokio::test]
async fn a_repeat_redeem_from_another_address_is_refused() {
    let h = harness().await;
    create(&h.app, STAGE, &create_body("lima")).await;
    let token = token_in(&h.fake.launches()[0].user_data);
    let vm = vm_ip(&h, &token);
    assert_eq!(redeem_from(&h, &token, &vm).await.0, StatusCode::OK);
    // Caddy appends the real address: an earlier entry, which the caller sent, does not count.
    for other in ["198.51.100.7".to_owned(), format!("{vm}, 198.51.100.7")] {
        let (status, body) = redeem_from(&h, &token, &other).await;
        assert_eq!(
            (status, body),
            (StatusCode::NOT_FOUND, not_found()),
            "{other}"
        );
    }
    // The VM can still ask again.
    let (status, body) = redeem_from(&h, &token, &format!("198.51.100.7, {vm}")).await;
    assert_eq!(
        (status, body),
        (StatusCode::OK, create_body("lima")["bootstrap"].clone())
    );
}

#[tokio::test]
async fn a_redeem_from_another_address_is_refused_and_keeps_the_token() {
    let h = harness().await;
    let (_, vm) = create(&h.app, STAGE, &create_body("lima")).await;
    let id = vm["id"].as_str().unwrap();
    let token = token_in(&h.fake.launches()[0].user_data);
    let own = vm_ip(&h, &token);
    // A thief that holds the token, from the address of a tenant, of a neighbor of the VM, of
    // another range, and from IPv6. An entry that the thief puts before its own does not count.
    for from in [
        "198.51.100.7",
        "192.0.2.10",
        "203.0.113.2",
        "2001:db8::1",
        "::ffff:198.51.100.7",
        &format!("{own}, 198.51.100.7"),
    ] {
        let (status, body) = redeem_from(&h, &token, from).await;
        assert_eq!(
            (status, body),
            (StatusCode::NOT_FOUND, not_found()),
            "{from}"
        );
    }
    // Nothing was delivered or tagged, so the VM still has its token.
    settle().await; // a tag call would run after the answer
    assert!(!h.fake.instance(id).unwrap().setup_delivered);
    let (_, listed) = call(&h.app, "GET", "/v1/vms", Some(STAGE)).await;
    assert_eq!(listed[0]["setup"], "pending");
    let (status, body) = redeem(&h, &token).await;
    assert_eq!(
        (status, body),
        (StatusCode::OK, create_body("lima")["bootstrap"].clone())
    );
}

#[tokio::test]
async fn a_redeem_without_a_usable_address_is_refused_and_keeps_the_token() {
    let h = harness().await;
    create(&h.app, STAGE, &create_body("lima")).await;
    let token = token_in(&h.fake.launches()[0].user_data);
    let auth = format!("Bearer {token}");
    let own = vm_ip(&h, &token);
    for from in [
        None,
        Some(""),
        Some("unknown"),
        Some("203.0.113.1,"),
        Some(&format!("{own}:443")),
    ] {
        let (status, body) = call_from(&h.app, "POST", "/v1/bootstrap", Some(&auth), from).await;
        assert_eq!(
            (status, body),
            (StatusCode::NOT_FOUND, not_found()),
            "{from:?}"
        );
    }
    assert_eq!(redeem(&h, &token).await.0, StatusCode::OK);
}

#[tokio::test]
async fn a_redeem_is_refused_while_the_vm_has_no_public_ip() {
    let h = harness().await;
    let (_, vm) = create(&h.app, STAGE, &create_body("lima")).await;
    let id = vm["id"].as_str().unwrap();
    let token = token_in(&h.fake.launches()[0].user_data);
    let own = vm_ip(&h, &token);
    h.fake.set_public_ip(id, None);
    // No address is the VM's, its former one included.
    for from in [own.as_str(), "198.51.100.7"] {
        let (status, body) = redeem_from(&h, &token, from).await;
        assert_eq!(
            (status, body),
            (StatusCode::NOT_FOUND, not_found()),
            "{from}"
        );
    }
    // The VM gets an address, and the token still works from it.
    h.fake.set_public_ip(id, Some("198.51.100.8"));
    assert_eq!(
        redeem_from(&h, &token, "198.51.100.8").await.0,
        StatusCode::OK
    );
}

#[tokio::test]
async fn a_redeem_is_refused_when_ec2_does_not_know_the_vm() {
    let h = harness().await;
    // The store has a token for an instance that EC2 does not list.
    let store = &h.state.bootstrap;
    store.insert(
        "token",
        empty_payload(),
        "i-00000000000000ff",
        "stage",
        "ghost",
    );
    for from in ["203.0.113.1", "198.51.100.7"] {
        let (status, body) = redeem_from(&h, "token", from).await;
        assert_eq!(
            (status, body),
            (StatusCode::NOT_FOUND, not_found()),
            "{from}"
        );
    }
    // The failed attempts did not use the token up.
    assert!(store.instance_of("token").is_some());
}

#[tokio::test]
async fn a_token_works_only_from_its_own_vm() {
    let h = harness().await;
    create(&h.app, STAGE, &create_body("a")).await;
    create(&h.app, STAGE, &create_body("b")).await;
    let launches = h.fake.launches();
    let (token_a, token_b) = (
        token_in(&launches[0].user_data),
        token_in(&launches[1].user_data),
    );
    let (ip_a, ip_b) = (vm_ip(&h, &token_a), vm_ip(&h, &token_b));
    assert_ne!(ip_a, ip_b);
    // Each VM presents the token of the other one.
    for (token, from) in [(&token_a, &ip_b), (&token_b, &ip_a)] {
        let (status, body) = redeem_from(&h, token, from).await;
        assert_eq!((status, body), (StatusCode::NOT_FOUND, not_found()));
    }
    // Each VM still gets its own payload.
    assert_eq!(redeem(&h, &token_a).await.0, StatusCode::OK);
    assert_eq!(redeem(&h, &token_b).await.0, StatusCode::OK);
}

#[tokio::test]
async fn a_redeem_accepts_the_ipv4_mapped_forms_of_the_vm_address() {
    let h = harness().await;
    create(&h.app, STAGE, &create_body("lima")).await;
    let token = token_in(&h.fake.launches()[0].user_data);
    assert_eq!(vm_ip(&h, &token), "203.0.113.1");
    // 203.0.113.1 is 0xcb007101.
    for from in ["::ffff:203.0.113.1", "::ffff:cb00:7101", "203.0.113.1"] {
        let (status, body) = redeem_from(&h, &token, from).await;
        assert_eq!(
            (status, body),
            (StatusCode::OK, create_body("lima")["bootstrap"].clone()),
            "{from}"
        );
    }
}

#[tokio::test]
async fn an_unknown_token_does_not_reach_aws_and_an_aws_failure_is_a_502() {
    let h = harness().await;
    create(&h.app, STAGE, &create_body("lima")).await;
    let token = token_in(&h.fake.launches()[0].user_data);
    let own = vm_ip(&h, &token);
    h.fake.fail(true);
    // The store does not know these tokens, so there is no AWS call to fail: a 404, not a 502.
    for auth in [Some("Bearer nope"), Some("Basic abc"), Some(STAGE), None] {
        let (status, body) = call_from(&h.app, "POST", "/v1/bootstrap", auth, Some(&own)).await;
        assert_eq!(
            (status, body),
            (StatusCode::NOT_FOUND, not_found()),
            "{auth:?}"
        );
    }
    // The holder of a live token reaches the lookup, and its failure is a 502 as everywhere else.
    let (status, body) = redeem_from(&h, &token, &own).await;
    assert_eq!((status, body), (StatusCode::BAD_GATEWAY, aws_failed()));
    // The token is not used up: the VM asks again when AWS is back.
    h.fake.fail(false);
    assert_eq!(redeem_from(&h, &token, &own).await.0, StatusCode::OK);
}

#[tokio::test]
async fn a_vm_with_a_new_address_in_the_grace_time_is_refused() {
    let h = harness().await;
    let (_, vm) = create(&h.app, STAGE, &create_body("lima")).await;
    let id = vm["id"].as_str().unwrap();
    let token = token_in(&h.fake.launches()[0].user_data);
    let first = vm_ip(&h, &token);
    assert_eq!(redeem(&h, &token).await.0, StatusCode::OK);
    // EC2 gives the VM a new address, as at a stop and a start. The new address is the VM's, but
    // the first redeem came from another one.
    h.fake.set_public_ip(id, Some("198.51.100.8"));
    let (status, body) = redeem_from(&h, &token, "198.51.100.8").await;
    assert_eq!((status, body), (StatusCode::NOT_FOUND, not_found()));
    // The old address is not the VM's any more.
    let (status, body) = redeem_from(&h, &token, &first).await;
    assert_eq!((status, body), (StatusCode::NOT_FOUND, not_found()));
}

#[tokio::test]
async fn redeem_still_delivers_when_tagging_fails() {
    let h = harness().await;
    let (_, vm) = create(&h.app, STAGE, &create_body("lima")).await;
    let token = token_in(&h.fake.launches()[0].user_data);
    h.fake.fail_tagging(true);
    let (status, body) = redeem(&h, &token).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, create_body("lima")["bootstrap"]);
    // A repeat within the grace time gets the payload again, and the VM is still not marked.
    settle().await; // the failed tag call runs after the answer
    // In the grace time, the VM shows `pending`, not `expired`, while its tag is missing.
    let (_, listed) = call(&h.app, "GET", "/v1/vms", Some(STAGE)).await;
    assert_eq!(listed[0]["setup"], "pending");
    assert_eq!(redeem(&h, &token).await.0, StatusCode::OK);
    settle().await;
    assert!(
        !h.fake
            .instance(vm["id"].as_str().unwrap())
            .unwrap()
            .setup_delivered
    );
}

#[tokio::test]
async fn redeem_answers_without_waiting_for_the_tag() {
    let h = harness().await;
    create(&h.app, STAGE, &create_body("lima")).await;
    let token = token_in(&h.fake.launches()[0].user_data);
    h.fake.hang_tagging(true);
    tokio::time::pause();
    let start = tokio::time::Instant::now();
    let (status, body) = redeem(&h, &token).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, create_body("lima")["bootstrap"]);
    // With the clock paused, a wait for the tag shows as seconds.
    let waited = start.elapsed();
    assert!(
        waited < Duration::from_secs(1),
        "the redeem waited {waited:?} for the tag"
    );
}

#[tokio::test]
async fn delete_before_redeem_drops_the_token() {
    let h = harness().await;
    let (_, vm) = create(&h.app, STAGE, &create_body("lima")).await;
    let token = token_in(&h.fake.launches()[0].user_data);
    let uri = format!("/v1/vms/{}", vm["id"].as_str().unwrap());
    assert_eq!(
        call_raw(&h.app, "DELETE", &uri, Some(STAGE)).await.0,
        StatusCode::ACCEPTED
    );
    let (status, body) = redeem(&h, &token).await;
    assert_eq!((status, body), (StatusCode::NOT_FOUND, not_found()));
}
