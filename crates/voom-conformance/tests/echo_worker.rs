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
    ProtocolError, WorkerCredentials,
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
        let remote_terminal = remote_probe(&client, &launch.credentials).await?;
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
        Ok::<_, ProtocolError>((
            probe.response,
            progress,
            terminal,
            remote_terminal,
            unsupported,
        ))
    }
    .await;

    let shutdown = launch.shutdown(Duration::from_secs(5)).await;
    let (response, progress, terminal, remote_terminal, unsupported) = outcomes.unwrap();
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
    assert_remote_artifact_access(remote_terminal);
    assert!(
        matches!(unsupported, Err(ProtocolError::UnknownOperation { .. })),
        "expected UnknownOperation, got {unsupported:?}"
    );
}

async fn remote_probe(
    client: &HttpClient,
    credentials: &WorkerCredentials,
) -> Result<NdjsonOutcome, ProtocolError> {
    let mut probe = client
        .dispatch(
            credentials,
            "echo-remote-probe-23",
            OperationRequest {
                operation: OperationKind::ProbeFile,
                lease_id: LeaseId(23),
                payload: serde_json::json!({
                    "path": "/tmp/remote.mov",
                    "artifact_access_plan": {
                        "id": 9,
                        "owner_node_id": 3
                    }
                }),
                heartbeat_deadline_ms: 1_000,
                progress_idle_deadline_ms: 1_000,
            },
        )
        .await?;
    let _progress = probe.frames.next_frame().await?;
    probe.frames.next_frame().await
}

fn assert_remote_artifact_access(terminal: NdjsonOutcome) {
    let NdjsonOutcome::Terminated(ProgressFrame::Result { payload, .. }) = terminal else {
        panic!("expected remote terminal result, got {terminal:?}");
    };
    assert_eq!(
        payload,
        serde_json::json!({
            "echoed_path": "/tmp/remote.mov",
            "artifact_access": {
                "validated": true,
                "owner_node_id": 3
            }
        })
    );
}

/// Issue #478: the control plane normalizes a namespaced byte-work ticket's
/// operation through `TicketOperation::normalize().matching_token()` before
/// dispatch, so the worker protocol only ever sees bare wire tokens. This
/// pins that a token derived from the reserved workflow namespace executes
/// identically to its canonical encoding at the protocol surface.
#[tokio::test]
async fn echo_worker_executes_the_normalized_namespaced_operation_token() {
    use voom_core::TicketOperation;

    let namespaced = TicketOperation::new("synthetic.workflow.operation.probe_file").unwrap();
    let normalized = namespaced.normalize().matching_token();
    assert_eq!(normalized.as_str(), "probe_file");

    let harness = Harness::new(env!("CARGO_BIN_EXE_echo-worker"));
    let launch = harness.launch().await.unwrap();

    let outcome = async {
        let client = HttpClient::new(launch.bound);
        let mut stream = client
            .dispatch(
                &launch.credentials,
                "echo-normalized-31",
                OperationRequest {
                    // What dispatch_to_child sends after normalization: the
                    // bare matching token, never the namespaced form.
                    operation: voom_core::OperationKind::from_wire(normalized.as_str())
                        .unwrap(),
                    lease_id: LeaseId(31),
                    payload: serde_json::json!({"path": "/tmp/input.mov"}),
                    heartbeat_deadline_ms: 1_000,
                    progress_idle_deadline_ms: 1_000,
                },
            )
            .await?;
        let _accepted = stream.frames.next_frame().await?;
        stream.frames.next_frame().await
    }
    .await;

    let shutdown = launch.shutdown(Duration::from_secs(5)).await;
    let terminal = outcome.unwrap();
    assert!(shutdown.unwrap().success(), "worker exited cleanly");
    match terminal {
        NdjsonOutcome::Terminated(ProgressFrame::Result { .. }) => {}
        other => panic!("expected a terminal result frame, got {other:?}"),
    }
}
