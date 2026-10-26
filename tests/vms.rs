use std::sync::Arc;

use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode, header},
};
use fabrium_control_plane::{
    AppState,
    bootstrap::Payload,
    build_app,
    config::Config,
    db,
    ec2::{Action, FakeEc2, Instance, fake},
};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

// SHA-256 of "stage-secret" and of "prod-secret". The tenants are `stage` and `prod-sim`.
const STAGE_HASH: &str = "6007fed9aaab6d871e6d973f04c07424f0e7799e33ac7f65fa5b1f29bdb34ffb";
const PROD_HASH: &str = "e2d097e6ec0d559bc076f6e49643bee69a8bd5ae0333158434cdd58abd263c60";
const STAGE: &str = "Bearer stage-secret";
const PROD: &str = "Bearer prod-secret";

struct Harness {
    app: Router,
    fake: Arc<FakeEc2>,
    state: AppState,
}

async fn harness() -> Harness {
    let config = Config::from_lookup(|k| match k {
        "API_KEYS" => Some(format!("stage:{STAGE_HASH},prod-sim:{PROD_HASH}")),
        "PUBLIC_URL" => Some("https://cp.example.org".to_owned()),
        _ => None,
    })
    .unwrap();
    let fake = Arc::new(FakeEc2::default());
    let state = AppState::new(
        db::connect("sqlite::memory:").await.unwrap(),
        config.api_keys,
        config.vm,
        fake.clone(),
    );
    Harness {
        app: build_app(state.clone()),
        fake,
        state,
    }
}

/// The status and the raw body.
async fn call_raw(
    app: &Router,
    method: &str,
    uri: &str,
    auth: Option<&str>,
) -> (StatusCode, Vec<u8>) {
    let mut req = Request::builder().method(method).uri(uri);
    if let Some(auth) = auth {
        req = req.header(header::AUTHORIZATION, auth);
    }
    let res = app
        .clone()
        .oneshot(req.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (status, bytes.to_vec())
}

/// The status and the JSON body. The body is `Null` if it is empty or not JSON.
async fn call(app: &Router, method: &str, uri: &str, auth: Option<&str>) -> (StatusCode, Value) {
    let (status, bytes) = call_raw(app, method, uri, auth).await;
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

fn aws_failed() -> Value {
    json!({ "error": "aws request failed" })
}

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

fn empty_payload() -> Payload {
    Payload {
        files: vec![],
        script: "true".to_owned(),
    }
}

/// An instance of tenant `stage` with a known launch time.
fn stage_vm(id: &str, name: &str, state: &str, instance_type: &str) -> Instance {
    fake::instance(id, name, Some("stage"), state, instance_type)
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
    let payload = Payload {
        files: vec![],
        script: "true".to_owned(),
    };
    h.state
        .bootstrap
        .insert("token", payload, "i-0000000000000002");

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
    h.state.bootstrap.insert("token", empty_payload(), ID);
    let (status, _) = call_raw(&h.app, "DELETE", &format!("/v1/vms/{ID}"), Some(STAGE)).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert!(!h.state.bootstrap.has_live(ID));
    assert!(h.state.bootstrap.redeem("token").is_none());
}

#[tokio::test]
async fn a_failed_terminate_keeps_the_pending_payload() {
    let h = harness().await;
    h.fake.seed(stage_vm(ID, "lima", "pending", "t3a.medium"));
    h.state.bootstrap.insert("token", empty_payload(), ID);
    h.fake.fail_actions(true);
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
            _ => h.fake.fail_actions(true),
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
