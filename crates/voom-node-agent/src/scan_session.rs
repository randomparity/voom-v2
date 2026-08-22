//! Agent-side scan-session pump (ADR 0077, design C4).
//!
//! One `scan_library` lease drives one durable scan session: start it on the
//! control plane, stream candidates from the scan worker's own child, run each
//! candidate through a bounded hash→sidecar-hash→probe pipeline against the
//! cross-worker [`ChildEndpointRegistry`], and submit ordered idempotent
//! observation batches. Sessions never resume across agent restarts — a
//! re-delivered ticket whose session is no longer startable by this
//! incarnation fails closed rather than replaying accepted locators into
//! duplicate-locator conflicts.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value as JsonValue, json};
use tokio::time::Instant;
use voom_core::{
    ErrorCode, FailureClass, LeaseId, NodeIncarnationId, OperationKind, ScanObservationEvidence,
    ScanSessionId, ScanSidecarEvidence,
};
use voom_worker_protocol::{
    ClientHandle, HashFileRequest, HashFileResult, NdjsonOutcome, ObservedFileFacts,
    OperationRequest, ProbeFileRequest, ProbeFileResult, ProgressFrame, ScanCandidate,
    ScanCandidateFile, ScanLibraryResult, WorkerCredentials,
};

use crate::client::{ControlPlaneClient, LeaseDispatch, RetryRequest};
use crate::runtime::{ChildEndpoint, ChildEndpointRegistry, LeaseOutcome};
use crate::scan_client::{
    ScanBatchRequest, ScanCompleteRequest, ScanFailRequest, ScanObservationWire, ScanStartRequest,
};

/// Candidate pipelines in flight at once.
const MAX_IN_FLIGHT: usize = 4;
/// Observations buffered before a batch flush is forced.
const FLUSH_OBSERVATION_COUNT: usize = 1000;
/// Estimated serialized-batch bytes that force a flush before the API's
/// ~1 MiB request-body cap can reject the batch.
const FLUSH_ESTIMATED_BYTES: usize = 512 * 1024;
/// Sidecar digest entries tolerated per primary; overflow degrades the whole
/// observation to evidence-less.
const MAX_SIDECAR_ENTRIES: usize = 64;
/// Serialized-evidence bytes tolerated per primary.
const MAX_EVIDENCE_BYTES: usize = 64 * 1024;
/// How long the pump waits for a hash/probe child to (re)appear before
/// failing the session. Child restarts take ~250 ms.
const ENDPOINT_WAIT: Duration = Duration::from_secs(2);

/// The runtime facts the pump needs beyond its transports.
#[derive(Debug, Clone)]
pub(crate) struct ScanPumpContext {
    pub node_id: u64,
    pub incarnation_id: NodeIncarnationId,
    pub lease_id: LeaseId,
    pub lease_ttl_ms: u32,
    pub progress_timeout: Duration,
}

/// What one candidate contributed to the run counters.
#[derive(Default)]
struct CandidateContribution {
    observation: Option<ScanObservationWire>,
    failed_content: bool,
    degraded_evidence: bool,
}

/// A fatal pump condition: the durable session fails best-effort and the
/// lease settles `Fail`.
struct PumpFailure {
    class: FailureClass,
    reason: String,
    session: Option<ScanSessionId>,
}

impl PumpFailure {
    fn new(class: FailureClass, reason: impl Into<String>) -> Self {
        Self {
            class,
            reason: reason.into(),
            session: None,
        }
    }

    fn for_session(session: ScanSessionId, class: FailureClass, reason: impl Into<String>) -> Self {
        Self {
            class,
            reason: reason.into(),
            session: Some(session),
        }
    }
}

