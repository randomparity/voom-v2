use voom_core::LeaseId;
use voom_worker_protocol::{
    ClientHandle, HttpClient, NdjsonOutcome, OperationKind, OperationRequest, ProtocolError,
};

use crate::manifest::{ActiveBinary, OperationCase};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PayloadKind {
    Valid,
    Invalid,
}

pub(crate) fn operation_request(
    lease_id: LeaseId,
    case: &OperationCase,
    payload_kind: PayloadKind,
) -> OperationRequest {
    let payload = match payload_kind {
        PayloadKind::Valid => case.valid_payload.clone(),
        PayloadKind::Invalid => case.invalid_payload.clone(),
    };
    OperationRequest {
        operation: case.operation,
        lease_id,
        payload,
        heartbeat_deadline_ms: 1_000,
        progress_idle_deadline_ms: 1_000,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct OperationCaseCheckNames {
    valid: String,
    invalid: String,
}

pub(crate) fn operation_case_check_names(entry: &ActiveBinary) -> Vec<String> {
    operation_case_checks(entry)
        .into_iter()
        .flat_map(|check| [check.valid, check.invalid])
        .collect()
}

fn operation_case_checks(entry: &ActiveBinary) -> Vec<OperationCaseCheckNames> {
    entry
        .operations
        .iter()
        .map(|case| {
            let operation = operation_name(case.operation);
            OperationCaseCheckNames {
                valid: format!(
                    "{}::{operation}::operation_case_accepts_valid_payload",
                    entry.name
                ),
                invalid: format!(
                    "{}::{operation}::operation_case_rejects_invalid_payload",
                    entry.name
                ),
            }
        })
        .collect()
}

#[expect(
    clippy::too_many_lines,
    reason = "suite assembly stays explicit so each conformance check has a stable name"
)]
pub async fn run(launch: &mut crate::WorkerLaunch, entry: &ActiveBinary) -> crate::SuiteResult {
    let client = HttpClient::new(launch.bound);
    let mut result = crate::SuiteResult::default();
    let Some(primary_case) = entry.operations.first() else {
        result.fail(
            format!("{}::typed_suite_has_operation_case", entry.name),
            "active binary has no operation cases",
        );
        return result;
    };

    record(
        &mut result,
        format!("{}::handshake_returns_supported_version", entry.name),
        handshake_returns_supported_version(&client),
    )
    .await;
    record(
        &mut result,
        format!("{}::handshake_rejects_unsupported_version", entry.name),
        handshake_rejects_unsupported_version(&client),
    )
    .await;
    record(
        &mut result,
        format!("{}::unknown_operation_rejected", entry.name),
        unknown_operation_rejected(&client, launch, entry),
    )
    .await;
    record(
        &mut result,
        format!(
            "{}::{}::progress_seq_starts_at_zero",
            entry.name,
            operation_name(primary_case.operation)
        ),
        progress_seq_starts_at_zero(&client, launch, primary_case),
    )
    .await;
    record(
        &mut result,
        format!(
            "{}::{}::progress_seq_is_monotonic",
            entry.name,
            operation_name(primary_case.operation)
        ),
        progress_seq_is_monotonic(&client, launch, primary_case),
    )
    .await;
    record(
        &mut result,
        format!(
            "{}::{}::terminal_frame_is_last",
            entry.name,
            operation_name(primary_case.operation)
        ),
        terminal_frame_is_last(&client, launch, primary_case),
    )
    .await;
    record(
        &mut result,
        format!("{}::wrong_bearer_rejected", entry.name),
        wrong_bearer_rejected(&client, launch, primary_case),
    )
    .await;
    record(
        &mut result,
        format!("{}::wrong_worker_id_rejected", entry.name),
        wrong_worker_id_rejected(&client, launch, primary_case),
    )
    .await;
    record(
        &mut result,
        format!("{}::wrong_worker_epoch_rejected", entry.name),
        wrong_worker_epoch_rejected(&client, launch, primary_case),
    )
    .await;
    record(
        &mut result,
        format!(
            "{}::idempotency_same_logical_request_replay_returns_cached_response",
            entry.name
        ),
        idempotency_same_logical_request_replay_returns_cached_response(
            &client,
            launch,
            primary_case,
        ),
    )
    .await;
    record(
        &mut result,
        format!(
            "{}::idempotency_same_key_different_body_rejected",
            entry.name
        ),
        idempotency_same_key_different_body_rejected(&client, launch, primary_case),
    )
    .await;

    let check_names = operation_case_check_names(entry);
    for (case, names) in entry.operations.iter().zip(check_names.chunks_exact(2)) {
        record(
            &mut result,
            names[0].clone(),
            accepts_valid_payload(&client, launch, case),
        )
        .await;
        record(
            &mut result,
            names[1].clone(),
            rejects_invalid_payload(&client, launch, case),
        )
        .await;
    }

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

async fn handshake_rejects_unsupported_version(client: &HttpClient) -> Result<(), ProtocolError> {
    expect_protocol_err(
        client.handshake(voom_core::PROTOCOL_VERSION + 1).await,
        |e| matches!(e, ProtocolError::UnsupportedProtocolVersion { .. }),
    )
}

async fn accepts_valid_payload(
    client: &HttpClient,
    launch: &crate::WorkerLaunch,
    case: &OperationCase,
) -> Result<(), ProtocolError> {
    let mut stream = client
        .dispatch(
            &launch.credentials,
            &format!("typed-valid-{}", operation_name(case.operation)),
            operation_request(LeaseId(10), case, PayloadKind::Valid),
        )
        .await?;
    require_frame(&mut stream.frames).await?;
    require_terminal(&mut stream.frames).await?;
    Ok(())
}

async fn rejects_invalid_payload(
    client: &HttpClient,
    launch: &crate::WorkerLaunch,
    case: &OperationCase,
) -> Result<(), ProtocolError> {
    expect_protocol_err(
        client
            .dispatch(
                &launch.credentials,
                &format!("typed-invalid-{}", operation_name(case.operation)),
                operation_request(LeaseId(11), case, PayloadKind::Invalid),
            )
            .await,
        |e| matches!(e, ProtocolError::InvalidPayload { .. }),
    )
}

async fn unknown_operation_rejected(
    client: &HttpClient,
    launch: &crate::WorkerLaunch,
    entry: &ActiveBinary,
) -> Result<(), ProtocolError> {
    let Some(case) = entry.operations.first() else {
        return Err(ProtocolError::InvalidPayload {
            detail: "active binary has no operation cases".to_owned(),
        });
    };
    let Some(unknown) = OperationKind::ALL.iter().copied().find(|operation| {
        !entry
            .operations
            .iter()
            .any(|case| case.operation == *operation)
    }) else {
        return Err(ProtocolError::InvalidPayload {
            detail: "active binary declares every fixed operation".to_owned(),
        });
    };
    let mut req = operation_request(LeaseId(12), case, PayloadKind::Valid);
    req.operation = unknown;
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
    case: &OperationCase,
) -> Result<(), ProtocolError> {
    let mut stream = client
        .dispatch(
            &launch.credentials,
            "typed-progress-zero",
            operation_request(LeaseId(13), case, PayloadKind::Valid),
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
    case: &OperationCase,
) -> Result<(), ProtocolError> {
    let mut stream = client
        .dispatch(
            &launch.credentials,
            "typed-progress-monotonic",
            operation_request(LeaseId(14), case, PayloadKind::Valid),
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
    case: &OperationCase,
) -> Result<(), ProtocolError> {
    let mut stream = client
        .dispatch(
            &launch.credentials,
            "typed-terminal-last",
            operation_request(LeaseId(15), case, PayloadKind::Valid),
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
    case: &OperationCase,
) -> Result<(), ProtocolError> {
    let mut creds = launch.credentials.clone();
    creds.secret = secrecy::SecretString::from("wrong");
    expect_protocol_err(
        client
            .dispatch(
                &creds,
                "typed-wrong-bearer",
                operation_request(LeaseId(16), case, PayloadKind::Valid),
            )
            .await,
        |e| matches!(e, ProtocolError::UnauthorizedBearer),
    )
}

async fn wrong_worker_id_rejected(
    client: &HttpClient,
    launch: &crate::WorkerLaunch,
    case: &OperationCase,
) -> Result<(), ProtocolError> {
    let mut creds = launch.credentials.clone();
    creds.worker_id = voom_core::WorkerId(999);
    expect_protocol_err(
        client
            .dispatch(
                &creds,
                "typed-wrong-worker-id",
                operation_request(LeaseId(17), case, PayloadKind::Valid),
            )
            .await,
        |e| matches!(e, ProtocolError::UnknownWorkerId { .. }),
    )
}

async fn wrong_worker_epoch_rejected(
    client: &HttpClient,
    launch: &crate::WorkerLaunch,
    case: &OperationCase,
) -> Result<(), ProtocolError> {
    let mut creds = launch.credentials.clone();
    creds.worker_epoch += 1;
    expect_protocol_err(
        client
            .dispatch(
                &creds,
                "typed-wrong-worker-epoch",
                operation_request(LeaseId(18), case, PayloadKind::Valid),
            )
            .await,
        |e| matches!(e, ProtocolError::StaleWorkerEpoch { .. }),
    )
}

async fn idempotency_same_logical_request_replay_returns_cached_response(
    client: &HttpClient,
    launch: &crate::WorkerLaunch,
    case: &OperationCase,
) -> Result<(), ProtocolError> {
    let req = operation_request(LeaseId(19), case, PayloadKind::Valid);
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
    case: &OperationCase,
) -> Result<(), ProtocolError> {
    let first_req = operation_request(LeaseId(20), case, PayloadKind::Valid);
    let mut second_req = first_req.clone();
    second_req.payload = conflict_payload(&second_req.payload);
    let mut first = client
        .dispatch(&launch.credentials, "typed-replay-conflict", first_req)
        .await?;
    drain_stream(&mut first.frames).await?;
    expect_protocol_err(
        client
            .dispatch(&launch.credentials, "typed-replay-conflict", second_req)
            .await,
        |e| matches!(e, ProtocolError::DuplicateIdempotencyKey { .. }),
    )
}

async fn record<F>(result: &mut crate::SuiteResult, name: String, fut: F)
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

fn conflict_payload(payload: &serde_json::Value) -> serde_json::Value {
    let mut payload = payload.clone();
    if let Some(object) = payload.as_object_mut() {
        object.insert("__voom_conflict".to_owned(), serde_json::json!(true));
    }
    payload
}

fn operation_name(operation: OperationKind) -> String {
    serde_json::to_value(operation)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_else(|| format!("{operation:?}"))
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
            NdjsonOutcome::Terminated(_) => return Ok(()),
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
