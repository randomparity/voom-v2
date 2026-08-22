//! Tests for the fenced node-local commit-intent executor (ADR 0074 Task 6).

use std::collections::{HashMap, VecDeque};
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use rand::SeedableRng;
use tempfile::TempDir;
use tokio::sync::{Mutex, watch};
use voom_core::ids::{ArtifactCommitIntentId, ArtifactCommitRecordId};
use voom_core::{
    ArtifactAccessMode, ArtifactHandleId, LeaseId, NodeId, NodeIncarnationId, OperationKind,
    StorageRootId, VoomError,
};

use super::*;
use crate::client::{
    AcquireOutcome, AcquireRequest, ActivateOutcome, ActivateRequest, CommitApplyingOutcome,
    CommitApplyingRequest, CommitAuthorizeOutcome, CommitAuthorizeRequest, CommitCompleteOutcome,
    CommitCompleteRequest, CommitExpectedFacts, CommitOpenOutcome, CommitOpenRequest,
    CommitOutcomeEvidence, CommitOutcomeRequest, CompleteOutcome, CompleteRequest,
    DeactivateOutcome, DeactivateRequest, FailOutcome, FailRequest, LeaseHeartbeatOutcome,
    LeaseHeartbeatRequest, NodeHeartbeatOutcome, NodeHeartbeatRequest, RetryRequest,
};
use crate::config::{AgentConfig, StorageRootBinding};
use crate::runtime::ControlPlaneApi;

const INTENT_ID: u64 = 77;
const FENCE_HEX: &str = "abab";

fn observed(bytes: &[u8]) -> CommitObservedFacts {
    CommitObservedFacts {
        size_bytes: bytes.len() as u64,
        content_hash: format!("blake3:{}", blake3::hash(bytes).to_hex()),
    }
}

fn expected_facts(bytes: &[u8]) -> CommitExpectedFacts {
    let facts = observed(bytes);
    CommitExpectedFacts {
        size_bytes: facts.size_bytes,
        content_hash: facts.content_hash,
    }
}

fn open_intent(state: &str, bytes: &[u8]) -> OpenCommitIntent {
    OpenCommitIntent {
        id: ArtifactCommitIntentId(INTENT_ID),
        state: state.to_owned(),
        artifact_handle_id: ArtifactHandleId(5),
        staging_storage_root_id: StorageRootId(1),
        staging_provider_relative_locator: "staging/asset.bin".to_owned(),
        staging_location_epoch: 3,
        target_storage_root_id: StorageRootId(1),
        target_provider_relative_locator: "library/asset.bin".to_owned(),
        target_root_epoch: 4,
        intent_epoch: 0,
        expected_facts: expected_facts(bytes),
    }
}

fn authorize_outcome(bytes: &[u8]) -> CommitAuthorizeOutcome {
    CommitAuthorizeOutcome {
        intent_id: ArtifactCommitIntentId(INTENT_ID),
        commit_record_id: ArtifactCommitRecordId(9),
        staging_storage_root_id: StorageRootId(1),
        staging_provider_relative_locator: "staging/asset.bin".to_owned(),
        target_storage_root_id: StorageRootId(1),
        target_provider_relative_locator: "library/asset.bin".to_owned(),
        expected_size_bytes: bytes.len() as u64,
        expected_content_hash: format!("blake3:{}", blake3::hash(bytes).to_hex()),
        fence_hex: FENCE_HEX.to_owned(),
    }
}

#[derive(Debug, Default)]
struct FakeCommitControlPlane {
    open_queue: Mutex<VecDeque<CommitOpenOutcome>>,
    authorize: Mutex<Option<CommitAuthorizeOutcome>>,
    /// When set, the complete route rejects with this conflict.
    complete_conflict: Mutex<Option<String>>,
    calls: Mutex<Vec<String>>,
    authorize_keys: Mutex<Vec<String>>,
    evidences: Mutex<Vec<CommitOutcomeEvidence>>,
    fences_sent_to_complete: Mutex<Vec<String>>,
}

#[async_trait]
impl ControlPlaneApi for FakeCommitControlPlane {
    async fn activate(
        &self,
        _node_id: NodeId,
        _request: &RetryRequest<ActivateRequest>,
    ) -> Result<ActivateOutcome, VoomError> {
        Err(VoomError::Internal("unused".to_owned()))
    }