/// Decode the scan-run facts this pump needs from the ticket payload. The
/// agent deliberately does not depend on the control-plane payload type; the
/// fields are read strictly so a malformed ticket fails loudly.
fn parse_scan_ticket(payload: &JsonValue) -> Result<(ScanSessionId, String, Vec<String>), String> {
    let rendered = payload
        .get("rendered_payload")
        .ok_or("ticket payload has no rendered_payload")?;
    let session = rendered
        .get("scan_session_id")
        .and_then(JsonValue::as_str)
        .ok_or("ticket payload has no scan_session_id")?;
    let session = session
        .parse::<u64>()
        .map_err(|error| format!("scan_session_id {session:?} is not a session id: {error}"))?;
    let provider_locator = rendered
        .get("provider_locator")
        .and_then(JsonValue::as_str)
        .ok_or("scan ticket has no provider_locator")?
        .to_owned();
    let allowlist = rendered
        .get("extension_allowlist")
        .and_then(JsonValue::as_array)
        .map(|entries| {
            entries
                .iter()
                .map(|entry| {
                    entry
                        .as_str()
                        .map(str::to_owned)
                        .ok_or_else(|| "extension_allowlist entry is not a string".to_owned())
                })
                .collect::<Result<Vec<_>, _>>()
        })
        .transpose()?
        .unwrap_or_default();
    Ok((ScanSessionId(session), provider_locator, allowlist))
}

fn scan_idempotency_key(context: &ScanPumpContext, session: ScanSessionId, suffix: &str) -> String {
    scan_idempotency_key_for_test(context, session, suffix)
}

fn scan_idempotency_key_for_test(
    context: &ScanPumpContext,
    session: ScanSessionId,
    suffix: &str,
) -> String {
    format!("{}-scan-{}-{suffix}", context.incarnation_id, session.0)
}

/// Entry point wired into `run_lease`: drive one scan session to its terminal
/// control-plane state, then settle the lease.
pub(crate) async fn pump_scan_session(
    dispatch: &LeaseDispatch,
    scan_child: Arc<dyn ClientHandle>,
    scan_credentials: &WorkerCredentials,
    registry: &ChildEndpointRegistry,
    scan_client: &ControlPlaneClient,
    context: &ScanPumpContext,
) -> LeaseOutcome {
    match run_pump(
        dispatch,
        scan_child,
        scan_credentials,
        registry,
        scan_client,
        context,
    )
    .await
    {
        Ok(outcome) => outcome,
        Err(failure) => {
            fail_session_best_effort(scan_client, context, failure.session, &failure.reason);
            LeaseOutcome::Failure(failure.class, failure.reason, json!({}))
        }
    }
}

/// Start the durable session. A conflict means this ticket was re-delivered
/// after an agent restart and the session is no longer ours to drive — fail
/// closed rather than resuming mid-tree.
async fn start_scan_run(
    scan_client: &ControlPlaneClient,
    context: &ScanPumpContext,
    session: ScanSessionId,
) -> Result<(), PumpFailure> {
    let start_request = RetryRequest::new(
        scan_idempotency_key(context, session, "start"),
        &ScanStartRequest {
            incarnation_id: context.incarnation_id,
        },
    )
    .map_err(|error| {
        PumpFailure::for_session(
            session,
            FailureClass::MalformedWorkerResult,
            error.to_string(),
        )
    })?;
    match scan_client
        .start_scan_session(context.node_id, session, &start_request)
        .await
    {
        Ok(outcome) if outcome.status == voom_core::ScanSessionStatus::Running => Ok(()),
        Ok(outcome) => Err(PumpFailure::for_session(
            session,
            FailureClass::ExternalSystemUnavailable,
            format!(
                "scan session {} started as {:?}, expected running",
                session.0, outcome.status
            ),
        )),
        Err(voom_core::VoomError::Conflict(message) | voom_core::VoomError::NotFound(message)) => {
            Err(PumpFailure::for_session(
                session,
                FailureClass::ExternalSystemUnavailable,
                format!(
                    "scan session {} could not be started by this agent incarnation \
                     (agent restarted mid-session; re-request the scan): {message}",
                    session.0
                ),
            ))
        }
        Err(error) => Err(PumpFailure::for_session(
            session,
            FailureClass::ExternalSystemUnavailable,
            format!("start scan session {}: {error}", session.0),
        )),
    }
}

