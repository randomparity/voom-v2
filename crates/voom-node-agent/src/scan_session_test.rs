//! Pump tests against a real control plane and scripted worker children
//! (ADR 0077 design C4 test plan). The control plane is the production
//! `ControlPlane` behind the production axum router, so start/batch/complete/
//! fail exercise the durable session state machine, not a mock of it.

use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use secrecy::{ExposeSecret, SecretString};
use serde_json::{Value as JsonValue, json};
use sqlx::SqlitePool;
use tokio::io::AsyncWriteExt;
use voom_control_plane::ControlPlane;
use voom_control_plane::execution::{RemoteActivateInput, RemoteWorkerDeclaration};
use voom_control_plane::workers::RegisterNodeInput;
use voom_core::{
    ArtifactAccessMode, ErrorCode, FailureClass, LeaseId, NodeId, NodeIncarnationId, NodeKind,
    OperationKind, WorkerId,
};
use voom_store::test_support::sqlite_url_for;
use voom_test_support::TempDatabase;
use voom_worker_protocol::{
    ClientHandle, DispatchStream, HandshakeResponse, HashFileResult, NdjsonReader,
    ObservedFileFacts, OperationRequest, OperationResponse, ProbeFileResult, ProgressFrame,
    ScanCandidate, ScanCandidateFile, WorkerCredentials,
};

use super::pump_scan_session;
use crate::client::{ArtifactAccessPlan, ControlPlaneClient, LeaseDispatch};
use crate::config::{AgentConfig, LoadedAgentConfig, TokenSource};
use crate::runtime::{ChildEndpoint, ChildEndpointRegistry, LeaseOutcome};
use crate::scan_session::ScanPumpContext;

const SESSION: u64 = 1;
/// `1970-01-01T00:00:00Z` — the shared fixture timestamp.
const T0: &str = "1970-01-01T00:00:00Z";
const HASH: &str = "blake3:0000000000000000000000000000000000000000000000000000000000000000";

// ---------------------------------------------------------------- fixtures --

struct PumpFixture {
    _database: TempDatabase,
    /// Keeps the control plane (and its pool) alive alongside the server.
    _cp: ControlPlane,
    node_id: NodeId,
    /// Independent pool for raw assertions; `pool_for_test` is crate-private
    /// to the control plane.
    db: SqlitePool,
    client: ControlPlaneClient,
    incarnation_id: NodeIncarnationId,
    dispatch: LeaseDispatch,
    _server: tokio::task::JoinHandle<()>,
}

/// Seed a real control plane with an activated owner node and one available
/// root, request a scan run, and return the pump inputs.
/// Register one remote node and activate it with the scan worker set.
async fn seed_owner_node(cp: &ControlPlane) -> (NodeId, NodeIncarnationId, SecretString) {
    let incarnation_id: NodeIncarnationId = "0123456789abcdef0123456789abcdef".parse().unwrap();
    let registered = cp
        .register_node(RegisterNodeInput {
            name: "pump-owner".to_owned(),
            kind: NodeKind::Remote,
            heartbeat_ttl_seconds: 600,
            metadata: json!({}),
        })
        .await
        .unwrap();
    cp.remote_activate(RemoteActivateInput {
        node_id: registered.node.id,
        token: registered.token.clone(),
        idempotency_key: "activate-pump-owner".to_owned(),
        request_hash: "activate-pump-owner-body".to_owned(),
        incarnation_id,
        workers: vec![
            RemoteWorkerDeclaration {
                logical_name: "scan".to_owned(),
                operations: vec![OperationKind::ScanLibrary],
                artifact_access: vec![ArtifactAccessMode::SharedMount],
                max_parallel: 1,
            },
            RemoteWorkerDeclaration {
                logical_name: "hash".to_owned(),
                operations: vec![OperationKind::HashFile],
                artifact_access: vec![ArtifactAccessMode::SharedMount],
                max_parallel: 2,
            },
            RemoteWorkerDeclaration {
                logical_name: "probe".to_owned(),
                operations: vec![OperationKind::ProbeFile],
                artifact_access: vec![ArtifactAccessMode::SharedMount],
                max_parallel: 2,
            },
        ],
    })
    .await
    .unwrap();
    (
        registered.node.id,
        incarnation_id,
        SecretString::from(registered.token.expose_secret().to_owned()),
    )
}