    async fn deactivate(
        &self,
        _node_id: NodeId,
        _request: &RetryRequest<DeactivateRequest>,
    ) -> Result<DeactivateOutcome, VoomError> {
        Err(VoomError::Internal("unused".to_owned()))
    }

    async fn node_heartbeat(
        &self,
        _node_id: NodeId,
        _request: &RetryRequest<NodeHeartbeatRequest>,
    ) -> Result<NodeHeartbeatOutcome, VoomError> {
        Err(VoomError::Internal("unused".to_owned()))
    }

    async fn acquire(
        &self,
        _request: &RetryRequest<AcquireRequest>,
    ) -> Result<AcquireOutcome, VoomError> {
        Err(VoomError::Internal("unused".to_owned()))
    }

    async fn lease_heartbeat(
        &self,
        _lease_id: LeaseId,
        _request: &RetryRequest<LeaseHeartbeatRequest>,
    ) -> Result<LeaseHeartbeatOutcome, VoomError> {
        Err(VoomError::Internal("unused".to_owned()))
    }

    async fn complete(
        &self,
        _lease_id: LeaseId,
        _request: &RetryRequest<CompleteRequest>,
    ) -> Result<CompleteOutcome, VoomError> {
        Err(VoomError::Internal("unused".to_owned()))
    }

    async fn fail(
        &self,
        _lease_id: LeaseId,
        _request: &RetryRequest<FailRequest>,
    ) -> Result<FailOutcome, VoomError> {
        Err(VoomError::Internal("unused".to_owned()))
    }

    async fn commit_open(
        &self,
        _request: &RetryRequest<CommitOpenRequest>,
    ) -> Result<CommitOpenOutcome, VoomError> {
        self.calls.lock().await.push("open".to_owned());
        Ok(self
            .open_queue
            .lock()
            .await
            .pop_front()
            .unwrap_or(CommitOpenOutcome { intents: vec![] }))
    }

    async fn authorize_commit_intent(
        &self,
        intent_id: ArtifactCommitIntentId,
        request: &RetryRequest<CommitAuthorizeRequest>,
    ) -> Result<CommitAuthorizeOutcome, VoomError> {
        self.calls.lock().await.push("authorize".to_owned());
        self.authorize_keys
            .lock()
            .await
            .push(request.idempotency_key().to_owned());
        self.authorize
            .lock()
            .await
            .clone()
            .map(|mut outcome| {
                outcome.intent_id = intent_id;
                outcome
            })
            .ok_or_else(|| VoomError::Conflict(format!("commit intent {intent_id} not pending")))
    }

    async fn report_commit_applying(
        &self,
        intent_id: ArtifactCommitIntentId,
        _request: &RetryRequest<CommitApplyingRequest>,
    ) -> Result<CommitApplyingOutcome, VoomError> {
        self.calls.lock().await.push("applying".to_owned());
        Ok(CommitApplyingOutcome { intent_id })
    }

    async fn report_commit_outcome(
        &self,
        intent_id: ArtifactCommitIntentId,
        request: &RetryRequest<CommitOutcomeRequest>,
    ) -> Result<crate::client::CommitReceiptOutcome, VoomError> {
        self.calls.lock().await.push("outcome".to_owned());
        let body: serde_json::Value = serde_json::from_slice(request.body()).unwrap();
        let evidence: CommitOutcomeEvidence =
            serde_json::from_value(body["evidence"].clone()).unwrap();
        self.evidences.lock().await.push(evidence);
        Ok(crate::client::CommitReceiptOutcome {
            intent_id,
            kind: "reported".to_owned(),
        })
    }

    async fn complete_commit_intent(
        &self,
        intent_id: ArtifactCommitIntentId,
        request: &RetryRequest<CommitCompleteRequest>,
    ) -> Result<CommitCompleteOutcome, VoomError> {
        self.calls.lock().await.push("complete".to_owned());
        let body: serde_json::Value = serde_json::from_slice(request.body()).unwrap();
        self.fences_sent_to_complete
            .lock()
            .await
            .push(body["fence_hex"].as_str().unwrap().to_owned());
        if let Some(conflict) = self.complete_conflict.lock().await.clone() {
            return Err(VoomError::Conflict(conflict));
        }
        Ok(CommitCompleteOutcome {
            intent_id,
            commit_record_id: ArtifactCommitRecordId(9),
            result_file_version_id: None,
            result_file_location_id: None,
        })
    }
}