async fn run_pump(
    dispatch: &LeaseDispatch,
    scan_child: Arc<dyn ClientHandle>,
    scan_credentials: &WorkerCredentials,
    registry: &ChildEndpointRegistry,
    scan_client: &ControlPlaneClient,
    context: &ScanPumpContext,
) -> Result<LeaseOutcome, PumpFailure> {
    let (session, provider_locator, extension_allowlist) =
        parse_scan_ticket(&dispatch.dispatch_payload)
            .map_err(|message| PumpFailure::new(FailureClass::MalformedWorkerResult, message))?;

    start_scan_run(scan_client, context, session).await?;

    // Enumerate the root through the scan worker's own child.
    let enumeration = dispatch_scan_library(
        scan_child.as_ref(),
        scan_credentials,
        context,
        &provider_locator,
        &extension_allowlist,
    )
    .await
    .map_err(|mut failure| {
        failure.session = Some(session);
        failure
    })?;

    let mut batches = BatchSubmitter::new(scan_client, context, session);
    let mut pipelines = tokio::task::JoinSet::new();
    let mut observed_count = 0_u64;
    let mut failed_content_count = 0_u64;
    let mut degraded_evidence_count = 0_u64;
    for frame in enumeration.candidate_frames {
        for candidate in frame {
            while pipelines.len() >= MAX_IN_FLIGHT {
                record_contribution(
                    pipelines
                        .join_next()
                        .await
                        .unwrap_or_else(|| unreachable!("len() >= 1 guarantees a pending task")),
                    &mut batches,
                    &mut observed_count,
                    &mut failed_content_count,
                    &mut degraded_evidence_count,
                )
                .await?;
            }
            pipelines.spawn(process_candidate(
                registry.clone(),
                PathBuf::from(&provider_locator),
                candidate,
                context.clone(),
            ));
        }
    }
    while let Some(joined) = pipelines.join_next().await {
        record_contribution(
            joined,
            &mut batches,
            &mut observed_count,
            &mut failed_content_count,
            &mut degraded_evidence_count,
        )
        .await?;
    }
    batches.flush_if_nonempty().await?;

    // Complete: publication happens control-plane side inside the completion
    // transaction; the summary carries only counts known agent-side.
    let complete_request = RetryRequest::new(
        scan_idempotency_key(context, session, "complete"),
        &ScanCompleteRequest {
            incarnation_id: context.incarnation_id,
            last_sequence: batches.last_sequence(),
            observation_count: batches.cumulative(),
        },
    )
    .map_err(|error| {
        PumpFailure::for_session(
            session,
            FailureClass::MalformedWorkerResult,
            error.to_string(),
        )
    })?;
    scan_client
        .complete_scan_session(context.node_id, session, &complete_request)
        .await
        .map_err(|error| {
            PumpFailure::for_session(
                session,
                FailureClass::ExternalSystemUnavailable,
                format!("complete scan session {}: {error}", session.0),
            )
        })?;

    Ok(LeaseOutcome::Complete(json!({
        "scan_session_id": session.0.to_string(),
        "observed_count": observed_count,
        "failed_content_count": failed_content_count,
        "degraded_evidence_count": degraded_evidence_count,
        "skipped_count": enumeration.result.skipped_count,
    })))
}

/// Join one finished candidate pipeline into the counters and batch buffer.
async fn record_contribution(
    joined: Result<Result<CandidateContribution, PumpFailure>, tokio::task::JoinError>,
    batches: &mut BatchSubmitter<'_>,
    observed_count: &mut u64,
    failed_content_count: &mut u64,
    degraded_evidence_count: &mut u64,
) -> Result<(), PumpFailure> {
    let contribution = joined.map_err(|error| {
        PumpFailure::new(
            FailureClass::WorkerCrash,
            format!("candidate pipeline crashed: {error}"),
        )
    })??;
    if contribution.failed_content {
        *failed_content_count += 1;
    }
    if contribution.degraded_evidence {
        *degraded_evidence_count += 1;
    }
    if let Some(observation) = contribution.observation {
        *observed_count += 1;
        batches.push(observation);
        // Flush at the count or byte budget so a burst of evidence-dense
        // candidates never exceeds the API's request-body cap.
        while batches.should_flush() {
            batches.flush().await?;
        }
    }
    Ok(())
}

/// Best-effort terminal failure of the durable session. A rejected or lost
/// fail call must not mask the original failure: the lease still settles Fail.
fn fail_session_best_effort(
    scan_client: &ControlPlaneClient,
    context: &ScanPumpContext,
    session: Option<ScanSessionId>,
    reason: &str,
) {
    let Some(session) = session else {
        return;
    };
    let Ok(request) = RetryRequest::new(
        scan_idempotency_key(context, session, "fail"),
        &ScanFailRequest {
            incarnation_id: context.incarnation_id,
            reason: truncate_reason(reason),
        },
    ) else {
        return;
    };
    let client = scan_client.clone();
    let node_id = context.node_id;
    drop(tokio::spawn(async move {
        let _ = client.fail_scan_session(node_id, session, &request).await;
    }));
}

