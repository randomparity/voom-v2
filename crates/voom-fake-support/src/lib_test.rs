use super::*;
use voom_worker_protocol::OperationKind;

#[test]
fn provider_definition_rejects_unsupported_operation() {
    let provider = provider_definition("fake-prober").unwrap();
    let req = request(
        OperationKind::Remux,
        serde_json::json!({"path": "/library/movie.mkv"}),
    );
    let err = dispatch_provider(&provider, &req).unwrap_err();
    assert!(matches!(
        err,
        voom_worker_protocol::ProtocolError::UnknownOperation { .. }
    ));
}

#[test]
fn provider_definition_accepts_secondary_operation() {
    let provider = provider_definition("fake-prober").unwrap();
    let req = request(
        OperationKind::HashFile,
        serde_json::json!({"path": "/library/movie.mkv"}),
    );
    let dispatch = dispatch_provider(&provider, &req).unwrap();
    assert_eq!(dispatch.response.lease_id, voom_core::LeaseId(42));
    assert!(
        String::from_utf8(dispatch.body)
            .unwrap()
            .contains("\"operation\":\"hash_file\"")
    );
}

#[test]
fn missing_path_is_invalid_payload() {
    let provider = provider_definition("fake-scanner").unwrap();
    let req = request(
        OperationKind::ScanLibrary,
        serde_json::json!({"scenario": "default"}),
    );
    let err = dispatch_provider(&provider, &req).unwrap_err();
    assert!(matches!(
        err,
        voom_worker_protocol::ProtocolError::InvalidPayload { .. }
    ));
}

fn request(
    operation: OperationKind,
    payload: serde_json::Value,
) -> voom_worker_protocol::OperationRequest {
    voom_worker_protocol::OperationRequest {
        operation,
        lease_id: voom_core::LeaseId(42),
        payload,
        heartbeat_deadline_ms: 1_000,
        progress_idle_deadline_ms: 1_000,
    }
}
