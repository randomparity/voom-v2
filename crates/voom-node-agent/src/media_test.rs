//! Media-executor tests against a scripted worker child (ADR 0075 design C3
//! test strategy). Hermetic: every fixture lives under a unique temp dir, the
//! child is an in-process [`ClientHandle`] over a duplex stream, and no
//! control plane is contacted because the executor settles outcomes locally.

use std::collections::HashMap;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use secrecy::SecretString;
use serde_json::{Value as JsonValue, json};
use tempfile::TempDir;
use tokio::sync::Mutex;
use voom_core::ids::ArtifactCommitIntentId;
use voom_core::{
    ArtifactAccessMode, FailureClass, LeaseId, NodeId, NodeIncarnationId, OperationKind, TicketId,
    VoomError, WorkerId,
};
use voom_worker_protocol::{
    ClientHandle, DispatchStream, HandshakeResponse, NdjsonReader, OperationRequest,
    OperationResponse, ProgressFrame, ProtocolError, WorkerCredentials,
};

use crate::client::{
    AcquireOutcome, AcquireRequest, ActivateOutcome, ActivateRequest, ArtifactAccessPlan,
    CommitApplyingOutcome, CommitApplyingRequest, CommitAuthorizeOutcome, CommitAuthorizeRequest,
    CommitCompleteOutcome, CommitCompleteRequest, CommitOpenOutcome, CommitOpenRequest,
    CommitOutcomeRequest, CommitReceiptOutcome, CompleteOutcome, CompleteRequest,
    DeactivateOutcome, DeactivateRequest, FailOutcome, FailRequest, LeaseDispatch,
    LeaseHeartbeatOutcome, LeaseHeartbeatRequest, NodeHeartbeatOutcome, NodeHeartbeatRequest,
    RetryRequest,
};
use crate::config::WorkerConfig;
use crate::runtime::{ChildEndpoint, ChildEndpointRegistry, ControlPlaneApi, CoordinatorContext};

use super::is_media_dispatch_operation;
use super::media_outcome;

/// Shared fixture bytes: what the "source" file on disk contains.
const SOURCE_BYTES: &[u8] = b"voom-media-source-bytes";
/// What the scripted transcode child writes as the staged output.
const OUTPUT_BYTES: &[u8] = b"transcoded-output-bytes";

// ---------------------------------------------------------------- fixtures --

fn hash_of(bytes: &[u8]) -> String {
    format!("blake3:{}", blake3::hash(bytes).to_hex())
}

fn write_source(root: &TempDir) -> PathBuf {
    let directory = root.path().join("media");
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join("source.mkv");
    std::fs::write(&path, SOURCE_BYTES).unwrap();
    path
}

fn ensure_output_parent(root: &TempDir) -> PathBuf {
    let directory = root.path().join("staged");
    std::fs::create_dir_all(&directory).unwrap();
    directory.join("out.mkv")
}

fn storage_roots(root: &TempDir) -> HashMap<u64, PathBuf> {
    HashMap::from([(1_u64, root.path().to_path_buf())])
}

/// A rendered ticket payload: declaration scalars beside the nested envelope.
fn media_payload(envelope: &JsonValue) -> JsonValue {
    json!({"ticket_id": 7, "media_dispatch": envelope})
}

fn transcode_audio_envelope(source_size: u64, source_hash: &str) -> JsonValue {
    json!({
        "operation": "transcode_audio",
        "schema": voom_core::PROTOCOL_VERSION,
        "source": {
            "storage_root_id": 1,
            "provider_relative_locator": "media/source.mkv"
        },
        "expected": {
            "size_bytes": source_size,
            "content_hash": source_hash,
            "modified_at": null,
            "local_file_key": null
        },
        "output_container": "mkv",
        "output": {
            "storage_root_id": 1,
            "provider_relative_locator": "staged/out.mkv",
            "overwrite": false
        },
        "selection": {
            "selected_streams": [
                {"snapshot_stream_id": "s1", "provider_stream_index": 1}
            ]
        },
        "settings": {"target_codec": "opus", "profile": "default"}
    })
}

fn audio_observed(size_bytes: u64, content_hash: &str) -> JsonValue {
    json!({"size_bytes": size_bytes, "content_hash": content_hash})
}

