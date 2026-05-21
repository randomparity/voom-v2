use super::*;
use crate::manifest::{ActiveBinary, OperationCase};

#[test]
fn operation_request_uses_manifest_case_operation_payload_and_deadlines() {
    let case = OperationCase {
        operation: voom_worker_protocol::OperationKind::Remux,
        valid_payload: serde_json::json!({"path": "/library/example.mkv", "container": "mkv"}),
        invalid_payload: serde_json::json!({"container": "bad_container"}),
    };
    let req = operation_request(voom_core::LeaseId(7), &case, PayloadKind::Valid);
    assert_eq!(req.operation, voom_worker_protocol::OperationKind::Remux);
    assert_eq!(
        req.payload,
        serde_json::json!({"path": "/library/example.mkv", "container": "mkv"})
    );
    assert_eq!(req.lease_id, voom_core::LeaseId(7));
    assert_eq!(req.heartbeat_deadline_ms, 1_000);
    assert_eq!(req.progress_idle_deadline_ms, 1_000);
}

#[test]
fn operation_request_can_use_manifest_invalid_payload() {
    let case = OperationCase {
        operation: voom_worker_protocol::OperationKind::TranscodeVideo,
        valid_payload: serde_json::json!({"path": "/library/example.mkv", "target_codec": "h265"}),
        invalid_payload: serde_json::json!({"path": "/library/example.mkv", "target_codec": "bad_codec"}),
    };
    let req = operation_request(voom_core::LeaseId(8), &case, PayloadKind::Invalid);
    assert_eq!(
        req.operation,
        voom_worker_protocol::OperationKind::TranscodeVideo
    );
    assert_eq!(
        req.payload,
        serde_json::json!({"path": "/library/example.mkv", "target_codec": "bad_codec"})
    );
}

#[test]
fn operation_case_checks_include_invalid_payload_for_every_operation() {
    let entry = ActiveBinary {
        name: "fake-transcoder".to_owned(),
        target: "fake-transcoder".to_owned(),
        status: "active".to_owned(),
        required: true,
        operations: vec![
            OperationCase {
                operation: voom_worker_protocol::OperationKind::TranscodeVideo,
                valid_payload: serde_json::json!({
                    "path": "/library/example.mkv",
                    "target_codec": "h265"
                }),
                invalid_payload: serde_json::json!({
                    "path": "/library/example.mkv",
                    "target_codec": "bad_codec"
                }),
            },
            OperationCase {
                operation: voom_worker_protocol::OperationKind::ExtractAudio,
                valid_payload: serde_json::json!({
                    "path": "/library/example.mkv",
                    "target_codec": "h265"
                }),
                invalid_payload: serde_json::json!({
                    "path": "/library/example.mkv",
                    "target_codec": "bad_codec"
                }),
            },
        ],
        path: None,
    };

    let names = operation_case_check_names(&entry);

    assert!(names.contains(
        &"fake-transcoder::transcode_video::operation_case_accepts_valid_payload".to_owned()
    ));
    assert!(names.contains(
        &"fake-transcoder::transcode_video::operation_case_rejects_invalid_payload".to_owned()
    ));
    assert!(names.contains(
        &"fake-transcoder::extract_audio::operation_case_accepts_valid_payload".to_owned()
    ));
    assert!(names.contains(
        &"fake-transcoder::extract_audio::operation_case_rejects_invalid_payload".to_owned()
    ));
}