/// Terminal reasons are bounded at 1024 bytes; keep the head, which carries
/// the classification.
fn truncate_reason(reason: &str) -> String {
    if reason.len() <= 1024 {
        return reason.to_owned();
    }
    let mut cut = 1024;
    while !reason.is_char_boundary(cut) {
        cut -= 1;
    }
    reason[..cut].to_owned()
}

/// One enumeration run: every candidate frame in arrival order plus the
/// worker's final tally.
struct Enumeration {
    candidate_frames: Vec<Vec<ScanCandidate>>,
    result: ScanLibraryResult,
}

async fn dispatch_scan_library(
    scan_child: &dyn ClientHandle,
    credentials: &WorkerCredentials,
    context: &ScanPumpContext,
    provider_locator: &str,
    extension_allowlist: &[String],
) -> Result<Enumeration, PumpFailure> {
    let request = OperationRequest {
        operation: OperationKind::ScanLibrary,
        lease_id: context.lease_id,
        payload: json!({
            "provider_locator": provider_locator,
            "extension_allowlist": extension_allowlist,
        }),
        heartbeat_deadline_ms: context.lease_ttl_ms,
        progress_idle_deadline_ms: duration_millis_u32(context.progress_timeout),
    };
    let key = format!(
        "{}-{}-scan-library",
        context.incarnation_id, context.lease_id.0
    );
    let mut stream = open_dispatch_stream(
        scan_child,
        credentials,
        &key,
        request,
        context.progress_timeout,
    )
    .await
    .map_err(|message| PumpFailure::new(FailureClass::MalformedWorkerResult, message))?;
    let mut enumeration = Enumeration {
        candidate_frames: Vec::new(),
        result: ScanLibraryResult {
            discovered_count: 0,
            skipped_count: 0,
        },
    };
    loop {
        let frame = tokio::time::timeout(context.progress_timeout, stream.next_frame()).await;
        match frame {
            Err(_) => {
                return Err(PumpFailure::new(
                    FailureClass::WorkerTimeout,
                    format!(
                        "scan worker made no progress for {:?}",
                        context.progress_timeout
                    ),
                ));
            }
            Ok(Err(error)) => {
                return Err(PumpFailure::new(
                    FailureClass::MalformedWorkerResult,
                    format!("scan worker stream failed: {error}"),
                ));
            }
            Ok(Ok(NdjsonOutcome::StreamEnd)) => {
                return Err(PumpFailure::new(
                    FailureClass::MalformedWorkerResult,
                    "scan worker ended its stream before a terminal frame",
                ));
            }
            Ok(Ok(NdjsonOutcome::Frame(ProgressFrame::Progress { payload, .. }))) => {
                let candidates =
                    voom_worker_protocol::decode_candidate_progress(&payload.unwrap_or(json!({})))
                        .map_err(|error| {
                            PumpFailure::new(FailureClass::MalformedWorkerResult, error.to_string())
                        })?;
                enumeration.candidate_frames.push(candidates);
            }
            Ok(Ok(NdjsonOutcome::Frame(_))) => {
                return Err(PumpFailure::new(
                    FailureClass::MalformedWorkerResult,
                    "scan worker emitted a terminal frame as non-terminal progress",
                ));
            }
            Ok(Ok(NdjsonOutcome::Terminated(ProgressFrame::Progress { .. }))) => {
                return Err(PumpFailure::new(
                    FailureClass::MalformedWorkerResult,
                    "scan worker terminated with a progress frame",
                ));
            }
            Ok(Ok(NdjsonOutcome::Terminated(ProgressFrame::Result { payload, .. }))) => {
                enumeration.result = serde_json::from_value(payload).map_err(|error| {
                    PumpFailure::new(
                        FailureClass::MalformedWorkerResult,
                        format!("decode scan_library result: {error}"),
                    )
                })?;
                return Ok(enumeration);
            }
            Ok(Ok(NdjsonOutcome::Terminated(ProgressFrame::Error { class, message, .. }))) => {
                // A fatal worker crash fails the whole session; the frame's own
                // classification rides through untouched.
                return Err(PumpFailure::new(
                    class,
                    format!("scan worker failed: {message}"),
                ));
            }
        }
    }
}