struct Fixture {
    api: Arc<FakeCommitControlPlane>,
    context: CommitCoordinatorContext,
    root: TempDir,
    /// Survives across `drive` calls exactly as the coordinator's own
    /// per-incarnation cache does.
    authorize_requests: Mutex<HashMap<u64, RetryRequest<CommitAuthorizeRequest>>>,
}

fn fixture_with_bytes(staging_bytes: &[u8]) -> Fixture {
    let root = TempDir::new().unwrap();
    let storage_roots = HashMap::from([(1_u64, root.path().to_path_buf())]);
    let api = Arc::new(FakeCommitControlPlane::default());
    let context = CommitCoordinatorContext {
        api: Arc::clone(&api) as Arc<dyn ControlPlaneApi>,
        node_id: NodeId(1),
        incarnation_id: NodeIncarnationId::generate().unwrap(),
        poll_interval: Duration::from_millis(50),
        storage_roots,
    };
    if !staging_bytes.is_empty() {
        let staging = root.path().join("staging/asset.bin");
        std::fs::create_dir_all(staging.parent().unwrap()).unwrap();
        std::fs::write(&staging, staging_bytes).unwrap();
    }
    Fixture {
        api,
        context,
        root,
        authorize_requests: Mutex::new(HashMap::new()),
    }
}

impl Fixture {
    fn target_path(&self) -> PathBuf {
        self.root.path().join("library/asset.bin")
    }

    async fn queue_listing(&self, intent: OpenCommitIntent) {
        self.api
            .open_queue
            .lock()
            .await
            .push_back(CommitOpenOutcome {
                intents: vec![intent],
            });
    }

    async fn drive(&self) -> Result<(), VoomError> {
        let mut authorize_requests = self.authorize_requests.lock().await;
        drive_open_intents(&self.context, &mut authorize_requests).await
    }

    async fn calls(&self) -> Vec<String> {
        self.api.calls.lock().await.clone()
    }
}

/// The config knob contract: bindings must be absolute and unique.
#[test]
fn config_rejects_relative_and_duplicate_storage_root_bindings() {
    let mut config = sample_agent_config();
    config.storage_roots[0].provider_locator = PathBuf::from("relative/path");
    assert!(
        matches!(
            config.validate(),
            Err(VoomError::Config(message)) if message.contains("must be an absolute path"),
        ),
        "a relative provider locator must fail validation"
    );

    let mut config = sample_agent_config();
    config.storage_roots.push(config.storage_roots[0].clone());
    assert!(
        matches!(
            config.validate(),
            Err(VoomError::Config(message)) if message.contains("more than once"),
        ),
        "a duplicate binding must fail validation"
    );
}

/// `storage_root_bindings` is the spawn-time index used by runtime wiring.
#[test]
fn runtime_index_builds_and_rejects_duplicate_bindings() {
    let config = sample_agent_config();
    let indexed = crate::runtime::storage_root_bindings(&config).unwrap();
    assert_eq!(indexed.get(&1), Some(&PathBuf::from("/tmp/voom-root")));

    let mut config = sample_agent_config();
    config.storage_roots.push(StorageRootBinding {
        storage_root_id: 1,
        provider_locator: PathBuf::from("/tmp/other"),
    });
    assert!(crate::runtime::storage_root_bindings(&config).is_err());
}

#[tokio::test]
async fn happy_path_journals_applying_then_promotes_and_completes() {
    let bytes = b"artifact-bytes".to_vec();
    let f = fixture_with_bytes(&bytes);
    *f.api.authorize.lock().await = Some(authorize_outcome(&bytes));
    f.queue_listing(open_intent("pending", &bytes)).await;

    f.drive().await.unwrap();

    assert_eq!(
        f.calls().await,
        vec!["open", "authorize", "applying", "outcome", "complete"]
    );
    let evidences = f.api.evidences.lock().await;
    assert!(
        matches!(
            evidences.as_slice(),
            [CommitOutcomeEvidence::Applied(applied)] if applied.observed == observed(&bytes),
        ),
        "expected one matching applied evidence, got {evidences:?}"
    );
    drop(evidences);
    assert_eq!(
        f.api.fences_sent_to_complete.lock().await.as_slice(),
        &[FENCE_HEX.to_owned()][..]
    );
    let promoted = tokio::fs::read(f.target_path()).await.unwrap();
    assert_eq!(promoted, bytes);
    assert!(
        !temp_sibling_present(&f.target_path()).await.unwrap(),
        "the temp sibling must be consumed by the install"
    );
}

