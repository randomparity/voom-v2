use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;
use secrecy::SecretString;
use serde_json::{Value, json};
use time::OffsetDateTime;
use tokio::io::{AsyncWriteExt, DuplexStream};
use voom_core::rng_test_support::FrozenRng;
use voom_core::{
    ErrorCode, FailureClass, FileVersionId, JobId, LeaseId, MediaSnapshotId, SystemClock, WorkerId,
};
use voom_scheduler::SingleWorkerPerKindSelector;
use voom_store::repo::identity::{DiscoveredFile, FileLocationKind, IngestOutcome};
use voom_store::repo::jobs::NewJob;
use voom_store::repo::leases::NewLease;
use voom_store::repo::tickets::NewTicket;
use voom_store::repo::workers::{NewCapability, NewGrant, NewWorker, WorkerKind};
use voom_worker_protocol::{
    ClientHandle, DispatchStream, HandshakeResponse, NdjsonReader, OperationKind, OperationRequest,
    OperationResponse, PercentBps, ProgressFrame, ProtocolError, RemuxObservedFacts, RemuxRequest,
    RemuxResult, RemuxStatus, TranscodeVideoRequest, WorkerCredentials,
};

use crate::workflow::executor::{
    WorkflowExecutor, WorkflowExecutorOptions, WorkflowRunSummary, is_synthetic_root_ticket,
};
use crate::workflow::model::{ConcurrencyPolicy, OperationNode, WorkflowNode, WorkflowPlan};
use crate::workflow::runtime::WorkerRuntimeRegistry;
use crate::workflow::ticket_payload::WorkflowTicketPayload;
use crate::workflow::timing::EffectiveTiming;
use voom_plan::TargetRef;

const T0: OffsetDateTime = OffsetDateTime::UNIX_EPOCH;

#[tokio::test]
async fn executor_never_exceeds_max_in_flight_dispatches() {
    let fixture = ExecutorFixture::with_ready_tickets(6).await;
    let summary = fixture
        .run_with_policy(ConcurrencyPolicy {
            max_in_flight_dispatches: 2,
        })
        .await
        .unwrap();

    assert!(summary.peak_active_workflow_leases <= 2);
    assert_eq!(summary.dispatch_count, 6);
    assert_eq!(summary.operation_count(OperationKind::HashFile), 6);
}

#[tokio::test]
async fn local_reservations_prevent_worker_capacity_overrun() {
    let fixture = ExecutorFixture::single_worker_max_parallel(1).await;
    let worker_id = fixture.worker_id();
    let summary = fixture
        .run_with_policy(ConcurrencyPolicy {
            max_in_flight_dispatches: 4,
        })
        .await
        .unwrap();

    assert_eq!(summary.max_active_for_worker(worker_id), 1);
    assert_eq!(summary.dispatch_count, 4);
}

#[tokio::test]
async fn no_eligible_worker_is_recorded_before_lease_dispatch() {
    let fixture = ExecutorFixture::without_workers(1).await;
    let err = fixture.run().await.unwrap_err();

    assert_eq!(err.source.error_code(), ErrorCode::NoEligibleWorker);
    assert_eq!(err.summary.dispatch_count, 0);
    assert_eq!(err.summary.peak_active_workflow_leases, 0);
    assert_eq!(err.summary.failure_count, 1);
    assert_eq!(fixture.lease_count().await, 0);
}

#[tokio::test]
async fn ambiguous_worker_selection_is_recorded_before_lease_dispatch() {
    let fixture = ExecutorFixture::ambiguous_workers().await;
    let err = fixture.run().await.unwrap_err();

    assert_eq!(err.source.error_code(), ErrorCode::AmbiguousWorkerSelection);
    assert_eq!(err.summary.dispatch_count, 0);
    assert_eq!(err.summary.peak_active_workflow_leases, 0);
    assert_eq!(err.summary.failure_count, 1);
    assert_eq!(fixture.lease_count().await, 0);
}

#[tokio::test]
async fn malformed_result_frame_fails_terminally() {
    let fixture = ExecutorFixture::single_worker_with_behavior(FakeBehavior::MalformedFrame).await;
    let err = fixture.run().await.unwrap_err();

    assert_eq!(err.source.error_code(), ErrorCode::MalformedWorkerResult);
    assert_eq!(err.summary.dispatch_count, 1);
    assert_eq!(err.summary.failure_count, 1);
}

#[tokio::test]
async fn progress_timeout_fails_terminally() {
    let fixture = ExecutorFixture::single_worker_with_behavior(FakeBehavior::Hang).await;
    let err = fixture
        .run_with_options(timeout_options())
        .await
        .unwrap_err();

    assert_eq!(err.source.error_code(), ErrorCode::WorkerTimeout);
    assert_eq!(err.summary.dispatch_count, 1);
    assert_eq!(err.summary.failure_count, 1);
}

#[tokio::test]
async fn retriable_dispatch_failure_retries_before_terminal_failure() {
    let fixture = ExecutorFixture::single_worker_with_behavior(FakeBehavior::Crash).await;
    let mut options = WorkflowExecutorOptions::for_tests();
    options.max_attempts = 2;

    let err = fixture.run_with_options(options).await.unwrap_err();

    assert_eq!(err.source.error_code(), ErrorCode::WorkerCrash);
    assert_eq!(err.summary.dispatch_count, 2);
    assert_eq!(err.summary.retry_count, 1);
    assert_eq!(err.summary.failure_count, 1);
}

#[tokio::test]
async fn retriable_pre_lease_failure_retries_before_terminal_failure() {
    let fixture = ExecutorFixture::without_workers(1).await;
    let mut options = WorkflowExecutorOptions::for_tests();
    options.max_attempts = 2;

    let err = fixture.run_with_options(options).await.unwrap_err();

    assert_eq!(err.source.error_code(), ErrorCode::NoEligibleWorker);
    assert_eq!(err.summary.dispatch_count, 0);
    assert_eq!(err.summary.retry_count, 1);
    assert_eq!(err.summary.failure_count, 1);
    assert_eq!(fixture.lease_count().await, 0);
}

#[tokio::test]
async fn heartbeat_timeout_fails_terminally() {
    let fixture = ExecutorFixture::single_worker_with_behavior(FakeBehavior::Hang).await;
    let mut options = timeout_options();
    options.progress_idle_timeout = Duration::from_millis(250);
    options.heartbeat_timeout = Duration::from_millis(20);
    options.chaos.disable_heartbeat_ticks = true;
    let err = fixture.run_with_options(options).await.unwrap_err();

    assert_eq!(err.source.error_code(), ErrorCode::WorkerTimeout);
    assert_eq!(err.summary.dispatch_count, 1);
    assert_eq!(err.summary.failure_count, 1);
}

#[tokio::test]
async fn heartbeat_timeout_wins_when_watchdog_deadlines_tie() {
    let fixture = ExecutorFixture::single_worker_with_behavior(FakeBehavior::Hang).await;
    let mut options = timeout_options();
    options.progress_idle_timeout = Duration::from_millis(20);
    options.heartbeat_timeout = Duration::from_millis(20);
    options.heartbeat_interval = Duration::from_secs(1);
    options.chaos.disable_heartbeat_ticks = true;

    let err = fixture.run_with_options(options).await.unwrap_err();

    assert_eq!(err.source.error_code(), ErrorCode::WorkerTimeout);
    let failed_class = fixture.first_ticket_failed_class().await;
    assert_eq!(failed_class, "worker_timeout");
}

#[tokio::test]
async fn heartbeat_watchdog_is_not_starved_by_progress_frames() {
    let fixture = ExecutorFixture::single_worker_with_behavior(FakeBehavior::ProgressFlood).await;
    let mut options = timeout_options();
    options.progress_idle_timeout = Duration::from_secs(1);
    options.heartbeat_timeout = Duration::from_millis(20);
    options.heartbeat_interval = Duration::from_secs(1);
    options.chaos.disable_heartbeat_ticks = true;

    let err = fixture.run_with_options(options).await.unwrap_err();

    assert_eq!(err.source.error_code(), ErrorCode::WorkerTimeout);
    let failed_class = fixture.first_ticket_failed_class().await;
    assert_eq!(failed_class, "worker_timeout");
}

#[tokio::test]
async fn worker_crash_fails_terminally() {
    let fixture = ExecutorFixture::single_worker_with_behavior(FakeBehavior::Crash).await;
    let err = fixture.run().await.unwrap_err();

    assert_eq!(err.source.error_code(), ErrorCode::WorkerCrash);
    assert_eq!(err.summary.dispatch_count, 1);
    assert_eq!(err.summary.failure_count, 1);
}

