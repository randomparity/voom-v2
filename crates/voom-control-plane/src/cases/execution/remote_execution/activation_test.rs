use super::super::{
    RemoteAcquireInput, RemoteAcquireOutcome, RemoteActivateInput, RemoteDeactivateInput,
    RemoteNodeHeartbeatInput, RemoteWorkerDeclaration, RemoteWorkerReadinessInput,
};

use secrecy::SecretString;
use serde_json::json;
use time::{Duration, OffsetDateTime};
use tracing::instrument::WithSubscriber;
use voom_core::{
    ArtifactAccessMode, ErrorCode, LibraryId, NodeIncarnationEndReason, NodeIncarnationStatus,
    OperationKind, ProviderLocator, ScanSessionStatus, StorageProviderKind, StorageRootId,
    WorkerId, WorkerReadiness,
    clock_test_support::{FrozenClock, ManualClock},
};
use voom_store::repo::execution::nodes::NodeKind;
use voom_store::repo::execution::tickets::NewTicket;
use voom_store::repo::library::libraries::{LibraryMediaKind, NewLibrary};
use voom_store::repo::library::library_roots::{
    HiddenFilePolicy, LibraryScanMode, NewLibraryRoot, SymlinkPolicy,
};
use voom_worker_protocol::{VaapiVideoAcceleratorDescriptor, VideoAcceleratorDescriptor};

use crate::cases::workers::nodes::RegisterNodeInput;
use crate::scan::RemoteScanStartInput;

const T0: OffsetDateTime = OffsetDateTime::UNIX_EPOCH;

