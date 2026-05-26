use super::*;
use voom_worker_protocol::{OperationDispatch, OperationKind, ProgressFrame, http::OperationBody};

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
        String::from_utf8(body_bytes_for_test(dispatch))
            .unwrap()
            .contains("\"operation\":\"hash_file\"")
    );
}

#[test]
fn provider_definition_for_operation_reuses_catalog_secondary_operations() {
    let provider = provider_definition_for_operation(OperationKind::HashFile).unwrap();

    assert_eq!(provider.binary_name, "fake-prober");
    assert_eq!(provider.provider, "fake-prober");
}

#[test]
fn scanner_fan_out_count_controls_file_count() {
    let req = request(
        OperationKind::ScanLibrary,
        serde_json::json!({"path": "/library", "fan_out_count": 3}),
    );
    let result = dispatch_provider(&provider_definition("fake-scanner").unwrap(), &req).unwrap();
    let body = body_bytes_for_test(result);
    let frames = decode_frames(&body);
    let terminal = terminal_payload(&frames);
    assert_eq!(terminal["files"].as_array().unwrap().len(), 3);
    assert_eq!(terminal["files"][0]["path"], "/library/file-000.mkv");
    assert_eq!(terminal["files"][2]["path"], "/library/file-002.mkv");
}

#[test]
fn scanner_rejects_zero_and_over_cap_fan_out_count() {
    for fan_out_count in [0_u64, u64::from(MAX_FAKE_FAN_OUT_COUNT) + 1] {
        let req = request(
            OperationKind::ScanLibrary,
            serde_json::json!({"path": "/library", "fan_out_count": fan_out_count}),
        );
        let err =
            dispatch_provider(&provider_definition("fake-scanner").unwrap(), &req).unwrap_err();
        assert!(matches!(
            err,
            voom_worker_protocol::ProtocolError::InvalidPayload { .. }
        ));
    }
}

#[test]
fn timed_request_rejects_excessive_progress_frame_count() {
    let req = request(
        OperationKind::ScanLibrary,
        serde_json::json!({
            "path": "/library",
            "duration_ms": MAX_FAKE_DURATION_MS,
            "progress_interval_ms": 1_u64
        }),
    );
    let err = dispatch_provider(&provider_definition("fake-scanner").unwrap(), &req).unwrap_err();
    assert!(matches!(
        err,
        voom_worker_protocol::ProtocolError::InvalidPayload { .. }
    ));
}

#[test]
fn quality_needs_transcode_from_bound_codec() {
    let req = request(
        OperationKind::ScoreQuality,
        serde_json::json!({
            "path": "/library/file-001.mkv",
            "profile": "default",
            "codec": "h264"
        }),
    );
    let result =
        dispatch_provider(&provider_definition("fake-quality-scorer").unwrap(), &req).unwrap();
    let body = body_bytes_for_test(result);
    let frames = decode_frames(&body);
    let payload = terminal_payload(&frames);
    assert_eq!(payload["needs_transcode"], true);
}

#[test]
fn prober_result_preserves_bound_codec_when_present() {
    let req = request(
        OperationKind::ProbeFile,
        serde_json::json!({"path": "/library/file-000.mkv", "codec": "h265"}),
    );
    let result = dispatch_provider(&provider_definition("fake-prober").unwrap(), &req).unwrap();
    let body = body_bytes_for_test(result);
    let frames = decode_frames(&body);
    let payload = terminal_payload(&frames);
    assert_eq!(payload["codec"], "h265");
}

#[test]
fn remux_and_transcode_emit_output_path() {
    let remux = request(
        OperationKind::Remux,
        serde_json::json!({"path": "/library/file-000.mkv", "container": "mkv"}),
    );
    let transcode = request(
        OperationKind::TranscodeVideo,
        serde_json::json!({"path": "/library/file-001.mkv", "target_codec": "h265"}),
    );
    for (provider, req) in [("fake-remuxer", remux), ("fake-transcoder", transcode)] {
        let result = dispatch_provider(&provider_definition(provider).unwrap(), &req).unwrap();
        let body = body_bytes_for_test(result);
        let frames = decode_frames(&body);
        let payload = terminal_payload(&frames);
        assert!(
            payload["output_path"]
                .as_str()
                .unwrap()
                .starts_with("/library/")
        );
    }
}