/// The typed result the scripted transcode child emits for [`OUTPUT_BYTES`],
/// reporting `reported_output_size` so the fact-mismatch test can lie by one.
fn transcode_audio_result(reported_output_size: u64) -> JsonValue {
    json!({
        "status": "transcoded",
        "provider": "test-ffmpeg",
        "provider_version": "0",
        "input_pre": audio_observed(SOURCE_BYTES.len() as u64, &hash_of(SOURCE_BYTES)),
        "input_post": audio_observed(SOURCE_BYTES.len() as u64, &hash_of(SOURCE_BYTES)),
        "output": audio_observed(reported_output_size, &hash_of(OUTPUT_BYTES)),
        "output_container": "mkv",
        "selected_snapshot_stream_ids": ["s1"],
        "output_audio_codecs": ["opus"],
        "selected_output_streams": [
            {
                "snapshot_stream_id": "s1",
                "output_provider_stream_index": 0,
                "codec": "opus"
            }
        ]
    })
}

fn result_frame(lease_id: LeaseId, payload: JsonValue) -> ProgressFrame {
    ProgressFrame::Result {
        lease_id,
        seq: 0,
        emitted_at: time::OffsetDateTime::UNIX_EPOCH,
        payload,
    }
}

type Script = Box<dyn Fn(&OperationRequest) -> ProgressFrame + Send + Sync>;

/// One in-process child: records every request, then answers through the
/// caller's script. Scripts may touch the filesystem to emulate real workers.
struct ScriptedMediaWorker {
    requests: Mutex<Vec<(OperationKind, JsonValue)>>,
    script: Script,
}

impl ScriptedMediaWorker {
    fn new(script: Script) -> Arc<Self> {
        Arc::new(Self {
            requests: Mutex::new(Vec::new()),
            script,
        })
    }
}

impl std::fmt::Debug for ScriptedMediaWorker {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("ScriptedMediaWorker").finish()
    }
}

async fn write_frame(writer: &mut tokio::io::DuplexStream, frame: &ProgressFrame) {
    let mut bytes = serde_json::to_vec(frame).unwrap();
    bytes.push(b'\n');
    tokio::io::AsyncWriteExt::write_all(writer, &bytes)
        .await
        .unwrap();
    tokio::io::AsyncWriteExt::flush(writer).await.unwrap();
}

fn stream_for(request: &OperationRequest, reader: tokio::io::DuplexStream) -> DispatchStream {
    let reader: Pin<Box<dyn tokio::io::AsyncRead + Send + Unpin>> = Box::pin(reader);
    DispatchStream {
        response: OperationResponse {
            lease_id: request.lease_id,
            accepted_at: time::OffsetDateTime::UNIX_EPOCH,
        },
        frames: NdjsonReader::new(reader, request.lease_id),
    }
}

#[async_trait::async_trait]
impl ClientHandle for ScriptedMediaWorker {
    async fn handshake(&self, offered: u32) -> Result<HandshakeResponse, ProtocolError> {
        Ok(HandshakeResponse { agreed: offered })
    }

    async fn identity(
        &self,
        credentials: &WorkerCredentials,
    ) -> Result<voom_worker_protocol::WorkerIdentityResponse, ProtocolError> {
        Ok(voom_worker_protocol::WorkerIdentityResponse {
            worker_id: credentials.worker_id,
            worker_epoch: credentials.worker_epoch,
            protocol_version: voom_core::PROTOCOL_VERSION,
            proof: String::new(),
        })
    }

    async fn dispatch(
        &self,
        _credentials: &WorkerCredentials,
        _idempotency_key: &str,
        request: OperationRequest,
    ) -> Result<DispatchStream, ProtocolError> {
        self.requests
            .lock()
            .await
            .push((request.operation, request.payload.clone()));
        let frame = (self.script)(&request);
        let (mut writer, reader) = tokio::io::duplex(64 * 1024);
        tokio::spawn(async move {
            write_frame(&mut writer, &frame).await;
        });
        Ok(stream_for(&request, reader))
    }
}

/// A stand-in control-plane API; the media executor never calls it.
#[derive(Debug)]
struct UnusedControlPlane;

macro_rules! never_called {
    () => {
        unimplemented!("the media executor settles outcomes without the control plane")
    };
}

#[async_trait::async_trait]
impl ControlPlaneApi for UnusedControlPlane {
    async fn activate(
        &self,
        _: NodeId,
        _: &RetryRequest<ActivateRequest>,
    ) -> Result<ActivateOutcome, VoomError> {
        never_called!()
    }

    async fn worker_readiness(
        &self,
        _: NodeId,
        _: WorkerId,
        _: &RetryRequest<crate::client::WorkerReadinessRequest>,
    ) -> Result<crate::client::WorkerReadinessOutcome, VoomError> {
        never_called!()
    }