#[tokio::test]
async fn existing_matching_target_converges_without_rewrite() {
    let bytes = b"already-promoted".to_vec();
    let f = fixture_with_bytes(&bytes);
    *f.api.authorize.lock().await = Some(authorize_outcome(&bytes));

    let target = f.target_path();
    std::fs::create_dir_all(target.parent().unwrap()).unwrap();
    std::fs::write(&target, &bytes).unwrap();
    let inode_before = std::fs::metadata(&target).unwrap().ino();

    f.queue_listing(open_intent("pending", &bytes)).await;
    f.drive().await.unwrap();

    let inode_after = std::fs::metadata(&target).unwrap().ino();
    assert_eq!(inode_before, inode_after, "target must not be rewritten");
    assert_eq!(
        f.calls().await,
        vec!["open", "authorize", "applying", "outcome", "complete"]
    );
    let evidences = f.api.evidences.lock().await;
    assert!(matches!(
        evidences.as_slice(),
        [CommitOutcomeEvidence::Applied(_)]
    ));
}

#[tokio::test]
async fn staging_drift_reports_mismatched_without_promotion() {
    let pinned = b"pinned-bytes".to_vec();
    let f = fixture_with_bytes(b"drifted-bytes");
    *f.api.authorize.lock().await = Some(authorize_outcome(&pinned));
    f.queue_listing(open_intent("pending", &pinned)).await;

    f.drive().await.unwrap();

    assert_eq!(
        f.calls().await,
        vec!["open", "authorize", "applying", "outcome"]
    );
    let evidences = f.api.evidences.lock().await;
    assert!(
        matches!(
            evidences.as_slice(),
            [CommitOutcomeEvidence::Mismatched(mismatched)]
                if mismatched.observed == Some(observed(b"drifted-bytes")),
        ),
        "expected mismatched evidence with drifted observed facts, got {evidences:?}"
    );
    drop(evidences);
    assert!(
        !tokio::fs::try_exists(f.target_path()).await.unwrap(),
        "drifted staging must never be promoted"
    );
}

#[tokio::test]
async fn fence_mismatch_surfaces_error() {
    let bytes = b"fenced-bytes".to_vec();
    let f = fixture_with_bytes(&bytes);
    *f.api.authorize.lock().await = Some(authorize_outcome(&bytes));
    *f.api.complete_conflict.lock().await =
        Some("commit fence does not match the minted fence".to_owned());
    f.queue_listing(open_intent("pending", &bytes)).await;

    let error = f.drive().await.unwrap_err();

    assert!(
        matches!(&error, VoomError::Conflict(message) if message.contains("commit fence")),
        "fence mismatch must surface as a conflict, got {error:?}"
    );
    assert_eq!(
        f.calls().await,
        vec!["open", "authorize", "applying", "outcome", "complete"]
    );
}

#[tokio::test]
async fn authorized_resume_replays_the_frozen_authorize_request() {
    let bytes = b"resume-bytes".to_vec();
    let f = fixture_with_bytes(&bytes);
    *f.api.authorize.lock().await = Some(authorize_outcome(&bytes));
    f.queue_listing(open_intent("pending", &bytes)).await;
    f.drive().await.unwrap();

    // A later poll rediscovers the same intent still authorized: the frozen
    // request replays instead of attempting a fresh authorization.
    f.queue_listing(open_intent("authorized", &bytes)).await;
    f.drive().await.unwrap();

    let keys = f.api.authorize_keys.lock().await;
    assert_eq!(keys.len(), 2, "two authorize rounds");
    assert_eq!(keys[0], keys[1], "the frozen idempotency key must replay");
}

#[tokio::test]
async fn recovery_required_target_matching_files_applied_evidence() {
    let bytes = b"recovered-bytes".to_vec();
    let f = fixture_with_bytes(b"");
    let target = f.target_path();
    std::fs::create_dir_all(target.parent().unwrap()).unwrap();
    std::fs::write(&target, &bytes).unwrap();

    f.queue_listing(open_intent("recovery_required", &bytes))
        .await;
    f.drive().await.unwrap();

    assert_eq!(f.calls().await, vec!["open", "outcome"]);
    assert!(f.api.authorize_keys.lock().await.is_empty());
    assert!(matches!(
        f.api.evidences.lock().await.as_slice(),
        [CommitOutcomeEvidence::Applied(_)]
    ));
}