/// Terminal outcome of one single-file child dispatch.
enum WorkerTerminal {
    Result(JsonValue),
    Error(FailureClass, ErrorCode, String),
}

async fn dispatch_until_terminal(
    child: &dyn ClientHandle,
    credentials: &WorkerCredentials,
    request: OperationRequest,
    idempotency_key: &str,
    progress_timeout: Duration,
) -> Result<WorkerTerminal, String> {
    let mut stream = open_dispatch_stream(
        child,
        credentials,
        idempotency_key,
        request,
        progress_timeout,
    )
    .await?;
    loop {
        let frame = tokio::time::timeout(progress_timeout, stream.next_frame()).await;
        match frame {
            Err(_) => return Err(format!("worker made no progress for {progress_timeout:?}")),
            Ok(Err(error)) => return Err(format!("worker stream failed: {error}")),
            Ok(Ok(NdjsonOutcome::StreamEnd)) => {
                return Err("worker ended its stream before a terminal frame".to_owned());
            }
            // Single-file operations carry no interesting progress frames.
            Ok(Ok(NdjsonOutcome::Frame(ProgressFrame::Progress { .. }))) => {}
            Ok(Ok(NdjsonOutcome::Frame(_))) => {
                return Err("worker emitted a terminal frame as non-terminal progress".to_owned());
            }
            Ok(Ok(NdjsonOutcome::Terminated(ProgressFrame::Progress { .. }))) => {
                return Err("worker terminated with a progress frame".to_owned());
            }
            Ok(Ok(NdjsonOutcome::Terminated(ProgressFrame::Result { payload, .. }))) => {
                return Ok(WorkerTerminal::Result(payload));
            }
            Ok(Ok(NdjsonOutcome::Terminated(ProgressFrame::Error {
                class,
                code,
                message,
                ..
            }))) => return Ok(WorkerTerminal::Error(class, code, message)),
        }
    }
}

/// Open one child dispatch stream, bounded by the progress timeout.
async fn open_dispatch_stream(
    child: &dyn ClientHandle,
    credentials: &WorkerCredentials,
    idempotency_key: &str,
    request: OperationRequest,
    progress_timeout: Duration,
) -> Result<voom_worker_protocol::NdjsonStream, String> {
    let dispatch = tokio::time::timeout(
        progress_timeout,
        child.dispatch(credentials, idempotency_key, request),
    )
    .await;
    match dispatch {
        Ok(Ok(stream)) => Ok(stream.frames),
        Ok(Err(error)) => Err(format!("worker dispatch failed: {error}")),
        Err(_) => Err(format!(
            "worker dispatch did not start within {progress_timeout:?}"
        )),
    }
}

/// Why one file produced no usable identity facts.
enum CandidateGap {
    /// `ArtifactUnavailable` + `NOT_FOUND`: absence is real.
    Vanished,
    /// Any other failure: existence recorded, identity not published.
    UnusableIdentity,
}

/// Classify a worker error frame by its `(class, code)` pair, never by text.
fn classify_single_file_error(
    class: FailureClass,
    code: ErrorCode,
    message: &str,
) -> Result<CandidateGap, PumpFailure> {
    match (class, code) {
        (FailureClass::ArtifactUnavailable, ErrorCode::NotFound) => Ok(CandidateGap::Vanished),
        (
            FailureClass::ArtifactChecksumMismatch
            | FailureClass::ArtifactUnavailable
            | FailureClass::MalformedMedia,
            _,
        ) => Ok(CandidateGap::UnusableIdentity),
        (class, code) => Err(PumpFailure::new(
            class,
            format!("single-file worker failed ({code:?}): {message}"),
        )),
    }
}