#[tokio::test]
async fn missing_runtime_fails_before_lease_acquire() {
    let mut fixture = ExecutorFixture::without_workers(1).await;
    fixture
        .register_worker_without_runtime("hash-worker", OperationKind::HashFile, 1)
        .await;

    let err = fixture.run().await.unwrap_err();

    assert_eq!(err.source.error_code(), ErrorCode::ConfigInvalid);
    assert_eq!(err.summary.dispatch_count, 0);
    assert_eq!(fixture.lease_count().await, 0);
}

#[tokio::test]
async fn dispatch_setup_error_fails_acquired_lease() {
    let fixture = ExecutorFixture::single_worker_with_behavior(FakeBehavior::DispatchError).await;

    let err = fixture.run().await.unwrap_err();

    assert_eq!(err.source.error_code(), ErrorCode::WorkerCrash);
    assert_eq!(err.summary.dispatch_count, 1);
    assert_eq!(err.summary.failure_count, 1);
    assert_eq!(fixture.held_lease_count().await, 0);
}

#[tokio::test]
async fn ready_lookup_is_scoped_to_active_workflow_job() {
    let mut fixture = ExecutorFixture::with_ready_tickets(1).await;
    fixture.seed_other_job_ready_ticket(100).await;
    let mut options = WorkflowExecutorOptions::for_tests();
    options.ready_batch_size = 1;
    options.progress_idle_timeout = Duration::from_secs(5);
    options.heartbeat_timeout = Duration::from_secs(5);

    let summary = fixture.run_with_options(options).await.unwrap();

    assert_eq!(summary.dispatch_count, 1);
    assert_eq!(fixture.other_job_ready_count().await, 1);
}

#[tokio::test]
async fn policy_transcode_root_ticket_carries_source_ids_and_operation_payload() {
    let mut fixture = ExecutorFixture::without_workers(0).await;
    fixture.plan = policy_transcode_plan(TargetRef::FileVersion {
        id: FileVersionId(11),
    });

    let err = fixture.run().await.unwrap_err();

    assert_eq!(err.source.error_code(), ErrorCode::NoEligibleWorker);
    let ticket_payload = fixture.first_ticket_payload().await;
    let workflow_payload = WorkflowTicketPayload::parse_ticket(
        "synthetic.workflow.operation.transcode_video",
        ticket_payload,
    )
    .unwrap();
    assert_eq!(workflow_payload.operation, OperationKind::TranscodeVideo);
    assert_eq!(
        workflow_payload.rendered_payload["operation"],
        "transcode_video"
    );
    assert_eq!(
        workflow_payload.rendered_payload["source_file_version_id"],
        11
    );
    assert_eq!(workflow_payload.rendered_payload["target_codec"], "hevc");
    assert_eq!(workflow_payload.rendered_payload["container"], "mkv");
    assert_eq!(workflow_payload.rendered_payload["profile"], "default-hevc");
}

#[tokio::test]
async fn policy_transcode_file_location_target_carries_source_version_and_location() {
    let mut fixture = ExecutorFixture::without_workers(0).await;
    let (source_file_version_id, source_location_id) = fixture.seed_local_source().await;
    fixture.plan = policy_transcode_plan(TargetRef::FileLocation {
        id: source_location_id,
    });

    let err = fixture.run().await.unwrap_err();

    assert_eq!(err.source.error_code(), ErrorCode::NoEligibleWorker);
    let ticket_payload = fixture.first_ticket_payload().await;
    let workflow_payload = WorkflowTicketPayload::parse_ticket(
        "synthetic.workflow.operation.transcode_video",
        ticket_payload,
    )
    .unwrap();
    assert_eq!(
        workflow_payload.rendered_payload["source_file_version_id"],
        source_file_version_id.0
    );
    assert_eq!(
        workflow_payload.rendered_payload["source_location_id"],
        source_location_id.0
    );
}

#[tokio::test]
async fn policy_transcode_ticket_carries_staging_and_target_roots_from_options() {
    let mut fixture = ExecutorFixture::without_workers(0).await;
    fixture.plan = policy_transcode_plan(TargetRef::FileVersion {
        id: FileVersionId(11),
    });
    let mut options = WorkflowExecutorOptions::for_tests();
    options.transcode_staging_root = PathBuf::from("/tmp/voom-stage");
    options.transcode_target_dir = PathBuf::from("/media/normalized");

    let err = fixture.run_with_options(options).await.unwrap_err();

    assert_eq!(err.source.error_code(), ErrorCode::NoEligibleWorker);
    let ticket_payload = fixture.first_ticket_payload().await;
    let workflow_payload = WorkflowTicketPayload::parse_ticket(
        "synthetic.workflow.operation.transcode_video",
        ticket_payload,
    )
    .unwrap();
    assert_eq!(
        workflow_payload.rendered_payload["staging_root"],
        "/tmp/voom-stage"
    );
    assert_eq!(
        workflow_payload.rendered_payload["target_dir"],
        "/media/normalized"
    );
}

#[tokio::test]
async fn policy_remux_root_ticket_carries_source_ids_and_operation_payload() {
    let mut fixture = ExecutorFixture::without_workers(0).await;
    fixture.plan = policy_remux_plan(TargetRef::FileVersion {
        id: FileVersionId(11),
    });

    let err = fixture.run().await.unwrap_err();

    assert_eq!(err.source.error_code(), ErrorCode::NoEligibleWorker);
    let ticket_payload = fixture.first_ticket_payload().await;
    let workflow_payload =
        WorkflowTicketPayload::parse_ticket("synthetic.workflow.operation.remux", ticket_payload)
            .unwrap();
    assert_eq!(workflow_payload.operation, OperationKind::Remux);
    assert_eq!(workflow_payload.rendered_payload["operation"], "remux");
    assert_eq!(
        workflow_payload.rendered_payload["source_file_version_id"],
        11
    );
    assert_eq!(workflow_payload.rendered_payload["remux"]["type"], "remux");
    assert_eq!(
        workflow_payload.rendered_payload["remux"]["container"],
        "mkv"
    );
    assert_eq!(
        workflow_payload.rendered_payload["remux"]["track_order"],
        json!(["video", "audio", "subtitle"])
    );
}

#[tokio::test]
async fn policy_remux_file_location_target_carries_source_version_and_location() {
    let mut fixture = ExecutorFixture::without_workers(0).await;
    let (source_file_version_id, source_location_id) = fixture.seed_local_source().await;
    fixture.plan = policy_remux_plan(TargetRef::FileLocation {
        id: source_location_id,
    });

    let err = fixture.run().await.unwrap_err();

    assert_eq!(err.source.error_code(), ErrorCode::NoEligibleWorker);
    let ticket_payload = fixture.first_ticket_payload().await;
    let workflow_payload =
        WorkflowTicketPayload::parse_ticket("synthetic.workflow.operation.remux", ticket_payload)
            .unwrap();
    assert_eq!(
        workflow_payload.rendered_payload["source_file_version_id"],
        source_file_version_id.0
    );
    assert_eq!(
        workflow_payload.rendered_payload["source_location_id"],
        source_location_id.0
    );
}

#[tokio::test]
async fn policy_remux_ticket_carries_staging_and_target_roots_from_options() {
    let mut fixture = ExecutorFixture::without_workers(0).await;
    fixture.plan = policy_remux_plan(TargetRef::FileVersion {
        id: FileVersionId(11),
    });
    let mut options = WorkflowExecutorOptions::for_tests();
    options.remux_staging_root = PathBuf::from("/tmp/voom-remux-stage");
    options.remux_target_dir = PathBuf::from("/media/remuxed");

    let err = fixture.run_with_options(options).await.unwrap_err();

    assert_eq!(err.source.error_code(), ErrorCode::NoEligibleWorker);
    let ticket_payload = fixture.first_ticket_payload().await;
    let workflow_payload =
        WorkflowTicketPayload::parse_ticket("synthetic.workflow.operation.remux", ticket_payload)
            .unwrap();
    assert_eq!(
        workflow_payload.rendered_payload["staging_root"],
        "/tmp/voom-remux-stage"
    );
    assert_eq!(
        workflow_payload.rendered_payload["target_dir"],
        "/media/remuxed"
    );
}