#[tokio::test]
async fn recovery_required_absent_target_files_resolved_not_applied() {
    let bytes = b"never-promoted".to_vec();
    let f = fixture_with_bytes(b"");
    f.queue_listing(open_intent("recovery_required", &bytes))
        .await;
    f.drive().await.unwrap();

    assert_eq!(f.calls().await, vec!["open", "outcome"]);
    let evidences = f.api.evidences.lock().await;
    assert!(
        matches!(
            evidences.as_slice(),
            [CommitOutcomeEvidence::OutcomeUnknown(unknown)]
                if unknown.reason == RESOLVED_NOT_APPLIED_REASON,
        ),
        "expected resolved-not-applied outcome-unknown evidence, got {evidences:?}"
    );
}

/// A restarted agent rediscovers an `authorized` intent its prior incarnation
/// authorized. The fresh authorize is refused as not-pending; the executor
/// must classify that conflict, skip the intent for control-plane recovery,
/// and keep polling instead of exiting fatally.
#[tokio::test]
async fn restarted_agent_defers_authorized_intent_minted_by_prior_incarnation() {
    let bytes = b"restart-bytes".to_vec();
    let f = fixture_with_bytes(&bytes);
    f.queue_listing(open_intent("authorized", &bytes)).await;

    // Fresh incarnation: no frozen authorize request is cached yet.
    f.drive().await.unwrap();
    assert_eq!(f.calls().await, vec!["open", "authorize"]);
    assert!(f.api.evidences.lock().await.is_empty());
    assert!(f.api.fences_sent_to_complete.lock().await.is_empty());
}

/// The same scenario through the real coordinator loop: the classified
/// conflict must not produce `CoordinatorExit::Fatal`, and the normal poll
/// cadence must continue afterwards.
#[tokio::test(start_paused = true)]
async fn coordinator_survives_restart_authorized_conflict_and_keeps_polling() {
    let bytes = b"survive-bytes".to_vec();
    let root = TempDir::new().unwrap();
    let api = Arc::new(FakeCommitControlPlane::default());
    let context = CommitCoordinatorContext {
        api: Arc::clone(&api) as Arc<dyn ControlPlaneApi>,
        node_id: NodeId(1),
        incarnation_id: NodeIncarnationId::generate().unwrap(),
        poll_interval: Duration::from_secs(1),
        storage_roots: HashMap::from([(1_u64, root.path().to_path_buf())]),
    };
    let queue = async |intent| {
        api.open_queue.lock().await.push_back(CommitOpenOutcome {
            intents: vec![intent],
        });
    };
    queue(open_intent("authorized", &bytes)).await;

    let (shutdown_tx, shutdown_rx) = watch::channel(ShutdownKind::Running);
    let joined = tokio::spawn(run_commit_coordinator(
        context,
        shutdown_rx,
        StdRng::from_os_rng(),
    ));
    for _ in 0..2_000 {
        if api.calls.lock().await.len() >= 4 {
            break;
        }
        tokio::time::advance(Duration::from_millis(10)).await;
        if api.calls.lock().await.len() == 2 {
            // The next cycle must still list and defer, proving cadence.
            queue(open_intent("authorized", &bytes)).await;
        }
    }
    assert!(
        api.calls.lock().await.len() >= 4,
        "the coordinator stopped polling: {:?}",
        api.calls.lock().await
    );

    shutdown_tx.send(ShutdownKind::User).unwrap();
    assert!(
        matches!(
            joined.await.unwrap(),
            CoordinatorExit::Shutdown(LeaseSettlement::Completed)
        ),
        "a restart-authorized conflict must never fatal the coordinator"
    );
    assert!(api.evidences.lock().await.is_empty());
}

/// Sink-side containment: traversal locators are rejected before any join.
#[tokio::test]
async fn resolve_rooted_path_rejects_traversal_locators() {
    let f = fixture_with_bytes(b"");
    for locator in [
        "../../etc/passwd",
        "/etc/passwd",
        "library/../secrets.bin",
        "library\\asset.bin",
        "",
    ] {
        let error = resolve_rooted_path(&f.context, StorageRootId(1), locator)
            .await
            .unwrap_err();
        assert!(
            matches!(error, VoomError::Config(_)),
            "locator {locator:?} must be rejected, got {error:?}"
        );
    }
}

