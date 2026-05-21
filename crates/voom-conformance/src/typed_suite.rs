use voom_core::LeaseId;
use voom_worker_protocol::{
    ClientHandle, HttpClient, NdjsonOutcome, OperationKind, OperationRequest, ProtocolError,
};

pub(crate) fn probe_request(lease_id: LeaseId, path: &str) -> OperationRequest {
    OperationRequest {
        operation: OperationKind::ProbeFile,
        lease_id,
        payload: serde_json::json!({ "path": path }),
        heartbeat_deadline_ms: 1_000,
        progress_idle_deadline_ms: 1_000,
    }
}

pub(crate) fn missing_path_request(lease_id: LeaseId) -> OperationRequest {
    OperationRequest {
        operation: OperationKind::ProbeFile,
        lease_id,
        payload: serde_json::json!({}),
        heartbeat_deadline_ms: 1_000,
        progress_idle_deadline_ms: 1_000,
    }
}

pub async fn run(launch: &mut crate::WorkerLaunch) -> crate::SuiteResult {
    let client = HttpClient::new(launch.bound);
    let mut result = crate::SuiteResult::default();

    record(
        &mut result,
        "handshake_returns_supported_version",
        handshake_returns_supported_version(&client),
    )
    .await;
    record(
        &mut result,
        "handshake_rejects_below_supported_min",
        handshake_rejects_below_supported_min(&client),
    )
    .await;
    record(
        &mut result,
        "probe_file_accepts_valid_payload",
        probe_file_accepts_valid_payload(&client, launch),
    )
    .await;
    record(
        &mut result,
        "probe_file_rejects_missing_path",
        probe_file_rejects_missing_path(&client, launch),
    )
    .await;
    record(
        &mut result,
        "unknown_operation_rejected",
        unknown_operation_rejected(&client, launch),
    )
    .await;
    record(
        &mut result,
        "progress_seq_starts_at_zero",
        progress_seq_starts_at_zero(&client, launch),
    )
    .await;
    record(
        &mut result,
        "progress_seq_is_monotonic",
        progress_seq_is_monotonic(&client, launch),
    )
    .await;
    record(
        &mut result,
        "terminal_frame_is_last",
        terminal_frame_is_last(&client, launch),
    )
    .await;
    record(
        &mut result,
        "wrong_bearer_rejected",
        wrong_bearer_rejected(&client, launch),
    )
    .await;
    record(
        &mut result,
        "wrong_worker_id_rejected",
        wrong_worker_id_rejected(&client, launch),
    )
    .await;
    record(
        &mut result,
        "wrong_worker_epoch_rejected",
        wrong_worker_epoch_rejected(&client, launch),
    )
    .await;
    record(
        &mut result,
        "idempotency_same_logical_request_replay_returns_cached_response",
        idempotency_same_logical_request_replay_returns_cached_response(&client, launch),
    )
    .await;
    record(
        &mut result,
        "idempotency_same_key_different_body_rejected",
        idempotency_same_key_different_body_rejected(&client, launch),
    )
    .await;

    result
}

async fn handshake_returns_supported_version(client: &HttpClient) -> Result<(), ProtocolError> {
    let resp = client.handshake(voom_core::PROTOCOL_VERSION).await?;
    if resp.agreed == voom_core::PROTOCOL_VERSION {
        Ok(())
    } else {
        Err(ProtocolError::InvalidPayload {
            detail: format!("agreed={}", resp.agreed),
        })
    }
}

async fn handshake_rejects_below_supported_min(client: &HttpClient) -> Result<(), ProtocolError> {
    expect_protocol_err(
        client
            .handshake(voom_core::PROTOCOL_VERSION_SUPPORTED_MIN - 1)
            .await,
        |e| matches!(e, ProtocolError::UnsupportedProtocolVersion { .. }),
    )
}

async fn probe_file_accepts_valid_payload(
    client: &HttpClient,
    launch: &crate::WorkerLaunch,
) -> Result<(), ProtocolError> {
    let mut stream = client
        .dispatch(
            &launch.credentials,
            "typed-valid",
            probe_request(LeaseId(10), "/library/example.mkv"),
        )
        .await?;
    require_frame(&mut stream.frames).await?;
    require_terminal(&mut stream.frames).await?;
    Ok(())
}