#[tokio::test]
async fn policy_remux_ticket_uses_default_remux_roots() {
    let mut fixture = ExecutorFixture::without_workers(0).await;
    fixture.plan = policy_remux_plan(TargetRef::FileVersion {
        id: FileVersionId(11),
    });

    let err = fixture.run().await.unwrap_err();

    assert_eq!(err.source.error_code(), ErrorCode::NoEligibleWorker);
    let ticket_payload = fixture.first_ticket_payload().await;
    let workflow_payload =
        WorkflowTicketPayload::parse_ticket("synthetic.workflow.operation.remux", ticket_payload)
            .unwrap();
    assert_eq!(
        workflow_payload.rendered_payload["staging_root"],
        "/tmp/voom-test/remux/staging"
    );
    assert_eq!(
        workflow_payload.rendered_payload["target_dir"],
        "/tmp/voom-test/remux/output"
    );
}

#[tokio::test]
async fn malformed_policy_remux_payload_is_rejected_before_default_fallback() {
    let mut fixture = ExecutorFixture::without_workers(0).await;
    fixture.plan = policy_remux_plan_with_payload(
        TargetRef::FileVersion {
            id: FileVersionId(11),
        },
        json!({"type": "remux"}),
    );

    let err = fixture.run().await.unwrap_err();

    assert_eq!(err.source.error_code(), ErrorCode::ConfigInvalid);
    assert!(err.source.to_string().contains("missing `container`"));
    assert_eq!(fixture.ticket_count().await, 0);
}

#[tokio::test]
async fn policy_remux_without_snapshot_pin_is_rejected_before_ticket_creation() {
    let mut fixture = ExecutorFixture::without_workers(0).await;
    fixture.plan = policy_remux_plan_with_payload(
        TargetRef::FileVersion {
            id: FileVersionId(11),
        },
        json!({
            "type": "remux",
            "container": "mkv",
            "track_actions": [],
            "track_order": ["video", "audio", "subtitle"],
            "defaults": [],
        }),
    );

    let err = fixture.run().await.unwrap_err();

    assert_eq!(err.source.error_code(), ErrorCode::ConfigInvalid);
    assert!(err.source.to_string().contains("source_media_snapshot_id"));
    assert_eq!(fixture.ticket_count().await, 0);
}

#[tokio::test]
async fn non_policy_remux_root_ticket_uses_default_payload() {
    let mut fixture = ExecutorFixture::without_workers(0).await;
    fixture.plan = non_policy_remux_plan();

    let err = fixture.run().await.unwrap_err();

    assert_eq!(err.source.error_code(), ErrorCode::NoEligibleWorker);
    let ticket_payload = fixture.first_ticket_payload().await;
    let workflow_payload =
        WorkflowTicketPayload::parse_ticket("synthetic.workflow.operation.remux", ticket_payload)
            .unwrap();
    assert_eq!(workflow_payload.rendered_payload["operation"], "remux");
    assert_eq!(
        workflow_payload.rendered_payload["path"],
        "/library/root.mkv"
    );
    assert_eq!(workflow_payload.rendered_payload["container"], "mkv");
    assert!(workflow_payload.rendered_payload.get("remux").is_none());
}

#[tokio::test]
async fn unsupported_policy_remux_target_is_rejected_before_default_fallback() {
    let mut fixture = ExecutorFixture::without_workers(0).await;
    fixture.plan = policy_remux_plan(TargetRef::Synthetic {
        key: "variant-1".to_owned(),
        kind: voom_policy::TargetKind::MediaVariant,
    });

    let err = fixture.run().await.unwrap_err();

    assert_eq!(err.source.error_code(), ErrorCode::ConfigInvalid);
    assert!(
        err.source
            .to_string()
            .contains("remux requires file_version or file_location target")
    );
    assert_eq!(fixture.ticket_count().await, 0);
}

#[tokio::test]
async fn policy_remux_ticket_runs_real_remux_path() {
    let mut fixture = ExecutorFixture::without_workers(0).await;
    let dir = tempfile::tempdir().unwrap();
    let source_path = dir.path().join("Movie.mkv");
    let (source_file_version_id, _source_location_id) = fixture
        .seed_local_source_at_path(&source_path, b"movie-bytes")
        .await;
    let snapshot_id = fixture.record_source_snapshot(source_file_version_id).await;
    fixture.plan = policy_remux_plan_for_snapshot(
        TargetRef::FileVersion {
            id: source_file_version_id,
        },
        snapshot_id,
    );
    fixture
        .register_worker(
            "remux-worker",
            OperationKind::Remux,
            1,
            FakeBehavior::RequireRemuxProtocolPayloadThenMkvtoolnixUnavailable,
        )
        .await;
    let mut options = WorkflowExecutorOptions::for_tests();
    options.remux_staging_root = dir.path().join("stage");
    options.remux_target_dir = dir.path().join("out");

    let err = fixture.run_with_options(options).await.unwrap_err();

    assert_eq!(err.summary.dispatch_count, 1);
    assert_eq!(err.summary.failure_count, 1);
    assert_eq!(err.source.error_code(), ErrorCode::WorkerCrash);
    assert!(
        err.source.to_string().contains("mkvtoolnix"),
        "policy remux with source ids must use remux protocol dispatch, got: {}",
        err.source
    );
}

#[tokio::test]
async fn policy_remux_ticket_with_source_ids_requires_registered_runtime() {
    let mut fixture = ExecutorFixture::without_workers(0).await;
    let dir = tempfile::tempdir().unwrap();
    let source_path = dir.path().join("Movie.mkv");
    let (source_file_version_id, _source_location_id) = fixture
        .seed_local_source_at_path(&source_path, b"movie-bytes")
        .await;
    let snapshot_id = fixture.record_source_snapshot(source_file_version_id).await;
    fixture.plan = policy_remux_plan_for_snapshot(
        TargetRef::FileVersion {
            id: source_file_version_id,
        },
        snapshot_id,
    );
    fixture
        .register_worker_without_runtime("remux-worker", OperationKind::Remux, 1)
        .await;

    let err = fixture.run().await.unwrap_err();

    assert_eq!(err.source.error_code(), ErrorCode::ConfigInvalid);
    assert!(
        err.source
            .to_string()
            .contains("missing runtime for worker"),
        "policy remux with source ids must require the real remux runtime, got: {}",
        err.source
    );
    assert_eq!(err.summary.dispatch_count, 0);
    assert_eq!(fixture.lease_count().await, 0);
    let ticket_payload = fixture.first_ticket_payload().await;
    let workflow_payload =
        WorkflowTicketPayload::parse_ticket("synthetic.workflow.operation.remux", ticket_payload)
            .unwrap();
    assert_eq!(workflow_payload.operation, OperationKind::Remux);
    assert_eq!(
        workflow_payload.rendered_payload["source_file_version_id"],
        source_file_version_id.0
    );
}

#[tokio::test]
async fn policy_remux_ticket_succeeds_with_fake_runtime_remux_result() {
    let mut fixture = ExecutorFixture::without_workers(0).await;
    let dir = tempfile::tempdir().unwrap();
    let source_path = dir.path().join("Movie.mkv");
    let (source_file_version_id, _source_location_id) = fixture
        .seed_local_source_at_path(&source_path, b"movie-bytes")
        .await;
    let snapshot_id = fixture.record_source_snapshot(source_file_version_id).await;
    fixture.plan = policy_remux_plan_for_snapshot(
        TargetRef::FileVersion {
            id: source_file_version_id,
        },
        snapshot_id,
    );
    fixture
        .register_worker(
            "remux-worker",
            OperationKind::Remux,
            1,
            FakeBehavior::RequireRemuxProtocolPayload,
        )
        .await;
    let mut options = WorkflowExecutorOptions::for_tests();
    options.remux_staging_root = dir.path().join("stage");
    options.remux_target_dir = dir.path().join("out");

    let summary = fixture.run_with_options(options).await.unwrap();

    assert_eq!(summary.operation_count(OperationKind::Remux), 1);
    assert_eq!(summary.failure_count, 0);
}

