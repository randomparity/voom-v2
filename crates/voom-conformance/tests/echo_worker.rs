#![expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "integration tests favor unwrap/panic over plumbing Result<()> through every assertion"
)]

use std::time::Duration;

use voom_conformance::Harness;
use voom_core::LeaseId;
use voom_worker_protocol::{
    ClientHandle, HttpClient, NdjsonOutcome, OperationKind, OperationRequest, ProgressFrame,
    ProtocolError,
};

#[tokio::test]
async fn echo_worker_binary_preserves_probe_and_unknown_operation_contracts() {
    let harness = Harness::new(env!("CARGO_BIN_EXE_echo-worker"));
    let launch = harness.launch().await.unwrap();
    let bound = launch.bound;

    let outcomes = async {
        let client = HttpClient::new(bound);
        let mut probe = client
            .dispatch(
                &launch.credentials,
                "echo-probe-21",
                OperationRequest {
                    operation: OperationKind::ProbeFile,
                    lease_id: LeaseId(21),
                    payload: serde_json::json!({"path": "/tmp/input.mov"}),
                    heartbeat_deadline_ms: 1_000,
                    progress_idle_deadline_ms: 1_000,
                },
            )
            .await?;
        let progress = probe.frames.next_frame().await?;
        let terminal = probe.frames.next_frame().await?;
        let unsupported = client
            .dispatch(
                &launch.credentials,
                "echo-hash-22",
                OperationRequest {
                    operation: OperationKind::HashFile,
                    lease_id: LeaseId(22),
                    payload: serde_json::json!({}),
                    heartbeat_deadline_ms: 1_000,
                    progress_idle_deadline_ms: 1_000,
                },
            )
            .await;
        Ok::<_, ProtocolError>((probe.response, progress, terminal, unsupported))
    }
    .await;

    let shutdown = launch.shutdown(Duration::from_secs(5)).await;
    let (response, progress, terminal, unsupported) = outcomes.unwrap();
    let status = shutdown.unwrap();

    assert!(bound.ip().is_loopback(), "worker bound to {bound}");
    assert_ne!(bound.port(), 0, "worker must resolve its ephemeral port");
    assert!(status.success(), "worker exited with {status}");
    assert_eq!(response.lease_id, LeaseId(21));

    let NdjsonOutcome::Frame(ProgressFrame::Progress {
        lease_id,
        seq,
        percent,
        message,
        payload,
        ..
    }) = progress
    else {
        panic!("expected one progress frame, got {progress:?}");
    };
    assert_eq!(lease_id, LeaseId(21));
    assert_eq!(seq, 0);
    assert_eq!(percent, None);
    assert_eq!(message.as_deref(), Some("probing /tmp/input.mov"));
    assert_eq!(payload, None);

    let NdjsonOutcome::Terminated(ProgressFrame::Result {
        lease_id,
        seq,
        payload,
        ..
    }) = terminal
    else {
        panic!("expected one terminal result, got {terminal:?}");
    };
    assert_eq!(lease_id, LeaseId(21));
    assert_eq!(seq, 1);
    assert_eq!(
        payload,
        serde_json::json!({"echoed_path": "/tmp/input.mov"})
    );
    assert!(
        matches!(unsupported, Err(ProtocolError::UnknownOperation { .. })),
        "expected UnknownOperation, got {unsupported:?}"
    );
}