    async fn deactivate(
        &self,
        _: NodeId,
        _: &RetryRequest<DeactivateRequest>,
    ) -> Result<DeactivateOutcome, VoomError> {
        never_called!()
    }

    async fn node_heartbeat(
        &self,
        _: NodeId,
        _: &RetryRequest<NodeHeartbeatRequest>,
    ) -> Result<NodeHeartbeatOutcome, VoomError> {
        never_called!()
    }

    async fn acquire(&self, _: &RetryRequest<AcquireRequest>) -> Result<AcquireOutcome, VoomError> {
        never_called!()
    }

    async fn lease_heartbeat(
        &self,
        _: LeaseId,
        _: &RetryRequest<LeaseHeartbeatRequest>,
    ) -> Result<LeaseHeartbeatOutcome, VoomError> {
        never_called!()
    }

    async fn complete(
        &self,
        _: LeaseId,
        _: &RetryRequest<CompleteRequest>,
    ) -> Result<CompleteOutcome, VoomError> {
        never_called!()
    }

    async fn fail(
        &self,
        _: LeaseId,
        _: &RetryRequest<FailRequest>,
    ) -> Result<FailOutcome, VoomError> {
        never_called!()
    }

    async fn commit_open(
        &self,
        _: &RetryRequest<CommitOpenRequest>,
    ) -> Result<CommitOpenOutcome, VoomError> {
        never_called!()
    }

    async fn authorize_commit_intent(
        &self,
        _: ArtifactCommitIntentId,
        _: &RetryRequest<CommitAuthorizeRequest>,
    ) -> Result<CommitAuthorizeOutcome, VoomError> {
        never_called!()
    }

    async fn report_commit_applying(
        &self,
        _: ArtifactCommitIntentId,
        _: &RetryRequest<CommitApplyingRequest>,
    ) -> Result<CommitApplyingOutcome, VoomError> {
        never_called!()
    }

    async fn report_commit_outcome(
        &self,
        _: ArtifactCommitIntentId,
        _: &RetryRequest<CommitOutcomeRequest>,
    ) -> Result<CommitReceiptOutcome, VoomError> {
        never_called!()
    }

    async fn complete_commit_intent(
        &self,
        _: ArtifactCommitIntentId,
        _: &RetryRequest<CommitCompleteRequest>,
    ) -> Result<CommitCompleteOutcome, VoomError> {
        never_called!()
    }
}

fn credentials() -> WorkerCredentials {
    WorkerCredentials {
        worker_id: WorkerId(31),
        worker_epoch: 0,
        secret: SecretString::from("media-child-secret"),
    }
}

fn media_worker_config(name: &str, operations: Vec<OperationKind>) -> WorkerConfig {
    WorkerConfig {
        name: name.to_owned(),
        program: PathBuf::from("/bin/false"),
        args: Vec::new(),
        operations,
        artifact_access: vec![ArtifactAccessMode::SharedMount],
        dependencies: crate::config::WorkerDependencyPaths::default(),
        accelerator: None,
        max_parallel: 2,
    }
}

fn context(
    storage_roots: HashMap<u64, PathBuf>,
    endpoints: ChildEndpointRegistry,
) -> CoordinatorContext {
    CoordinatorContext {
        client: Arc::new(UnusedControlPlane),
        scan_client: None,
        node_id: NodeId(7),
        incarnation_id: NodeIncarnationId::generate().unwrap(),
        worker_id: WorkerId(14),
        lease_ttl: Duration::from_secs(30),
        progress_timeout: Duration::from_secs(5),
        poll_interval: Duration::from_millis(50),
        shutdown_grace: Duration::from_secs(1),
        worker: media_worker_config("ffmpeg", vec![OperationKind::TranscodeAudio]),
        endpoints,
        storage_roots,
        fatal_tx: tokio::sync::mpsc::unbounded_channel().0,
    }
}

fn media_dispatch(operation: &str, payload: JsonValue) -> LeaseDispatch {
    LeaseDispatch {
        lease_id: LeaseId(423),
        scheduler_decision_id: 1,
        ticket_id: TicketId(9),
        worker_id: WorkerId(14),
        operation: operation.to_owned(),
        dispatch_payload: payload,
        lease_ttl_seconds: 30,
        heartbeat_after_seconds: 10,
        artifact_access_plan: ArtifactAccessPlan {
            id: 1,
            owner_node_id: Some(7),
            access_evidence: None,
        },
    }
}