#[tokio::test]
async fn policy_remux_success_event_append_is_atomic_with_ticket_success() {
    let mut fixture = ExecutorFixture::without_workers(0).await;
    let dir = tempfile::tempdir().unwrap();
    let source_path = dir.path().join("Movie.mkv");
    let (source_file_version_id, source_location_id) = fixture
        .seed_local_source_at_path(&source_path, b"movie-bytes")
        .await;
    let snapshot_id = fixture.record_source_snapshot(source_file_version_id).await;
    let worker_id = fixture
        .register_worker(
            "remux-worker",
            OperationKind::Remux,
            1,
            FakeBehavior::RequireRemuxProtocolPayload,
        )
        .await;
    let (ticket, workflow_payload, lease) = fixture
        .acquire_policy_remux_ticket(
            worker_id,
            source_file_version_id,
            source_location_id,
            snapshot_id,
            1,
        )
        .await;
    sqlx::query(
        "CREATE TRIGGER fail_workflow_remux_success_event \
         BEFORE INSERT ON events WHEN NEW.kind = 'artifact.remux_succeeded' \
         BEGIN SELECT RAISE(ABORT, 'event log unavailable'); END",
    )
    .execute(&fixture.cp.pool)
    .await
    .unwrap();
    let runtime = fixture.registry.get(worker_id).unwrap();
    let mut options = WorkflowExecutorOptions::for_tests();
    options.remux_staging_root = dir.path().join("stage");
    options.remux_target_dir = dir.path().join("out");

    let err = super::dispatch_control_plane_remux(
        &fixture.cp,
        &runtime,
        &ticket,
        &workflow_payload,
        lease.id,
        &workflow_payload.rendered_payload,
        &options,
    )
    .await
    .unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::DbUnreachable);
    assert_eq!(
        fixture.event_count("artifact.remux_succeeded").await,
        0,
        "failed success-event append must not be reported as workflow success"
    );
    assert_eq!(fixture.event_count("ticket.succeeded").await, 0);
    assert_eq!(fixture.ticket_state(ticket.id).await, "leased");
}

#[tokio::test]
async fn policy_remux_post_commit_snapshot_failure_is_not_retryable_remux_failure() {
    let mut fixture = ExecutorFixture::without_workers(0).await;
    let dir = tempfile::tempdir().unwrap();
    let source_path = dir.path().join("Movie.mkv");
    let (source_file_version_id, source_location_id) = fixture
        .seed_local_source_at_path(&source_path, b"movie-bytes")
        .await;
    let snapshot_id = fixture.record_source_snapshot(source_file_version_id).await;
    let worker_id = fixture
        .register_worker(
            "remux-worker",
            OperationKind::Remux,
            1,
            FakeBehavior::RequireRemuxProtocolPayload,
        )
        .await;
    let (ticket, workflow_payload, lease) = fixture
        .acquire_policy_remux_ticket(
            worker_id,
            source_file_version_id,
            source_location_id,
            snapshot_id,
            2,
        )
        .await;
    sqlx::query(
        "CREATE TRIGGER fail_workflow_remux_result_snapshot \
         BEFORE INSERT ON media_snapshots \
         BEGIN SELECT RAISE(ABORT, 'probe unavailable'); END",
    )
    .execute(&fixture.cp.pool)
    .await
    .unwrap();
    let runtime = fixture.registry.get(worker_id).unwrap();
    let mut options = WorkflowExecutorOptions::for_tests();
    options.remux_staging_root = dir.path().join("stage");
    options.remux_target_dir = dir.path().join("out");

    let err = super::dispatch_control_plane_remux(
        &fixture.cp,
        &runtime,
        &ticket,
        &workflow_payload,
        lease.id,
        &workflow_payload.rendered_payload,
        &options,
    )
    .await
    .unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::ExternalSystemUnavailable);
    assert!(dir.path().join("out/Movie.remux.mkv").exists());
    assert_eq!(fixture.event_count("artifact.remux_failed").await, 0);
    assert_eq!(fixture.event_count("ticket.failed_retriable").await, 0);
    assert_eq!(fixture.ticket_state(ticket.id).await, "leased");
}

#[tokio::test]
async fn policy_remux_dispatch_uses_workflow_lease_and_idempotency_key() {
    let mut fixture = ExecutorFixture::without_workers(0).await;
    let dir = tempfile::tempdir().unwrap();
    let source_path = dir.path().join("Movie.mkv");
    let (source_file_version_id, _source_location_id) = fixture
        .seed_local_source_at_path(&source_path, b"movie-bytes")
        .await;
    let snapshot_id = fixture.record_source_snapshot(source_file_version_id).await;
    fixture.plan = policy_remux_plan_for_snapshot(
        TargetRef::FileVersion {
            id: source_file_version_id,
        },
        snapshot_id,
    );
    fixture
        .register_worker(
            "remux-worker",
            OperationKind::Remux,
            1,
            FakeBehavior::RequireCorrelatedRemuxDispatch,
        )
        .await;
    let mut options = WorkflowExecutorOptions::for_tests();
    options.remux_staging_root = dir.path().join("stage");
    options.remux_target_dir = dir.path().join("out");

    let summary = fixture.run_with_options(options).await.unwrap();

    assert_eq!(summary.operation_count(OperationKind::Remux), 1);
    assert_eq!(summary.failure_count, 0);
}

#[tokio::test]
async fn invalid_policy_remux_payload_fails_acquired_lease() {
    let mut fixture = ExecutorFixture::without_workers(0).await;
    let worker_id = fixture
        .register_worker(
            "remux-worker",
            OperationKind::Remux,
            1,
            FakeBehavior::RequireRemuxProtocolPayload,
        )
        .await;
    let job = fixture
        .cp
        .open_job(NewJob {
            kind: "synthetic.workflow".to_owned(),
            priority: 0,
            created_at: T0,
        })
        .await
        .unwrap();
    let workflow_payload = WorkflowTicketPayload::new_for_test(
        "workflow-1",
        "plan-1",
        "policy-node_remux",
        "root",
        OperationKind::Remux,
        json!({
            "operation": "remux",
            "source_file_version_id": 11
        }),
    );
    let ticket = fixture
        .cp
        .create_ticket(NewTicket {
            job_id: Some(job.id),
            kind: "synthetic.workflow.operation.remux".to_owned(),
            priority: 0,
            payload: workflow_payload.to_ticket_payload().unwrap(),
            max_attempts: 1,
            created_at: T0,
        })
        .await
        .unwrap();
    fixture
        .cp
        .mark_ready_if_unblocked(ticket.id, T0)
        .await
        .unwrap();
    let lease = fixture
        .cp
        .acquire_lease(NewLease {
            ticket_id: ticket.id,
            worker_id,
            ttl: time::Duration::seconds(5),
            now: T0,
        })
        .await
        .unwrap();
    let runtime = fixture.registry.get(worker_id).unwrap();

    let err = super::dispatch_control_plane_remux(
        &fixture.cp,
        &runtime,
        &ticket,
        &workflow_payload,
        lease.id,
        &workflow_payload.rendered_payload,
        &WorkflowExecutorOptions::for_tests(),
    )
    .await
    .unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::ConfigInvalid);
    assert_eq!(fixture.held_lease_count().await, 0);
    assert_eq!(fixture.first_ticket_failed_class().await, "worker_crash");
}

#[tokio::test]
async fn policy_transcode_dispatch_sends_worker_protocol_payload() {
    let mut fixture = ExecutorFixture::without_workers(0).await;
    let dir = tempfile::tempdir().unwrap();
    let source_path = dir.path().join("Movie.mkv");
    let (source_file_version_id, _source_location_id) = fixture
        .seed_local_source_at_path(&source_path, b"movie-bytes")
        .await;
    fixture.plan = policy_transcode_plan(TargetRef::FileVersion {
        id: source_file_version_id,
    });
    fixture
        .register_worker(
            "transcode-worker",
            OperationKind::TranscodeVideo,
            1,
            FakeBehavior::RequireTranscodeProtocolPayload,
        )
        .await;
    let mut options = WorkflowExecutorOptions::for_tests();
    options.transcode_staging_root = dir.path().join("stage");
    options.transcode_target_dir = dir.path().join("out");

    let summary = fixture.run_with_options(options).await.unwrap();

    assert_eq!(summary.operation_count(OperationKind::TranscodeVideo), 1);
}

#[tokio::test]
async fn policy_transcode_success_result_includes_generated_staging_path() {
    let mut fixture = ExecutorFixture::without_workers(0).await;
    let dir = tempfile::tempdir().unwrap();
    let source_path = dir.path().join("Movie.mkv");
    let (source_file_version_id, _source_location_id) = fixture
        .seed_local_source_at_path(&source_path, b"movie-bytes")
        .await;
    fixture.plan = policy_transcode_plan(TargetRef::FileVersion {
        id: source_file_version_id,
    });
    fixture
        .register_worker(
            "transcode-worker",
            OperationKind::TranscodeVideo,
            1,
            FakeBehavior::RequireTranscodeProtocolPayload,
        )
        .await;
    let mut options = WorkflowExecutorOptions::for_tests();
    options.transcode_staging_root = dir.path().join("stage");
    options.transcode_target_dir = dir.path().join("out");

    fixture.run_with_options(options).await.unwrap();

    let result = fixture.first_ticket_result().await;
    let staging_path = result["staging_path"].as_str().unwrap();
    assert!(staging_path.ends_with("ticket-1/lease-1/Movie.hevc.mkv"));
    assert_eq!(result["staged_artifact_handle_id"], 1);
    assert_eq!(result["staged_artifact_location_id"], 1);
    assert_eq!(result["verification_id"], 1);
    assert_eq!(result["commit_record_id"], 1);
    assert_eq!(result["result_file_version_id"], 2);
    assert_eq!(result["result_file_location_id"], 2);
    assert_eq!(result["result_media_snapshot_id"], 1);
}

