#![expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "integration tests favor unwrap/panic over plumbing Result<()> through every assertion"
)]

#[path = "../src/bin/echo_worker.rs"]
#[expect(
    dead_code,
    reason = "integration test includes the echo-worker binary module but calls only the handler"
)]
mod echo_worker;

use voom_core::LeaseId;
use voom_worker_protocol::http::OperationBody;
use voom_worker_protocol::{OperationKind, OperationRequest, ProgressFrame};

#[tokio::test]
async fn probe_file_echoes_path_after_progress_frame() {
    let dispatch = echo_worker::handle_operation(OperationRequest {
        operation: OperationKind::ProbeFile,
        lease_id: LeaseId(21),
        payload: serde_json::json!({"path": "/tmp/input.mov"}),
        heartbeat_deadline_ms: 1_000,
        progress_idle_deadline_ms: 1_000,
    })
    .await
    .unwrap();

    assert_eq!(dispatch.response.lease_id, LeaseId(21));
    let OperationBody::Buffered(body) = dispatch.body else {
        panic!("echo worker must return a buffered response");
    };
    let lines = String::from_utf8(body).unwrap();
    let frames: Vec<ProgressFrame> = lines
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();

    assert_eq!(frames.len(), 2);
    assert!(matches!(
        &frames[0],
        ProgressFrame::Progress {
            lease_id: LeaseId(21),
            seq: 0,
            message: Some(message),
            ..
        } if message == "probing /tmp/input.mov"
    ));
    assert!(matches!(
        &frames[1],
        ProgressFrame::Result {
            lease_id: LeaseId(21),
            seq: 1,
            payload,
            ..
        } if payload == &serde_json::json!({"echoed_path": "/tmp/input.mov"})
    ));
}