#[derive(Clone, Default)]
struct LogBuffer(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

struct LogWriter(LogBuffer);

impl std::io::Write for LogWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'writer> tracing_subscriber::fmt::MakeWriter<'writer> for LogBuffer {
    type Writer = LogWriter;

    fn make_writer(&'writer self) -> Self::Writer {
        LogWriter(self.clone())
    }
}

#[tokio::test]
async fn worker_readiness_is_fenced_authenticated_and_reversible() {
    let (cp, _clock, _tmp) = cp_with_manual_clock(T0).await;
    let registered = cp
        .register_node(RegisterNodeInput {
            name: "readiness-owner".to_owned(),
            kind: NodeKind::Remote,
            heartbeat_ttl_seconds: 60,
            metadata: json!({}),
        })
        .await
        .unwrap();
    let activation = cp
        .remote_activate(activation_input(
            registered.node.id,
            registered.token.clone(),
        ))
        .await
        .unwrap();
    let worker_id = activation.workers[0].worker_id;
    assert_eq!(
        cp.workers
            .get(worker_id)
            .await
            .unwrap()
            .unwrap()
            .status
            .as_str(),
        "registered"
    );

    let unauthorized = cp
        .remote_worker_readiness(RemoteWorkerReadinessInput {
            node_id: registered.node.id,
            token: SecretString::from("wrong-token"),
            incarnation_id: activation.incarnation_id,
            worker_id: WorkerId(u64::MAX),
            readiness: WorkerReadiness::Ready,
        })
        .await
        .unwrap_err();
    assert_eq!(unauthorized.error_code(), ErrorCode::Unauthorized);

    for (readiness, expected_status) in [
        (WorkerReadiness::Ready, "active"),
        (WorkerReadiness::NotReady, "registered"),
        (WorkerReadiness::Ready, "active"),
    ] {
        cp.remote_worker_readiness(RemoteWorkerReadinessInput {
            node_id: registered.node.id,
            token: registered.token.clone(),
            incarnation_id: activation.incarnation_id,
            worker_id,
            readiness,
        })
        .await
        .unwrap();
        assert_eq!(
            cp.workers
                .get(worker_id)
                .await
                .unwrap()
                .unwrap()
                .status
                .as_str(),
            expected_status
        );
    }
}

#[tokio::test]
async fn worker_readiness_rejects_wrong_incarnation_node_and_expired_heartbeat() {
    let (cp, clock, _tmp) = cp_with_manual_clock(T0).await;
    let first = register_remote_node(&cp).await;
    let first_activation = cp
        .remote_activate(activation_input_for(first.node.id, first.token.clone(), 20))
        .await
        .unwrap();
    let second = cp
        .register_node(RegisterNodeInput {
            name: "second-readiness-node".to_owned(),
            kind: NodeKind::Remote,
            heartbeat_ttl_seconds: 60,
            metadata: json!({}),
        })
        .await
        .unwrap();
    let second_activation = cp
        .remote_activate(activation_input_for(
            second.node.id,
            second.token.clone(),
            21,
        ))
        .await
        .unwrap();
    let worker_id = first_activation.workers[0].worker_id;

    let wrong_incarnation = cp
        .remote_worker_readiness(RemoteWorkerReadinessInput {
            node_id: first.node.id,
            token: first.token.clone(),
            incarnation_id: second_activation.incarnation_id,
            worker_id,
            readiness: WorkerReadiness::Ready,
        })
        .await
        .unwrap_err();
    assert_eq!(wrong_incarnation.error_code(), ErrorCode::Conflict);

    let wrong_node = cp
        .remote_worker_readiness(RemoteWorkerReadinessInput {
            node_id: second.node.id,
            token: second.token,
            incarnation_id: second_activation.incarnation_id,
            worker_id,
            readiness: WorkerReadiness::Ready,
        })
        .await
        .unwrap_err();
    assert_eq!(wrong_node.error_code(), ErrorCode::Conflict);

    sqlx::query("UPDATE workers SET status = 'stale' WHERE id = ?")
        .bind(i64::try_from(worker_id.0).unwrap())
        .execute(cp.pool_for_test())
        .await
        .unwrap();
    let stale_worker = cp
        .remote_worker_readiness(RemoteWorkerReadinessInput {
            node_id: first.node.id,
            token: first.token.clone(),
            incarnation_id: first_activation.incarnation_id,
            worker_id,
            readiness: WorkerReadiness::Ready,
        })
        .await
        .unwrap_err();
    assert!(stale_worker.to_string().contains("Stale"), "{stale_worker}");
    sqlx::query("UPDATE workers SET status = 'registered' WHERE id = ?")
        .bind(i64::try_from(worker_id.0).unwrap())
        .execute(cp.pool_for_test())
        .await
        .unwrap();

    clock.advance(time::Duration::seconds(61));
    let expired = cp
        .remote_worker_readiness(RemoteWorkerReadinessInput {
            node_id: first.node.id,
            token: first.token,
            incarnation_id: first_activation.incarnation_id,
            worker_id,
            readiness: WorkerReadiness::Ready,
        })
        .await
        .unwrap_err();
    assert!(
        expired.to_string().contains("heartbeat expired"),
        "{expired}"
    );
    assert_eq!(
        cp.workers
            .get(worker_id)
            .await
            .unwrap()
            .unwrap()
            .status
            .as_str(),
        "registered"
    );
}

#[tokio::test]
async fn activation_rejects_accelerators_without_transcode_or_stable_identity() {
    let (cp, _clock, _tmp) = cp_with_manual_clock(T0).await;
    let registered = register_remote_node(&cp).await;
    let mut input = activation_input(registered.node.id, registered.token);
    input.workers[0].accelerator = Some(VideoAcceleratorDescriptor::Vaapi(
        VaapiVideoAcceleratorDescriptor {
            pci_address: "0000:f4:00.0".to_owned(),
            device_name: "Radeon Pro".to_owned(),
            driver_version: "Mesa 26.1".to_owned(),
            encoders: vec!["hevc_vaapi".to_owned()],
            decoders: vec!["hevc".to_owned()],
            max_sessions: 2,
        },
    ));

    let error = cp.remote_activate(input.clone()).await.unwrap_err();
    assert!(
        error
            .to_string()
            .contains("does not declare transcode_video"),
        "{error}"
    );
    input.workers[0].operations = vec![OperationKind::TranscodeVideo];
    let Some(VideoAcceleratorDescriptor::Vaapi(accelerator)) =
        input.workers[0].accelerator.as_mut()
    else {
        return;
    };
    accelerator.pci_address = "/dev/dri/renderD128".to_owned();
    let error = cp.remote_activate(input).await.unwrap_err();
    assert!(error.to_string().contains("pci_address"), "{error}");
    assert!(
        cp.get_node(registered.node.id)
            .await
            .unwrap()
            .unwrap()
            .active_incarnation_id
            .is_none()
    );
}

#[tokio::test]
async fn activation_rejects_invalid_descriptor_collections_before_persistence() {
    let (cp, _clock, _tmp) = cp_with_manual_clock(T0).await;
    let registered = register_remote_node(&cp).await;
    let mut input = activation_input(registered.node.id, registered.token);
    input.workers = vec![RemoteWorkerDeclaration {
        logical_name: "transcode".to_owned(),
        operations: vec![OperationKind::TranscodeVideo],
        artifact_access: vec![ArtifactAccessMode::SharedMount],
        accelerator: Some(VideoAcceleratorDescriptor::Vaapi(
            VaapiVideoAcceleratorDescriptor {
                pci_address: "0000:f4:00.0".to_owned(),
                device_name: "Radeon Pro".to_owned(),
                driver_version: "Mesa 26.1".to_owned(),
                encoders: vec!["hevc_vaapi".to_owned(), "hevc_vaapi".to_owned()],
                decoders: vec!["hevc".to_owned()],
                max_sessions: 2,
            },
        )),
        max_parallel: 2,
    }];
    let counts_before = activation_row_counts(&cp).await;

    let error = cp.remote_activate(input).await.unwrap_err();
    assert_eq!(error.error_code(), ErrorCode::ConfigInvalid);
    assert!(error.to_string().contains("duplicate"), "{error}");
    assert_eq!(activation_row_counts(&cp).await, counts_before);
}

#[tokio::test]
async fn activation_persists_accelerator_only_on_the_transcode_capability() {
    let (cp, _clock, _tmp) = cp_with_manual_clock(T0).await;
    let registered = register_remote_node(&cp).await;
    let mut input = activation_input(registered.node.id, registered.token);
    input.workers = vec![RemoteWorkerDeclaration {
        logical_name: "media".to_owned(),
        operations: vec![OperationKind::ProbeFile, OperationKind::TranscodeVideo],
        artifact_access: vec![ArtifactAccessMode::SharedMount],
        accelerator: Some(VideoAcceleratorDescriptor::Vaapi(
            VaapiVideoAcceleratorDescriptor {
                pci_address: "0000:f4:00.0".to_owned(),
                device_name: "Radeon Pro".to_owned(),
                driver_version: "Mesa 26.1".to_owned(),
                encoders: vec!["hevc_vaapi".to_owned()],
                decoders: vec!["hevc".to_owned()],
                max_sessions: 2,
            },
        )),
        max_parallel: 2,
    }];

    let outcome = cp.remote_activate(input).await.unwrap();
    let rows: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT operation, hardware, extra FROM worker_capabilities \
         WHERE worker_id = ? ORDER BY operation",
    )
    .bind(i64::try_from(outcome.workers[0].worker_id.0).unwrap())
    .fetch_all(cp.pool_for_test())
    .await
    .unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].0, "probe_file");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&rows[0].1).unwrap(),
        json!([])
    );
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&rows[0].2).unwrap(),
        json!({})
    );
    assert_eq!(rows[1].0, "transcode_video");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&rows[1].1).unwrap(),
        json!(["vaapi:pci-0000:f4:00.0"])
    );
    let transcode_extra = serde_json::from_str::<serde_json::Value>(&rows[1].2).unwrap();
    assert_eq!(transcode_extra["accelerator"]["backend"], "vaapi");
}
impl LogBuffer {
    fn text(&self) -> String {
        String::from_utf8(
            self.0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone(),
        )
        .unwrap()
    }
}

#[test]
fn remote_activation_outcomes_reject_unknown_fields() {
    let incarnation_id = "0123456789abcdef0123456789abcdef".parse().unwrap();
    let mut activation = serde_json::to_value(super::super::RemoteActivateOutcome {
        node_id: voom_core::NodeId(1),
        node_epoch: 2,
        incarnation_id,
        heartbeat_ttl_seconds: 60,
        workers: vec![super::super::ActivatedWorker {
            logical_name: "probe".to_owned(),
            worker_id: voom_core::WorkerId(3),
            worker_epoch: 0,
        }],
    })
    .unwrap();
    activation["unexpected"] = json!(true);
    assert!(serde_json::from_value::<super::super::RemoteActivateOutcome>(activation).is_err());

    let mut deactivation = serde_json::to_value(super::super::RemoteDeactivateOutcome {
        node_id: voom_core::NodeId(1),
        node_epoch: 3,
        incarnation_id,
        status: NodeIncarnationStatus::Retired,
        reason: NodeIncarnationEndReason::GracefulShutdown,
        retired_worker_ids: vec![voom_core::WorkerId(3)],
    })
    .unwrap();
    deactivation["unexpected"] = json!(true);
    assert!(serde_json::from_value::<super::super::RemoteDeactivateOutcome>(deactivation).is_err());
}

