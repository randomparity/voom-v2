use super::*;
use time::OffsetDateTime;
use voom_core::LeaseId;

fn fixed_time() -> OffsetDateTime {
    OffsetDateTime::from_unix_timestamp(1_779_192_000).unwrap()
}

#[test]
fn percent_bps_zero_and_full_construct() {
    assert_eq!(PercentBps::ZERO.bps(), 0);
    assert_eq!(PercentBps::FULL.bps(), 10_000);
}

#[test]
fn percent_bps_try_from_accepts_boundaries() {
    assert_eq!(PercentBps::try_from(0).unwrap().bps(), 0);
    assert_eq!(PercentBps::try_from(10_000).unwrap().bps(), 10_000);
}

#[test]
fn percent_bps_try_from_rejects_over_max() {
    assert!(PercentBps::try_from(10_001).is_err());
    assert!(PercentBps::try_from(65_535).is_err());
}

#[test]
fn percent_bps_deserialize_rejects_over_max() {
    let res: Result<PercentBps, _> = serde_json::from_str("10001");
    assert!(res.is_err(), "deserializing 10001 should reject");
}

#[test]
fn percent_bps_round_trips_serde() {
    let p = PercentBps::try_from(5000).unwrap();
    let json = serde_json::to_string(&p).unwrap();
    assert_eq!(json, "5000");
    let back: PercentBps = serde_json::from_str(&json).unwrap();
    assert_eq!(p, back);
}

#[test]
fn operation_request_round_trips() {
    let req = OperationRequest {
        operation: OperationKind::ProbeFile,
        lease_id: LeaseId(7),
        payload: serde_json::json!({"path": "/tmp/x"}),
        heartbeat_deadline_ms: 5_000,
        progress_idle_deadline_ms: 30_000,
    };
    let json = serde_json::to_string(&req).unwrap();
    let back: OperationRequest = serde_json::from_str(&json).unwrap();
    assert_eq!(req, back);
}

#[test]
fn operation_request_rejects_unknown_top_level_fields() {
    let raw = r#"{
        "operation": "probe_file",
        "lease_id": 7,
        "payload": null,
        "heartbeat_deadline_ms": 5000,
        "progress_idle_deadline_ms": 30000,
        "rogue_extra": true
    }"#;
    let res: Result<OperationRequest, _> = serde_json::from_str(raw);
    assert!(res.is_err(), "unknown field rogue_extra must reject");
}

#[test]
fn operation_response_round_trips() {
    let resp = OperationResponse {
        lease_id: LeaseId(7),
        accepted_at: fixed_time(),
    };
    let json = serde_json::to_string(&resp).unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&json).unwrap()["accepted_at"],
        "2026-05-19T12:00:00Z"
    );
    let back: OperationResponse = serde_json::from_str(&json).unwrap();
    assert_eq!(resp, back);
}

#[test]
fn progress_frame_progress_round_trips() {
    let frame = ProgressFrame::Progress {
        lease_id: LeaseId(1),
        seq: 0,
        emitted_at: fixed_time(),
        percent: Some(PercentBps::try_from(2500).unwrap()),
        message: Some("scanning".into()),
        payload: Some(serde_json::json!({"path": "/foo"})),
    };
    let json = serde_json::to_string(&frame).unwrap();
    let back: ProgressFrame = serde_json::from_str(&json).unwrap();
    assert_eq!(frame, back);
    assert_eq!(back.lease_id(), LeaseId(1));
    assert_eq!(back.seq(), 0);
    assert!(!back.is_terminal());
}

#[test]
fn progress_frame_result_is_terminal() {
    let frame = ProgressFrame::Result {
        lease_id: LeaseId(1),
        seq: 1,
        emitted_at: fixed_time(),
        payload: serde_json::json!({"ok": true}),
    };
    let json = serde_json::to_string(&frame).unwrap();
    let back: ProgressFrame = serde_json::from_str(&json).unwrap();
    assert_eq!(frame, back);
    assert!(back.is_terminal());
}

#[test]
fn progress_frame_error_round_trips() {
    let frame = ProgressFrame::Error {
        lease_id: LeaseId(1),
        seq: 1,
        emitted_at: fixed_time(),
        class: voom_core::FailureClass::WorkerTimeout,
        code: voom_core::ErrorCode::WorkerTimeout,
        message: "deadline exceeded".into(),
        payload: None,
    };
    let json = serde_json::to_string(&frame).unwrap();
    let back: ProgressFrame = serde_json::from_str(&json).unwrap();
    assert_eq!(frame, back);
    assert!(back.is_terminal());
}

#[test]
fn protocol_error_unsupported_version_round_trips() {
    let err = ProtocolError::UnsupportedProtocolVersion {
        offered: 99,
        supported_min: 1,
        supported_max: 1,
    };
    let json = serde_json::to_string(&err).unwrap();
    let back: ProtocolError = serde_json::from_str(&json).unwrap();
    assert_eq!(err, back);
}

#[test]
fn protocol_error_wrong_lease_round_trips() {
    let err = ProtocolError::WrongLeaseId {
        expected: LeaseId(1),
        got: LeaseId(2),
    };
    let json = serde_json::to_string(&err).unwrap();
    let back: ProtocolError = serde_json::from_str(&json).unwrap();
    assert_eq!(err, back);
}