/// Hash one file through the registry-resolved hash worker. On success the
/// hash worker has already proven pre/post stat agreement, so the returned
/// facts are internally stable.
async fn hash_one(
    registry: &ChildEndpointRegistry,
    root_provider_locator: &str,
    relative_locator: &str,
    context: &ScanPumpContext,
    dispatch_tag: &str,
) -> Result<Result<HashFileResult, CandidateGap>, PumpFailure> {
    let Some(endpoint) = wait_for_endpoint(registry, OperationKind::HashFile).await else {
        return Err(PumpFailure::new(
            FailureClass::NoEligibleWorker,
            format!("no live hash_file worker endpoint for {dispatch_tag}"),
        ));
    };
    let request = OperationRequest {
        operation: OperationKind::HashFile,
        lease_id: context.lease_id,
        payload: json!(HashFileRequest {
            provider_locator: root_provider_locator.to_owned(),
            provider_relative_locator: relative_locator.to_owned(),
        }),
        heartbeat_deadline_ms: context.lease_ttl_ms,
        progress_idle_deadline_ms: duration_millis_u32(context.progress_timeout),
    };
    let key = format!("{dispatch_tag}-hash");
    match dispatch_until_terminal(
        endpoint.client.as_ref(),
        &endpoint.credentials,
        request,
        &key,
        context.progress_timeout,
    )
    .await
    {
        Ok(WorkerTerminal::Result(payload)) => {
            let result: HashFileResult = serde_json::from_value(payload).map_err(|error| {
                PumpFailure::new(
                    FailureClass::MalformedWorkerResult,
                    format!("decode hash_file result: {error}"),
                )
            })?;
            Ok(Ok(result))
        }
        Ok(WorkerTerminal::Error(class, code, message)) => {
            Ok(Err(classify_single_file_error(class, code, &message)?))
        }
        Err(message) => Err(PumpFailure::new(FailureClass::WorkerCrash, message)),
    }
}

/// Probe outcome for the primary file.
enum ProbeOutcome {
    /// Probe agreed with the hash facts; the snapshot is identity evidence.
    Snapshot(JsonValue),
    /// Real absence: the primary is gone.
    Vanished,
    /// Probe ran but the facts disagree: identity unusable.
    Disagree,
}

/// Probe the primary against the agreed hash facts. The ffprobe worker
/// verifies size/hash at both probe points itself; the pump re-checks the
/// full fact set including modification time.
async fn probe_primary(
    registry: &ChildEndpointRegistry,
    root: &std::path::Path,
    primary: &ScanCandidateFile,
    hash: &HashFileResult,
    context: &ScanPumpContext,
    dispatch_tag: &str,
) -> Result<ProbeOutcome, PumpFailure> {
    let Some(endpoint) = wait_for_endpoint(registry, OperationKind::ProbeFile).await else {
        return Err(PumpFailure::new(
            FailureClass::NoEligibleWorker,
            format!("no live probe_file worker endpoint for {dispatch_tag}"),
        ));
    };
    // Absolute canonical-root join, never option-like: argv arrays cannot be
    // injected, but the shape is asserted anyway.
    let path = root.join(primary.provider_relative_locator.as_str());
    let path_text = path.to_string_lossy().to_string();
    assert!(
        path.is_absolute() && !path_text.starts_with('-'),
        "probe paths must be absolute canonical-root joins, got {path_text}"
    );
    let request = OperationRequest {
        operation: OperationKind::ProbeFile,
        lease_id: context.lease_id,
        payload: json!(ProbeFileRequest {
            path: path.to_string_lossy().to_string(),
            expected: voom_worker_protocol::ExpectedFileFacts {
                size_bytes: hash.size_bytes,
                content_hash: hash.content_hash.clone(),
                modified_at: Some(hash.modified_at.clone()),
                local_file_key: None,
            },
        }),
        heartbeat_deadline_ms: context.lease_ttl_ms,
        progress_idle_deadline_ms: duration_millis_u32(context.progress_timeout),
    };
    let key = format!("{dispatch_tag}-probe");
    match dispatch_until_terminal(
        endpoint.client.as_ref(),
        &endpoint.credentials,
        request,
        &key,
        context.progress_timeout,
    )
    .await
    {
        Ok(WorkerTerminal::Result(payload)) => {
            let result: ProbeFileResult = serde_json::from_value(payload).map_err(|error| {
                PumpFailure::new(
                    FailureClass::MalformedWorkerResult,
                    format!("decode probe_file result: {error}"),
                )
            })?;
            if agrees(&result.pre_probe, hash) && agrees(&result.post_probe, hash) {
                Ok(ProbeOutcome::Snapshot(result.snapshot))
            } else {
                Ok(ProbeOutcome::Disagree)
            }
        }
        Ok(WorkerTerminal::Error(class, code, message)) => {
            Ok(match classify_single_file_error(class, code, &message)? {
                CandidateGap::Vanished => ProbeOutcome::Vanished,
                CandidateGap::UnusableIdentity => ProbeOutcome::Disagree,
            })
        }
        Err(message) => Err(PumpFailure::new(FailureClass::WorkerCrash, message)),
    }
}