#[tokio::test]
async fn remote_activation_registers_complete_manifest_atomically_and_replays() {
    let (cp, _tmp) = cp_at(T0).await;
    let registered = cp
        .register_node(RegisterNodeInput {
            name: "remote-node".to_owned(),
            kind: NodeKind::Remote,
            heartbeat_ttl_seconds: 60,
            metadata: json!({}),
        })
        .await
        .unwrap();
    let input = activation_input(registered.node.id, registered.token.clone());

    let first = cp.remote_activate(input.clone()).await.unwrap();
    let replay = cp.remote_activate(input).await.unwrap();

    assert_eq!(first, replay);
    assert_eq!(first.workers.len(), 2);
    assert_eq!(
        cp.list_node_incarnations(registered.node.id, 10)
            .await
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn remote_activation_bounds_fresh_successes_per_node_without_poisoning_replay() {
    let (cp, clock, _tmp) = cp_with_manual_clock(T0).await;
    let first_node = register_remote_node(&cp).await;
    let second_node = cp
        .register_node(RegisterNodeInput {
            name: "second-remote-node".to_owned(),
            kind: NodeKind::Remote,
            heartbeat_ttl_seconds: 60,
            metadata: json!({}),
        })
        .await
        .unwrap();
    let mut first_input = None;
    let mut first_outcome = None;
    for ordinal in 1..=5 {
        let input = activation_input_for(first_node.node.id, first_node.token.clone(), ordinal);
        let outcome = cp.remote_activate(input.clone()).await.unwrap();
        if ordinal == 1 {
            first_input = Some(input);
            first_outcome = Some(outcome);
        }
    }
    let first_input = first_input.unwrap();
    let first_outcome = first_outcome.unwrap();
    assert_eq!(
        cp.remote_activate(first_input).await.unwrap(),
        first_outcome
    );
    cp.remote_activate(activation_input_for(
        second_node.node.id,
        second_node.token,
        100,
    ))
    .await
    .unwrap();

    let rejected = activation_input_for(first_node.node.id, first_node.token.clone(), 6);
    let counts_before = activation_row_counts(&cp).await;
    let error = cp.remote_activate(rejected.clone()).await.unwrap_err();
    assert_eq!(error.error_code(), ErrorCode::Conflict);
    assert!(error.to_string().contains("5 activations per 60 seconds"));
    assert_eq!(activation_row_counts(&cp).await, counts_before);
    let rejected_replay_rows: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM remote_idempotency_keys \
         WHERE node_id = ? AND idempotency_key = ?",
    )
    .bind(i64::try_from(first_node.node.id.0).unwrap())
    .bind(&rejected.idempotency_key)
    .fetch_one(cp.pool_for_test())
    .await
    .unwrap();
    assert_eq!(rejected_replay_rows, 0);

    let mut duplicate = activation_input_for(first_node.node.id, first_node.token.clone(), 5);
    duplicate.idempotency_key = "activate-duplicate-incarnation".to_owned();
    duplicate.request_hash = "activation-duplicate-incarnation-body".to_owned();
    let duplicate_error = cp.remote_activate(duplicate).await.unwrap_err();
    assert!(
        duplicate_error
            .to_string()
            .contains("was already activated")
    );

    clock.advance(time::Duration::seconds(60));
    assert!(cp.remote_activate(rejected.clone()).await.is_err());
    clock.advance(time::Duration::nanoseconds(1));
    cp.remote_activate(rejected).await.unwrap();
}

#[tokio::test]
async fn remote_activation_samples_quota_window_after_writer_serialization() {
    let (cp, clock, _tmp) = cp_with_manual_clock(T0).await;
    let registered = register_remote_node(&cp).await;
    for ordinal in 1..=5 {
        cp.remote_activate(activation_input_for(
            registered.node.id,
            registered.token.clone(),
            ordinal,
        ))
        .await
        .unwrap();
    }
    // Hold the write lock so the call under test contends. Raw here,
    // as in voom-store's tests: this reserves the lock without
    // writing, which no production opener describes.
    let holding_tx = cp
        .pool_for_test()
        .begin_with("BEGIN IMMEDIATE")
        .await
        .unwrap();
    let waiting_cp = cp.clone();
    let waiting_input = activation_input_for(registered.node.id, registered.token, 6);
    let waiting = tokio::spawn(async move { waiting_cp.remote_activate(waiting_input).await });
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    clock.advance(time::Duration::seconds(61));
    holding_tx.commit().await.unwrap();

    waiting.await.unwrap().unwrap();
}

#[tokio::test]
async fn remote_activation_quota_rejection_is_operator_visible_without_secrets() {
    let (cp, _clock, _tmp) = cp_with_manual_clock(T0).await;
    let registered = register_remote_node(&cp).await;
    for ordinal in 1..=5 {
        cp.remote_activate(activation_input_for(
            registered.node.id,
            registered.token.clone(),
            ordinal,
        ))
        .await
        .unwrap();
    }
    let mut rejected = activation_input_for(registered.node.id, registered.token, 6);
    rejected.idempotency_key = "secret-idempotency-key".to_owned();
    rejected.request_hash = "secret-request-hash".to_owned();
    rejected.workers[0].logical_name = "secret-worker-name".to_owned();
    let rejected_incarnation = rejected.incarnation_id.to_string();
    let logs = LogBuffer::default();
    let subscriber = tracing_subscriber::fmt()
        .without_time()
        .with_ansi(false)
        .with_writer(logs.clone())
        .finish();
    let error = cp
        .remote_activate(rejected)
        .with_subscriber(subscriber)
        .await
        .unwrap_err();

    assert_eq!(error.error_code(), ErrorCode::Conflict);
    let output = logs.text();
    assert!(output.contains("remote node activation quota exceeded"));
    assert!(output.contains("activation_count=5"));
    assert!(output.contains("activation_limit=5"));
    assert!(output.contains("window_seconds=60"));
    assert!(!output.contains("secret-idempotency-key"));
    assert!(!output.contains("secret-request-hash"));
    assert!(!output.contains("secret-worker-name"));
    assert!(!output.contains(&rejected_incarnation));
}

#[tokio::test]
async fn remote_activation_rejects_reused_keys_and_incarnations_without_mutation() {
    let (cp, _tmp) = cp_at(T0).await;
    let registered = register_remote_node(&cp).await;
    let first_input = activation_input(registered.node.id, registered.token.clone());
    let first = cp.remote_activate(first_input.clone()).await.unwrap();

    let mut changed_body = first_input.clone();
    changed_body.request_hash = "different-body".to_owned();
    let key_error = cp.remote_activate(changed_body).await.unwrap_err();
    assert_eq!(key_error.error_code(), ErrorCode::Conflict);

    let mut changed_key = first_input;
    changed_key.idempotency_key = "activate-2".to_owned();
    let incarnation_error = cp.remote_activate(changed_key).await.unwrap_err();
    assert_eq!(incarnation_error.error_code(), ErrorCode::Conflict);

    let history = cp
        .list_node_incarnations(registered.node.id, 10)
        .await
        .unwrap();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].status, NodeIncarnationStatus::Active);
    assert_eq!(
        history[0].worker_count,
        u32::try_from(first.workers.len()).unwrap()
    );
}

