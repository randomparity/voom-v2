#![expect(
    clippy::panic,
    reason = "benchmark-worker tests fail fast on unexpected frame variants"
)]

use super::*;

fn request(lease_id: u64, payload: serde_json::Value) -> OperationRequest {
    OperationRequest {
        operation: OperationKind::ProbeFile,
        lease_id: voom_core::LeaseId(lease_id),
        payload,
        heartbeat_deadline_ms: 1000,
        progress_idle_deadline_ms: 1000,
    }
}

#[test]
fn missing_mode_defaults_to_baseline_after_path_validation() {
    let parsed = parse_payload(serde_json::json!({"path": "/library/example.mkv"})).unwrap();
    assert_eq!(parsed.mode, BenchmarkMode::Baseline);
    assert_eq!(parsed.path, "/library/example.mkv");
    assert_eq!(parsed.operations, None);
    assert_eq!(parsed.emit_every, None);
}

#[test]
fn missing_path_is_invalid_even_when_mode_is_baseline() {
    let err = parse_payload(serde_json::json!({"mode": "baseline"})).unwrap_err();
    assert!(err.to_string().contains("payload missing path"));
}

#[test]
fn accepts_benchmark_with_valid_operations_and_emit_every() {
    let parsed = parse_payload(serde_json::json!({
        "path": "/library/example.mkv",
        "mode": "benchmark",
        "operations": 100,
        "emit_every": 10
    }))
    .unwrap();
    let config = benchmark_config(&parsed).unwrap();
    assert_eq!(config.operations, 100);
    assert_eq!(config.emit_every, 10);
    assert_eq!(config.progress_frames, 10);
}

#[test]
fn missing_emit_every_defaults_to_operations() {
    let parsed = parse_payload(serde_json::json!({
        "path": "/library/example.mkv",
        "mode": "benchmark",
        "operations": 25
    }))
    .unwrap();
    let config = benchmark_config(&parsed).unwrap();
    assert_eq!(config.emit_every, 25);
    assert_eq!(config.progress_frames, 1);
}

#[test]
fn rejects_unknown_mode() {
    let err = parse_payload(serde_json::json!({
        "path": "/library/example.mkv",
        "mode": "fast"
    }))
    .unwrap_err();
    assert!(err.to_string().contains("unknown benchmark mode"));
}

#[test]
fn rejects_missing_zero_and_excessive_operations() {
    for payload in [
        serde_json::json!({"path": "/library/example.mkv", "mode": "benchmark"}),
        serde_json::json!({"path": "/library/example.mkv", "mode": "benchmark", "operations": 0}),
        serde_json::json!({"path": "/library/example.mkv", "mode": "benchmark", "operations": 10_001}),
    ] {
        let err = parse_payload(payload).unwrap_err();
        assert!(matches!(err, ProtocolError::InvalidPayload { .. }));
    }
}

#[test]
fn rejects_invalid_emit_every() {
    for payload in [
        serde_json::json!({"path": "/library/example.mkv", "mode": "benchmark", "operations": 10, "emit_every": 0}),
        serde_json::json!({"path": "/library/example.mkv", "mode": "benchmark", "operations": 10, "emit_every": 11}),
    ] {
        let err = parse_payload(payload).unwrap_err();
        assert!(matches!(err, ProtocolError::InvalidPayload { .. }));
    }
}

#[test]
fn accepts_max_operations_when_progress_frame_count_is_capped() {
    let parsed = parse_payload(serde_json::json!({
        "path": "/library/example.mkv",
        "mode": "benchmark",
        "operations": 10_000,
        "emit_every": 100
    }))
    .unwrap();
    assert_eq!(benchmark_config(&parsed).unwrap().progress_frames, 100);
}

#[test]
fn rejects_max_operations_with_one_frame_per_operation() {
    let err = parse_payload(serde_json::json!({
        "path": "/library/example.mkv",
        "mode": "benchmark",
        "operations": 10_000,
        "emit_every": 1
    }))
    .unwrap_err();
    assert!(err.to_string().contains("progress_frames"));
}

#[test]
fn baseline_dispatch_emits_progress_and_result() {
    let req = request(7, serde_json::json!({"path": "/library/example.mkv"}));
    let dispatch = baseline_dispatch(&req, "/library/example.mkv").unwrap();
    let body = String::from_utf8(dispatch.body).unwrap();
    assert!(body.contains("\"kind\":\"progress\""));
    assert!(body.contains("\"kind\":\"result\""));
    assert!(body.contains("\"mode\":\"baseline\""));
}

#[test]
fn benchmark_dispatch_emits_cadence_and_final_totals() {
    let req = request(8, serde_json::json!({}));
    let config = BenchmarkConfig {
        path: "/library/example.mkv".to_owned(),
        operations: 25,
        emit_every: 10,
        progress_frames: 3,
    };
    let dispatch = benchmark_dispatch(&req, &config).unwrap();
    let frames: Vec<ProgressFrame> = dispatch
        .body
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_slice(line).unwrap())
        .collect();
    let mut progress_elapsed = Vec::new();
    let mut progress_cadence = Vec::new();
    let mut result_elapsed = None;
    let mut result_throughput = None;

    for frame in frames {
        match frame {
            ProgressFrame::Progress { payload, .. } => {
                let payload = payload.unwrap();
                assert_eq!(payload["mode"], "benchmark");
                assert_eq!(payload["operations_total"], 25);
                let elapsed = payload["elapsed_worker_ns"].as_u64().unwrap();
                assert!(elapsed > 0);
                if let Some(previous) = progress_elapsed.last() {
                    assert!(elapsed >= *previous);
                }
                progress_elapsed.push(elapsed);
                progress_cadence.push((
                    payload["sample_index"].as_u64().unwrap(),
                    payload["operations_completed"].as_u64().unwrap(),
                ));
            }
            ProgressFrame::Result { payload, .. } => {
                assert_eq!(payload["mode"], "benchmark");
                assert_eq!(payload["operations_total"], 25);
                assert_eq!(payload["progress_frames"], 3);
                result_elapsed = Some(payload["elapsed_worker_ns"].as_u64().unwrap());
                result_throughput = Some(payload["worker_ops_per_second_milli"].as_u64().unwrap());
            }
            ProgressFrame::Error { message, .. } => panic!("unexpected error frame: {message}"),
        }
    }

    assert_eq!(progress_elapsed.len(), 3);
    assert_eq!(progress_cadence, vec![(0, 10), (1, 20), (2, 25)]);
    let result_elapsed = result_elapsed.unwrap();
    assert!(result_elapsed > 0);
    assert!(result_elapsed >= *progress_elapsed.last().unwrap());
    assert!(result_throughput.unwrap() > 0);
}

#[test]
fn body_size_guard_accepts_at_or_below_limit() {
    let body = vec![b'x'; MAX_BENCHMARK_RESPONSE_BODY_BYTES];
    enforce_benchmark_body_size(&body, MAX_BENCHMARK_RESPONSE_BODY_BYTES).unwrap();
}

#[test]
fn benchmark_dispatch_rejects_generated_body_above_limit() {
    let req = request(9, serde_json::json!({}));
    let config = BenchmarkConfig {
        path: "/library/example.mkv".to_owned(),
        operations: 10,
        emit_every: 10,
        progress_frames: 1,
    };
    let err = benchmark_dispatch_with_body_limit(&req, &config, 1).unwrap_err();
    assert!(err.to_string().contains("benchmark response body"));
}