/// The exact agreement predicate: probe-observed facts equal the agreed hash
/// facts, including modification time.
fn agrees(observed: &ObservedFileFacts, hash: &HashFileResult) -> bool {
    observed.size_bytes == hash.size_bytes
        && observed.content_hash == hash.content_hash
        && observed.modified_at.as_deref() == Some(hash.modified_at.as_str())
}

/// Run one candidate through hash → sidecar hashes → probe and produce at
/// most one observation. Any `NOT_FOUND` inside the candidate records real
/// absence (no observation); any other identity failure records existence
/// without evidence.
async fn process_candidate(
    registry: ChildEndpointRegistry,
    root: PathBuf,
    candidate: ScanCandidate,
    context: ScanPumpContext,
) -> Result<CandidateContribution, PumpFailure> {
    let mut contribution = CandidateContribution {
        observation: None,
        failed_content: false,
        degraded_evidence: false,
    };
    let primary = &candidate.primary;
    let locator = primary.provider_relative_locator.as_str();
    let root_str = root.to_string_lossy().to_string();
    let tag = format!("{}-{locator}", context.incarnation_id);

    let hash = match hash_one(&registry, &root_str, locator, &context, &tag).await {
        Ok(Ok(hash)) => hash,
        Ok(Err(CandidateGap::Vanished)) => return Ok(contribution),
        Ok(Err(CandidateGap::UnusableIdentity)) => {
            return Ok(evidence_less(&mut contribution, primary));
        }
        Err(failure) => return Err(failure),
    };

    let mut sidecars = Vec::with_capacity(candidate.sidecars.len());
    for sidecar in &candidate.sidecars {
        let sidecar_tag = format!("{tag}-{}", sidecar.provider_relative_locator);
        match hash_one(
            &registry,
            &root_str,
            sidecar.provider_relative_locator.as_str(),
            &context,
            &sidecar_tag,
        )
        .await
        {
            Ok(Ok(sidecar_hash)) => {
                let Some(hex) = sidecar_hash.content_hash.strip_prefix("blake3:") else {
                    return Err(PumpFailure::new(
                        FailureClass::MalformedWorkerResult,
                        format!(
                            "sidecar {} produced a non-blake3 digest",
                            sidecar.provider_relative_locator.as_str()
                        ),
                    ));
                };
                sidecars.push(ScanSidecarEvidence {
                    provider_relative_locator: sidecar
                        .provider_relative_locator
                        .as_str()
                        .to_owned(),
                    role: sidecar.kind.clone().unwrap_or_else(|| "unknown".to_owned()),
                    sha256_hex: hex.to_owned(),
                    size_bytes: sidecar_hash.size_bytes,
                });
            }
            Ok(Err(CandidateGap::Vanished)) => return Ok(contribution),
            Ok(Err(CandidateGap::UnusableIdentity)) => {
                return Ok(evidence_less(&mut contribution, primary));
            }
            Err(failure) => return Err(failure),
        }
    }

    let snapshot = match probe_primary(&registry, &root, primary, &hash, &context, &tag).await {
        Ok(ProbeOutcome::Snapshot(snapshot)) => snapshot,
        Ok(ProbeOutcome::Vanished) => return Ok(contribution),
        Ok(ProbeOutcome::Disagree) => {
            return Ok(evidence_less(&mut contribution, primary));
        }
        Err(failure) => return Err(failure),
    };

    let evidence = ScanObservationEvidence {
        content_hash: hash.content_hash,
        size_bytes: hash.size_bytes,
        modified_at: hash.modified_at.clone(),
        file_key: hash.file_key,
        sidecars,
        probe_snapshot: snapshot,
    };
    let over_bound = evidence.sidecars.len() > MAX_SIDECAR_ENTRIES
        || serde_json::to_vec(&evidence).is_ok_and(|bytes| bytes.len() > MAX_EVIDENCE_BYTES);
    let evidence = if over_bound {
        contribution.degraded_evidence = true;
        None
    } else {
        Some(evidence)
    };
    contribution.observation = Some(ScanObservationWire {
        provider_relative_locator: locator.to_owned(),
        provider_object_identity: primary.provider_object_identity.clone(),
        size_bytes: primary.size_bytes,
        modified_at: primary.modified_at.clone(),
        stability_started_at: hash.stability_started_at,
        stability_confirmed_at: hash.stability_confirmed_at,
        evidence,
    });
    Ok(contribution)
}