#[tokio::test]
async fn policy_transcode_heartbeats_outer_workflow_lease_while_running() {
    let mut fixture = ExecutorFixture::without_workers(0).await;
    let dir = tempfile::tempdir().unwrap();
    let source_path = dir.path().join("Movie.mkv");
    let (source_file_version_id, _source_location_id) = fixture
        .seed_local_source_at_path(&source_path, b"movie-bytes")
        .await;
    fixture.plan = policy_transcode_plan(TargetRef::FileVersion {
        id: source_file_version_id,
    });
    fixture
        .register_worker(
            "transcode-worker",
            OperationKind::TranscodeVideo,
            1,
            FakeBehavior::SlowTranscodeResult,
        )
        .await;
    let mut options = WorkflowExecutorOptions::for_tests();
    options.heartbeat_interval = Duration::from_millis(10);
    options.transcode_staging_root = dir.path().join("stage");
    options.transcode_target_dir = dir.path().join("out");

    fixture.run_with_options(options).await.unwrap();

    let (acquired_at, last_heartbeat_at) = fixture.first_lease_heartbeat_window().await;
    assert!(
        last_heartbeat_at > acquired_at,
        "long control-plane transcode must keep the outer workflow lease fresh"
    );
}

#[tokio::test]
async fn policy_transcode_dispatch_rejects_malformed_worker_result() {
    let mut fixture = ExecutorFixture::without_workers(0).await;
    let dir = tempfile::tempdir().unwrap();
    let source_path = dir.path().join("Movie.mkv");
    let (source_file_version_id, _source_location_id) = fixture
        .seed_local_source_at_path(&source_path, b"movie-bytes")
        .await;
    fixture.plan = policy_transcode_plan(TargetRef::FileVersion {
        id: source_file_version_id,
    });
    fixture
        .register_worker(
            "transcode-worker",
            OperationKind::TranscodeVideo,
            1,
            FakeBehavior::MalformedTranscodeResult,
        )
        .await;
    let mut options = WorkflowExecutorOptions::for_tests();
    options.transcode_staging_root = dir.path().join("stage");
    options.transcode_target_dir = dir.path().join("out");

    let err = fixture.run_with_options(options).await.unwrap_err();

    assert_eq!(err.summary.failure_count, 1);
    assert_eq!(
        fixture.first_ticket_failed_class().await,
        "malformed_worker_result"
    );
}

