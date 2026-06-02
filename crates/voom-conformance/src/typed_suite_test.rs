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
        valid_payload: valid_transcode_video_payload(),
        invalid_payload: invalid_transcode_video_payload(),
    };
    let req = operation_request(voom_core::LeaseId(8), &case, PayloadKind::Invalid);
    assert_eq!(
        req.operation,
        voom_worker_protocol::OperationKind::TranscodeVideo
    );
    assert_eq!(req.payload, invalid_transcode_video_payload());
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
                valid_payload: valid_transcode_video_payload(),
                invalid_payload: invalid_transcode_video_payload(),
            },
            OperationCase {
                operation: voom_worker_protocol::OperationKind::ExtractAudio,
                valid_payload: valid_extract_audio_payload(),
                invalid_payload: invalid_extract_audio_payload(),
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

fn valid_transcode_video_payload() -> serde_json::Value {
    transcode_video_payload("hevc")
}

fn invalid_transcode_video_payload() -> serde_json::Value {
    transcode_video_payload("bad_codec")
}

fn transcode_video_payload(output_video_codec: &str) -> serde_json::Value {
    serde_json::json!({
        "input": {
            "path": "/library/example.mkv",
            "expected": {
                "size_bytes": 5_u64,
                "content_hash": "blake3:input"
            }
        },
        "output": {
            "staging_root": "/tmp/voom-stage",
            "path": "/tmp/voom-stage/example.video.mkv",
            "container": "mkv",
            "video_codec": output_video_codec,
            "overwrite": false
        },
        "profile": {
            "name": "default-hevc",
            "target_codec": "hevc",
            "encoder": "libx265",
            "crf": 23_u8,
            "preset": "medium"
        },
        "copy_video": false
    })
}

fn valid_extract_audio_payload() -> serde_json::Value {
    extract_audio_payload("opus")
}

fn invalid_extract_audio_payload() -> serde_json::Value {
    extract_audio_payload("bad_codec")
}

fn extract_audio_payload(audio_codec: &str) -> serde_json::Value {
    serde_json::json!({
        "input": {
            "path": "/library/example.mkv",
            "expected": {
                "size_bytes": 5_u64,
                "content_hash": "blake3:input"
            }
        },
        "output": {
            "staging_root": "/tmp/voom-stage",
            "path": "/tmp/voom-stage/example.audio.ogg",
            "container": "ogg",
            "audio_codec": audio_codec,
            "overwrite": false
        },
        "selection": {
            "snapshot_stream_id": "stream-1",
            "provider_stream_index": 1_u32
        }
    })
}
