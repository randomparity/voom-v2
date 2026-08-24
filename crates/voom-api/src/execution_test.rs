#![expect(
    clippy::panic,
    reason = "hash serialization failures should fail this focused unit test"
)]

use serde::de::DeserializeOwned;
use serde_json::json;

use super::*;

#[test]
fn request_hash_includes_route_instance() {
    let body = json!({"node_id": 1, "worker_id": 2, "result": {"ok": true}});

    let a = match stable_request_hash("POST", "/v1/execution/lease/1/complete", &body) {
        Ok(hash) => hash,
        Err(err) => panic!("{err}"),
    };
    let b = match stable_request_hash("POST", "/v1/execution/lease/2/complete", &body) {
        Ok(hash) => hash,
        Err(err) => panic!("{err}"),
    };

    assert_ne!(a, b);
}

#[test]
fn execution_request_dtos_reject_unknown_fields() {
    let incarnation_id = "0123456789abcdef0123456789abcdef";
    assert_unknown_field_rejected::<ActivateRequest>(json!({
        "incarnation_id": incarnation_id,
        "workers": [],
        "unknown": true
    }));
    assert_unknown_field_rejected::<DeactivateRequest>(json!({
        "incarnation_id": incarnation_id,
        "reason": "graceful_shutdown",
        "unknown": true
    }));
    assert_unknown_field_rejected::<WorkerReadinessRequest>(json!({
        "incarnation_id": incarnation_id,
        "readiness": "ready",
        "unknown": true
    }));
    assert_unknown_field_rejected::<AcquireRequest>(json!({
        "node_id": 1,
        "worker_id": 2,
        "unknown": true
    }));
    assert_unknown_field_rejected::<NodeHeartbeatRequest>(json!({"unknown": true}));
    assert_unknown_field_rejected::<LeaseHeartbeatRequest>(json!({
        "node_id": 1,
        "worker_id": 2,
        "unknown": true
    }));
    assert_unknown_field_rejected::<CompleteRequest>(json!({
        "node_id": 1,
        "worker_id": 2,
        "result": {},
        "unknown": true
    }));
    assert_unknown_field_rejected::<FailRequest>(json!({
        "node_id": 1,
        "worker_id": 2,
        "reason": "timed out",
        "class": FailureClass::WorkerTimeout,
        "unknown": true
    }));
}

#[test]
fn execution_request_dtos_require_incarnation_fences() {
    assert_missing_incarnation_rejected::<ActivateRequest>(json!({"workers": []}));
    assert_missing_incarnation_rejected::<DeactivateRequest>(json!({
        "reason": "graceful_shutdown"
    }));
    assert_missing_incarnation_rejected::<WorkerReadinessRequest>(json!({
        "readiness": "ready"
    }));
    assert_missing_incarnation_rejected::<AcquireRequest>(json!({
        "node_id": 1,
        "worker_id": 2
    }));
    assert_missing_incarnation_rejected::<NodeHeartbeatRequest>(json!({}));
    assert_missing_incarnation_rejected::<LeaseHeartbeatRequest>(json!({
        "node_id": 1,
        "worker_id": 2
    }));
    assert_missing_incarnation_rejected::<CompleteRequest>(json!({
        "node_id": 1,
        "worker_id": 2,
        "result": {}
    }));
    assert_missing_incarnation_rejected::<FailRequest>(json!({
        "node_id": 1,
        "worker_id": 2,
        "reason": "timed out",
        "class": FailureClass::WorkerTimeout
    }));
}

fn assert_unknown_field_rejected<T: DeserializeOwned>(value: JsonValue) {
    let Err(err) = serde_json::from_value::<T>(value) else {
        panic!("request body with an unknown field should be rejected");
    };
    assert!(
        err.to_string().contains("unknown field"),
        "expected unknown-field error, got {err}"
    );
}

fn assert_missing_incarnation_rejected<T: DeserializeOwned>(value: JsonValue) {
    let Err(err) = serde_json::from_value::<T>(value) else {
        panic!("request body without an incarnation fence should be rejected");
    };
    assert!(
        err.to_string().contains("incarnation_id"),
        "expected missing-incarnation error, got {err}"
    );
}