/// Registry carrying the media child's own entry plus an optional live ffprobe
/// endpoint for the staged-output snapshot attachment.
fn registry_with(probe: Option<Arc<dyn ClientHandle>>) -> ChildEndpointRegistry {
    let mut workers = vec![media_worker_config(
        "ffmpeg",
        vec![OperationKind::TranscodeAudio],
    )];
    if probe.is_some() {
        workers.push(media_worker_config(
            "ffprobe",
            vec![OperationKind::ProbeFile],
        ));
    }
    let registry = ChildEndpointRegistry::new(&workers);
    if let Some(client) = probe {
        registry.publish(
            "ffprobe",
            ChildEndpoint {
                client,
                credentials: credentials(),
                operations: vec![OperationKind::ProbeFile],
            },
        );
    }
    registry
}

/// Script answering a transcode-audio dispatch by writing the staged output
/// and reporting its true facts.
fn transcode_script(output: PathBuf) -> Script {
    Box::new(move |request| {
        std::fs::write(&output, OUTPUT_BYTES).unwrap();
        result_frame(
            request.lease_id,
            transcode_audio_result(OUTPUT_BYTES.len() as u64),
        )
    })
}

/// Script emulating the ffprobe child: echoes the requested expected facts
/// back as observed and attaches a recognizable snapshot.
fn probe_script() -> Script {
    Box::new(|request| {
        let expected = &request.payload["expected"];
        let observed = json!({
            "size_bytes": expected["size_bytes"],
            "content_hash": expected["content_hash"]
        });
        result_frame(
            request.lease_id,
            json!({
                "status": "probed",
                "provider": "test-ffprobe",
                "provider_version": "0",
                "pre_probe": observed,
                "post_probe": observed,
                "snapshot": {"format": "probe-v1"}
            }),
        )
    })
}

async fn run(
    root: &TempDir,
    worker: Arc<dyn ClientHandle>,
    registry: ChildEndpointRegistry,
    payload: JsonValue,
) -> super::LeaseOutcome {
    let context = context(storage_roots(root), registry);
    let credentials = credentials();
    media_outcome(
        &media_dispatch("transcode_audio", payload),
        worker,
        &credentials,
        &context,
        &context.storage_roots,
        &context.endpoints,
    )
    .await
}

async fn recorded_requests(worker: &Arc<ScriptedMediaWorker>) -> Vec<(OperationKind, JsonValue)> {
    worker.requests.lock().await.clone()
}

// ------------------------------------------------------------------ tests --

#[test]
fn media_operations_are_exactly_the_byte_touching_set() {
    for operation in [
        "probe_file",
        "transcode_audio",
        "extract_audio",
        "transcode_video",
        "remux",
        "back_up_file",
        "verify_artifact",
    ] {
        assert!(is_media_dispatch_operation(operation));
    }
    for operation in ["scan_library", "hash_file", "commit_artifact", "nonsense"] {
        assert!(!is_media_dispatch_operation(operation));
    }
}

/// A payload without the nested `media_dispatch` object is not a renderable
/// media dispatch and must fail before any child sees work.
#[tokio::test]
async fn missing_envelope_object_fails_before_child_dispatch() {
    let root = TempDir::new().unwrap();
    let worker = ScriptedMediaWorker::new(Box::new(|_| unreachable!("no dispatch expected")));
    let outcome = run(&root, worker.clone(), registry_with(None), json!({})).await;

    assert!(matches!(
        outcome,
        super::LeaseOutcome::Failure(FailureClass::MalformedWorkerResult, _, _)
    ));
    assert!(recorded_requests(&worker).await.is_empty());
}

#[tokio::test]
async fn unknown_envelope_field_fails_before_child_dispatch() {
    let root = TempDir::new().unwrap();
    let mut envelope = transcode_audio_envelope(SOURCE_BYTES.len() as u64, &hash_of(SOURCE_BYTES));
    envelope["sneaky"] = json!("extra");
    let worker = ScriptedMediaWorker::new(Box::new(|_| unreachable!("no dispatch expected")));
    let outcome = run(
        &root,
        worker.clone(),
        registry_with(None),
        media_payload(&envelope),
    )
    .await;

    assert!(matches!(
        outcome,
        super::LeaseOutcome::Failure(FailureClass::MalformedWorkerResult, _, _)
    ));
    assert!(recorded_requests(&worker).await.is_empty());
}