#[test]
fn fake_transcoder_accepts_audio_transcode_codec() {
    let req = request(
        OperationKind::TranscodeAudio,
        serde_json::json!({"path": "/library/file-001.mkv", "target_codec": "opus"}),
    );

    let result = dispatch_provider(&provider_definition("fake-transcoder").unwrap(), &req).unwrap();
    let body = body_bytes_for_test(result);
    let frames = decode_frames(&body);
    let payload = terminal_payload(&frames);

    assert_eq!(payload["operation"], "transcode_audio");
    assert_eq!(payload["target_codec"], "opus");
}

#[test]
fn artifact_access_evidence_validates_selected_advertised_mode() {
    let payload = artifact_access_payload("shared_mount", &["shared_mount"]);

    let evidence = synthetic_artifact_access_evidence(&payload).unwrap();

    assert_eq!(
        evidence,
        serde_json::json!({
            "artifact_access": {
                "inputs_consumed": ["handle:input:1"],
                "outputs_declared": ["handle:output:1"],
                "mode": "shared_mount",
                "validated": true
            }
        })
    );
}

#[test]
fn artifact_access_evidence_rejects_unadvertised_selected_mode() {
    let payload = artifact_access_payload("shared_mount", &["control_plane_placeholder"]);

    let err = synthetic_artifact_access_evidence(&payload).unwrap_err();

    let voom_worker_protocol::ProtocolError::InvalidPayload { detail } = err else {
        panic!("expected invalid payload for unadvertised artifact access mode");
    };
    assert!(detail.contains("artifact access mode shared_mount is not advertised"));
}

#[test]
fn artifact_access_evidence_is_empty_without_plan() {
    let evidence =
        synthetic_artifact_access_evidence(&serde_json::json!({"path": "/library/movie.mkv"}))
            .unwrap();

    assert_eq!(evidence, serde_json::json!({}));
}

#[test]
fn artifact_access_evidence_is_merged_into_result_payload() {
    let req = request(
        OperationKind::ProbeFile,
        serde_json::json!({
            "path": "/library/file-000.mkv",
            "artifact_access_plan": {
                "id": 7,
                "input_handles": ["handle:input:1"],
                "output_handles": ["handle:output:1"],
                "selected_access_mode": "shared_mount"
            },
            "advertised_artifact_access": ["shared_mount"]
        }),
    );

    let result = dispatch_provider(&provider_definition("fake-prober").unwrap(), &req).unwrap();
    let body = body_bytes_for_test(result);
    let frames = decode_frames(&body);
    let payload = terminal_payload(&frames);

    assert_eq!(
        payload["artifact_access"],
        serde_json::json!({
            "inputs_consumed": ["handle:input:1"],
            "outputs_declared": ["handle:output:1"],
            "mode": "shared_mount",
            "validated": true
        })
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

fn body_bytes_for_test(dispatch: OperationDispatch) -> Vec<u8> {
    match dispatch.body {
        OperationBody::Buffered(body) => body,
        OperationBody::Streaming(_) => {
            panic!("expected buffered fake dispatch for no-duration unit test")
        }
    }
}

fn decode_frames(body: &[u8]) -> Vec<ProgressFrame> {
    std::str::from_utf8(body)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn terminal_payload(frames: &[ProgressFrame]) -> &serde_json::Value {
    let Some(ProgressFrame::Result { payload, .. }) = frames.last() else {
        panic!("expected terminal result frame");
    };
    payload
}

fn artifact_access_payload(
    selected_access_mode: &str,
    advertised_artifact_access: &[&str],
) -> serde_json::Value {
    serde_json::json!({
        "artifact_access_plan": {
            "id": 7,
            "input_handles": ["handle:input:1"],
            "output_handles": ["handle:output:1"],
            "selected_access_mode": selected_access_mode
        },
        "advertised_artifact_access": advertised_artifact_access
    })
}