#[tokio::test]
async fn policy_transcode_dispatch_rejects_wrong_output_facts() {
    let mut fixture = ExecutorFixture::without_workers(0).await;
    let dir = tempfile::tempdir().unwrap();
    let source_path = dir.path().join("Movie.mkv");
    let (source_file_version_id, _source_location_id) = fixture
        .seed_local_source_at_path(&source_path, b"movie-bytes")
        .await;
    fixture.plan = policy_transcode_plan(TargetRef::FileVersion {
        id: source_file_version_id,
    });
    fixture
        .register_worker(
            "transcode-worker",
            OperationKind::TranscodeVideo,
            1,
            FakeBehavior::WrongTranscodeOutputFacts,
        )
        .await;
    let mut options = WorkflowExecutorOptions::for_tests();
    options.transcode_staging_root = dir.path().join("stage");
    options.transcode_target_dir = dir.path().join("out");

    let err = fixture.run_with_options(options).await.unwrap_err();

    assert_eq!(err.summary.failure_count, 1);
    assert_eq!(
        fixture.first_ticket_failed_class().await,
        "malformed_worker_result"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn policy_transcode_rejects_symlink_staging_root_before_worker_dispatch() {
    let mut fixture = ExecutorFixture::without_workers(0).await;
    let dir = tempfile::tempdir().unwrap();
    let source_path = dir.path().join("Movie.mkv");
    let (source_file_version_id, _source_location_id) = fixture
        .seed_local_source_at_path(&source_path, b"movie-bytes")
        .await;
    fixture.plan = policy_transcode_plan(TargetRef::FileVersion {
        id: source_file_version_id,
    });
    fixture
        .register_worker(
            "transcode-worker",
            OperationKind::TranscodeVideo,
            1,
            FakeBehavior::RequireTranscodeProtocolPayload,
        )
        .await;
    let outside = dir.path().join("outside");
    tokio::fs::create_dir(&outside).await.unwrap();
    let symlink_root = dir.path().join("stage-link");
    std::os::unix::fs::symlink(&outside, &symlink_root).unwrap();
    let mut options = WorkflowExecutorOptions::for_tests();
    options.transcode_staging_root = symlink_root;
    options.transcode_target_dir = dir.path().join("out");

    let err = fixture.run_with_options(options).await.unwrap_err();

    assert_eq!(err.summary.failure_count, 1);
    assert_eq!(fixture.first_ticket_failed_class().await, "worker_crash");
}

#[test]
fn summary_branch_count_only_excludes_synthetic_root_ticket() {
    let synthetic_root = WorkflowTicketPayload::new_for_test(
        "workflow",
        "plan",
        "scan",
        "root",
        OperationKind::ScanLibrary,
        json!({"path": "/library"}),
    );
    let mut real_root_branch = WorkflowTicketPayload::new_for_test(
        "workflow",
        "plan",
        "probe",
        "root",
        OperationKind::ProbeFile,
        json!({"path": "/library/root.mkv"}),
    );
    real_root_branch.source_file = Some(json!({"path": "/library/root.mkv"}));

    assert!(is_synthetic_root_ticket(&synthetic_root));
    assert!(!is_synthetic_root_ticket(&real_root_branch));
}

struct ExecutorFixture {
    cp: crate::ControlPlane,
    _tmp: tempfile::NamedTempFile,
    plan: WorkflowPlan,
    registry: WorkerRuntimeRegistry,
    first_worker_id: Option<WorkerId>,
    other_job_id: Option<JobId>,
}

impl ExecutorFixture {
    async fn with_ready_tickets(ticket_count: usize) -> Self {
        let mut fixture = Self::without_workers(ticket_count).await;
        let worker_id = fixture
            .register_worker(
                "hash-worker",
                OperationKind::HashFile,
                8,
                FakeBehavior::Success,
            )
            .await;
        fixture.first_worker_id = Some(worker_id);
        fixture
    }

    async fn single_worker_max_parallel(max_parallel: u32) -> Self {
        let mut fixture = Self::without_workers(4).await;
        let worker_id = fixture
            .register_worker(
                "hash-worker",
                OperationKind::HashFile,
                max_parallel,
                FakeBehavior::Success,
            )
            .await;
        fixture.first_worker_id = Some(worker_id);
        fixture
    }

    async fn single_worker_with_behavior(behavior: FakeBehavior) -> Self {
        let mut fixture = Self::without_workers(1).await;
        let worker_id = fixture
            .register_worker("hash-worker", OperationKind::HashFile, 1, behavior)
            .await;
        fixture.first_worker_id = Some(worker_id);
        fixture
    }

    async fn ambiguous_workers() -> Self {
        let mut fixture = Self::without_workers(1).await;
        let first = fixture
            .register_worker(
                "hash-worker-a",
                OperationKind::HashFile,
                1,
                FakeBehavior::Success,
            )
            .await;
        let _second = fixture
            .register_worker(
                "hash-worker-b",
                OperationKind::HashFile,
                1,
                FakeBehavior::Success,
            )
            .await;
        fixture.first_worker_id = Some(first);
        fixture
    }

    async fn without_workers(ticket_count: usize) -> Self {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let url = format!("sqlite://{}", tmp.path().display());
        let _ = voom_store::init(&url).await.unwrap();
        let pool = voom_store::connect(&url).await.unwrap();
        let cp = crate::ControlPlane::open_with_pool_and_rng(
            pool,
            Arc::new(SystemClock),
            Arc::new(Mutex::new(FrozenRng::new(0))),
        )
        .await
        .unwrap();
        Self {
            cp,
            _tmp: tmp,
            plan: independent_hash_plan(ticket_count),
            registry: WorkerRuntimeRegistry::new(),
            first_worker_id: None,
            other_job_id: None,
        }
    }

    async fn register_worker(
        &mut self,
        name: &str,
        operation: OperationKind,
        max_parallel: u32,
        behavior: FakeBehavior,
    ) -> WorkerId {
        let worker = self
            .register_worker_without_runtime(name, operation, max_parallel)
            .await;
        let client = Arc::new(FakeClient::new(worker, behavior));
        self.registry.register_in_process_runtime(
            worker,
            client,
            WorkerCredentials {
                worker_id: worker,
                worker_epoch: 0,
                secret: SecretString::from("test-secret"),
            },
        );
        worker
    }

    async fn register_worker_without_runtime(
        &mut self,
        name: &str,
        operation: OperationKind,
        max_parallel: u32,
    ) -> WorkerId {
        let worker = self
            .cp
            .register_worker(NewWorker {
                name: name.to_owned(),
                kind: WorkerKind::Synthetic,
                registered_at: T0,
                node_id: None,
            })
            .await
            .unwrap();
        let operation_name = operation_name(operation);
        self.cp
            .record_capability(NewCapability {
                worker_id: worker.id,
                operation: operation_name.clone(),
                codecs: Vec::new(),
                hardware: Vec::new(),
                artifact_access: Vec::new(),
                extra: json!({}),
            })
            .await
            .unwrap();
        self.cp
            .record_grant(NewGrant {
                worker_id: worker.id,
                can_execute: vec![operation_name.clone()],
                can_access_read: Vec::new(),
                can_access_write: Vec::new(),
                denies: Vec::new(),
                max_parallel: json!({ operation_name: max_parallel }),
            })
            .await
            .unwrap();
        worker.id
    }

    fn worker_id(&self) -> WorkerId {
        self.first_worker_id.unwrap()
    }

    async fn run(&self) -> Result<WorkflowRunSummary, crate::workflow::executor::WorkflowRunError> {
        self.run_with_options(WorkflowExecutorOptions::for_tests())
            .await
    }

    async fn run_with_policy(
        &self,
        concurrency: ConcurrencyPolicy,
    ) -> Result<WorkflowRunSummary, crate::workflow::executor::WorkflowRunError> {
        let mut plan = self.plan.clone();
        plan.concurrency = concurrency;
        self.executor_with_options(WorkflowExecutorOptions::for_tests())
            .submit_and_run(plan)
            .await
    }

    async fn run_with_options(
        &self,
        options: WorkflowExecutorOptions,
    ) -> Result<WorkflowRunSummary, crate::workflow::executor::WorkflowRunError> {
        self.executor_with_options(options)
            .submit_and_run(self.plan.clone())
            .await
    }

    fn executor_with_options(
        &self,
        options: WorkflowExecutorOptions,
    ) -> WorkflowExecutor<SingleWorkerPerKindSelector> {
        WorkflowExecutor::with_options(
            self.cp.clone(),
            SingleWorkerPerKindSelector,
            self.registry.clone(),
            options,
        )
    }

    async fn lease_count(&self) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM leases")
            .fetch_one(&self.cp.pool)
            .await
            .unwrap()
    }

    async fn held_lease_count(&self) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM leases WHERE state = 'held'")
            .fetch_one(&self.cp.pool)
            .await
            .unwrap()
    }

    async fn ticket_count(&self) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM tickets")
            .fetch_one(&self.cp.pool)
            .await
            .unwrap()
    }

    async fn first_lease_heartbeat_window(&self) -> (String, String) {
        sqlx::query_as("SELECT acquired_at, last_heartbeat_at FROM leases ORDER BY id ASC LIMIT 1")
            .fetch_one(&self.cp.pool)
            .await
            .unwrap()
    }

    async fn seed_other_job_ready_ticket(&mut self, priority: i64) {
        let job = self
            .cp
            .open_job(NewJob {
                kind: "other.workflow".to_owned(),
                priority,
                created_at: T0,
            })
            .await
            .unwrap();
        let operation = OperationKind::HashFile;
        let rendered_payload = json!({
            "operation": operation_name(operation),
            "branch_id": "other",
            "path": "/library/other.mkv",
            "duration_ms": 10_u64,
            "progress_interval_ms": 1_u64,
        });
        let payload = WorkflowTicketPayload {
            workflow_id: "other-workflow".to_owned(),
            plan_id: "other-plan".to_owned(),
            node_id: "hash-other".to_owned(),
            branch_id: "other".to_owned(),
            operation,
            rendered_payload,
            timing: EffectiveTiming::for_test(10, 1),
            source_file: None,
        }
        .to_ticket_payload()
        .unwrap();
        let ticket = self
            .cp
            .create_ticket(NewTicket {
                job_id: Some(job.id),
                kind: format!("synthetic.workflow.operation.{}", operation_name(operation)),
                priority,
                payload,
                max_attempts: 1,
                created_at: T0,
            })
            .await
            .unwrap();
        self.cp
            .mark_ready_if_unblocked(ticket.id, T0)
            .await
            .unwrap();
        self.other_job_id = Some(job.id);
    }

    async fn other_job_ready_count(&self) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM tickets WHERE job_id = ? AND state = 'ready'")
            .bind(i64::try_from(self.other_job_id.unwrap().0).unwrap())
            .fetch_one(&self.cp.pool)
            .await
            .unwrap()
    }

    async fn first_ticket_failed_class(&self) -> String {
        let payload: String = sqlx::query_scalar(
            "SELECT payload FROM events \
             WHERE kind = 'ticket.failed_terminal' \
             ORDER BY event_id ASC LIMIT 1",
        )
        .fetch_one(&self.cp.pool)
        .await
        .unwrap();
        serde_json::from_str::<Value>(&payload).unwrap()["class"]
            .as_str()
            .unwrap()
            .to_owned()
    }

    async fn seed_local_source(&self) -> (FileVersionId, voom_core::FileLocationId) {
        self.seed_local_source_at_path(PathBuf::from("/library/source.mkv"), b"source")
            .await
    }

    async fn seed_local_source_at_path(
        &self,
        path: impl AsRef<std::path::Path>,
        bytes: &[u8],
    ) -> (FileVersionId, voom_core::FileLocationId) {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            let _ = tokio::fs::create_dir_all(parent).await;
        }
        let _ = tokio::fs::write(path, bytes).await;
        let outcome = self
            .cp
            .record_discovered_file(
                DiscoveredFile {
                    location_kind: FileLocationKind::LocalPath,
                    location_value: path.to_string_lossy().into_owned(),
                    content_hash: format!("blake3:{}", blake3::hash(bytes).to_hex()),
                    size_bytes: bytes.len().try_into().unwrap(),
                    observed_at: T0,
                    proof: None,
                },
                None,
            )
            .await
            .unwrap();
        match outcome {
            IngestOutcome::NewFileAsset {
                file_version_id,
                file_location_id,
                ..
            } => (file_version_id, file_location_id),
            IngestOutcome::AliasAttached { .. } => panic!("seed must create a new file asset"),
        }
    }

    async fn record_source_snapshot(&self, file_version_id: FileVersionId) -> MediaSnapshotId {
        self.cp
            .record_media_snapshot(
                file_version_id,
                None,
                json!({
                    "streams": [
                        {
                            "id": "stream-0",
                            "index": 0,
                            "kind": "video",
                            "codec_name": "h264",
                            "disposition": {"default": true}
                        },
                        {
                            "id": "stream-1",
                            "index": 1,
                            "kind": "audio",
                            "codec_name": "aac",
                            "language": "eng",
                            "channels": 2,
                            "disposition": {"default": false}
                        }
                    ]
                }),
                T0,
            )
            .await
            .unwrap()
            .id
    }

    async fn first_ticket_payload(&self) -> Value {
        let payload: String =
            sqlx::query_scalar("SELECT payload FROM tickets ORDER BY id ASC LIMIT 1")
                .fetch_one(&self.cp.pool)
                .await
                .unwrap();
        serde_json::from_str(&payload).unwrap()
    }

    async fn first_ticket_result(&self) -> Value {
        let result: String =
            sqlx::query_scalar("SELECT result FROM tickets ORDER BY id ASC LIMIT 1")
                .fetch_one(&self.cp.pool)
                .await
                .unwrap();
        serde_json::from_str(&result).unwrap()
    }

    async fn acquire_policy_remux_ticket(
        &self,
        worker_id: WorkerId,
        source_file_version_id: FileVersionId,
        source_location_id: voom_core::FileLocationId,
        source_media_snapshot_id: MediaSnapshotId,
        max_attempts: u32,
    ) -> (
        voom_store::repo::tickets::Ticket,
        WorkflowTicketPayload,
        voom_store::repo::leases::Lease,
    ) {
        let job = self
            .cp
            .open_job(NewJob {
                kind: "synthetic.workflow".to_owned(),
                priority: 0,
                created_at: T0,
            })
            .await
            .unwrap();
        let workflow_payload = WorkflowTicketPayload::new_for_test(
            "workflow-1",
            "plan-1",
            "policy-node_remux",
            "root",
            OperationKind::Remux,
            json!({
                "operation": "remux",
                "source_file_version_id": source_file_version_id.0,
                "source_location_id": source_location_id.0,
                "remux": {
                    "type": "remux",
                    "container": "mkv",
                    "source_media_snapshot_id": source_media_snapshot_id.0,
                    "track_actions": [],
                    "track_order": ["video", "audio"],
                    "defaults": [{"target": "audio", "strategy": "first"}]
                }
            }),
        );
        let ticket = self
            .cp
            .create_ticket(NewTicket {
                job_id: Some(job.id),
                kind: "synthetic.workflow.operation.remux".to_owned(),
                priority: 0,
                payload: workflow_payload.to_ticket_payload().unwrap(),
                max_attempts,
                created_at: T0,
            })
            .await
            .unwrap();
        self.cp
            .mark_ready_if_unblocked(ticket.id, T0)
            .await
            .unwrap();
        let lease = self
            .cp
            .acquire_lease(NewLease {
                ticket_id: ticket.id,
                worker_id,
                ttl: time::Duration::seconds(5),
                now: T0,
            })
            .await
            .unwrap();
        (ticket, workflow_payload, lease)
    }

    async fn event_count(&self, kind: &str) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM events WHERE kind = ?")
            .bind(kind)
            .fetch_one(&self.cp.pool)
            .await
            .unwrap()
    }

    async fn ticket_state(&self, ticket_id: voom_core::TicketId) -> String {
        sqlx::query_scalar("SELECT state FROM tickets WHERE id = ?")
            .bind(i64::try_from(ticket_id.0).unwrap())
            .fetch_one(&self.cp.pool)
            .await
            .unwrap()
    }
}