#[tokio::test]
async fn schema_mismatch_fails_before_child_dispatch() {
    let root = TempDir::new().unwrap();
    let mut envelope = transcode_audio_envelope(SOURCE_BYTES.len() as u64, &hash_of(SOURCE_BYTES));
    envelope["schema"] = json!(voom_core::PROTOCOL_VERSION + 1);
    let worker = ScriptedMediaWorker::new(Box::new(|_| unreachable!("no dispatch expected")));
    let outcome = run(
        &root,
        worker.clone(),
        registry_with(None),
        media_payload(&envelope),
    )
    .await;

    assert!(matches!(
        outcome,
        super::LeaseOutcome::Failure(FailureClass::MalformedWorkerResult, _, _)
    ));
    assert!(recorded_requests(&worker).await.is_empty());
}

#[tokio::test]
async fn binding_miss_fails_before_child_dispatch() {
    let root = TempDir::new().unwrap();
    let mut envelope = transcode_audio_envelope(SOURCE_BYTES.len() as u64, &hash_of(SOURCE_BYTES));
    envelope["source"]["storage_root_id"] = json!(99);
    let worker = ScriptedMediaWorker::new(Box::new(|_| unreachable!("no dispatch expected")));
    let outcome = run(
        &root,
        worker.clone(),
        registry_with(None),
        media_payload(&envelope),
    )
    .await;

    assert!(matches!(
        outcome,
        super::LeaseOutcome::Failure(FailureClass::MalformedWorkerResult, _, _)
    ));
    assert!(recorded_requests(&worker).await.is_empty());
}

#[tokio::test]
async fn locator_escape_fails_without_child_dispatch() {
    let root = TempDir::new().unwrap();
    let mut envelope = transcode_audio_envelope(SOURCE_BYTES.len() as u64, &hash_of(SOURCE_BYTES));
    envelope["output"]["provider_relative_locator"] = json!("../escape.bin");
    let worker = ScriptedMediaWorker::new(Box::new(|_| unreachable!("no dispatch expected")));
    let outcome = run(
        &root,
        worker.clone(),
        registry_with(None),
        media_payload(&envelope),
    )
    .await;

    assert!(matches!(
        outcome,
        super::LeaseOutcome::Failure(FailureClass::MalformedWorkerResult, _, _)
    ));
    assert!(recorded_requests(&worker).await.is_empty());
}

#[tokio::test]
async fn source_fact_mismatch_fails_without_child_dispatch() {
    let root = TempDir::new().unwrap();
    write_source(&root);
    // Pin facts for different bytes than the fixture actually wrote.
    let envelope =
        transcode_audio_envelope(SOURCE_BYTES.len() as u64, &hash_of(b"drifted-content"));
    let worker = ScriptedMediaWorker::new(Box::new(|_| unreachable!("no dispatch expected")));
    let outcome = run(
        &root,
        worker.clone(),
        registry_with(None),
        media_payload(&envelope),
    )
    .await;

    match outcome {
        super::LeaseOutcome::Failure(FailureClass::ArtifactChecksumMismatch, reason, evidence) => {
            assert!(reason.contains("source"), "{reason}");
            assert!(evidence.get("agent_observed").is_some());
        }
        other => unreachable!("expected checksum-mismatch failure, got {other:?}"),
    }
    assert!(recorded_requests(&worker).await.is_empty());
}

