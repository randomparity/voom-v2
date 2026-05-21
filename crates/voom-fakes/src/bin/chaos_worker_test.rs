use std::time::Duration;

use super::*;

#[test]
fn missing_mode_defaults_to_baseline_after_path_validation() {
    let parsed = parse_payload(serde_json::json!({"path": "/library/example.mkv"})).unwrap();
    assert_eq!(parsed.mode, ChaosMode::Baseline);
    assert_eq!(parsed.path, "/library/example.mkv");
    assert_eq!(parsed.progress_count, 3);
    assert_eq!(parsed.progress_interval, Duration::from_millis(50));
    assert_eq!(parsed.stall, Duration::from_millis(500));
}

#[test]
fn missing_path_is_invalid_even_when_mode_is_baseline() {
    let err = parse_payload(serde_json::json!({"mode": "baseline"})).unwrap_err();
    assert!(err.to_string().contains("payload missing path"));
}

#[test]
fn accepts_each_known_mode() {
    for mode in [
        "baseline",
        "crash",
        "stall",
        "malformed_result",
        "non_converging_progress",
        "deadline_exceeded",
    ] {
        let parsed =
            parse_payload(serde_json::json!({"path": "/library/example.mkv", "mode": mode}))
                .unwrap();
        assert_eq!(parsed.path, "/library/example.mkv");
    }
}

#[test]
fn rejects_unknown_mode() {
    let err = parse_payload(serde_json::json!({
        "path": "/library/example.mkv",
        "mode": "unknown"
    }))
    .unwrap_err();
    assert!(err.to_string().contains("unknown chaos mode"));
}

#[test]
fn accepts_explicit_count_and_timing_values() {
    let parsed = parse_payload(serde_json::json!({
        "path": "/library/example.mkv",
        "progress_count": 7,
        "progress_interval_ms": 125,
        "stall_ms": 250
    }))
    .unwrap();
    assert_eq!(parsed.progress_count, 7);
    assert_eq!(parsed.progress_interval, Duration::from_millis(125));
    assert_eq!(parsed.stall, Duration::from_millis(250));
}

#[test]
fn rejects_excessive_timing_values() {
    let err = parse_payload(serde_json::json!({
        "path": "/library/example.mkv",
        "stall_ms": 30001
    }))
    .unwrap_err();
    assert!(err.to_string().contains("stall_ms"));
}

#[test]
fn rejects_excessive_progress_count() {
    let err = parse_payload(serde_json::json!({
        "path": "/library/example.mkv",
        "progress_count": 129
    }))
    .unwrap_err();
    assert!(err.to_string().contains("progress_count"));
}

#[test]
fn baseline_body_has_progress_then_result() {
    let req = voom_worker_protocol::OperationRequest {
        operation: voom_worker_protocol::OperationKind::ProbeFile,
        lease_id: voom_core::LeaseId(42),
        payload: serde_json::json!({"path": "/library/example.mkv"}),
        heartbeat_deadline_ms: 1000,
        progress_idle_deadline_ms: 1000,
    };
    let payload = parse_payload(req.payload.clone()).unwrap();
    let body = baseline_body(&req, &payload).unwrap();
    let lines = std::str::from_utf8(&body)
        .unwrap()
        .lines()
        .collect::<Vec<_>>();
    assert_eq!(lines.len(), 2);
    assert!(lines[0].contains("\"kind\":\"progress\""));
    assert!(lines[0].contains("\"seq\":0"));
    assert!(lines[1].contains("\"kind\":\"result\""));
    assert!(lines[1].contains("\"seq\":1"));
}

#[test]
fn malformed_body_is_not_valid_progress_json() {
    assert!(serde_json::from_slice::<serde_json::Value>(&malformed_body()).is_err());
}