async fn probe_file_rejects_missing_path(
    client: &HttpClient,
    launch: &crate::WorkerLaunch,
) -> Result<(), ProtocolError> {
    expect_protocol_err(
        client
            .dispatch(
                &launch.credentials,
                "typed-missing-path",
                missing_path_request(LeaseId(11)),
            )
            .await,
        |e| matches!(e, ProtocolError::InvalidPayload { .. }),
    )
}

async fn unknown_operation_rejected(
    client: &HttpClient,
    launch: &crate::WorkerLaunch,
) -> Result<(), ProtocolError> {
    let mut req = probe_request(LeaseId(12), "/library/example.mkv");
    req.operation = OperationKind::HashFile;
    expect_protocol_err(
        client
            .dispatch(&launch.credentials, "typed-unknown-operation", req)
            .await,
        |e| matches!(e, ProtocolError::UnknownOperation { .. }),
    )
}

async fn progress_seq_starts_at_zero(
    client: &HttpClient,
    launch: &crate::WorkerLaunch,
) -> Result<(), ProtocolError> {
    let mut stream = client
        .dispatch(
            &launch.credentials,
            "typed-progress-zero",
            probe_request(LeaseId(13), "/library/example.mkv"),
        )
        .await?;
    let frame = require_frame(&mut stream.frames).await?;
    if frame.seq() == 0 {
        Ok(())
    } else {
        Err(ProtocolError::OutOfOrderFrame {
            expected_seq: 0,
            got_seq: frame.seq(),
        })
    }
}

async fn progress_seq_is_monotonic(
    client: &HttpClient,
    launch: &crate::WorkerLaunch,
) -> Result<(), ProtocolError> {
    let mut stream = client
        .dispatch(
            &launch.credentials,
            "typed-progress-monotonic",
            probe_request(LeaseId(14), "/library/example.mkv"),
        )
        .await?;
    let first = require_frame(&mut stream.frames).await?;
    let terminal = require_terminal(&mut stream.frames).await?;
    if terminal.seq() == first.seq() + 1 {
        Ok(())
    } else {
        Err(ProtocolError::OutOfOrderFrame {
            expected_seq: first.seq() + 1,
            got_seq: terminal.seq(),
        })
    }
}

async fn terminal_frame_is_last(
    client: &HttpClient,
    launch: &crate::WorkerLaunch,
) -> Result<(), ProtocolError> {
    let mut stream = client
        .dispatch(
            &launch.credentials,
            "typed-terminal-last",
            probe_request(LeaseId(15), "/library/example.mkv"),
        )
        .await?;
    require_frame(&mut stream.frames).await?;
    require_terminal(&mut stream.frames).await?;
    expect_protocol_err(stream.frames.next_frame().await, |e| {
        matches!(e, ProtocolError::UnexpectedFrameAfterTerminal)
    })
}

async fn wrong_bearer_rejected(
    client: &HttpClient,
    launch: &crate::WorkerLaunch,
) -> Result<(), ProtocolError> {
    let mut creds = launch.credentials.clone();
    creds.secret = secrecy::SecretString::from("wrong");
    expect_protocol_err(
        client
            .dispatch(
                &creds,
                "typed-wrong-bearer",
                probe_request(LeaseId(16), "/library/example.mkv"),
            )
            .await,
        |e| matches!(e, ProtocolError::UnauthorizedBearer),
    )
}

async fn wrong_worker_id_rejected(
    client: &HttpClient,
    launch: &crate::WorkerLaunch,
) -> Result<(), ProtocolError> {
    let mut creds = launch.credentials.clone();
    creds.worker_id = voom_core::WorkerId(999);
    expect_protocol_err(
        client
            .dispatch(
                &creds,
                "typed-wrong-worker-id",
                probe_request(LeaseId(17), "/library/example.mkv"),
            )
            .await,
        |e| matches!(e, ProtocolError::UnknownWorkerId { .. }),
    )
}