#[tokio::test]
async fn happy_path_completes_with_agent_observed_evidence() {
    let root = TempDir::new().unwrap();
    // The resolver canonicalizes the storage root before joining the handle's
    // locator (macOS hands back /var/... while the bytes live under
    // /private/var/...), so expectations must be built from canonical paths
    // too — same fixture rule as commit_test's rooted resolver tests.
    let source = tokio::fs::canonicalize(write_source(&root)).await.unwrap();
    let staged_parent = ensure_output_parent(&root);
    let output = tokio::fs::canonicalize(staged_parent.parent().unwrap())
        .await
        .unwrap()
        .join(staged_parent.file_name().unwrap());
    let probe = ScriptedMediaWorker::new(probe_script());
    let worker = ScriptedMediaWorker::new(transcode_script(output.clone()));
    let outcome = run(
        &root,
        worker.clone(),
        registry_with(Some(probe)),
        media_payload(&transcode_audio_envelope(
            SOURCE_BYTES.len() as u64,
            &hash_of(SOURCE_BYTES),
        )),
    )
    .await;

    let result = match outcome {
        super::LeaseOutcome::Complete(result) => result,
        other => unreachable!("expected completion, got {other:?}"),
    };

    // The path-based child request was built from resolved node-local paths.
    let requests = recorded_requests(&worker).await;
    assert_eq!(requests.len(), 1);
    let (operation, payload) = &requests[0];
    assert_eq!(*operation, OperationKind::TranscodeAudio);
    assert_eq!(payload["input"]["path"], json!(source.to_string_lossy()));
    assert_eq!(payload["output"]["overwrite"], json!(false));
    assert_eq!(
        payload["output"]["staging_root"],
        json!(output.parent().unwrap().to_string_lossy())
    );
    assert_eq!(payload["output"]["path"], json!(output.to_string_lossy()));

    // Evidence carries this agent's own observations plus the ffprobe snapshot.
    let observed = &result["agent_observed"];
    assert_eq!(
        observed["input_pre"]["content_hash"],
        json!(hash_of(SOURCE_BYTES))
    );
    assert_eq!(
        observed["input_post"]["content_hash"],
        json!(hash_of(SOURCE_BYTES))
    );
    let outputs = observed["outputs"].as_array().unwrap();
    assert_eq!(outputs.len(), 1);
    assert_eq!(
        outputs[0]["provider_relative_locator"],
        json!("staged/out.mkv")
    );
    assert_eq!(
        outputs[0]["facts"]["size_bytes"],
        json!(OUTPUT_BYTES.len() as u64)
    );
    assert_eq!(
        outputs[0]["facts"]["content_hash"],
        json!(hash_of(OUTPUT_BYTES))
    );
    assert_eq!(outputs[0]["snapshot"]["format"], json!("probe-v1"));

    // The staged output survives completion; settlement stays data-only.
    assert_eq!(std::fs::read(&output).unwrap(), OUTPUT_BYTES);
}

#[tokio::test]
async fn stale_residue_is_cleared_before_child_dispatch() {
    let root = TempDir::new().unwrap();
    write_source(&root);
    let output = ensure_output_parent(&root);
    std::fs::write(&output, b"residue-from-a-crashed-attempt").unwrap();

    // Whether the planned output was already gone when the child ran.
    let absent_at_dispatch = Arc::new(AtomicBool::new(false));
    let watcher = Arc::clone(&absent_at_dispatch);
    let watched_output = output.clone();
    let script = Box::new(move |request: &OperationRequest| {
        watcher.store(!watched_output.exists(), Ordering::SeqCst);
        std::fs::write(&watched_output, OUTPUT_BYTES).unwrap();
        result_frame(
            request.lease_id,
            transcode_audio_result(OUTPUT_BYTES.len() as u64),
        )
    });
    let worker = ScriptedMediaWorker::new(script);
    let outcome = run(
        &root,
        worker,
        registry_with(None),
        media_payload(&transcode_audio_envelope(
            SOURCE_BYTES.len() as u64,
            &hash_of(SOURCE_BYTES),
        )),
    )
    .await;

    assert!(matches!(outcome, super::LeaseOutcome::Complete(_)));
    assert!(absent_at_dispatch.load(Ordering::SeqCst));
}

#[tokio::test]
async fn output_fact_mismatch_fails_with_evidence() {
    let root = TempDir::new().unwrap();
    write_source(&root);
    let output = ensure_output_parent(&root);
    // The child writes real bytes but lies about their length by one.
    let lying_output = output.clone();
    let script = Box::new(move |request: &OperationRequest| {
        std::fs::write(&lying_output, OUTPUT_BYTES).unwrap();
        result_frame(
            request.lease_id,
            transcode_audio_result(OUTPUT_BYTES.len() as u64 + 1),
        )
    });
    let worker = ScriptedMediaWorker::new(script);
    let outcome = run(
        &root,
        worker,
        registry_with(None),
        media_payload(&transcode_audio_envelope(
            SOURCE_BYTES.len() as u64,
            &hash_of(SOURCE_BYTES),
        )),
    )
    .await;

    match outcome {
        super::LeaseOutcome::Failure(class, reason, evidence) => {
            assert_eq!(class, FailureClass::ArtifactChecksumMismatch);
            assert!(reason.contains("output"), "{reason}");
            let observed = &evidence["agent_observed"];
            assert_eq!(
                observed["input_post"]["content_hash"],
                json!(hash_of(SOURCE_BYTES))
            );
        }
        other => unreachable!("expected checksum-mismatch failure, got {other:?}"),
    }
    // The mismatching bytes stay put for control-plane recovery inspection.
    assert_eq!(std::fs::read(&output).unwrap(), OUTPUT_BYTES);
}
