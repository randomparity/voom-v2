#![expect(
    clippy::panic,
    reason = "hash serialization failures should fail this focused unit test"
)]

use serde_json::json;

use super::*;

#[test]
fn request_hash_includes_route_instance() {
    let body = json!({"node_id": 1, "worker_id": 2});

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
fn node_heartbeat_request_rejects_unknown_fields() {
    let request: NodeHeartbeatRequest = match serde_json::from_value(json!({})) {
        Ok(request) => request,
        Err(err) => panic!("{err}"),
    };
    let hash = match stable_request_hash("POST", "/v1/execution/node/1/heartbeat", &request) {
        Ok(hash) => hash,
        Err(err) => panic!("{err}"),
    };
    assert!(!hash.is_empty());

    let Err(err) = serde_json::from_value::<NodeHeartbeatRequest>(json!({"node_id": 1})) else {
        panic!("node heartbeat body with fields should be rejected");
    };
    assert!(
        err.to_string().contains("unknown field"),
        "expected unknown-field error, got {err}"
    );
}