#[tokio::test]
async fn remote_activation_supersedes_workers_and_reuses_manifest_under_distinct_names() {
    let (cp, _tmp) = cp_at(T0).await;
    let registered = register_remote_node(&cp).await;
    let first = cp
        .remote_activate(activation_input(
            registered.node.id,
            registered.token.clone(),
        ))
        .await
        .unwrap();
    let held_lease_id = hold_hash_lease(&cp, &registered, &first).await;
    let mut replacement = activation_input(registered.node.id, registered.token.clone());
    replacement.idempotency_key = "activate-2".to_owned();
    replacement.request_hash = "activation-body-2".to_owned();
    replacement.incarnation_id = "fedcba9876543210fedcba9876543210".parse().unwrap();
    let second = cp.remote_activate(replacement).await.unwrap();

    assert_ne!(first.incarnation_id, second.incarnation_id);
    for (old, new) in first.workers.iter().zip(&second.workers) {
        assert_ne!(old.worker_id, new.worker_id);
        assert_eq!(
            cp.workers()
                .get(old.worker_id)
                .await
                .unwrap()
                .unwrap()
                .status,
            voom_core::WorkerStatus::Retired
        );
        let old_name = &cp.workers().get(old.worker_id).await.unwrap().unwrap().name;
        let new_name = &cp.workers().get(new.worker_id).await.unwrap().unwrap().name;
        assert_ne!(old_name, new_name);
        assert!(old_name.contains(&first.incarnation_id.to_string()));
        assert!(new_name.contains(&second.incarnation_id.to_string()));
    }
    let history = cp
        .list_node_incarnations(registered.node.id, 10)
        .await
        .unwrap();
    assert_eq!(history.len(), 2);
    assert_eq!(history[0].status, NodeIncarnationStatus::Active);
    assert_eq!(history[1].status, NodeIncarnationStatus::Superseded);
    assert_eq!(
        history[1].end_reason,
        Some(NodeIncarnationEndReason::Superseded)
    );
    assert_eq!(
        cp.leases().get(held_lease_id).await.unwrap().unwrap().state,
        voom_store::repo::execution::leases::LeaseState::Held
    );
}