/// Create one enabled movie library with an active local root owned by `node`.
async fn seeded_pump_root(cp: &ControlPlane, node_id: NodeId) -> voom_core::StorageRootId {
    let library = cp
        .create_library(voom_store::repo::library::libraries::NewLibrary {
            slug: "pump".to_owned(),
            display_name: "Pump".to_owned(),
            media_kind: voom_store::repo::library::libraries::LibraryMediaKind::Movie,
            description: None,
            enabled: true,
        })
        .await
        .unwrap();
    let root = cp
        .create_library_root(voom_store::repo::library::library_roots::NewLibraryRoot {
            library_id: library.id,
            owner_node_id: node_id,
            provider_kind: voom_core::StorageProviderKind::LocalFilesystem,
            provider_locator: voom_core::ProviderLocator::new("/media/pump-root".to_owned())
                .unwrap(),
            display_locator: "/media/pump-root".to_owned(),
            include_globs: Vec::new(),
            exclude_globs: Vec::new(),
            extension_allowlist: vec![".mkv".to_owned()],
            scan_mode: voom_store::repo::library::library_roots::LibraryScanMode::ManualRecursive,
            symlink_policy: voom_store::repo::library::library_roots::SymlinkPolicy::Reject,
            hidden_file_policy: voom_store::repo::library::library_roots::HiddenFilePolicy::Ignore,
            max_depth: None,
            stability_seconds: 0,
            debounce_seconds: 0,
            default_output_root_id: None,
            default_staging_root_id: None,
            default_backup_root_id: None,
            enabled: true,
        })
        .await
        .unwrap();
    cp.activate_library_root(root.id, "pump-fixture".to_owned())
        .await
        .unwrap();
    root.id
}

async fn pump_fixture() -> PumpFixture {
    let database = TempDatabase::new().unwrap();
    let url = sqlite_url_for(database.path());
    voom_store::init(&url).await.unwrap();
    let cp = ControlPlane::open(&url).await.unwrap();

    let (node_id, incarnation_id, node_token) = seed_owner_node(&cp).await;
    let root = seeded_pump_root(&cp, node_id).await;

    let outcome = cp.request_scan_run(root, 600).await.unwrap();
    let voom_control_plane::scan::ScanRunOutcome::Requested(requested) = outcome else {
        unreachable!("an available fixture root requests cleanly");
    };
    let ticket = cp
        .tickets()
        .get(requested.ticket_id)
        .await
        .unwrap()
        .unwrap();

    // Serve the production API routes over loopback for the pump's client.
    let health = voom_control_plane::HealthPlane::open(&url).await.unwrap();
    let app = voom_api::router_with_control_plane(health, cp.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let config = LoadedAgentConfig {
        config: AgentConfig {
            control_plane_url: format!("http://{address}"),
            ca_cert: None,
            node_id,
            poll_interval_ms: 50,
            lease_ttl_seconds: 30,
            progress_idle_timeout_seconds: 5,
            shutdown_grace_seconds: 1,
            node_token: TokenSource::Env {
                name: "VOOM_TEST_TOKEN".to_owned(),
            },
            workers: Vec::new(),
        },
        node_token,
    };

    let assertion_pool = voom_store::connect(&url).await.unwrap();
    PumpFixture {
        _database: database,
        _cp: cp.clone(),
        node_id,
        db: assertion_pool,
        client: ControlPlaneClient::from_config(&config).unwrap(),
        incarnation_id,
        dispatch: LeaseDispatch {
            lease_id: LeaseId(77),
            scheduler_decision_id: 1,
            ticket_id: requested.ticket_id,
            worker_id: WorkerId(3),
            operation: "scan_library".to_owned(),
            dispatch_payload: ticket.payload,
            lease_ttl_seconds: 30,
            heartbeat_after_seconds: 10,
            artifact_access_plan: ArtifactAccessPlan {
                id: 1,
                owner_node_id: Some(node_id.0),
                access_evidence: None,
            },
        },
        _server: server,
    }
}

fn pump_context(node_id: NodeId, incarnation_id: NodeIncarnationId) -> ScanPumpContext {
    ScanPumpContext {
        node_id: node_id.0,
        incarnation_id,
        lease_id: LeaseId(77),
        lease_ttl_ms: 30_000,
        progress_timeout: Duration::from_secs(5),
    }
}

async fn session_status(fixture: &PumpFixture) -> String {
    sqlx::query_scalar("SELECT status FROM scan_sessions WHERE id = ?")
        .bind(i64::try_from(SESSION).unwrap())
        .fetch_one(&fixture.db)
        .await
        .unwrap()
}

async fn session_counters(fixture: &PumpFixture) -> (i64, i64) {
    sqlx::query_as("SELECT batch_count, observation_count FROM scan_sessions WHERE id = ?")
        .bind(i64::try_from(SESSION).unwrap())
        .fetch_one(&fixture.db)
        .await
        .unwrap()
}

/// The single observation's evidence JSON, if any.
async fn only_evidence(fixture: &PumpFixture) -> Option<String> {
    sqlx::query_scalar("SELECT evidence_json FROM scan_observations WHERE scan_session_id = ?")
        .bind(i64::try_from(SESSION).unwrap())
        .fetch_one(&fixture.db)
        .await
        .unwrap()
}

async fn observation_count(fixture: &PumpFixture) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM scan_observations WHERE scan_session_id = ?")
        .bind(i64::try_from(SESSION).unwrap())
        .fetch_one(&fixture.db)
        .await
        .unwrap()
}

