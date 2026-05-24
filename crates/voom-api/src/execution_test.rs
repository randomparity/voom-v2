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