#[derive(Debug)]
struct FakeClient {
    worker_id: WorkerId,
    behavior: FakeBehavior,
    active: Arc<AtomicU32>,
    max_active: AtomicU32,
}

impl FakeClient {
    fn new(worker_id: WorkerId, behavior: FakeBehavior) -> Self {
        Self {
            worker_id,
            behavior,
            active: Arc::new(AtomicU32::new(0)),
            max_active: AtomicU32::new(0),
        }
    }

    fn enter_active(&self) {
        let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_active.fetch_max(active, Ordering::SeqCst);
    }
}

#[derive(Debug, Clone, Copy)]
enum FakeBehavior {
    Success,
    MalformedFrame,
    Hang,
    ProgressFlood,
    Crash,
    DispatchError,
    RequireTranscodeProtocolPayload,
    RequireRemuxProtocolPayload,
    RequireCorrelatedRemuxDispatch,
    RequireRemuxProtocolPayloadThenMkvtoolnixUnavailable,
    SlowTranscodeResult,
    MalformedTranscodeResult,
    WrongTranscodeOutputFacts,
}

#[async_trait]
impl ClientHandle for FakeClient {
    async fn handshake(&self, _offered: u32) -> Result<HandshakeResponse, ProtocolError> {
        Err(ProtocolError::InternalServerError)
    }

    async fn dispatch(
        &self,
        _creds: &WorkerCredentials,
        _idempotency_key: &str,
        request: OperationRequest,
    ) -> Result<DispatchStream, ProtocolError> {
        assert_eq!(_creds.worker_id, self.worker_id);
        if matches!(self.behavior, FakeBehavior::DispatchError) {
            return Err(ProtocolError::InternalServerError);
        }
        if matches!(self.behavior, FakeBehavior::RequireTranscodeProtocolPayload) {
            serde_json::from_value::<TranscodeVideoRequest>(request.payload.clone()).map_err(
                |err| ProtocolError::InvalidPayload {
                    detail: format!("transcode payload must match worker protocol: {err}"),
                },
            )?;
        }
        if matches!(
            self.behavior,
            FakeBehavior::RequireRemuxProtocolPayload
                | FakeBehavior::RequireCorrelatedRemuxDispatch
                | FakeBehavior::RequireRemuxProtocolPayloadThenMkvtoolnixUnavailable
        ) {
            serde_json::from_value::<RemuxRequest>(request.payload.clone()).map_err(|err| {
                ProtocolError::InvalidPayload {
                    detail: format!("remux payload must match worker protocol: {err}"),
                }
            })?;
        }
        if matches!(self.behavior, FakeBehavior::RequireCorrelatedRemuxDispatch) {
            if request.lease_id != LeaseId(1) {
                return Err(ProtocolError::InvalidPayload {
                    detail: format!(
                        "remux lease id must be workflow lease 1, got {:?}",
                        request.lease_id
                    ),
                });
            }
            if _idempotency_key != "ticket-1-lease-1" {
                return Err(ProtocolError::InvalidPayload {
                    detail: format!(
                        "remux idempotency key must be ticket-1-lease-1, got {_idempotency_key}"
                    ),
                });
            }
        }
        if matches!(
            self.behavior,
            FakeBehavior::MalformedTranscodeResult | FakeBehavior::WrongTranscodeOutputFacts
        ) {
            serde_json::from_value::<TranscodeVideoRequest>(request.payload.clone()).map_err(
                |err| ProtocolError::InvalidPayload {
                    detail: format!("transcode payload must match worker protocol: {err}"),
                },
            )?;
        }
        self.enter_active();
        let (reader, writer) = tokio::io::duplex(16 * 1024);
        let behavior = self.behavior;
        let lease_id = request.lease_id;
        let active = self.active.clone();
        tokio::spawn(async move {
            write_behavior(writer, request, behavior).await;
            active.fetch_sub(1, Ordering::SeqCst);
        });
        Ok(DispatchStream {
            response: OperationResponse {
                lease_id,
                accepted_at: Utc::now(),
            },
            frames: NdjsonReader::new(
                Box::pin(reader) as Pin<Box<dyn tokio::io::AsyncRead + Send + Unpin>>,
                lease_id,
            ),
        })
    }
}