#[tokio::test]
async fn remote_activation_validates_duplicate_manifest_before_superseding_current() {
    let (cp, _tmp) = cp_at(T0).await;
    let registered = register_remote_node(&cp).await;
    let first = cp
        .remote_activate(activation_input(
            registered.node.id,
            registered.token.clone(),
        ))
        .await
        .unwrap();
    let mut duplicate = activation_input(registered.node.id, registered.token.clone());
    duplicate.idempotency_key = "activate-duplicate".to_owned();
    duplicate.request_hash = "duplicate-body".to_owned();
    duplicate.incarnation_id = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".parse().unwrap();
    duplicate.workers[1].logical_name = duplicate.workers[0].logical_name.clone();

    let error = cp.remote_activate(duplicate).await.unwrap_err();
    assert_eq!(error.error_code(), ErrorCode::ConfigInvalid);
    let current = cp.get_node(registered.node.id).await.unwrap().unwrap();
    assert_eq!(current.active_incarnation_id, Some(first.incarnation_id));
    assert_eq!(
        cp.list_node_incarnations(registered.node.id, 10)
            .await
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn remote_activation_rejects_corrupt_pointer_before_reservation_or_worker_mutation() {
    let (cp, _tmp) = cp_at(T0).await;
    let target = register_remote_node(&cp).await;
    let owner = cp
        .register_node(RegisterNodeInput {
            name: "other-remote-node".to_owned(),
            kind: NodeKind::Remote,
            heartbeat_ttl_seconds: 60,
            metadata: json!({}),
        })
        .await
        .unwrap();
    let corrupt_id = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    sqlx::query(
        "INSERT INTO node_incarnations \
         (incarnation_id, node_id, status, started_at, last_seen_at) \
         VALUES (?, ?, 'active', '1970-01-01T00:00:00Z', '1970-01-01T00:00:00Z')",
    )
    .bind(corrupt_id)
    .bind(i64::try_from(owner.node.id.0).unwrap())
    .execute(cp.pool_for_test())
    .await
    .unwrap();
    sqlx::query("UPDATE nodes SET active_incarnation_id = ? WHERE id = ?")
        .bind(corrupt_id)
        .bind(i64::try_from(target.node.id.0).unwrap())
        .execute(cp.pool_for_test())
        .await
        .unwrap();
    let workers_before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM workers")
        .fetch_one(cp.pool_for_test())
        .await
        .unwrap();

    let error = cp
        .remote_activate(activation_input(target.node.id, target.token))
        .await
        .unwrap_err();

    assert_eq!(error.error_code(), ErrorCode::DbUnreachable);
    let workers_after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM workers")
        .fetch_one(cp.pool_for_test())
        .await
        .unwrap();
    let replay_rows: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM remote_idempotency_keys WHERE node_id = ?")
            .bind(i64::try_from(target.node.id.0).unwrap())
            .fetch_one(cp.pool_for_test())
            .await
            .unwrap();
    assert_eq!(workers_after, workers_before);
    assert_eq!(replay_rows, 0);
}

#[tokio::test]
async fn remote_activation_deactivation_is_terminal_atomic_and_replayable() {
    let (cp, _tmp) = cp_at(T0).await;
    let registered = register_remote_node(&cp).await;
    let activated = cp
        .remote_activate(activation_input(
            registered.node.id,
            registered.token.clone(),
        ))
        .await
        .unwrap();
    let input = RemoteDeactivateInput {
        node_id: registered.node.id,
        token: registered.token,
        idempotency_key: "deactivate-1".to_owned(),
        request_hash: "deactivation-body-1".to_owned(),
        incarnation_id: activated.incarnation_id,
        reason: NodeIncarnationEndReason::GracefulShutdown,
    };

    let first = cp.remote_deactivate(input.clone()).await.unwrap();
    let replay = cp.remote_deactivate(input).await.unwrap();

    assert_eq!(first, replay);
    assert_eq!(first.status, NodeIncarnationStatus::Retired);
    assert_eq!(first.retired_worker_ids.len(), 2);
    assert_eq!(
        cp.get_node(registered.node.id)
            .await
            .unwrap()
            .unwrap()
            .active_incarnation_id,
        None
    );
}

#[tokio::test]
async fn remote_activation_pruning_removes_only_old_unreferenced_history_and_preserves_replay() {
    let (cp, clock, _tmp) = cp_with_manual_clock(T0).await;
    let eligible = register_remote_node(&cp).await;
    let activation = activation_input(eligible.node.id, eligible.token.clone());
    let activated = cp.remote_activate(activation.clone()).await.unwrap();
    cp.remote_deactivate(RemoteDeactivateInput {
        node_id: eligible.node.id,
        token: eligible.token,
        idempotency_key: "prune-deactivate".to_owned(),
        request_hash: "prune-deactivate-body".to_owned(),
        incarnation_id: activated.incarnation_id,
        reason: NodeIncarnationEndReason::GracefulShutdown,
    })
    .await
    .unwrap();
    let events_before: Vec<(i64, String, String)> =
        sqlx::query_as("SELECT event_id, kind, payload FROM events ORDER BY event_id")
            .fetch_all(cp.pool_for_test())
            .await
            .unwrap();
    clock.advance(Duration::seconds(61));

    cp.prune_node_activation_history(eligible.node.id, T0 + Duration::days(1))
        .await
        .unwrap();

    assert!(
        cp.list_node_incarnations(eligible.node.id, 10)
            .await
            .unwrap()
            .is_empty()
    );
    let worker_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM workers WHERE node_id = ?")
        .bind(i64::try_from(eligible.node.id.0).unwrap())
        .fetch_one(cp.pool_for_test())
        .await
        .unwrap();
    assert_eq!(worker_count, 0);
    assert_eq!(cp.remote_activate(activation).await.unwrap(), activated);
    assert_eq!(
        sqlx::query_as::<_, (i64, String, String)>(
            "SELECT event_id, kind, payload FROM events ORDER BY event_id",
        )
        .fetch_all(cp.pool_for_test())
        .await
        .unwrap(),
        events_before
    );
}

#[tokio::test]
async fn remote_activation_pruning_preserves_window_evidence_after_clock_moves_backward() {
    let (cp, clock, _tmp) = cp_with_manual_clock(T0).await;
    let registered = register_remote_node(&cp).await;
    let first = cp
        .remote_activate(activation_input_for(
            registered.node.id,
            registered.token.clone(),
            1,
        ))
        .await
        .unwrap();
    clock.set(T0 - Duration::minutes(2));
    cp.remote_activate(activation_input_for(
        registered.node.id,
        registered.token.clone(),
        2,
    ))
    .await
    .unwrap();
    clock.set(T0 + Duration::seconds(30));
    for ordinal in 3..=6 {
        cp.remote_activate(activation_input_for(
            registered.node.id,
            registered.token.clone(),
            ordinal,
        ))
        .await
        .unwrap();
    }

    cp.prune_node_activation_history(registered.node.id, T0 + Duration::days(1))
        .await
        .unwrap();

    let retained = cp
        .list_node_incarnations(registered.node.id, 10)
        .await
        .unwrap();
    assert!(retained.iter().any(|item| {
        item.id == first.incarnation_id
            && item.worker_count == u32::try_from(first.workers.len()).unwrap()
    }));
    let error = cp
        .remote_activate(activation_input_for(
            registered.node.id,
            registered.token,
            7,
        ))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("5 activations per 60 seconds"));
}

#[tokio::test]
async fn remote_activation_pruning_retains_live_and_referenced_records() {
    let (cp, clock, _tmp) = cp_with_manual_clock(T0).await;
    let referenced = register_remote_node(&cp).await;
    let activated = cp
        .remote_activate(activation_input(
            referenced.node.id,
            referenced.token.clone(),
        ))
        .await
        .unwrap();
    hold_hash_lease(&cp, &referenced, &activated).await;
    cp.remote_deactivate(RemoteDeactivateInput {
        node_id: referenced.node.id,
        token: referenced.token,
        idempotency_key: "referenced-deactivate".to_owned(),
        request_hash: "referenced-deactivate-body".to_owned(),
        incarnation_id: activated.incarnation_id,
        reason: NodeIncarnationEndReason::GracefulShutdown,
    })
    .await
    .unwrap();
    let live = cp
        .register_node(RegisterNodeInput {
            name: "live-prune-node".to_owned(),
            kind: NodeKind::Remote,
            heartbeat_ttl_seconds: 60,
            metadata: json!({}),
        })
        .await
        .unwrap();
    cp.remote_activate(activation_input_for(live.node.id, live.token, 9))
        .await
        .unwrap();
    clock.advance(Duration::seconds(61));

    cp.prune_node_activation_history(referenced.node.id, T0 + Duration::days(1))
        .await
        .unwrap();

    let retained = cp
        .list_node_incarnations(referenced.node.id, 10)
        .await
        .unwrap();
    assert_eq!(retained.len(), 1);
    assert_eq!(retained[0].worker_count, 1);
    assert_eq!(
        cp.list_node_incarnations(live.node.id, 10).await.unwrap()[0].status,
        NodeIncarnationStatus::Active
    );
}

#[tokio::test]
async fn remote_activation_pruning_rolls_back_all_candidates_on_delete_failure() {
    let (cp, clock, _tmp) = cp_with_manual_clock(T0).await;
    let registered = register_remote_node(&cp).await;
    cp.remote_activate(activation_input_for(
        registered.node.id,
        registered.token.clone(),
        1,
    ))
    .await
    .unwrap();
    let current = cp
        .remote_activate(activation_input_for(
            registered.node.id,
            registered.token.clone(),
            2,
        ))
        .await
        .unwrap();
    cp.remote_deactivate(RemoteDeactivateInput {
        node_id: registered.node.id,
        token: registered.token,
        idempotency_key: "rollback-deactivate".to_owned(),
        request_hash: "rollback-deactivate-body".to_owned(),
        incarnation_id: current.incarnation_id,
        reason: NodeIncarnationEndReason::GracefulShutdown,
    })
    .await
    .unwrap();
    sqlx::query(&format!(
        "CREATE TRIGGER reject_later_incarnation_delete \
         BEFORE DELETE ON node_incarnations \
         WHEN OLD.incarnation_id = '{}' \
         BEGIN SELECT RAISE(ABORT, 'injected prune failure'); END",
        current.incarnation_id
    ))
    .execute(cp.pool_for_test())
    .await
    .unwrap();
    let counts_before = activation_row_counts(&cp).await;
    clock.advance(Duration::seconds(61));

    let error = cp
        .prune_node_activation_history(registered.node.id, T0 + Duration::days(1))
        .await
        .unwrap_err();

    assert!(error.to_string().contains("injected prune failure"));
    assert_eq!(activation_row_counts(&cp).await, counts_before);
}

#[tokio::test]
async fn remote_activation_failed_deactivation_records_closed_status_reason_pair() {
    let (cp, _tmp) = cp_at(T0).await;
    let registered = register_remote_node(&cp).await;
    let activated = cp
        .remote_activate(activation_input(
            registered.node.id,
            registered.token.clone(),
        ))
        .await
        .unwrap();

    let outcome = cp
        .remote_deactivate(RemoteDeactivateInput {
            node_id: registered.node.id,
            token: registered.token,
            idempotency_key: "deactivate-failed".to_owned(),
            request_hash: "deactivation-failed-body".to_owned(),
            incarnation_id: activated.incarnation_id,
            reason: NodeIncarnationEndReason::ChildStartupFailed,
        })
        .await
        .unwrap();

    assert_eq!(outcome.status, NodeIncarnationStatus::Failed);
    assert_eq!(
        cp.list_node_incarnations(registered.node.id, 10)
            .await
            .unwrap()[0]
            .end_reason,
        Some(NodeIncarnationEndReason::ChildStartupFailed)
    );
}

#[tokio::test]
async fn remote_activation_stale_recovery_ends_incarnation_before_node_and_rejects_heartbeat() {
    let (cp, _tmp) = cp_at(T0).await;
    let registered = register_remote_node(&cp).await;
    let activated = cp
        .remote_activate(activation_input(
            registered.node.id,
            registered.token.clone(),
        ))
        .await
        .unwrap();

    cp.mark_stale_nodes(T0 + time::Duration::seconds(61))
        .await
        .unwrap();

    let incarnation = &cp
        .list_node_incarnations(registered.node.id, 10)
        .await
        .unwrap()[0];
    assert_eq!(incarnation.status, NodeIncarnationStatus::Failed);
    assert_eq!(
        incarnation.end_reason,
        Some(NodeIncarnationEndReason::HeartbeatExpired)
    );
    for worker in &activated.workers {
        assert_eq!(
            cp.workers()
                .get(worker.worker_id)
                .await
                .unwrap()
                .unwrap()
                .status,
            voom_core::WorkerStatus::Retired
        );
    }
    let ordered_kinds: Vec<String> = sqlx::query_scalar(
        "SELECT kind FROM events WHERE kind IN \
         ('worker.retired', 'node.incarnation_ended', 'node.marked_stale') ORDER BY event_id ASC",
    )
    .fetch_all(cp.pool_for_test())
    .await
    .unwrap();
    assert_eq!(
        ordered_kinds,
        vec![
            "worker.retired",
            "worker.retired",
            "node.incarnation_ended",
            "node.marked_stale"
        ]
    );
    let heartbeat = cp
        .remote_node_heartbeat(RemoteNodeHeartbeatInput {
            node_id: registered.node.id,
            token: registered.token,
            incarnation_id: activated.incarnation_id,
            idempotency_key: "stale-heartbeat".to_owned(),
            request_hash: "stale-heartbeat-body".to_owned(),
        })
        .await
        .unwrap_err();
    assert_eq!(heartbeat.error_code(), ErrorCode::Conflict);
}

#[tokio::test]
async fn remote_activation_logical_retire_ends_incarnation_before_node() {
    let (cp, _tmp) = cp_at(T0).await;
    let registered = register_remote_node(&cp).await;
    let activated = cp
        .remote_activate(activation_input(
            registered.node.id,
            registered.token.clone(),
        ))
        .await
        .unwrap();

    cp.retire_node(registered.node.id, activated.node_epoch, T0)
        .await
        .unwrap();

    let history = cp
        .list_node_incarnations(registered.node.id, 10)
        .await
        .unwrap();
    assert_eq!(history[0].status, NodeIncarnationStatus::Retired);
    assert_eq!(
        history[0].end_reason,
        Some(NodeIncarnationEndReason::LogicalNodeRetired)
    );
    let ordered_kinds: Vec<String> = sqlx::query_scalar(
        "SELECT kind FROM events WHERE kind IN \
         ('node.incarnation_ended', 'node.retired') ORDER BY event_id ASC",
    )
    .fetch_all(cp.pool_for_test())
    .await
    .unwrap();
    assert_eq!(
        ordered_kinds,
        vec!["node.incarnation_ended", "node.retired"]
    );
}

#[tokio::test]
async fn ending_incarnation_stales_scan_sessions_atomically() {
    let (cp, _tmp) = cp_at(T0).await;
    let registered = register_remote_node(&cp).await;
    let activated = cp
        .remote_activate(activation_input(
            registered.node.id,
            registered.token.clone(),
        ))
        .await
        .unwrap();
    let first =
        running_scan_session(&cp, &registered, activated.incarnation_id, "graceful-one").await;
    let second =
        running_scan_session(&cp, &registered, activated.incarnation_id, "graceful-two").await;
    let requested_root = create_scan_root(&cp, registered.node.id, "graceful-requested").await;
    let requested = cp.request_scan_session(requested_root, 300).await.unwrap();

    let outcome = cp
        .remote_deactivate(RemoteDeactivateInput {
            node_id: registered.node.id,
            token: registered.token,
            idempotency_key: "deactivate-scans".to_owned(),
            request_hash: "deactivate-scans-body".to_owned(),
            incarnation_id: activated.incarnation_id,
            reason: NodeIncarnationEndReason::GracefulShutdown,
        })
        .await
        .unwrap();

    assert_eq!(outcome.retired_worker_ids.len(), activated.workers.len());
    for id in [first.id, second.id] {
        assert_eq!(
            cp.scan_session(id).await.unwrap().status,
            ScanSessionStatus::Stale
        );
    }
    assert_eq!(
        cp.scan_session(requested.id).await.unwrap().status,
        ScanSessionStatus::Requested
    );
    let stale_subjects: Vec<i64> = sqlx::query_scalar(
        "SELECT subject_id FROM events WHERE kind = 'scan_session.stale' ORDER BY event_id ASC",
    )
    .fetch_all(cp.pool_for_test())
    .await
    .unwrap();
    assert_eq!(
        stale_subjects,
        vec![
            i64::try_from(first.id.0).unwrap(),
            i64::try_from(second.id.0).unwrap()
        ]
    );
    let ordered_kinds: Vec<String> = sqlx::query_scalar(
        "SELECT kind FROM events WHERE kind IN \
         ('scan_session.stale', 'node.incarnation_ended') ORDER BY event_id ASC",
    )
    .fetch_all(cp.pool_for_test())
    .await
    .unwrap();
    assert_eq!(
        ordered_kinds,
        vec![
            "scan_session.stale",
            "scan_session.stale",
            "node.incarnation_ended"
        ]
    );
}

#[tokio::test]
async fn failed_deactivation_reasons_stale_running_scan_sessions() {
    for (ordinal, reason) in [
        NodeIncarnationEndReason::ChildStartupFailed,
        NodeIncarnationEndReason::ChildRestartExhausted,
    ]
    .into_iter()
    .enumerate()
    {
        let (cp, _tmp) = cp_at(T0).await;
        let registered = register_remote_node(&cp).await;
        let activated = cp
            .remote_activate(activation_input(
                registered.node.id,
                registered.token.clone(),
            ))
            .await
            .unwrap();
        let session = running_scan_session(
            &cp,
            &registered,
            activated.incarnation_id,
            &format!("failed-{ordinal}"),
        )
        .await;

        cp.remote_deactivate(RemoteDeactivateInput {
            node_id: registered.node.id,
            token: registered.token,
            idempotency_key: format!("deactivate-failed-{ordinal}"),
            request_hash: format!("deactivate-failed-body-{ordinal}"),
            incarnation_id: activated.incarnation_id,
            reason,
        })
        .await
        .unwrap();

        let session = cp.scan_session(session.id).await.unwrap();
        assert_eq!(session.status, ScanSessionStatus::Stale);
        assert!(
            session
                .terminal_reason
                .unwrap()
                .as_str()
                .contains(reason.as_str())
        );
    }
}

#[tokio::test]
async fn supersession_and_logical_retirement_stale_running_scan_sessions() {
    let (supersede_cp, _tmp) = cp_at(T0).await;
    let superseded_node = register_remote_node(&supersede_cp).await;
    let first = supersede_cp
        .remote_activate(activation_input(
            superseded_node.node.id,
            superseded_node.token.clone(),
        ))
        .await
        .unwrap();
    let superseded_scan = running_scan_session(
        &supersede_cp,
        &superseded_node,
        first.incarnation_id,
        "superseded",
    )
    .await;
    supersede_cp
        .remote_activate(activation_input_for(
            superseded_node.node.id,
            superseded_node.token,
            2,
        ))
        .await
        .unwrap();
    assert_eq!(
        supersede_cp
            .scan_session(superseded_scan.id)
            .await
            .unwrap()
            .status,
        ScanSessionStatus::Stale
    );

    let (retire_cp, _tmp) = cp_at(T0).await;
    let retired_node = register_remote_node(&retire_cp).await;
    let activated = retire_cp
        .remote_activate(activation_input(
            retired_node.node.id,
            retired_node.token.clone(),
        ))
        .await
        .unwrap();
    let retired_scan = running_scan_session(
        &retire_cp,
        &retired_node,
        activated.incarnation_id,
        "retired",
    )
    .await;
    retire_cp
        .retire_node(retired_node.node.id, activated.node_epoch, T0)
        .await
        .unwrap();
    assert_eq!(
        retire_cp
            .scan_session(retired_scan.id)
            .await
            .unwrap()
            .status,
        ScanSessionStatus::Stale
    );
}

#[tokio::test]
async fn stale_scan_event_failure_rolls_back_incarnation_and_worker_retirement() {
    let (cp, _tmp) = cp_at(T0).await;
    let registered = register_remote_node(&cp).await;
    let activated = cp
        .remote_activate(activation_input(
            registered.node.id,
            registered.token.clone(),
        ))
        .await
        .unwrap();
    let session =
        running_scan_session(&cp, &registered, activated.incarnation_id, "rollback").await;
    let statuses_before = worker_statuses(&cp, &activated).await;
    sqlx::query(
        "CREATE TRIGGER reject_scan_stale_event BEFORE INSERT ON events \
         WHEN NEW.kind = 'scan_session.stale' \
         BEGIN SELECT RAISE(ABORT, 'forced stale event failure'); END",
    )
    .execute(cp.pool_for_test())
    .await
    .unwrap();

    let error = cp
        .remote_deactivate(RemoteDeactivateInput {
            node_id: registered.node.id,
            token: registered.token,
            idempotency_key: "deactivate-rollback".to_owned(),
            request_hash: "deactivate-rollback-body".to_owned(),
            incarnation_id: activated.incarnation_id,
            reason: NodeIncarnationEndReason::GracefulShutdown,
        })
        .await
        .unwrap_err();

    assert!(error.to_string().contains("forced stale event failure"));
    assert_eq!(
        cp.scan_session(session.id).await.unwrap().status,
        ScanSessionStatus::Running
    );
    assert_eq!(worker_statuses(&cp, &activated).await, statuses_before);
    assert_eq!(
        cp.list_node_incarnations(registered.node.id, 10)
            .await
            .unwrap()[0]
            .status,
        NodeIncarnationStatus::Active
    );
    assert_eq!(
        cp.get_node(registered.node.id)
            .await
            .unwrap()
            .unwrap()
            .active_incarnation_id,
        Some(activated.incarnation_id)
    );
}

fn activation_input(node_id: voom_core::NodeId, token: SecretString) -> RemoteActivateInput {
    RemoteActivateInput {
        node_id,
        token,
        idempotency_key: "activate-1".to_owned(),
        request_hash: "activation-body-1".to_owned(),
        incarnation_id: "0123456789abcdef0123456789abcdef".parse().unwrap(),
        workers: vec![
            RemoteWorkerDeclaration {
                logical_name: "probe".to_owned(),
                operations: vec![OperationKind::ProbeFile],
                artifact_access: vec![ArtifactAccessMode::ControlPlanePlaceholder],
                accelerator: None,
                max_parallel: 2,
            },
            RemoteWorkerDeclaration {
                logical_name: "hash".to_owned(),
                operations: vec![OperationKind::HashFile],
                artifact_access: vec![ArtifactAccessMode::SharedMount],
                accelerator: None,
                max_parallel: 1,
            },
        ],
    }
}

fn activation_input_for(
    node_id: voom_core::NodeId,
    token: SecretString,
    ordinal: u8,
) -> RemoteActivateInput {
    let mut input = activation_input(node_id, token);
    input.idempotency_key = format!("activate-{ordinal}");
    input.request_hash = format!("activation-body-{ordinal}");
    input.incarnation_id = format!("{ordinal:032x}").parse().unwrap();
    input
}

async fn activation_row_counts(cp: &crate::ControlPlane) -> Vec<i64> {
    let mut counts = Vec::new();
    for table in [
        "node_incarnations",
        "workers",
        "worker_capabilities",
        "worker_grants",
        "remote_idempotency_keys",
        "events",
    ] {
        let query = format!("SELECT COUNT(*) FROM {table}");
        counts.push(
            sqlx::query_scalar(&query)
                .fetch_one(cp.pool_for_test())
                .await
                .unwrap(),
        );
    }
    counts
}

async fn register_remote_node(cp: &crate::ControlPlane) -> crate::workers::RegisteredNode {
    cp.register_node(RegisterNodeInput {
        name: "remote-node".to_owned(),
        kind: NodeKind::Remote,
        heartbeat_ttl_seconds: 60,
        metadata: json!({}),
    })
    .await
    .unwrap()
}

async fn create_scan_root(
    cp: &crate::ControlPlane,
    owner_node_id: voom_core::NodeId,
    suffix: &str,
) -> StorageRootId {
    let library = cp
        .create_library(NewLibrary {
            slug: format!("activation-scan-{suffix}"),
            display_name: format!("Activation scan {suffix}"),
            media_kind: LibraryMediaKind::Movie,
            description: None,
            enabled: true,
        })
        .await
        .unwrap();
    let root = cp
        .create_library_root(scan_root_input(library.id, owner_node_id, suffix))
        .await
        .unwrap();
    cp.activate_library_root(root.id, format!("activation-scan-{suffix}"))
        .await
        .unwrap();
    root.id
}

fn scan_root_input(
    library_id: LibraryId,
    owner_node_id: voom_core::NodeId,
    suffix: &str,
) -> NewLibraryRoot {
    NewLibraryRoot {
        library_id,
        owner_node_id,
        provider_kind: StorageProviderKind::LocalFilesystem,
        provider_locator: ProviderLocator::new(format!("/activation-scan/{suffix}")).unwrap(),
        display_locator: format!("/activation-scan/{suffix}"),
        include_globs: Vec::new(),
        exclude_globs: Vec::new(),
        extension_allowlist: Vec::new(),
        scan_mode: LibraryScanMode::ManualRecursive,
        symlink_policy: SymlinkPolicy::Reject,
        hidden_file_policy: HiddenFilePolicy::Ignore,
        max_depth: None,
        stability_seconds: 0,
        debounce_seconds: 0,
        default_output_root_id: None,
        default_staging_root_id: None,
        default_backup_root_id: None,
        enabled: true,
    }
}

async fn running_scan_session(
    cp: &crate::ControlPlane,
    registered: &crate::workers::RegisteredNode,
    incarnation_id: voom_core::NodeIncarnationId,
    suffix: &str,
) -> crate::scan::ScanSession {
    let root_id = create_scan_root(cp, registered.node.id, suffix).await;
    let requested = cp.request_scan_session(root_id, 300).await.unwrap();
    cp.start_scan_session(RemoteScanStartInput {
        node_id: registered.node.id,
        scan_session_id: requested.id,
        incarnation_id,
        token: registered.token.clone(),
        idempotency_key: format!("start-{suffix}"),
        request_hash: format!("start-{suffix}-body"),
    })
    .await
    .unwrap();
    cp.scan_session(requested.id).await.unwrap()
}

async fn worker_statuses(
    cp: &crate::ControlPlane,
    activation: &super::super::RemoteActivateOutcome,
) -> Vec<voom_core::WorkerStatus> {
    let mut statuses = Vec::new();
    for worker in &activation.workers {
        statuses.push(
            cp.workers()
                .get(worker.worker_id)
                .await
                .unwrap()
                .unwrap()
                .status,
        );
    }
    statuses
}

async fn hold_hash_lease(
    cp: &crate::ControlPlane,
    registered: &crate::workers::RegisteredNode,
    activated: &super::super::RemoteActivateOutcome,
) -> voom_core::LeaseId {
    cp.remote_worker_readiness(RemoteWorkerReadinessInput {
        node_id: registered.node.id,
        token: registered.token.clone(),
        incarnation_id: activated.incarnation_id,
        worker_id: activated.workers[1].worker_id,
        readiness: WorkerReadiness::Ready,
    })
    .await
    .unwrap();
    let ticket = cp
        .create_ticket(NewTicket {
            job_id: None,
            kind: OperationKind::HashFile.into(),
            priority: 0,
            payload: json!({
                "dispatch": {"kind": "hash_file"},
                "artifact_access": {
                    "inputs": ["handle:input:test"],
                    "outputs": ["handle:output:test"]
                }
            }),
            max_attempts: 2,
            created_at: T0,
        })
        .await
        .unwrap();
    cp.mark_ready_if_unblocked(ticket.id, T0).await.unwrap();
    let outcome = cp
        .remote_acquire(RemoteAcquireInput {
            node_id: registered.node.id,
            token: registered.token.clone(),
            incarnation_id: activated.incarnation_id,
            worker_id: activated.workers[1].worker_id,
            idempotency_key: "hold-before-supersede".to_owned(),
            request_hash: "hold-before-supersede-body".to_owned(),
            lease_ttl_seconds: 60,
        })
        .await
        .unwrap();
    let RemoteAcquireOutcome::Leased(dispatch) = outcome else {
        panic!("expected activation worker to acquire the ready hash ticket");
    };
    dispatch.lease_id
}

async fn cp_at(now: OffsetDateTime) -> (crate::ControlPlane, voom_test_support::TempDatabase) {
    let tmp = voom_test_support::TempDatabase::new().unwrap();
    let url = format!("sqlite://{}", tmp.path().display());
    voom_store::init(&url).await.unwrap();
    let pool = voom_store::connect(&url).await.unwrap();
    let cp = crate::ControlPlane::open_with_pool_and_rng(
        pool,
        std::sync::Arc::new(FrozenClock::new(now)),
        std::sync::Arc::new(std::sync::Mutex::new(
            voom_core::rng_test_support::FrozenRng::new(0x0808_0808),
        )),
    )
    .await
    .unwrap();
    (cp, tmp)
}

async fn cp_with_manual_clock(
    now: OffsetDateTime,
) -> (
    crate::ControlPlane,
    std::sync::Arc<ManualClock>,
    voom_test_support::TempDatabase,
) {
    let tmp = voom_test_support::TempDatabase::new().unwrap();
    let url = format!("sqlite://{}", tmp.path().display());
    voom_store::init(&url).await.unwrap();
    let pool = voom_store::connect(&url).await.unwrap();
    let clock = std::sync::Arc::new(ManualClock::new(now));
    let cp = crate::ControlPlane::open_with_pool_and_rng(
        pool,
        clock.clone(),
        std::sync::Arc::new(std::sync::Mutex::new(
            voom_core::rng_test_support::FrozenRng::new(0x0808_0808),
        )),
    )
    .await
    .unwrap();
    (cp, clock, tmp)
}