// ------------------------------------------------------- worker fakes ------

async fn write_frame(writer: &mut tokio::io::DuplexStream, frame: &ProgressFrame) {
    let mut bytes = serde_json::to_vec(frame).unwrap();
    bytes.push(b'\n');
    writer.write_all(&bytes).await.unwrap();
    writer.flush().await.unwrap();
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

fn candidate(locator: &str) -> ScanCandidate {
    ScanCandidate {
        primary: ScanCandidateFile {
            provider_relative_locator: locator.to_owned(),
            provider_object_identity: "dev=1;ino=2".to_owned(),
            size_bytes: 123,
            modified_at: T0.to_owned(),
            kind: None,
        },
        sidecars: Vec::new(),
    }
}

fn sidecar_candidate(locator: &str, sidecar_locator: &str) -> ScanCandidate {
    let mut candidate = candidate(locator);
    candidate.sidecars.push(ScanCandidateFile {
        provider_relative_locator: sidecar_locator.to_owned(),
        provider_object_identity: "dev=1;ino=3".to_owned(),
        size_bytes: 45,
        modified_at: T0.to_owned(),
        kind: Some("external_subtitle".to_owned()),
    });
    candidate
}

fn agreeing_hash_result() -> HashFileResult {
    HashFileResult {
        content_hash: HASH.to_owned(),
        size_bytes: 123,
        modified_at: T0.to_owned(),
        file_key: Some(voom_core::FileKeyFacts {
            dev: 1,
            ino: 2,
            nlink: 1,
        }),
        stability_started_at: T0.to_owned(),
        stability_confirmed_at: T0.to_owned(),
        sidecars: Vec::new(),
    }
}

fn observed_agreeing() -> ObservedFileFacts {
    ObservedFileFacts {
        size_bytes: 123,
        content_hash: HASH.to_owned(),
        modified_at: Some(T0.to_owned()),
        local_file_key: None,
    }
}

fn agreeing_probe_result(snapshot: JsonValue) -> ProbeFileResult {
    ProbeFileResult {
        status: voom_worker_protocol::ProbeFileStatus::Probed,
        provider: "test-ffprobe".to_owned(),
        provider_version: "0".to_owned(),
        pre_probe: observed_agreeing(),
        post_probe: observed_agreeing(),
        snapshot,
    }
}

/// Fake scan worker: emits scripted candidate frames then a terminal frame.
#[derive(Debug)]
struct ScriptedScanWorker {
    frames: tokio::sync::Mutex<std::collections::VecDeque<ScanFrame>>,
}

#[derive(Debug, Clone)]
enum ScanFrame {
    Candidates(Vec<ScanCandidate>),
    /// Terminal success with `(discovered, skipped)`.
    Result(u64, u64),
    Crash,
}

impl ScriptedScanWorker {
    fn new(frames: Vec<ScanFrame>) -> Arc<Self> {
        Arc::new(Self {
            frames: tokio::sync::Mutex::new(frames.into()),
        })
    }
}

#[async_trait::async_trait]
impl ClientHandle for ScriptedScanWorker {
    async fn handshake(
        &self,
        offered: u32,
    ) -> Result<HandshakeResponse, voom_worker_protocol::ProtocolError> {
        Ok(HandshakeResponse { agreed: offered })
    }

    async fn identity(
        &self,
        credentials: &WorkerCredentials,
    ) -> Result<voom_worker_protocol::WorkerIdentityResponse, voom_worker_protocol::ProtocolError>
    {
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
    ) -> Result<DispatchStream, voom_worker_protocol::ProtocolError> {
        let (mut writer, reader) = tokio::io::duplex(64 * 1024);
        let mut frames = self.frames.lock().await.clone();
        tokio::spawn(async move {
            let mut seq = 0_u64;
            while let Some(frame) = frames.pop_front() {
                match frame {
                    ScanFrame::Candidates(candidates) => {
                        let payload =
                            voom_worker_protocol::encode_candidate_progress(&candidates).unwrap();
                        write_frame(
                            &mut writer,
                            &ProgressFrame::Progress {
                                lease_id: request.lease_id,
                                seq,
                                emitted_at: time::OffsetDateTime::UNIX_EPOCH,
                                percent: None,
                                message: None,
                                payload: Some(payload),
                            },
                        )
                        .await;
                        seq += 1;
                    }
                    ScanFrame::Result(discovered, skipped) => {
                        write_frame(
                            &mut writer,
                            &ProgressFrame::Result {
                                lease_id: request.lease_id,
                                seq,
                                emitted_at: time::OffsetDateTime::UNIX_EPOCH,
                                payload: json!({
                                    "discovered_count": discovered,
                                    "skipped_count": skipped,
                                }),
                            },
                        )
                        .await;
                        return;
                    }
                    ScanFrame::Crash => {
                        write_frame(
                            &mut writer,
                            &ProgressFrame::Error {
                                lease_id: request.lease_id,
                                seq,
                                emitted_at: time::OffsetDateTime::UNIX_EPOCH,
                                class: FailureClass::WorkerCrash,
                                code: ErrorCode::Internal,
                                message: "scripted crash".to_owned(),
                                payload: None,
                            },
                        )
                        .await;
                        return;
                    }
                }
            }
        });
        Ok(stream_for(&request, reader))
    }
}

/// How the fake single-file worker answers every request.
#[derive(Debug, Clone, Copy, PartialEq)]
enum SingleFileBehavior {
    Agree,
    NotFound,
    Unreadable,
    /// Probe-only: observed facts disagree with the expected hash facts.
    ModifiedDrift,
}

#[derive(Debug)]
struct ScriptedSingleFileWorker {
    operation: OperationKind,
    behavior: SingleFileBehavior,
}

impl ScriptedSingleFileWorker {
    fn new(operation: OperationKind, behavior: SingleFileBehavior) -> Arc<Self> {
        Arc::new(Self {
            operation,
            behavior,
        })
    }
}

#[async_trait::async_trait]
impl ClientHandle for ScriptedSingleFileWorker {
    async fn handshake(
        &self,
        offered: u32,
    ) -> Result<HandshakeResponse, voom_worker_protocol::ProtocolError> {
        Ok(HandshakeResponse { agreed: offered })
    }

    async fn identity(
        &self,
        credentials: &WorkerCredentials,
    ) -> Result<voom_worker_protocol::WorkerIdentityResponse, voom_worker_protocol::ProtocolError>
    {
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
    ) -> Result<DispatchStream, voom_worker_protocol::ProtocolError> {
        let (mut writer, reader) = tokio::io::duplex(64 * 1024);
        let operation = self.operation;
        let behavior = self.behavior;
        tokio::spawn(async move {
            let frame = match (operation, behavior) {
                (OperationKind::HashFile, SingleFileBehavior::Agree) => ProgressFrame::Result {
                    lease_id: request.lease_id,
                    seq: 0,
                    emitted_at: time::OffsetDateTime::UNIX_EPOCH,
                    payload: json!(agreeing_hash_result()),
                },
                (OperationKind::ProbeFile, SingleFileBehavior::Agree) => ProgressFrame::Result {
                    lease_id: request.lease_id,
                    seq: 0,
                    emitted_at: time::OffsetDateTime::UNIX_EPOCH,
                    payload: json!(agreeing_probe_result(json!({"format": "publish-v1"}))),
                },
                (OperationKind::ProbeFile, SingleFileBehavior::ModifiedDrift) => {
                    let mut drifted = observed_agreeing();
                    drifted.modified_at = Some("1971-01-01T00:00:00Z".to_owned());
                    let result = ProbeFileResult {
                        status: voom_worker_protocol::ProbeFileStatus::Probed,
                        provider: "test-ffprobe".to_owned(),
                        provider_version: "0".to_owned(),
                        pre_probe: drifted.clone(),
                        post_probe: drifted,
                        snapshot: json!({"format": "publish-v1"}),
                    };
                    ProgressFrame::Result {
                        lease_id: request.lease_id,
                        seq: 0,
                        emitted_at: time::OffsetDateTime::UNIX_EPOCH,
                        payload: json!(result),
                    }
                }
                (_, SingleFileBehavior::NotFound) => ProgressFrame::Error {
                    lease_id: request.lease_id,
                    seq: 0,
                    emitted_at: time::OffsetDateTime::UNIX_EPOCH,
                    class: FailureClass::ArtifactUnavailable,
                    code: ErrorCode::NotFound,
                    message: "scripted absence".to_owned(),
                    payload: None,
                },
                (_, _) => ProgressFrame::Error {
                    lease_id: request.lease_id,
                    seq: 0,
                    emitted_at: time::OffsetDateTime::UNIX_EPOCH,
                    class: FailureClass::ArtifactUnavailable,
                    code: ErrorCode::Internal,
                    message: "scripted unreadable".to_owned(),
                    payload: None,
                },
            };
            write_frame(&mut writer, &frame).await;
        });
        Ok(stream_for(&request, reader))
    }
}

fn credentials() -> WorkerCredentials {
    WorkerCredentials {
        worker_id: WorkerId(1),
        worker_epoch: 1,
        secret: SecretString::from("secret".to_owned()),
    }
}

fn registry_with(
    scan: Arc<dyn ClientHandle>,
    hash: Arc<dyn ClientHandle>,
    probe: Arc<dyn ClientHandle>,
) -> ChildEndpointRegistry {
    use crate::config::WorkerConfig;
    let named = |name: &str, operation: OperationKind| WorkerConfig {
        name: name.to_owned(),
        program: "/bin/true".into(),
        args: Vec::new(),
        operations: vec![operation],
        artifact_access: vec![voom_core::ArtifactAccessMode::SharedMount],
        max_parallel: 1,
    };
    let registry = ChildEndpointRegistry::new(&[
        named("scan", OperationKind::ScanLibrary),
        named("hash", OperationKind::HashFile),
        named("probe", OperationKind::ProbeFile),
    ]);
    let credentials = credentials();
    registry.publish(
        "scan",
        ChildEndpoint {
            client: scan,
            credentials: credentials.clone(),
            operations: vec![OperationKind::ScanLibrary],
        },
    );
    registry.publish(
        "hash",
        ChildEndpoint {
            client: hash,
            credentials: credentials.clone(),
            operations: vec![OperationKind::HashFile],
        },
    );
    registry.publish(
        "probe",
        ChildEndpoint {
            client: probe,
            credentials,
            operations: vec![OperationKind::ProbeFile],
        },
    );
    registry
}

// -------------------------------------------------------------- the tests --

#[tokio::test]
async fn ordered_batches_reach_the_control_plane_and_complete() {
    let fixture = pump_fixture().await;
    let context = pump_context(fixture.node_id, fixture.incarnation_id);
    let registry = registry_with(
        ScriptedScanWorker::new(vec![
            ScanFrame::Candidates(vec![candidate("a.mkv"), candidate("b.mkv")]),
            ScanFrame::Candidates(vec![candidate("c.mkv")]),
            ScanFrame::Result(3, 0),
        ]),
        ScriptedSingleFileWorker::new(OperationKind::HashFile, SingleFileBehavior::Agree),
        ScriptedSingleFileWorker::new(OperationKind::ProbeFile, SingleFileBehavior::Agree),
    );
    let scan_worker = registry.resolve(OperationKind::ScanLibrary).unwrap();

    let outcome = pump_scan_session(
        &fixture.dispatch,
        scan_worker.client.clone(),
        &scan_worker.credentials,
        &registry,
        &fixture.client,
        &context,
    )
    .await;

    let LeaseOutcome::Complete(summary) = outcome else {
        unreachable!("scripted workers drive this fixture to completion")
    };
    assert_eq!(summary["observed_count"], 3);
    assert_eq!(summary["failed_content_count"], 0);
    assert_eq!(summary["skipped_count"], 0);
    assert_eq!(session_status(&fixture).await, "succeeded");
    let (batches, observations) = session_counters(&fixture).await;
    assert_eq!((batches, observations), (1, 3));
}

#[tokio::test]
async fn empty_enumeration_completes_with_null_last_sequence() {
    let fixture = pump_fixture().await;
    let context = pump_context(fixture.node_id, fixture.incarnation_id);
    let registry = registry_with(
        ScriptedScanWorker::new(vec![ScanFrame::Result(0, 0)]),
        ScriptedSingleFileWorker::new(OperationKind::HashFile, SingleFileBehavior::Agree),
        ScriptedSingleFileWorker::new(OperationKind::ProbeFile, SingleFileBehavior::Agree),
    );
    let scan_worker = registry.resolve(OperationKind::ScanLibrary).unwrap();

    let outcome = pump_scan_session(
        &fixture.dispatch,
        scan_worker.client.clone(),
        &scan_worker.credentials,
        &registry,
        &fixture.client,
        &context,
    )
    .await;

    let LeaseOutcome::Complete(summary) = outcome else {
        unreachable!("scripted workers drive this fixture to completion")
    };
    assert_eq!(summary["observed_count"], 0);
    assert_eq!(session_counters(&fixture).await, (0, 0));
    assert_eq!(session_status(&fixture).await, "succeeded");
}

#[tokio::test]
async fn drift_candidate_yields_evidence_less_observation() {
    let fixture = pump_fixture().await;
    let context = pump_context(fixture.node_id, fixture.incarnation_id);
    let registry = registry_with(
        ScriptedScanWorker::new(vec![
            ScanFrame::Candidates(vec![candidate("drifting.mkv")]),
            ScanFrame::Result(1, 0),
        ]),
        ScriptedSingleFileWorker::new(OperationKind::HashFile, SingleFileBehavior::Agree),
        ScriptedSingleFileWorker::new(OperationKind::ProbeFile, SingleFileBehavior::ModifiedDrift),
    );
    let scan_worker = registry.resolve(OperationKind::ScanLibrary).unwrap();

    let outcome = pump_scan_session(
        &fixture.dispatch,
        scan_worker.client.clone(),
        &scan_worker.credentials,
        &registry,
        &fixture.client,
        &context,
    )
    .await;

    let LeaseOutcome::Complete(summary) = outcome else {
        unreachable!("scripted workers drive this fixture to completion")
    };
    assert_eq!(summary["observed_count"], 1);
    assert_eq!(summary["failed_content_count"], 1);
    assert!(
        only_evidence(&fixture).await.is_none(),
        "drift must publish no identity"
    );
}

#[tokio::test]
async fn vanished_candidate_records_no_observation() {
    let fixture = pump_fixture().await;
    let context = pump_context(fixture.node_id, fixture.incarnation_id);
    let registry = registry_with(
        ScriptedScanWorker::new(vec![
            ScanFrame::Candidates(vec![candidate("gone.mkv")]),
            ScanFrame::Result(1, 0),
        ]),
        ScriptedSingleFileWorker::new(OperationKind::HashFile, SingleFileBehavior::NotFound),
        ScriptedSingleFileWorker::new(OperationKind::ProbeFile, SingleFileBehavior::Agree),
    );
    let scan_worker = registry.resolve(OperationKind::ScanLibrary).unwrap();

    let outcome = pump_scan_session(
        &fixture.dispatch,
        scan_worker.client.clone(),
        &scan_worker.credentials,
        &registry,
        &fixture.client,
        &context,
    )
    .await;

    let LeaseOutcome::Complete(summary) = outcome else {
        unreachable!("scripted workers drive this fixture to completion")
    };
    assert_eq!(summary["observed_count"], 0);
    assert_eq!(observation_count(&fixture).await, 0);
}

#[tokio::test]
async fn unreadable_candidate_records_existence_without_evidence() {
    let fixture = pump_fixture().await;
    let context = pump_context(fixture.node_id, fixture.incarnation_id);
    let registry = registry_with(
        ScriptedScanWorker::new(vec![
            ScanFrame::Candidates(vec![candidate("locked.mkv")]),
            ScanFrame::Result(1, 0),
        ]),
        ScriptedSingleFileWorker::new(OperationKind::HashFile, SingleFileBehavior::Unreadable),
        ScriptedSingleFileWorker::new(OperationKind::ProbeFile, SingleFileBehavior::Agree),
    );
    let scan_worker = registry.resolve(OperationKind::ScanLibrary).unwrap();

    let outcome = pump_scan_session(
        &fixture.dispatch,
        scan_worker.client.clone(),
        &scan_worker.credentials,
        &registry,
        &fixture.client,
        &context,
    )
    .await;

    let LeaseOutcome::Complete(summary) = outcome else {
        unreachable!("scripted workers drive this fixture to completion")
    };
    assert_eq!(summary["observed_count"], 1);
    assert_eq!(summary["failed_content_count"], 1);
    assert!(only_evidence(&fixture).await.is_none());
}

#[tokio::test]
async fn sidecar_digests_ride_the_primary_evidence() {
    let fixture = pump_fixture().await;
    let context = pump_context(fixture.node_id, fixture.incarnation_id);
    let registry = registry_with(
        ScriptedScanWorker::new(vec![
            ScanFrame::Candidates(vec![sidecar_candidate("movie.mkv", "movie.srt")]),
            ScanFrame::Result(1, 0),
        ]),
        ScriptedSingleFileWorker::new(OperationKind::HashFile, SingleFileBehavior::Agree),
        ScriptedSingleFileWorker::new(OperationKind::ProbeFile, SingleFileBehavior::Agree),
    );
    let scan_worker = registry.resolve(OperationKind::ScanLibrary).unwrap();

    let outcome = pump_scan_session(
        &fixture.dispatch,
        scan_worker.client.clone(),
        &scan_worker.credentials,
        &registry,
        &fixture.client,
        &context,
    )
    .await;

    let LeaseOutcome::Complete(_) = outcome else {
        unreachable!("scripted workers drive this fixture to completion");
    };
    let evidence: JsonValue =
        serde_json::from_str(&only_evidence(&fixture).await.unwrap()).unwrap();
    assert_eq!(
        evidence["sidecars"][0]["provider_relative_locator"],
        "movie.srt"
    );
    assert_eq!(evidence["sidecars"][0]["role"], "external_subtitle");
}

#[tokio::test]
async fn oversized_enumeration_splits_into_ordered_batches() {
    let fixture = pump_fixture().await;
    let context = pump_context(fixture.node_id, fixture.incarnation_id);
    let mut frames = Vec::new();
    for index in 0..8 {
        frames.push(ScanFrame::Candidates(
            (0..150)
                .map(|offset| candidate(&format!("movie-{index}-{offset}.mkv")))
                .collect(),
        ));
    }
    frames.push(ScanFrame::Result(1200, 0));
    let registry = registry_with(
        ScriptedScanWorker::new(frames),
        ScriptedSingleFileWorker::new(OperationKind::HashFile, SingleFileBehavior::Agree),
        ScriptedSingleFileWorker::new(OperationKind::ProbeFile, SingleFileBehavior::Agree),
    );
    let scan_worker = registry.resolve(OperationKind::ScanLibrary).unwrap();

    let outcome = pump_scan_session(
        &fixture.dispatch,
        scan_worker.client.clone(),
        &scan_worker.credentials,
        &registry,
        &fixture.client,
        &context,
    )
    .await;

    let LeaseOutcome::Complete(summary) = outcome else {
        unreachable!("scripted workers drive this fixture to completion")
    };
    assert_eq!(summary["observed_count"], 1200);
    let (batches, observations) = session_counters(&fixture).await;
    assert_eq!(
        batches, 2,
        "1000-observation cap splits 1200 into two batches"
    );
    assert_eq!(observations, 1200);
}

#[tokio::test]
async fn evidence_dense_root_flushes_on_the_byte_budget() {
    let fixture = pump_fixture().await;
    let context = pump_context(fixture.node_id, fixture.incarnation_id);
    // One agreeing probe whose snapshot alone exceeds the 512 KiB batch
    // budget: the flush must fire before the API's request-body cap can.
    let registry = registry_with(
        ScriptedScanWorker::new(vec![
            ScanFrame::Candidates(
                (0..12)
                    .map(|offset| candidate(&format!("big-{offset}.mkv")))
                    .collect(),
            ),
            ScanFrame::Result(12, 0),
        ]),
        ScriptedSingleFileWorker::new(OperationKind::HashFile, SingleFileBehavior::Agree),
        Arc::new(DenseProbeWorker),
    );
    let scan_worker = registry.resolve(OperationKind::ScanLibrary).unwrap();

    let outcome = pump_scan_session(
        &fixture.dispatch,
        scan_worker.client.clone(),
        &scan_worker.credentials,
        &registry,
        &fixture.client,
        &context,
    )
    .await;

    let LeaseOutcome::Complete(_) = outcome else {
        unreachable!("scripted workers drive this fixture to completion");
    };
    // ~60 KiB evidence per observation reaches the 512 KiB budget partway
    // through the twelve candidates, so the run flushes mid-enumeration and
    // again at the end.
    let (batches, observations) = session_counters(&fixture).await;
    assert_eq!(observations, 12);
    assert!(batches >= 2, "byte budget must force an early flush");
}

/// Probe worker whose snapshot is ~600 KiB, forcing the byte-budget flush.
#[derive(Debug)]
struct DenseProbeWorker;

#[async_trait::async_trait]
impl ClientHandle for DenseProbeWorker {
    async fn handshake(
        &self,
        offered: u32,
    ) -> Result<HandshakeResponse, voom_worker_protocol::ProtocolError> {
        Ok(HandshakeResponse { agreed: offered })
    }

    async fn identity(
        &self,
        credentials: &WorkerCredentials,
    ) -> Result<voom_worker_protocol::WorkerIdentityResponse, voom_worker_protocol::ProtocolError>
    {
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
    ) -> Result<DispatchStream, voom_worker_protocol::ProtocolError> {
        let (mut writer, reader) = tokio::io::duplex(1024 * 1024);
        let lease_id = request.lease_id;
        tokio::spawn(async move {
            let mut snapshot = agreeing_probe_result(json!({}));
            snapshot.snapshot = json!({"blob": "x".repeat(60 * 1024)});
            write_frame(
                &mut writer,
                &ProgressFrame::Result {
                    lease_id,
                    seq: 0,
                    emitted_at: time::OffsetDateTime::UNIX_EPOCH,
                    payload: json!(snapshot),
                },
            )
            .await;
        });
        Ok(stream_for(&request, reader))
    }
}

#[tokio::test]
async fn fatal_scan_worker_crash_fails_session_and_lease() {
    let fixture = pump_fixture().await;
    let context = pump_context(fixture.node_id, fixture.incarnation_id);
    let registry = registry_with(
        ScriptedScanWorker::new(vec![ScanFrame::Crash]),
        ScriptedSingleFileWorker::new(OperationKind::HashFile, SingleFileBehavior::Agree),
        ScriptedSingleFileWorker::new(OperationKind::ProbeFile, SingleFileBehavior::Agree),
    );
    let scan_worker = registry.resolve(OperationKind::ScanLibrary).unwrap();

    let outcome = pump_scan_session(
        &fixture.dispatch,
        scan_worker.client.clone(),
        &scan_worker.credentials,
        &registry,
        &fixture.client,
        &context,
    )
    .await;

    let LeaseOutcome::Failure(class, reason, _) = outcome else {
        unreachable!("the scripted crash fails this session");
    };
    assert_eq!(class, FailureClass::WorkerCrash);
    assert!(reason.contains("scan worker failed"));
    // The best-effort fail call is fire-and-forget; poll briefly for it.
    for _ in 0..100 {
        if session_status(&fixture).await == "failed" {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    unreachable!(
        "session never reached failed; got {}",
        session_status(&fixture).await
    );
}

#[tokio::test]
async fn re_delivered_ticket_after_restart_fails_closed() {
    let fixture = pump_fixture().await;
    let context = pump_context(fixture.node_id, fixture.incarnation_id);
    let registry = registry_with(
        ScriptedScanWorker::new(vec![ScanFrame::Result(0, 0)]),
        ScriptedSingleFileWorker::new(OperationKind::HashFile, SingleFileBehavior::Agree),
        ScriptedSingleFileWorker::new(OperationKind::ProbeFile, SingleFileBehavior::Agree),
    );
    let scan_worker = registry.resolve(OperationKind::ScanLibrary).unwrap();

    // A restart mints a new incarnation; the replayed ticket arrives under it
    // and can no longer start the consumed session, so it must fail closed
    // instead of resuming mid-tree.
    let restarted_incarnation: NodeIncarnationId =
        "fedcba9876543210fedcba9876543210".parse().unwrap();
    let first = pump_scan_session(
        &fixture.dispatch,
        scan_worker.client.clone(),
        &scan_worker.credentials,
        &registry,
        &fixture.client,
        &context,
    )
    .await;
    assert!(matches!(first, LeaseOutcome::Complete(_)));

    let second = pump_scan_session(
        &fixture.dispatch,
        scan_worker.client.clone(),
        &scan_worker.credentials,
        &registry,
        &fixture.client,
        &pump_context(fixture.node_id, restarted_incarnation),
    )
    .await;
    let LeaseOutcome::Failure(_, reason, _) = second else {
        unreachable!("the consumed session cannot start again");
    };
    assert!(
        reason.contains("re-request the scan"),
        "restart rule must name the remedy: {reason}"
    );
}

#[tokio::test]
async fn batch_idempotency_keys_are_deterministic_per_sequence() {
    // The frozen-request replay contract lives in `client_test`; here we pin
    // the key format the durable batch route dedupes on.
    let node_id = NodeId(9_000_001);
    let incarnation: NodeIncarnationId = "0123456789abcdef0123456789abcdef".parse().unwrap();
    let context = pump_context(node_id, incarnation);
    let key =
        super::scan_idempotency_key_for_test(&context, voom_core::ScanSessionId(SESSION), "3");
    assert_eq!(key, format!("{incarnation}-scan-{SESSION}-3"));
}