async fn write_behavior(
    mut writer: DuplexStream,
    request: OperationRequest,
    behavior: FakeBehavior,
) {
    match behavior {
        FakeBehavior::Success
        | FakeBehavior::RequireTranscodeProtocolPayload
        | FakeBehavior::RequireRemuxProtocolPayload
        | FakeBehavior::RequireCorrelatedRemuxDispatch
        | FakeBehavior::RequireRemuxProtocolPayloadThenMkvtoolnixUnavailable
        | FakeBehavior::SlowTranscodeResult => {
            let delay = match behavior {
                FakeBehavior::SlowTranscodeResult => Duration::from_millis(80),
                _ => Duration::from_millis(25),
            };
            tokio::time::sleep(delay).await;
            let payload = match request.operation {
                OperationKind::TranscodeVideo => {
                    transcode_result_payload_for_request(&request).await
                }
                OperationKind::Remux
                    if matches!(
                        behavior,
                        FakeBehavior::RequireRemuxProtocolPayload
                            | FakeBehavior::RequireCorrelatedRemuxDispatch
                    ) =>
                {
                    remux_result_payload_for_request(&request).await
                }
                OperationKind::Remux
                    if matches!(
                        behavior,
                        FakeBehavior::RequireRemuxProtocolPayloadThenMkvtoolnixUnavailable
                    ) =>
                {
                    write_frame(&mut writer, mkvtoolnix_unavailable_frame(&request)).await;
                    return;
                }
                _ => json!({"ok": true}),
            };
            write_frame(&mut writer, result_frame(&request, payload)).await;
        }
        FakeBehavior::MalformedFrame => {
            let _ = writer.write_all(b"{not-json}\n").await;
        }
        FakeBehavior::Hang => {
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
        FakeBehavior::ProgressFlood => {
            for seq in 0..128 {
                tokio::time::sleep(Duration::from_millis(1)).await;
                write_frame(&mut writer, progress_frame(&request, seq)).await;
            }
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
        FakeBehavior::Crash | FakeBehavior::DispatchError => {}
        FakeBehavior::MalformedTranscodeResult => {
            tokio::time::sleep(Duration::from_millis(25)).await;
            write_frame(&mut writer, result_frame(&request, json!({"ok": true}))).await;
        }
        FakeBehavior::WrongTranscodeOutputFacts => {
            tokio::time::sleep(Duration::from_millis(25)).await;
            let mut payload = transcode_result_payload_for_request(&request).await;
            payload["output_container"] = json!("mp4");
            payload["output_video_codec"] = json!("h264");
            write_frame(&mut writer, result_frame(&request, payload)).await;
        }
    }
}

async fn transcode_result_payload_for_request(request: &OperationRequest) -> Value {
    let request = serde_json::from_value::<TranscodeVideoRequest>(request.payload.clone()).unwrap();
    let output_bytes = b"output!";
    tokio::fs::write(&request.output.path, output_bytes)
        .await
        .unwrap();
    json!({
        "status": "transcoded",
        "provider": "ffmpeg",
        "provider_version": "ffmpeg test",
        "input_pre": {
            "size_bytes": request.input.expected.size_bytes,
            "content_hash": request.input.expected.content_hash
        },
        "input_post": {
            "size_bytes": request.input.expected.size_bytes,
            "content_hash": request.input.expected.content_hash
        },
        "output": {
            "size_bytes": output_bytes.len(),
            "content_hash": format!("blake3:{}", blake3::hash(output_bytes).to_hex())
        },
        "output_container": "mkv",
        "output_video_codec": "hevc"
    })
}

async fn remux_result_payload_for_request(request: &OperationRequest) -> Value {
    let request = serde_json::from_value::<RemuxRequest>(request.payload.clone()).unwrap();
    let output_bytes = b"remux bytes";
    tokio::fs::write(&request.output.path, output_bytes)
        .await
        .unwrap();
    let input = RemuxObservedFacts {
        size_bytes: request.input.expected.size_bytes,
        content_hash: request.input.expected.content_hash,
        modified_at: None,
        local_file_key: None,
    };
    serde_json::to_value(RemuxResult {
        status: RemuxStatus::Remuxed,
        provider: "mkvtoolnix".to_owned(),
        provider_version: "test".to_owned(),
        input_pre: input.clone(),
        input_post: input,
        output: RemuxObservedFacts {
            size_bytes: output_bytes.len().try_into().unwrap(),
            content_hash: format!("blake3:{}", blake3::hash(output_bytes).to_hex()),
            modified_at: None,
            local_file_key: None,
        },
        output_container: "mkv".to_owned(),
        kept_snapshot_stream_ids: request
            .selection
            .keep_streams
            .iter()
            .map(|stream| stream.snapshot_stream_id.clone())
            .collect(),
        default_snapshot_stream_ids: request
            .selection
            .default_streams
            .iter()
            .map(|stream| stream.snapshot_stream_id.clone())
            .collect(),
    })
    .unwrap()
}

async fn write_frame(writer: &mut DuplexStream, frame: ProgressFrame) {
    let bytes = serde_json::to_vec(&frame).unwrap();
    writer.write_all(&bytes).await.unwrap();
    writer.write_all(b"\n").await.unwrap();
}

fn result_frame(request: &OperationRequest, payload: Value) -> ProgressFrame {
    ProgressFrame::Result {
        lease_id: request.lease_id,
        seq: 0,
        emitted_at: Utc::now(),
        payload,
    }
}

fn progress_frame(request: &OperationRequest, seq: u64) -> ProgressFrame {
    ProgressFrame::Progress {
        lease_id: request.lease_id,
        seq,
        emitted_at: Utc::now(),
        percent: Some(PercentBps::try_from(100).unwrap()),
        message: None,
        payload: None,
    }
}

fn mkvtoolnix_unavailable_frame(request: &OperationRequest) -> ProgressFrame {
    ProgressFrame::Error {
        lease_id: request.lease_id,
        seq: 0,
        emitted_at: Utc::now(),
        class: FailureClass::WorkerCrash,
        code: ErrorCode::WorkerCrash,
        message: "mkvtoolnix worker unavailable".to_owned(),
        payload: None,
    }
}

fn independent_hash_plan(ticket_count: usize) -> WorkflowPlan {
    WorkflowPlan {
        id: format!("executor-test-{ticket_count}"),
        seed: 2,
        nodes: (0..ticket_count)
            .map(|index| {
                WorkflowNode::Operation(OperationNode {
                    id: format!("hash-{index}"),
                    operation: OperationKind::HashFile,
                    policy_target: None,
                    operation_payload: Value::Null,
                    depends_on: Vec::new(),
                    depends_on_selected: Vec::new(),
                    provides_selected: None,
                })
            })
            .collect(),
        fan_out: crate::workflow::model::FanOutPolicy { max_files: 3 },
        concurrency: ConcurrencyPolicy {
            max_in_flight_dispatches: 4,
        },
        timing: crate::workflow::model::TimingPolicy {
            base_duration_ms: 10,
            jitter_ms: 0,
        },
    }
}

fn policy_transcode_plan(target: TargetRef) -> WorkflowPlan {
    WorkflowPlan {
        id: "policy-transcode-test".to_owned(),
        seed: 12,
        nodes: vec![WorkflowNode::Operation(OperationNode {
            id: "policy-node_transcode".to_owned(),
            operation: OperationKind::TranscodeVideo,
            policy_target: Some(target),
            operation_payload: json!({
                "type": "transcode_video",
                "target_codec": "hevc",
                "container": "mkv",
                "profile": "default-hevc",
            }),
            depends_on: Vec::new(),
            depends_on_selected: Vec::new(),
            provides_selected: None,
        })],
        fan_out: crate::workflow::model::FanOutPolicy { max_files: 1 },
        concurrency: ConcurrencyPolicy {
            max_in_flight_dispatches: 1,
        },
        timing: crate::workflow::model::TimingPolicy {
            base_duration_ms: 10,
            jitter_ms: 0,
        },
    }
}

fn policy_remux_plan(target: TargetRef) -> WorkflowPlan {
    policy_remux_plan_with_payload(
        target,
        json!({
            "type": "remux",
            "container": "mkv",
            "source_media_snapshot_id": 99,
            "track_actions": [],
            "track_order": ["video", "audio", "subtitle"],
            "defaults": [],
        }),
    )
}

fn policy_remux_plan_for_snapshot(
    target: TargetRef,
    source_media_snapshot_id: MediaSnapshotId,
) -> WorkflowPlan {
    policy_remux_plan_with_payload(
        target,
        json!({
            "type": "remux",
            "container": "mkv",
            "source_media_snapshot_id": source_media_snapshot_id.0,
            "track_actions": [],
            "track_order": ["video", "audio"],
            "defaults": [{"target": "audio", "strategy": "first"}],
        }),
    )
}

fn policy_remux_plan_with_payload(target: TargetRef, operation_payload: Value) -> WorkflowPlan {
    WorkflowPlan {
        id: "policy-remux-test".to_owned(),
        seed: 12,
        nodes: vec![WorkflowNode::Operation(OperationNode {
            id: "policy-node_remux".to_owned(),
            operation: OperationKind::Remux,
            policy_target: Some(target),
            operation_payload,
            depends_on: Vec::new(),
            depends_on_selected: Vec::new(),
            provides_selected: None,
        })],
        fan_out: crate::workflow::model::FanOutPolicy { max_files: 1 },
        concurrency: ConcurrencyPolicy {
            max_in_flight_dispatches: 1,
        },
        timing: crate::workflow::model::TimingPolicy {
            base_duration_ms: 10,
            jitter_ms: 0,
        },
    }
}

fn non_policy_remux_plan() -> WorkflowPlan {
    WorkflowPlan {
        id: "non-policy-remux-test".to_owned(),
        seed: 12,
        nodes: vec![WorkflowNode::Operation(OperationNode {
            id: "remux".to_owned(),
            operation: OperationKind::Remux,
            policy_target: None,
            operation_payload: Value::Null,
            depends_on: Vec::new(),
            depends_on_selected: Vec::new(),
            provides_selected: None,
        })],
        fan_out: crate::workflow::model::FanOutPolicy { max_files: 1 },
        concurrency: ConcurrencyPolicy {
            max_in_flight_dispatches: 1,
        },
        timing: crate::workflow::model::TimingPolicy {
            base_duration_ms: 10,
            jitter_ms: 0,
        },
    }
}

fn timeout_options() -> WorkflowExecutorOptions {
    let mut options = WorkflowExecutorOptions::for_tests();
    options.progress_idle_timeout = Duration::from_millis(20);
    options.heartbeat_timeout = Duration::from_millis(250);
    options.heartbeat_interval = Duration::from_millis(10);
    options
}

fn operation_name(operation: OperationKind) -> String {
    serde_json::to_value(operation)
        .unwrap()
        .as_str()
        .unwrap()
        .to_owned()
}