/// Record an identity failure: the candidate existed at discovery time, so an
/// evidence-less observation protects its location from retirement.
fn evidence_less(
    contribution: &mut CandidateContribution,
    primary: &ScanCandidateFile,
) -> CandidateContribution {
    contribution.failed_content = true;
    contribution.observation = Some(ScanObservationWire {
        provider_relative_locator: primary.provider_relative_locator.as_str().to_owned(),
        provider_object_identity: primary.provider_object_identity.clone(),
        size_bytes: primary.size_bytes,
        modified_at: primary.modified_at.clone(),
        stability_started_at: primary.modified_at.clone(),
        stability_confirmed_at: primary.modified_at.clone(),
        evidence: None,
    });
    std::mem::take(contribution)
}

/// Wait a bounded time for a live child whose operations include `operation`.
async fn wait_for_endpoint(
    registry: &ChildEndpointRegistry,
    operation: OperationKind,
) -> Option<ChildEndpoint> {
    let deadline = Instant::now() + ENDPOINT_WAIT;
    loop {
        if let Some(endpoint) = registry.resolve(operation) {
            return Some(endpoint);
        }
        if Instant::now() >= deadline {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// Ordered idempotent batch submission with a byte-aware flush rule.
struct BatchSubmitter<'a> {
    client: &'a ControlPlaneClient,
    context: &'a ScanPumpContext,
    session: ScanSessionId,
    next_sequence: u64,
    buffer: Vec<ScanObservationWire>,
    estimated_bytes: usize,
    cumulative: u64,
}

impl<'a> BatchSubmitter<'a> {
    fn new(
        client: &'a ControlPlaneClient,
        context: &'a ScanPumpContext,
        session: ScanSessionId,
    ) -> Self {
        Self {
            client,
            context,
            session,
            next_sequence: 0,
            buffer: Vec::new(),
            estimated_bytes: 0,
            cumulative: 0,
        }
    }

    fn push(&mut self, observation: ScanObservationWire) {
        self.estimated_bytes +=
            serde_json::to_vec(&observation).map_or(MAX_EVIDENCE_BYTES + 1024, |bytes| bytes.len());
        self.buffer.push(observation);
    }

    fn should_flush(&self) -> bool {
        self.buffer.len() >= FLUSH_OBSERVATION_COUNT
            || self.estimated_bytes >= FLUSH_ESTIMATED_BYTES
    }

    async fn flush_if_nonempty(&mut self) -> Result<(), PumpFailure> {
        if !self.buffer.is_empty() {
            self.flush().await?;
        }
        Ok(())
    }

    async fn flush(&mut self) -> Result<(), PumpFailure> {
        let sequence = self.next_sequence;
        let observations = std::mem::take(&mut self.buffer);
        self.estimated_bytes = 0;
        let request = RetryRequest::new(
            scan_idempotency_key(self.context, self.session, sequence.to_string().as_str()),
            &ScanBatchRequest {
                incarnation_id: self.context.incarnation_id,
                observations,
            },
        )
        .map_err(|error| {
            PumpFailure::for_session(
                self.session,
                FailureClass::MalformedWorkerResult,
                error.to_string(),
            )
        })?;
        let outcome = self
            .client
            .submit_scan_batch(self.context.node_id, self.session, sequence, &request)
            .await
            .map_err(|error| {
                PumpFailure::for_session(
                    self.session,
                    FailureClass::ExternalSystemUnavailable,
                    format!(
                        "submit scan batch {sequence} for session {}: {error}",
                        self.session.0
                    ),
                )
            })?;
        self.cumulative = outcome.cumulative_observation_count;
        self.next_sequence += 1;
        Ok(())
    }

    fn last_sequence(&self) -> Option<u64> {
        if self.next_sequence == 0 {
            None
        } else {
            Some(self.next_sequence - 1)
        }
    }

    fn cumulative(&self) -> u64 {
        self.cumulative
    }
}

fn duration_millis_u32(duration: Duration) -> u32 {
    u32::try_from(duration.as_millis()).unwrap_or(u32::MAX)
}

#[cfg(test)]
#[path = "scan_session_test.rs"]
mod tests;