async fn wrong_worker_epoch_rejected(
    client: &HttpClient,
    launch: &crate::WorkerLaunch,
) -> Result<(), ProtocolError> {
    let mut creds = launch.credentials.clone();
    creds.worker_epoch += 1;
    expect_protocol_err(
        client
            .dispatch(
                &creds,
                "typed-wrong-worker-epoch",
                probe_request(LeaseId(18), "/library/example.mkv"),
            )
            .await,
        |e| matches!(e, ProtocolError::StaleWorkerEpoch { .. }),
    )
}

async fn idempotency_same_logical_request_replay_returns_cached_response(
    client: &HttpClient,
    launch: &crate::WorkerLaunch,
) -> Result<(), ProtocolError> {
    let req = probe_request(LeaseId(19), "/library/example.mkv");
    let mut first = client
        .dispatch(&launch.credentials, "typed-replay", req.clone())
        .await?;
    drain_stream(&mut first.frames).await?;
    let mut second = client
        .dispatch(&launch.credentials, "typed-replay", req)
        .await?;
    drain_stream(&mut second.frames).await?;
    if first.response.lease_id == second.response.lease_id {
        Ok(())
    } else {
        Err(ProtocolError::WrongLeaseId {
            expected: first.response.lease_id,
            got: second.response.lease_id,
        })
    }
}

async fn idempotency_same_key_different_body_rejected(
    client: &HttpClient,
    launch: &crate::WorkerLaunch,
) -> Result<(), ProtocolError> {
    let mut first = client
        .dispatch(
            &launch.credentials,
            "typed-replay-conflict",
            probe_request(LeaseId(20), "/library/one.mkv"),
        )
        .await?;
    drain_stream(&mut first.frames).await?;
    expect_protocol_err(
        client
            .dispatch(
                &launch.credentials,
                "typed-replay-conflict",
                probe_request(LeaseId(20), "/library/two.mkv"),
            )
            .await,
        |e| matches!(e, ProtocolError::DuplicateIdempotencyKey { .. }),
    )
}

async fn record<F>(result: &mut crate::SuiteResult, name: &'static str, fut: F)
where
    F: std::future::Future<Output = Result<(), ProtocolError>>,
{
    match fut.await {
        Ok(()) => result.pass(name),
        Err(e) => result.fail(name, e.to_string()),
    }
}

fn expect_protocol_err(
    got: Result<impl std::fmt::Debug, ProtocolError>,
    predicate: impl FnOnce(&ProtocolError) -> bool,
) -> Result<(), ProtocolError> {
    match got {
        Ok(v) => Err(ProtocolError::InvalidPayload {
            detail: format!("expected error, got {v:?}"),
        }),
        Err(e) if predicate(&e) => Ok(()),
        Err(e) => Err(e),
    }
}

async fn require_frame(
    frames: &mut voom_worker_protocol::NdjsonStream,
) -> Result<voom_worker_protocol::ProgressFrame, ProtocolError> {
    match frames.next_frame().await? {
        NdjsonOutcome::Frame(frame) => Ok(frame),
        got => Err(ProtocolError::InvalidPayload {
            detail: format!("expected progress frame, got {got:?}"),
        }),
    }
}

async fn require_terminal(
    frames: &mut voom_worker_protocol::NdjsonStream,
) -> Result<voom_worker_protocol::ProgressFrame, ProtocolError> {
    match frames.next_frame().await? {
        NdjsonOutcome::Terminated(frame) => Ok(frame),
        got => Err(ProtocolError::InvalidPayload {
            detail: format!("expected terminal frame, got {got:?}"),
        }),
    }
}

async fn drain_stream(
    frames: &mut voom_worker_protocol::NdjsonStream,
) -> Result<(), ProtocolError> {
    loop {
        match frames.next_frame().await? {
            NdjsonOutcome::Frame(_) => {}
            NdjsonOutcome::Terminated(_) | NdjsonOutcome::Closed => return Ok(()),
            NdjsonOutcome::StreamEnd { .. } => {
                return Err(ProtocolError::MalformedFrame {
                    detail: "stream ended before terminal".to_owned(),
                });
            }
        }
    }
}

#[cfg(test)]
#[path = "typed_suite_test.rs"]
mod tests;