#[tokio::test]
async fn resolve_rooted_path_resolves_deep_valid_locator() {
    let f = fixture_with_bytes(b"");
    std::fs::create_dir_all(f.root.path().join("a/b")).unwrap();

    let resolved = resolve_rooted_path(&f.context, StorageRootId(1), "a/b/c/deep.bin")
        .await
        .unwrap();

    // The resolver canonicalizes the storage root and joins the locator
    // lexically (the locator need not exist). On macOS /var is a symlink to
    // /private/var, so the expectation must canonicalize the same root rather
    // than weakening the resolver.
    let root = tokio::fs::canonicalize(f.root.path()).await.unwrap();
    assert_eq!(resolved, root.join("a/b/c/deep.bin"));
}

#[cfg(unix)]
#[tokio::test]
async fn resolve_rooted_path_rejects_symlinked_intermediate_component() {
    let f = fixture_with_bytes(b"");
    let outside = TempDir::new().unwrap();
    std::os::unix::fs::symlink(outside.path(), f.root.path().join("escape")).unwrap();

    let error = resolve_rooted_path(&f.context, StorageRootId(1), "escape/asset.bin")
        .await
        .unwrap_err();

    assert!(
        matches!(&error, VoomError::Config(message) if message.contains("symlink")),
        "a symlinked intermediate component must be rejected, got {error:?}"
    );
}

/// Only the retired promoter's exact `.voom-tmp.<file>.<pid>.<counter>`
/// naming counts: similarly named targets own their own siblings.
#[tokio::test]
async fn temp_sibling_detection_matches_exact_promoter_naming() {
    let f = fixture_with_bytes(b"");
    let dir = f.root.path().join("library");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(".voom-tmp.data.bin2.123.4"), b"x").unwrap();
    std::fs::write(dir.join(".voom-tmp.data.bin.extra"), b"x").unwrap();
    let target = dir.join("data.bin");

    assert!(
        !temp_sibling_present(&target).await.unwrap(),
        "lookalike siblings of data.bin2 must not match data.bin"
    );

    std::fs::write(dir.join(".voom-tmp.data.bin.555.7"), b"x").unwrap();
    assert!(temp_sibling_present(&target).await.unwrap());
}

#[tokio::test]
async fn coordinator_exits_gracefully_then_forced_on_shutdown() {
    let root = TempDir::new().unwrap();
    let make_context = || CommitCoordinatorContext {
        api: Arc::new(FakeCommitControlPlane::default()) as Arc<dyn ControlPlaneApi>,
        node_id: NodeId(1),
        incarnation_id: NodeIncarnationId::generate().unwrap(),
        poll_interval: Duration::from_secs(50),
        storage_roots: HashMap::from([(1_u64, root.path().to_path_buf())]),
    };
    let context_a = make_context();
    let context_b = make_context();

    let (shutdown_tx, shutdown_rx) = watch::channel(ShutdownKind::Running);
    let joined = tokio::spawn(run_commit_coordinator(
        context_a,
        shutdown_rx,
        StdRng::from_os_rng(),
    ));
    shutdown_tx.send(ShutdownKind::User).unwrap();
    assert!(matches!(
        joined.await.unwrap(),
        CoordinatorExit::Shutdown(LeaseSettlement::Completed)
    ));

    let (forced_tx, forced_rx) = watch::channel(ShutdownKind::Running);
    let joined = tokio::spawn(run_commit_coordinator(
        context_b,
        forced_rx,
        StdRng::from_os_rng(),
    ));
    forced_tx.send(ShutdownKind::Forced).unwrap();
    assert!(matches!(
        joined.await.unwrap(),
        CoordinatorExit::Shutdown(LeaseSettlement::Forced)
    ));
}

fn sample_agent_config() -> AgentConfig {
    AgentConfig {
        control_plane_url: "http://127.0.0.1:1".to_owned(),
        ca_cert: None,
        node_id: NodeId(1),
        poll_interval_ms: 50,
        lease_ttl_seconds: 30,
        progress_idle_timeout_seconds: 30,
        shutdown_grace_seconds: 1,
        node_token: crate::config::TokenSource::Env {
            name: "VOOM_TOKEN".to_owned(),
        },
        storage_roots: vec![StorageRootBinding {
            storage_root_id: 1,
            provider_locator: PathBuf::from("/tmp/voom-root"),
        }],
        workers: vec![crate::config::WorkerConfig {
            name: "echo".to_owned(),
            program: PathBuf::from("/bin/echo"),
            args: vec![],
            operations: vec![OperationKind::HashFile],
            artifact_access: vec![ArtifactAccessMode::SharedMount],
            max_parallel: 1,
        }],
    }
}
