use std::collections::HashMap;
use std::error::Error;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use secrecy::SecretString;
use serde_json::{Value, json};
use time::OffsetDateTime;
use tokio::io::{AsyncWriteExt, DuplexStream};
use voom_core::clock_test_support::ManualClock;
use voom_core::rng_test_support::FrozenRng;
use voom_core::{
    ErrorCode, FailureClass, FileAssetId, FileLocationId, FileVersionId, JobId, LeaseId,
    MediaSnapshotId, StorageRootId, TicketId, TicketOperation, VoomError, WorkerId, WorkerKind,
};
use voom_store::repo::execution::jobs::NewJob;
use voom_store::repo::execution::leases::NewLease;
use voom_store::repo::execution::tickets::{NewTicket, Ticket};
use voom_store::repo::execution::workers::{NewCapability, NewGrant};
use voom_store::repo::media::identity::{
    DiscoveredFile, FileAssetRepo, FileVersionRepo, IngestOutcome, NewFileVersion, ProducedBy,
};
use voom_worker_protocol::{
    ClientHandle, DispatchStream, HandshakeResponse, NdjsonReader, OperationKind, OperationRequest,
    OperationResponse, PercentBps, ProgressFrame, ProtocolError, WorkerCredentials,
};

use super::super::leases::retry_on_database_locked;
use super::{
    CapacityDeferredTestSync, DispatchIdentity, DispatchReadyOutcome, NodeLocalSettleTestSync,
    RunInvocation, RunLoopState, SpawnOutcome, WorkflowFailureDisposition, WorkflowIdleState,
    workflow_failure_source,
};
use crate::workflow::execution::dispatch::{DispatchOutcome, DispatchTerminal};
use crate::workflow::execution::executor::WorkflowExecutorOptions;
use crate::workflow::execution::executor::tickets::parse_payload;
use crate::workflow::execution::operation_adapters::{
    LeaseHeartbeatContext, await_with_lease_heartbeats,
};
use crate::workflow::execution::runtime::WorkerRuntimeRegistry;
use crate::workflow::execution::timing::EffectiveTiming;
use crate::workflow::plan::access_declaration::{TicketStorageSource, declaration_for};
use crate::workflow::plan::model::{ConcurrencyPolicy, OperationNode, WorkflowPlan};
use crate::workflow::plan::ticket_payload::WorkflowTicketPayload;
use crate::workflow::summary::is_synthetic_root_ticket;
use crate::workflow::{WorkflowExecutor, WorkflowRunSummary};
use voom_plan::TargetRef;

const T0: OffsetDateTime = OffsetDateTime::UNIX_EPOCH;

#[tokio::test]
async fn retry_on_database_locked_retries_locked_errors_until_success() {
    let attempts = AtomicU32::new(0);

    let result = retry_on_database_locked(|| {
        let attempt = attempts.fetch_add(1, Ordering::SeqCst);
        async move {
            if attempt < 2 {
                Err(VoomError::database("database is locked"))
            } else {
                Ok("done")
            }
        }
    })
    .await
    .unwrap();

    assert_eq!(result, "done");
    assert_eq!(attempts.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn retry_on_database_locked_stops_after_eight_locked_errors() {
    let attempts = AtomicU32::new(0);

    let err = retry_on_database_locked(|| {
        attempts.fetch_add(1, Ordering::SeqCst);
        async { Err::<(), _>(VoomError::database("database is locked")) }
    })
    .await
    .unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::DbUnreachable);
    assert!(err.to_string().contains("database is locked"));
    assert_eq!(attempts.load(Ordering::SeqCst), 8);
}

#[tokio::test]
async fn retry_on_database_locked_does_not_retry_other_errors() {
    let attempts = AtomicU32::new(0);

    let err = retry_on_database_locked(|| {
        attempts.fetch_add(1, Ordering::SeqCst);
        async { Err::<(), _>(VoomError::Config("bad lease".to_owned())) }
    })
    .await
    .unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::ConfigInvalid);
    assert_eq!(attempts.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn fail_job_reports_transition_failure_without_claiming_durable_failure() {
    let fixture = ExecutorFixture::without_workers(0).await;
    let job_id = fixture.open_workflow_job().await;
    sqlx::query(
        "CREATE TRIGGER reject_job_failed_event \
         BEFORE INSERT ON events WHEN NEW.kind = 'job.failed' \
         BEGIN SELECT RAISE(ABORT, 'reject job.failed'); END",
    )
    .execute(&fixture.cp.pool)
    .await
    .unwrap();
    let mut state = RunLoopState::new(job_id, Duration::ZERO);

    let error = state
        .fail_job(
            &fixture.cp,
            job_id,
            VoomError::Internal("original workflow failure".to_owned()),
            Instant::now(),
        )
        .await;

    sqlx::query("DROP TRIGGER reject_job_failed_event")
        .execute(&fixture.cp.pool)
        .await
        .unwrap();
    assert!(!error.job_failed);
    assert_eq!(error.disposition, WorkflowFailureDisposition::Fatal);
    assert_eq!(
        fixture.job_state_and_epoch(job_id).await,
        ("open".to_owned(), 0)
    );
    assert_eq!(fixture.event_count("job.failed").await, 0);
    assert_sqlx_source(&error.source, "reject job.failed");
    let message = error.source.to_string();
    assert_fragments_in_order(
        &message,
        &[
            "workflow failed for job",
            "original workflow failure",
            "marking the job failed also failed",
            "reject job.failed",
        ],
    );
}

#[tokio::test]
async fn fail_job_preserves_transition_and_refresh_failures_in_order() {
    let fixture = ExecutorFixture::without_workers(0).await;
    let job_id = fixture.open_workflow_job().await;
    sqlx::query(
        "CREATE TRIGGER reject_job_failed_event \
         BEFORE INSERT ON events WHEN NEW.kind = 'job.failed' \
         BEGIN SELECT RAISE(ABORT, 'reject job.failed'); END",
    )
    .execute(&fixture.cp.pool)
    .await
    .unwrap();
    sqlx::query("ALTER TABLE leases RENAME TO unavailable_leases")
        .execute(&fixture.cp.pool)
        .await
        .unwrap();
    let mut state = RunLoopState::new(job_id, Duration::ZERO);

    let error = state
        .fail_job(
            &fixture.cp,
            job_id,
            VoomError::database_context("original workflow failure", sqlx::Error::RowNotFound),
            Instant::now(),
        )
        .await;

    sqlx::query("ALTER TABLE unavailable_leases RENAME TO leases")
        .execute(&fixture.cp.pool)
        .await
        .unwrap();
    sqlx::query("DROP TRIGGER reject_job_failed_event")
        .execute(&fixture.cp.pool)
        .await
        .unwrap();
    assert!(!error.job_failed);
    assert_eq!(
        fixture.job_state_and_epoch(job_id).await,
        ("open".to_owned(), 0)
    );
    assert_eq!(fixture.event_count("job.failed").await, 0);
    assert_sqlx_source(&error.source, "reject job.failed");
    let message = error.source.to_string();
    assert_fragments_in_order(
        &message,
        &[
            "original workflow failure",
            "marking the job failed also failed",
            "reject job.failed",
            "refreshing the workflow summary also failed",
            "no such table: leases",
        ],
    );
}

#[tokio::test]
async fn fail_job_preserves_original_when_refresh_fails_after_durable_transition() {
    let fixture = ExecutorFixture::without_workers(0).await;
    let job_id = fixture.open_workflow_job().await;
    sqlx::query("ALTER TABLE leases RENAME TO unavailable_leases")
        .execute(&fixture.cp.pool)
        .await
        .unwrap();
    let mut state = RunLoopState::new(job_id, Duration::ZERO);

    let error = state
        .fail_job(
            &fixture.cp,
            job_id,
            VoomError::database_context("original workflow failure", sqlx::Error::RowNotFound),
            Instant::now(),
        )
        .await;

    sqlx::query("ALTER TABLE unavailable_leases RENAME TO leases")
        .execute(&fixture.cp.pool)
        .await
        .unwrap();
    assert!(error.job_failed);
    assert_eq!(
        fixture.job_state_and_epoch(job_id).await,
        ("failed".to_owned(), 1)
    );
    assert_eq!(fixture.event_count("job.failed").await, 1);
    assert_sqlx_source(&error.source, "no such table: leases");
    let message = error.source.to_string();
    assert_fragments_in_order(
        &message,
        &[
            "original workflow failure",
            "refreshing the workflow summary also failed",
            "no such table: leases",
        ],
    );
}

#[tokio::test]
async fn fail_job_preserves_primary_database_source_when_transition_is_non_database() {
    let fixture = ExecutorFixture::without_workers(0).await;
    let job_id = fixture.open_workflow_job().await;
    fixture
        .cp
        .cancel_job(job_id, "operator cancelled workflow".to_owned(), T0)
        .await
        .unwrap();
    let mut state = RunLoopState::new(job_id, Duration::ZERO);

    let error = state
        .fail_job(
            &fixture.cp,
            job_id,
            VoomError::database_context("primary workflow query", sqlx::Error::RowNotFound),
            Instant::now(),
        )
        .await;

    assert!(!error.job_failed);
    assert_eq!(error.disposition, WorkflowFailureDisposition::Fatal);
    assert_eq!(
        fixture.job_state_and_epoch(job_id).await,
        ("cancelled".to_owned(), 1)
    );
    assert_eq!(fixture.event_count("job.failed").await, 0);
    assert_sqlx_source(&error.source, "no rows returned");
    assert_fragments_in_order(
        &error.source.to_string(),
        &[
            "primary workflow query",
            "marking the job failed also failed",
            "conflict",
            "cancelled",
        ],
    );
}

#[test]
fn failure_source_preserves_primary_database_source_when_refresh_is_non_database() {
    let error = workflow_failure_source(
        JobId(42),
        VoomError::database_context("primary workflow query", sqlx::Error::RowNotFound),
        None,
        Some(VoomError::Conflict(
            "summary changed concurrently".to_owned(),
        )),
    );

    assert_sqlx_source(&error, "no rows returned");
    assert_fragments_in_order(
        &error.to_string(),
        &[
            "primary workflow query",
            "refreshing the workflow summary also failed",
            "summary changed concurrently",
        ],
    );
}

#[tokio::test]
async fn isolated_failure_refresh_error_is_fatal_and_preserves_ticket_diagnostic() {
    let fixture = ExecutorFixture::without_workers(0).await;
    let job_id = fixture.open_workflow_job().await;
    sqlx::query("ALTER TABLE leases RENAME TO unavailable_leases")
        .execute(&fixture.cp.pool)
        .await
        .unwrap();
    let mut state = RunLoopState::new(job_id, Duration::ZERO);

    let error = state
        .finish_isolated_failure(
            &fixture.cp,
            job_id,
            VoomError::WorkerCrash("ticket worker crashed".to_owned()),
            Instant::now(),
        )
        .await;

    sqlx::query("ALTER TABLE unavailable_leases RENAME TO leases")
        .execute(&fixture.cp.pool)
        .await
        .unwrap();
    assert!(!error.job_failed);
    assert_eq!(error.disposition, WorkflowFailureDisposition::Fatal);
    assert_eq!(
        fixture.job_state_and_epoch(job_id).await,
        ("open".to_owned(), 0)
    );
    assert_eq!(fixture.event_count("job.failed").await, 0);
    assert_sqlx_source(&error.source, "no such table: leases");
    assert_fragments_in_order(
        &error.source.to_string(),
        &[
            "ticket worker crashed",
            "refreshing the workflow summary also failed",
            "no such table: leases",
        ],
    );
}

fn assert_sqlx_source(error: &VoomError, expected: &str) {
    assert_eq!(error.error_code(), ErrorCode::DbUnreachable);
    let source = error.source().unwrap();
    let sqlx_error = source.downcast_ref::<sqlx::Error>().unwrap();
    assert!(
        sqlx_error.to_string().contains(expected),
        "source {sqlx_error:?} did not contain {expected:?}"
    );
}

fn assert_fragments_in_order(actual: &str, expected: &[&str]) {
    let mut remaining = actual;
    for fragment in expected {
        let Some(index) = remaining.find(fragment) else {
            panic!("missing {fragment:?} after prior fragments in {actual:?}");
        };
        remaining = &remaining[index + fragment.len()..];
    }
}

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
async fn local_reservations_do_not_double_count_held_leases() {
    let fixture = ExecutorFixture::single_worker_max_parallel(2).await;
    let worker_id = fixture.worker_id();
    let summary = fixture
        .run_with_policy(ConcurrencyPolicy {
            max_in_flight_dispatches: 4,
        })
        .await
        .unwrap();

    assert_eq!(summary.max_active_for_worker(worker_id), 2);
    assert_eq!(summary.dispatch_count, 4);
}

#[tokio::test]
async fn capacity_deferred_ready_ticket_does_not_block_later_ready_ticket() {
    let fixture = ExecutorFixture::capacity_deferred_then_free_worker().await;
    let mut options = timeout_options();
    options.queue.ready_batch_size = 8;

    let err = fixture.run_with_options(options).await.unwrap_err();

    assert_eq!(err.source.error_code(), ErrorCode::WorkerTimeout);
    assert_eq!(err.summary.dispatch_count, 2);
    assert_eq!(
        err.summary
            .per_operation
            .get(&OperationKind::IdentifyMedia)
            .unwrap()
            .dispatch_count,
        1
    );
}

#[tokio::test]
async fn external_capacity_release_allows_eventual_dispatch_without_early_request() {
    let mut fixture = ExecutorFixture::without_workers(1).await;
    let worker_id = fixture
        .register_worker(
            "cross-connection-worker",
            OperationKind::HashFile,
            1,
            FakeBehavior::Success,
        )
        .await;
    let other = fixture.second_control_plane().await;
    let external_lease = fixture
        .occupy_worker_capacity(&other, worker_id, OperationKind::HashFile)
        .await;
    let capacity_deferred = CapacityDeferredTestSync {
        observed: Arc::new(tokio::sync::Notify::new()),
        resume: Arc::new(tokio::sync::Notify::new()),
    };
    let mut options = WorkflowExecutorOptions::for_tests();
    options.capacity_deferred_sync = Some(capacity_deferred.clone());
    let executor = fixture.executor_with_options(options);
    let plan = fixture.plan.clone();
    let run = tokio::spawn(async move { executor.submit_and_run(plan).await });
    let ticket = fixture.wait_for_workflow_ticket().await;

    tokio::time::timeout(
        Duration::from_secs(5),
        capacity_deferred.observed.notified(),
    )
    .await
    .unwrap();
    assert!(!run.is_finished());
    assert_eq!(fixture.worker_dispatch_count(worker_id), 0);
    assert_eq!(ticket.attempt, 0);
    assert_eq!(fixture.ticket_state(ticket.id).await, "ready");
    assert_eq!(
        fixture.ticket_event_count(ticket.id, "ticket.leased").await,
        0
    );
    assert_eq!(
        fixture
            .ticket_event_count(ticket.id, "ticket.failed_terminal")
            .await,
        0
    );

    other
        .release_lease(external_lease, json!({"status": "released"}), T0)
        .await
        .unwrap();
    capacity_deferred.resume.notify_one();
    let summary = tokio::time::timeout(Duration::from_secs(5), run)
        .await
        .unwrap()
        .unwrap()
        .unwrap();

    assert_eq!(summary.dispatch_count, 1);
    assert_eq!(fixture.worker_dispatch_count(worker_id), 1);
    assert_eq!(fixture.ticket_state(ticket.id).await, "succeeded");
}

#[tokio::test]
async fn external_capacity_timeout_fails_job_without_consuming_ticket() {
    let mut fixture = ExecutorFixture::without_workers(1).await;
    let worker_id = fixture
        .register_worker(
            "capacity-timeout-worker",
            OperationKind::HashFile,
            1,
            FakeBehavior::Success,
        )
        .await;
    let other = fixture.second_control_plane().await;
    let external_lease = fixture
        .occupy_worker_capacity(&other, worker_id, OperationKind::HashFile)
        .await;
    let mut options = WorkflowExecutorOptions::for_tests();
    options.queue.capacity_retry_interval = Duration::from_millis(10);
    options.queue.capacity_retry_timeout = Duration::from_millis(40);

    let error = fixture.run_with_options(options).await.unwrap_err();
    let ticket = fixture.first_workflow_ticket().await;

    assert_eq!(error.source.error_code(), ErrorCode::NoEligibleWorker);
    assert!(error.source.to_string().contains("capacity"));
    assert_eq!(error.summary.dispatch_count, 0);
    assert_eq!(fixture.worker_dispatch_count(worker_id), 0);
    assert_eq!(fixture.job_state(error.summary.job_id).await, "failed");
    assert_eq!(ticket.state.as_str(), "ready");
    assert_eq!(ticket.attempt, 0);
    assert_eq!(fixture.held_lease_count().await, 1);
    assert_eq!(
        fixture.ticket_event_count(ticket.id, "ticket.leased").await,
        0
    );
    assert_eq!(
        fixture
            .ticket_event_count(ticket.id, "ticket.failed_terminal")
            .await,
        0
    );
    assert_eq!(
        fixture
            .ticket_event_count(ticket.id, "ticket.failed_retriable")
            .await,
        0
    );
    assert_eq!(fixture.event_count("job.failed").await, 1);

    other
        .release_lease(external_lease, json!({"status": "cleanup"}), T0)
        .await
        .unwrap();
}

#[tokio::test]
async fn externally_leased_workflow_ticket_does_not_trigger_no_dispatch_failure() {
    let mut fixture = ExecutorFixture::without_workers(1).await;
    let worker_id = fixture
        .register_worker(
            "shared-invocation-worker",
            OperationKind::HashFile,
            2,
            FakeBehavior::Success,
        )
        .await;
    let job_id = fixture.open_workflow_job().await;
    let invocation_id = "shared-invocation";
    let workflow_id = format!("workflow-{}-{invocation_id}", job_id.0);
    let external_ticket = fixture
        .cp
        .create_ticket(NewTicket {
            job_id: Some(job_id),
            kind: workflow_ticket_op(OperationKind::HashFile),
            priority: 0,
            payload: WorkflowTicketPayload::new_for_test(
                &workflow_id,
                "external-plan",
                "external-hash",
                "external",
                OperationKind::HashFile,
                json!({
                    "operation": "hash_file",
                    "path": "/library/external.mkv"
                }),
            )
            .to_ticket_payload()
            .unwrap(),
            max_attempts: 1,
            created_at: T0,
        })
        .await
        .unwrap();
    fixture
        .cp
        .mark_ready_if_unblocked(external_ticket.id, T0)
        .await
        .unwrap();
    let external_lease = fixture
        .cp
        .acquire_lease(NewLease {
            ticket_id: external_ticket.id,
            worker_id,
            ttl: time::Duration::seconds(5),
            now: T0,
        })
        .await
        .unwrap();
    let mut options = WorkflowExecutorOptions::for_tests();
    options.queue.capacity_retry_interval = Duration::from_millis(10);
    options.queue.capacity_retry_timeout = Duration::from_millis(40);
    let executor = fixture.executor_with_options(options);
    let run = tokio::spawn(async move {
        executor
            .submit_and_run_invocation_in_job(
                job_id,
                invocation_id,
                independent_hash_plan(1),
                super::RunFailureMode::ContinueIndependent,
            )
            .await
    });

    for _ in 0..400 {
        if fixture.worker_dispatch_count(worker_id) == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        !run.is_finished(),
        "a healthy same-scope lease must not inherit the worker-capacity timeout"
    );

    fixture
        .cp
        .release_lease(external_lease.id, json!({"status": "released"}), T0)
        .await
        .unwrap();
    let summary = tokio::time::timeout(Duration::from_secs(2), run)
        .await
        .unwrap()
        .unwrap()
        .unwrap();

    assert_eq!(summary.dispatch_count, 1);
    assert_eq!(fixture.worker_dispatch_count(worker_id), 1);
}

#[tokio::test]
async fn stale_externally_leased_workflow_ticket_is_expired_and_finishes() {
    let mut fixture = ExecutorFixture::without_workers(1).await;
    let worker_id = fixture
        .register_worker(
            "stale-invocation-worker",
            OperationKind::HashFile,
            2,
            FakeBehavior::Success,
        )
        .await;
    let job_id = fixture.open_workflow_job().await;
    let invocation_id = "stale-invocation";
    let workflow_id = format!("workflow-{}-{invocation_id}", job_id.0);
    let external_ticket = fixture
        .cp
        .create_ticket(NewTicket {
            job_id: Some(job_id),
            kind: workflow_ticket_op(OperationKind::HashFile),
            priority: 0,
            payload: WorkflowTicketPayload::new_for_test(
                &workflow_id,
                "external-plan",
                "external-hash",
                "external",
                OperationKind::HashFile,
                json!({
                    "operation": "hash_file",
                    "path": "/library/stale.mkv"
                }),
            )
            .to_ticket_payload()
            .unwrap(),
            max_attempts: 1,
            created_at: T0,
        })
        .await
        .unwrap();
    fixture
        .cp
        .mark_ready_if_unblocked(external_ticket.id, T0)
        .await
        .unwrap();
    fixture
        .cp
        .acquire_lease(NewLease {
            ticket_id: external_ticket.id,
            worker_id,
            ttl: time::Duration::seconds(5),
            now: T0,
        })
        .await
        .unwrap();
    let mut options = WorkflowExecutorOptions::for_tests();
    options.queue.capacity_retry_interval = Duration::from_millis(10);
    options.queue.capacity_retry_timeout = Duration::from_millis(40);
    let executor = fixture.executor_with_options(options);
    let run = tokio::spawn(async move {
        executor
            .submit_and_run_invocation_in_job(
                job_id,
                invocation_id,
                independent_hash_plan(1),
                super::RunFailureMode::ContinueIndependent,
            )
            .await
    });

    for _ in 0..100 {
        if fixture.worker_dispatch_count(worker_id) == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    fixture.clock.advance(time::Duration::seconds(6));
    let error = tokio::time::timeout(Duration::from_secs(2), run)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();

    assert!(!error.job_failed);
    assert_eq!(
        fixture.ticket_state(external_ticket.id).await,
        "failed",
        "the executor must invoke normal expiry recovery for stale shared work"
    );
    assert_eq!(fixture.held_lease_count().await, 0);
    assert_eq!(fixture.worker_dispatch_count(worker_id), 1);
}

#[tokio::test]
async fn pending_same_scope_ticket_still_reports_no_dispatch_failure() {
    let mut fixture = ExecutorFixture::without_workers(1).await;
    fixture
        .register_worker(
            "stalled-invocation-worker",
            OperationKind::HashFile,
            1,
            FakeBehavior::Success,
        )
        .await;
    let job_id = fixture.open_workflow_job().await;
    let invocation_id = "stalled-invocation";
    let workflow_id = format!("workflow-{}-{invocation_id}", job_id.0);
    fixture
        .cp
        .create_ticket(NewTicket {
            job_id: Some(job_id),
            kind: workflow_ticket_op(OperationKind::HashFile),
            priority: 0,
            payload: WorkflowTicketPayload::new_for_test(
                &workflow_id,
                "stalled-plan",
                "stalled-hash",
                "stalled",
                OperationKind::HashFile,
                json!({
                    "operation": "hash_file",
                    "path": "/library/stalled.mkv"
                }),
            )
            .to_ticket_payload()
            .unwrap(),
            max_attempts: 1,
            created_at: T0,
        })
        .await
        .unwrap();
    let executor = fixture.executor_with_options(WorkflowExecutorOptions::for_tests());

    let error = executor
        .submit_and_run_invocation_in_job(
            job_id,
            invocation_id,
            independent_hash_plan(1),
            super::RunFailureMode::ContinueIndependent,
        )
        .await
        .unwrap_err();

    assert_eq!(error.source.error_code(), ErrorCode::Internal);
    assert!(error.source.to_string().contains("no dispatchable work"));
    assert!(error.job_failed);
}

#[tokio::test]
async fn cancelling_job_stops_external_capacity_wait_without_failure_events() {
    let mut fixture = ExecutorFixture::without_workers(1).await;
    let worker_id = fixture
        .register_worker(
            "capacity-cancel-worker",
            OperationKind::HashFile,
            1,
            FakeBehavior::Success,
        )
        .await;
    let other = fixture.second_control_plane().await;
    let external_lease = fixture
        .occupy_worker_capacity(&other, worker_id, OperationKind::HashFile)
        .await;
    let job_id = fixture.open_workflow_job().await;
    let executor = fixture.executor_with_options(WorkflowExecutorOptions::for_tests());
    let plan = fixture.plan.clone();
    let run = tokio::spawn(async move {
        executor
            .submit_and_run_invocation_in_job(
                job_id,
                "capacity-cancel",
                plan,
                super::RunFailureMode::AbortJob,
            )
            .await
    });
    let ticket = fixture.wait_for_workflow_ticket().await;
    fixture
        .cp
        .cancel_job(job_id, "operator cancelled wait".to_owned(), T0)
        .await
        .unwrap();

    let error = tokio::time::timeout(Duration::from_secs(1), run)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();

    assert_eq!(error.source.error_code(), ErrorCode::UserCancellation);
    assert!(!error.job_failed);
    assert_eq!(fixture.job_state(job_id).await, "cancelled");
    assert_eq!(fixture.ticket_state(ticket.id).await, "ready");
    assert_eq!(fixture.worker_dispatch_count(worker_id), 0);
    assert_eq!(
        fixture.ticket_event_count(ticket.id, "ticket.leased").await,
        0
    );
    assert_eq!(
        fixture
            .ticket_event_count(ticket.id, "ticket.failed_terminal")
            .await,
        0
    );
    assert_eq!(fixture.event_count("job.cancelled").await, 1);
    assert_eq!(fixture.event_count("job.failed").await, 0);

    other
        .release_lease(external_lease, json!({"status": "cleanup"}), T0)
        .await
        .unwrap();
}

#[tokio::test]
async fn stopped_capacity_wait_leaves_restartable_durable_state() {
    let mut fixture = ExecutorFixture::without_workers(1).await;
    let worker_id = fixture
        .register_worker(
            "capacity-restart-worker",
            OperationKind::HashFile,
            1,
            FakeBehavior::Success,
        )
        .await;
    let other = fixture.second_control_plane().await;
    let external_lease = fixture
        .occupy_worker_capacity(&other, worker_id, OperationKind::HashFile)
        .await;
    let job_id = fixture.open_workflow_job().await;
    let sync = CapacityDeferredTestSync {
        observed: Arc::new(tokio::sync::Notify::new()),
        resume: Arc::new(tokio::sync::Notify::new()),
    };
    let mut options = WorkflowExecutorOptions::for_tests();
    options.capacity_deferred_sync = Some(sync.clone());
    let executor = fixture.executor_with_options(options);
    let plan = fixture.plan.clone();
    let run = tokio::spawn(async move {
        executor
            .submit_and_run_invocation_in_job(
                job_id,
                "capacity-restart",
                plan,
                super::RunFailureMode::AbortJob,
            )
            .await
    });
    let ticket_before = fixture.wait_for_workflow_ticket().await;
    tokio::time::timeout(Duration::from_secs(5), sync.observed.notified())
        .await
        .unwrap();
    assert!(!run.is_finished());

    run.abort();
    assert!(run.await.unwrap_err().is_cancelled());
    let ticket_after = fixture
        .cp
        .tickets
        .get(ticket_before.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(fixture.job_state(job_id).await, "open");
    assert_eq!(ticket_after.state, ticket_before.state);
    assert_eq!(ticket_after.attempt, ticket_before.attempt);
    assert_eq!(ticket_after.epoch, ticket_before.epoch);
    assert_eq!(fixture.worker_dispatch_count(worker_id), 0);
    assert_eq!(
        fixture
            .ticket_event_count(ticket_before.id, "ticket.leased")
            .await,
        0
    );
    assert_eq!(
        fixture
            .ticket_event_count(ticket_before.id, "ticket.failed_terminal")
            .await,
        0
    );

    fixture
        .cp
        .cancel_job(job_id, "executor process stopped".to_owned(), T0)
        .await
        .unwrap();
    other
        .release_lease(external_lease, json!({"status": "released"}), T0)
        .await
        .unwrap();
    let summary = fixture.run().await.unwrap();

    assert_eq!(summary.dispatch_count, 1);
    assert_eq!(fixture.worker_dispatch_count(worker_id), 1);
    assert_eq!(fixture.job_state(job_id).await, "cancelled");
    assert_eq!(fixture.ticket_state(ticket_before.id).await, "ready");
}

#[tokio::test]
async fn dropped_active_dispatch_recovers_through_durable_lease_expiry() {
    let mut fixture = ExecutorFixture::single_worker_with_behavior(FakeBehavior::Hang).await;
    let worker_id = fixture.worker_id();
    let job_id = fixture.open_workflow_job().await;
    let mut options = WorkflowExecutorOptions::for_tests();
    options.queue.max_attempts = 2;
    options.timing.lease_ttl = Duration::from_secs(5);
    let executor = fixture.executor_with_options(options.clone());
    let plan = fixture.plan.clone();
    let run = tokio::spawn(async move {
        executor
            .submit_and_run_invocation_in_job(
                job_id,
                "active-dispatch-restart",
                plan,
                super::RunFailureMode::AbortJob,
            )
            .await
    });
    for _ in 0..200 {
        if fixture.worker_dispatch_count(worker_id) == 1 && fixture.held_lease_count().await == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(fixture.worker_dispatch_count(worker_id), 1);
    assert_eq!(fixture.held_lease_count().await, 1);
    let ticket = fixture.first_workflow_ticket().await;

    run.abort();
    assert!(run.await.unwrap_err().is_cancelled());
    assert_eq!(fixture.ticket_state(ticket.id).await, "leased");
    assert_eq!(fixture.event_count("ticket.failed_terminal").await, 0);

    fixture.clock.advance(time::Duration::seconds(6));
    let expiry = fixture
        .cp
        .expire_due(fixture.cp.clock().now())
        .await
        .unwrap();
    assert_eq!(expiry.requeued_tickets, vec![ticket.id]);
    assert_eq!(fixture.ticket_state(ticket.id).await, "ready");
    assert_eq!(fixture.held_lease_count().await, 0);

    let client = Arc::new(FakeClient::new(worker_id, FakeBehavior::Success));
    fixture.registry.register_in_process_runtime(
        worker_id,
        client.clone(),
        WorkerCredentials {
            worker_id,
            worker_epoch: 0,
            secret: SecretString::from("test-secret"),
        },
    );
    fixture.clients.insert(worker_id, client);
    let restart = fixture.executor_with_options(options);
    let workflow_id = format!("workflow-{}-active-dispatch-restart", job_id.0);
    let invocation = RunInvocation {
        job_id,
        workflow_id: &workflow_id,
        plan: &fixture.plan,
        failure_mode: super::RunFailureMode::AbortJob,
    };
    let mut state = RunLoopState::new(job_id, Duration::ZERO);
    let dispatch = restart
        .dispatch_ready_tickets(&mut state, &invocation)
        .await;
    assert!(dispatch.made_progress);
    state.wait_for_one(&restart, &invocation).await;

    assert_eq!(state.summary.dispatch_count, 1);
    assert!(state.fatal_error.is_none());
    assert_eq!(fixture.worker_dispatch_count(worker_id), 1);
    assert_eq!(fixture.ticket_state(ticket.id).await, "succeeded");
    assert_eq!(fixture.held_lease_count().await, 0);
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
async fn separate_deny_grant_removes_worker_before_lease_dispatch() {
    let fixture = ExecutorFixture::with_ready_tickets(1).await;
    fixture
        .deny_worker_operation(fixture.worker_id(), OperationKind::HashFile)
        .await;

    let err = fixture.run().await.unwrap_err();

    assert_eq!(err.source.error_code(), ErrorCode::NoEligibleWorker);
    assert_eq!(err.summary.dispatch_count, 0);
    assert_eq!(err.summary.peak_active_workflow_leases, 0);
    assert_eq!(err.summary.failure_count, 1);
    assert_eq!(fixture.lease_count().await, 0);
    assert_eq!(fixture.event_count("lease.acquired").await, 0);
    assert_eq!(fixture.event_count("ticket.leased").await, 0);
    assert_eq!(fixture.event_count("ticket.failed_terminal").await, 1);
    assert_eq!(
        fixture.first_ticket_failed_class().await,
        "no_eligible_worker"
    );
}

#[tokio::test]
async fn terminal_worker_ineligibility_never_enters_capacity_wait() {
    for ineligibility in [
        TerminalWorkerIneligibility::Stale,
        TerminalWorkerIneligibility::Retired,
        TerminalWorkerIneligibility::Denied,
        TerminalWorkerIneligibility::Incapable,
        TerminalWorkerIneligibility::Ungranted,
    ] {
        let fixture = ExecutorFixture::without_workers(1).await;
        seed_terminally_ineligible_worker(&fixture, ineligibility).await;

        let error = fixture.run().await.unwrap_err();
        let ticket = fixture.first_workflow_ticket().await;

        assert_eq!(
            error.source.error_code(),
            ErrorCode::NoEligibleWorker,
            "{}",
            ineligibility.label()
        );
        assert_eq!(error.summary.dispatch_count, 0);
        assert_eq!(ticket.state.as_str(), "failed");
        assert_eq!(ticket.attempt, 1);
        assert_eq!(fixture.lease_count().await, 0);
        assert_eq!(
            fixture
                .ticket_event_count(ticket.id, "ticket.failed_terminal")
                .await,
            1
        );
        assert_eq!(
            fixture.first_ticket_failed_class().await,
            "no_eligible_worker"
        );
    }
}

#[tokio::test]
async fn equal_load_dispatches_to_lowest_worker_id() {
    let fixture = ExecutorFixture::two_workers(1).await;
    let first = fixture.worker_id();
    let summary = fixture.run().await.unwrap();

    assert_eq!(summary.dispatch_count, 1);
    assert_eq!(summary.failure_count, 0);
    assert_eq!(fixture.worker_dispatch_count(first), 1);
    assert_eq!(
        fixture
            .clients
            .iter()
            .filter(|(worker_id, _)| **worker_id != first)
            .map(|(_, client)| client.dispatch_count())
            .sum::<u32>(),
        0
    );
}

#[tokio::test]
async fn least_loaded_selection_uses_two_workers_concurrently() {
    let fixture = ExecutorFixture::two_workers(4).await;
    let summary = fixture
        .run_with_policy(ConcurrencyPolicy {
            max_in_flight_dispatches: 4,
        })
        .await
        .unwrap();

    assert_eq!(summary.peak_active_workflow_leases, 2);
    assert_eq!(summary.dispatch_count, 4);
    assert!(
        fixture
            .clients
            .values()
            .all(|client| client.dispatch_count() > 0)
    );
    assert!(
        fixture
            .clients
            .keys()
            .all(|worker_id| summary.max_active_for_worker(*worker_id) == 1)
    );
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
    options.queue.max_attempts = 2;

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
    options.queue.max_attempts = 2;

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
    options.timing.progress_idle_timeout = Duration::from_millis(250);
    options.timing.heartbeat_timeout = Duration::ZERO;
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
    options.timing.progress_idle_timeout = Duration::ZERO;
    options.timing.heartbeat_timeout = Duration::ZERO;
    options.timing.heartbeat_interval = Duration::from_secs(1);
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
    options.timing.progress_idle_timeout = Duration::from_secs(1);
    options.timing.heartbeat_timeout = Duration::ZERO;
    options.timing.heartbeat_interval = Duration::from_secs(1);
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
async fn panicked_dispatch_fails_its_real_non_hash_ticket() {
    let (fixture, mut options) = panicking_identify_fixture().await;
    options.queue.max_attempts = 1;

    let Ok(result) =
        tokio::time::timeout(Duration::from_secs(5), fixture.run_with_options(options)).await
    else {
        panic!("panicked dispatch cleanup must not stall");
    };
    let err = result.unwrap_err();

    assert_eq!(err.source.error_code(), ErrorCode::WorkerCrash);
    assert_eq!(err.disposition, WorkflowFailureDisposition::Fatal);
    assert!(err.job_failed);
    assert_eq!(fixture.held_lease_count().await, 0);
    assert_eq!(fixture.first_ticket_failed_class().await, "worker_crash");
    let identify = err
        .summary
        .per_operation
        .get(&OperationKind::IdentifyMedia)
        .unwrap();
    assert_eq!(identify.failure_count, 1);
    assert_eq!(identify.last_failure_class, Some(FailureClass::WorkerCrash));
    assert!(
        !err.summary
            .per_operation
            .contains_key(&OperationKind::HashFile)
    );
}

#[tokio::test]
async fn panicked_dispatch_releases_capacity_before_retry() {
    let (fixture, mut options) = panicking_identify_fixture().await;
    options.queue.max_attempts = 2;

    let Ok(result) =
        tokio::time::timeout(Duration::from_secs(5), fixture.run_with_options(options)).await
    else {
        panic!("panicked dispatch retry must not stall at worker capacity");
    };
    let err = result.unwrap_err();

    assert_eq!(err.source.error_code(), ErrorCode::WorkerCrash);
    assert_eq!(err.summary.dispatch_count, 2);
    assert_eq!(err.summary.retry_count, 1);
    assert_eq!(fixture.held_lease_count().await, 0);
    let identify = err
        .summary
        .per_operation
        .get(&OperationKind::IdentifyMedia)
        .unwrap();
    assert_eq!(identify.dispatch_count, 2);
    assert_eq!(identify.failure_count, 2);
}

#[tokio::test]
async fn terminal_panicked_dispatch_is_isolated_in_continue_mode() {
    let (fixture, mut options) = panicking_identify_fixture().await;
    options.queue.max_attempts = 1;
    let job_id = fixture.open_workflow_job().await;
    let executor = fixture.executor_with_options(options);

    let err = executor
        .submit_and_run_invocation_in_job(
            job_id,
            "panic-continue",
            fixture.plan.clone(),
            super::RunFailureMode::ContinueIndependent,
        )
        .await
        .unwrap_err();

    assert_eq!(err.source.error_code(), ErrorCode::WorkerCrash);
    assert_eq!(err.disposition, WorkflowFailureDisposition::IsolatedTicket);
    assert!(!err.job_failed);
    assert_eq!(fixture.held_lease_count().await, 0);
}

#[tokio::test]
async fn every_join_consumer_clears_identity_and_reservation() {
    let fixture = ExecutorFixture::with_ready_tickets(0).await;
    let worker_id = fixture.first_worker_id.unwrap();
    let job_id = fixture.open_workflow_job().await;
    let executor = fixture.executor_with_options(WorkflowExecutorOptions::for_tests());
    let invocation = RunInvocation {
        job_id,
        workflow_id: "completion-bookkeeping",
        plan: &fixture.plan,
        failure_mode: super::RunFailureMode::ContinueIndependent,
    };

    let mut completed = tracked_failed_dispatch(&fixture, worker_id).await;
    completed
        .process_completed_dispatches(&executor, &invocation)
        .await;
    assert_completion_bookkeeping_cleared(&completed);

    let mut drained = tracked_failed_dispatch(&fixture, worker_id).await;
    drained.drain_active(&executor, &invocation).await;
    assert_completion_bookkeeping_cleared(&drained);

    let mut waited = tracked_failed_dispatch(&fixture, worker_id).await;
    waited.wait_for_one(&executor, &invocation).await;
    assert_completion_bookkeeping_cleared(&waited);
}

#[tokio::test]
async fn completed_task_without_identity_is_a_contextual_fatal_error() {
    let fixture = ExecutorFixture::without_workers(0).await;
    let job_id = fixture.open_workflow_job().await;
    let executor = fixture.executor_with_options(WorkflowExecutorOptions::for_tests());
    let invocation = RunInvocation {
        job_id,
        workflow_id: "missing-identity",
        plan: &fixture.plan,
        failure_mode: super::RunFailureMode::ContinueIndependent,
    };
    let mut state = RunLoopState::new(job_id, Duration::ZERO);
    let handle = state.active.spawn(async { successful_dispatch_outcome() });
    let task_id = handle.id();
    while !handle.is_finished() {
        tokio::task::yield_now().await;
    }

    state
        .process_completed_dispatches(&executor, &invocation)
        .await;

    assert!(state.active.is_empty());
    assert!(state.active_identities.is_empty());
    let error = state.fatal_error.unwrap();
    assert!(error.to_string().contains(&task_id.to_string()));
    assert!(error.to_string().contains("without a dispatch identity"));
}

#[tokio::test]
async fn mismatched_completed_task_identity_is_a_contextual_fatal_error() {
    let fixture = ExecutorFixture::without_workers(0).await;
    let job_id = fixture.open_workflow_job().await;
    let executor = fixture.executor_with_options(WorkflowExecutorOptions::for_tests());
    let invocation = RunInvocation {
        job_id,
        workflow_id: "mismatched-identity",
        plan: &fixture.plan,
        failure_mode: super::RunFailureMode::ContinueIndependent,
    };
    for identity in [
        DispatchIdentity {
            ticket_id: TicketId(19),
            worker_id: WorkerId(23),
            lease_id: LeaseId(31),
            operation: OperationKind::IdentifyMedia,
        },
        DispatchIdentity {
            ticket_id: TicketId(17),
            worker_id: WorkerId(29),
            lease_id: LeaseId(31),
            operation: OperationKind::IdentifyMedia,
        },
        DispatchIdentity {
            ticket_id: TicketId(17),
            worker_id: WorkerId(23),
            lease_id: LeaseId(31),
            operation: OperationKind::HashFile,
        },
    ] {
        let mut state = RunLoopState::new(job_id, Duration::ZERO);
        state.reservations.insert(identity.worker_id, 1);
        let handle = state.active.spawn(async { successful_dispatch_outcome() });
        state.active_identities.insert(handle.id(), identity);

        state.wait_for_one(&executor, &invocation).await;

        assert_completion_bookkeeping_cleared(&state);
        let error = state.fatal_error.unwrap();
        assert!(error.to_string().contains("identity mismatch"));
    }
}

#[tokio::test]
async fn drain_infrastructure_failure_supersedes_retained_ticket_error() {
    let fixture = ExecutorFixture::without_workers(0).await;
    let job_id = fixture.open_workflow_job().await;
    let executor = fixture.executor_with_options(WorkflowExecutorOptions::for_tests());
    let invocation = RunInvocation {
        job_id,
        workflow_id: "drain-failure-precedence",
        plan: &fixture.plan,
        failure_mode: super::RunFailureMode::AbortJob,
    };
    let mut state = RunLoopState::new(job_id, Duration::ZERO);
    state.active.spawn(async { successful_dispatch_outcome() });

    let error = state
        .fail_after_drain(
            &executor,
            &invocation,
            VoomError::WorkerCrash("retained ticket failure".to_owned()),
            std::time::Instant::now(),
        )
        .await;

    assert_eq!(error.source.error_code(), ErrorCode::Internal);
    assert!(
        error
            .source
            .to_string()
            .contains("without a dispatch identity")
    );
    assert!(!error.source.to_string().contains("retained ticket failure"));
    assert!(error.job_failed);
}

#[tokio::test]
async fn cancelled_dispatch_fails_real_lease_and_is_fatal_in_continue_mode() {
    let fixture = ExecutorFixture::with_ready_tickets(0).await;
    let worker_id = fixture.first_worker_id.unwrap();
    let job_id = fixture.open_workflow_job().await;
    let (ticket_id, lease_id) = fixture.create_heartbeat_test_lease(worker_id).await;
    let executor = fixture.executor_with_options(WorkflowExecutorOptions::for_tests());
    let invocation = RunInvocation {
        job_id,
        workflow_id: "cancelled-dispatch",
        plan: &fixture.plan,
        failure_mode: super::RunFailureMode::ContinueIndependent,
    };
    let identity = DispatchIdentity {
        ticket_id,
        worker_id,
        lease_id,
        operation: OperationKind::HashFile,
    };
    let mut state = RunLoopState::new(job_id, Duration::ZERO);
    state.reservations.insert(worker_id, 1);
    let handle = state
        .active
        .spawn(async { std::future::pending::<DispatchOutcome>().await });
    state.active_identities.insert(handle.id(), identity);

    state.active.abort_all();
    state.drain_active(&executor, &invocation).await;

    assert_completion_bookkeeping_cleared(&state);
    assert_eq!(fixture.held_lease_count().await, 0);
    assert_eq!(fixture.ticket_state(ticket_id).await, "failed");
    assert_eq!(fixture.first_ticket_failed_class().await, "worker_crash");
    assert_eq!(
        state.fatal_error.as_ref().unwrap().error_code(),
        ErrorCode::WorkerCrash
    );
}

#[tokio::test]
async fn join_cleanup_database_failure_preserves_source_and_operation_count() {
    let fixture = ExecutorFixture::with_ready_tickets(0).await;
    let worker_id = fixture.first_worker_id.unwrap();
    let job_id = fixture.open_workflow_job().await;
    let (ticket_id, lease_id) = fixture.create_heartbeat_test_lease(worker_id).await;
    let executor = fixture.executor_with_options(WorkflowExecutorOptions::for_tests());
    let invocation = RunInvocation {
        job_id,
        workflow_id: "join-cleanup-database-failure",
        plan: &fixture.plan,
        failure_mode: super::RunFailureMode::ContinueIndependent,
    };
    let identity = DispatchIdentity {
        ticket_id,
        worker_id,
        lease_id,
        operation: OperationKind::HashFile,
    };
    let mut state = RunLoopState::new(job_id, Duration::ZERO);
    state.reservations.insert(worker_id, 1);
    let handle = state
        .active
        .spawn(async { std::future::pending::<DispatchOutcome>().await });
    state.active_identities.insert(handle.id(), identity);
    state.active.abort_all();
    fixture.cp.pool.close().await;

    state.drain_active(&executor, &invocation).await;

    assert_completion_bookkeeping_cleared(&state);
    let failure = state.fatal_error.as_ref().unwrap();
    assert_eq!(failure.error_code(), ErrorCode::DbUnreachable);
    assert!(failure.source().is_some());
    assert!(failure.to_string().contains(&lease_id.to_string()));
    let hash = state
        .summary
        .per_operation
        .get(&OperationKind::HashFile)
        .unwrap();
    assert_eq!(hash.failure_count, 1);
    assert_eq!(hash.last_failure_class, Some(FailureClass::WorkerCrash));
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
    options.queue.ready_batch_size = 1;
    options.timing.progress_idle_timeout = Duration::from_secs(5);
    options.timing.heartbeat_timeout = Duration::from_secs(5);

    let summary = fixture.run_with_options(options).await.unwrap();

    assert_eq!(summary.dispatch_count, 1);
    assert_eq!(fixture.other_job_ready_count().await, 1);
}

#[tokio::test]
async fn malformed_ready_ticket_payload_reports_read_error() {
    let fixture = ExecutorFixture::without_workers(0).await;
    let job_id = fixture.open_workflow_job().await;
    fixture.seed_malformed_ready_workflow_ticket(job_id).await;
    let executor = fixture.executor_with_options(WorkflowExecutorOptions::for_tests());

    let err = executor
        .submit_and_run_invocation_in_job(
            job_id,
            "malformed-ready",
            independent_hash_plan(0),
            super::RunFailureMode::AbortJob,
        )
        .await
        .unwrap_err();

    assert_eq!(err.source.error_code(), ErrorCode::Internal);
    assert!(err.source.to_string().contains("workflow ready tickets"));
    assert!(err.source.to_string().contains("payload decode"));
    assert!(!err.source.to_string().contains("no dispatchable work"));
}

#[test]
fn planned_lineage_guard_rejects_incomplete_expectations() {
    let asset = FileAssetId(1);
    let version = FileVersionId(2);

    let empty = super::PlannedLineageGuard::new(0, Vec::new()).unwrap_err();
    let missing = super::PlannedLineageGuard::new(2, vec![(asset, version)]).unwrap_err();
    let duplicate =
        super::PlannedLineageGuard::new(2, vec![(asset, version), (asset, version)]).unwrap_err();

    assert!(empty.to_string().contains("at least one planned file"));
    assert!(missing.to_string().contains("2 planned files"));
    assert!(duplicate.to_string().contains("duplicate file asset"));
}

#[tokio::test]
async fn guarded_root_dispatch_waits_for_promoter_then_rejects_every_root() {
    let fixture = ExecutorFixture::without_workers(2).await;
    let asset = fixture.cp.identity.create_file_asset(T0).await.unwrap();
    let planned = fixture
        .cp
        .identity
        .create_file_version(NewFileVersion {
            file_asset_id: asset.id,
            content_hash: "planned".to_owned(),
            size_bytes: 1,
            produced_by: ProducedBy::Ingest,
            produced_from_version_id: None,
            created_at: T0,
        })
        .await
        .unwrap();
    let job_id = fixture.open_workflow_job().await;
    let mut promoter = crate::cases::begin_immediate_tx(&fixture.cp.pool)
        .await
        .unwrap();
    let current = fixture
        .cp
        .identity
        .create_file_version_in_tx(
            &mut promoter,
            NewFileVersion {
                file_asset_id: asset.id,
                content_hash: "current".to_owned(),
                size_bytes: 2,
                produced_by: ProducedBy::Transcode,
                produced_from_version_id: Some(planned.id),
                created_at: T0,
            },
        )
        .await
        .unwrap();
    let guard = super::PlannedLineageGuard::new(1, vec![(asset.id, planned.id)]).unwrap();
    let executor = fixture.executor_with_options(WorkflowExecutorOptions::for_tests());
    let mut invocation = tokio::spawn(async move {
        executor
            .submit_and_run_guarded_invocation_in_job(
                job_id,
                "guarded",
                independent_hash_plan(2),
                super::RunFailureMode::AbortJob,
                guard,
            )
            .await
    });

    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut invocation)
            .await
            .is_err(),
        "guarded dispatch must wait for the promoter's write transaction"
    );
    promoter.commit().await.unwrap();
    let error = invocation.await.unwrap().unwrap_err();

    assert_eq!(error.source.error_code(), ErrorCode::StaleIdentityEvidence);
    assert!(error.source.to_string().contains(&planned.id.to_string()));
    assert!(error.source.to_string().contains(&current.id.to_string()));
    assert!(!error.dispatch_started);
    assert_eq!(fixture.ticket_count_for_job(job_id).await, 0);
    assert_eq!(fixture.ticket_lifecycle_event_count().await, 0);
    assert_eq!(fixture.lease_count().await, 0);
}

#[tokio::test]
async fn guarded_error_after_root_commit_reports_dispatch_started() {
    let fixture = ExecutorFixture::without_workers(1).await;
    let asset = fixture.cp.identity.create_file_asset(T0).await.unwrap();
    let planned = fixture
        .cp
        .identity
        .create_file_version(NewFileVersion {
            file_asset_id: asset.id,
            content_hash: "planned".to_owned(),
            size_bytes: 1,
            produced_by: ProducedBy::Ingest,
            produced_from_version_id: None,
            created_at: T0,
        })
        .await
        .unwrap();
    let job_id = fixture.open_workflow_job().await;
    let guard = super::PlannedLineageGuard::new(1, vec![(asset.id, planned.id)]).unwrap();
    let executor = fixture.executor_with_options(WorkflowExecutorOptions::for_tests());

    let error = executor
        .submit_and_run_guarded_invocation_in_job(
            job_id,
            "guarded",
            independent_hash_plan(1),
            super::RunFailureMode::AbortJob,
            guard,
        )
        .await
        .unwrap_err();

    assert!(error.dispatch_started);
    assert_eq!(error.source.error_code(), ErrorCode::NoEligibleWorker);
    assert_eq!(fixture.ticket_count_for_job(job_id).await, 1);
    assert_eq!(fixture.ticket_lifecycle_event_count().await, 2);
}

#[tokio::test]
async fn malformed_failure_event_payload_reports_event_id() {
    let fixture = ExecutorFixture::without_workers(0).await;
    let ticket_id = TicketId(42);
    let result = sqlx::query(
        "INSERT INTO events \
         (occurred_at, kind, subject_type, subject_id, trace_id, payload) \
         VALUES (?, ?, ?, ?, NULL, ?)",
    )
    .bind("1970-01-01T00:00:00Z")
    .bind("ticket.failed_terminal")
    .bind("ticket")
    .bind(i64::try_from(ticket_id.0).unwrap())
    .bind(json!({"reason": "missing class"}).to_string())
    .execute(&fixture.cp.pool)
    .await
    .unwrap();
    let executor = fixture.executor_with_options(WorkflowExecutorOptions::for_tests());

    let err = executor.ticket_failure_class(ticket_id).await.unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::Internal);
    assert!(err.to_string().contains(&format!(
        "workflow failure event {}",
        result.last_insert_rowid()
    )));
    assert!(err.to_string().contains("missing class"));
}

#[tokio::test]
async fn invoked_runs_leave_job_open_and_accumulate_durable_counts() {
    let fixture = ExecutorFixture::with_ready_tickets(1).await;
    let job_id = fixture.open_workflow_job().await;
    let executor = fixture.executor_with_options(WorkflowExecutorOptions::for_tests());

    let first = executor
        .submit_and_run_invocation_in_job(
            job_id,
            "first",
            independent_hash_plan(1),
            super::RunFailureMode::AbortJob,
        )
        .await
        .unwrap();
    assert_eq!(first.job_id, job_id);
    assert_eq!(first.ticket_count, 1);
    assert_eq!(
        fixture.job_state(job_id).await,
        "open",
        "runner must not succeed the job; the caller owns succeed_job"
    );
    assert_eq!(
        fixture.non_terminal_ticket_count(job_id).await,
        0,
        "every ticket from the first call is terminal before the next call"
    );

    let second = executor
        .submit_and_run_invocation_in_job(
            job_id,
            "second",
            independent_hash_plan(1),
            super::RunFailureMode::AbortJob,
        )
        .await
        .unwrap();
    assert_eq!(
        second.ticket_count, 2,
        "counts are job-scoped, so the second call's summary is cumulative"
    );
    assert_eq!(fixture.job_state(job_id).await, "open");

    fixture.cp.succeed_job(job_id, T0).await.unwrap();
    assert_eq!(fixture.job_state(job_id).await, "succeeded");
}

#[tokio::test]
async fn aborting_invocation_drains_in_flight_dispatches_before_failing() {
    let mut fixture = ExecutorFixture::without_workers(3).await;
    fixture
        .register_worker(
            "hash-worker",
            OperationKind::HashFile,
            4,
            FakeBehavior::Crash,
        )
        .await;
    let job_id = fixture.open_workflow_job().await;
    // Three independent crashing dispatches run concurrently; the first failure
    // makes the runner fail the job. The drain contract requires every sibling
    // dispatch to reach a terminal state (lease released, ticket failed) before
    // the runner returns, so the coordinator's post-run inspection is race-free.
    let mut plan = independent_hash_plan(3);
    plan.concurrency = ConcurrencyPolicy {
        max_in_flight_dispatches: 3,
    };
    let executor = fixture.executor_with_options(WorkflowExecutorOptions::for_tests());

    let err = executor
        .submit_and_run_invocation_in_job(job_id, "drain", plan, super::RunFailureMode::AbortJob)
        .await
        .unwrap_err();

    assert_eq!(err.source.error_code(), ErrorCode::WorkerCrash);
    assert_eq!(fixture.job_state(job_id).await, "failed");
    assert_eq!(
        fixture.held_lease_count().await,
        0,
        "no dispatch may still hold a lease once the runner returns"
    );
    assert_eq!(
        fixture.leased_ticket_count(job_id).await,
        0,
        "no dispatch may still be in flight (ticket left leased) once the runner returns"
    );
}

#[tokio::test]
async fn continued_invocation_dispatches_every_independent_branch_and_leaves_job_open() {
    let mut fixture = ExecutorFixture::without_workers(3).await;
    fixture
        .register_worker(
            "hash-worker",
            OperationKind::HashFile,
            1,
            FakeBehavior::Crash,
        )
        .await;
    let job_id = fixture.open_workflow_job().await;
    let mut plan = independent_hash_plan(3);
    plan.concurrency.max_in_flight_dispatches = 1;
    let executor = fixture.executor_with_options(WorkflowExecutorOptions::for_tests());

    let error = executor
        .submit_and_run_invocation_in_job(
            job_id,
            "phase-0",
            plan,
            super::RunFailureMode::ContinueIndependent,
        )
        .await
        .unwrap_err();

    assert_eq!(error.summary.dispatch_count, 3);
    assert!(!error.job_failed);
    assert_eq!(
        error.disposition,
        WorkflowFailureDisposition::IsolatedTicket
    );
    assert_eq!(fixture.job_state(job_id).await, "open");
}

#[tokio::test]
async fn continued_invocation_returns_isolated_error_with_blocked_descendant() {
    let mut fixture = ExecutorFixture::without_workers(3).await;
    fixture
        .register_worker(
            "hash-worker",
            OperationKind::HashFile,
            1,
            FakeBehavior::Crash,
        )
        .await;
    fixture
        .register_worker(
            "identify-worker",
            OperationKind::IdentifyMedia,
            1,
            FakeBehavior::Success,
        )
        .await;
    let job_id = fixture.open_workflow_job().await;
    let mut blocked = simple_operation_node("blocked", OperationKind::ScoreQuality);
    blocked.depends_on = vec!["hash".to_owned(), "identify".to_owned()];
    let mut plan = independent_hash_plan(0);
    plan.nodes = vec![
        simple_operation_node("hash", OperationKind::HashFile),
        simple_operation_node("identify", OperationKind::IdentifyMedia),
        blocked,
    ];
    let executor = fixture.executor_with_options(WorkflowExecutorOptions::for_tests());

    let error = executor
        .submit_and_run_invocation_in_job(
            job_id,
            "continued-blocked",
            plan,
            super::RunFailureMode::ContinueIndependent,
        )
        .await
        .unwrap_err();

    assert_eq!(error.source.error_code(), ErrorCode::WorkerCrash);
    assert!(!error.job_failed);
    assert_eq!(error.summary.dispatch_count, 2);
    assert_eq!(fixture.job_state(job_id).await, "open");
}

#[tokio::test]
async fn later_invocation_ignores_a_continued_prior_failure() {
    let mut fixture = ExecutorFixture::without_workers(1).await;
    fixture
        .register_worker(
            "hash-worker",
            OperationKind::HashFile,
            1,
            FakeBehavior::Crash,
        )
        .await;
    fixture
        .register_worker(
            "identify-worker",
            OperationKind::IdentifyMedia,
            1,
            FakeBehavior::Success,
        )
        .await;
    let job_id = fixture.open_workflow_job().await;
    let executor = fixture.executor_with_options(WorkflowExecutorOptions::for_tests());
    executor
        .submit_and_run_invocation_in_job(
            job_id,
            "phase-0",
            independent_hash_plan(1),
            super::RunFailureMode::ContinueIndependent,
        )
        .await
        .unwrap_err();
    let mut later = independent_hash_plan(1);
    later.id = "later-identify-plan".to_owned();
    later.nodes[0].id = "identify-0".to_owned();
    later.nodes[0].operation = OperationKind::IdentifyMedia;

    let summary = executor
        .submit_and_run_invocation_in_job(job_id, "phase-1", later, super::RunFailureMode::AbortJob)
        .await
        .unwrap();

    assert_eq!(summary.failure_count, 1);
    assert_eq!(summary.ticket_count, 2);
    assert_eq!(
        summary.dispatch_count, 1,
        "dispatch telemetry is local to this invocation"
    );
    assert_eq!(fixture.job_state(job_id).await, "open");
}

#[tokio::test]
async fn policy_transcode_root_ticket_carries_source_ids_and_operation_payload() {
    let mut fixture = ExecutorFixture::without_workers(0).await;
    fixture.seed_default_staging_root().await;
    fixture.plan = policy_transcode_plan(TargetRef::FileVersion {
        id: voom_store::test_support::TEST_FILE_VERSION_ID,
    });
    let workflow_payload = fixture.first_ready_ticket_payload().await;

    assert_eq!(workflow_payload.operation, OperationKind::TranscodeVideo);
    assert_eq!(
        workflow_payload.rendered_payload["operation"],
        "transcode_video"
    );
    assert_eq!(
        workflow_payload.rendered_payload["source_file_version_id"],
        9_000_001
    );
    assert_eq!(workflow_payload.rendered_payload["target_codec"], "hevc");
    assert_eq!(workflow_payload.rendered_payload["container"], "mkv");
    assert_eq!(workflow_payload.rendered_payload["profile"], "default-hevc");
}

#[tokio::test]
async fn policy_transcode_file_location_target_carries_source_version_and_location() {
    let mut fixture = ExecutorFixture::without_workers(0).await;
    fixture.seed_default_staging_root().await;
    let (source_file_version_id, source_location_id) = fixture.seed_local_source().await;
    fixture.plan = policy_transcode_plan(TargetRef::FileLocation {
        id: source_location_id,
    });
    let workflow_payload = fixture.first_ready_ticket_payload().await;

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
async fn policy_transcode_file_location_target_rejects_retired_location() {
    let mut fixture = ExecutorFixture::without_workers(0).await;
    let (_source_file_version_id, source_location_id) = fixture.seed_local_source().await;
    fixture.retire_source_location(source_location_id).await;
    fixture.plan = policy_transcode_plan(TargetRef::FileLocation {
        id: source_location_id,
    });

    let err = fixture.run().await.unwrap_err();

    // Both target shapes now route through the shared `select_location`, which
    // classifies a retired location as NotFound rather than a config error.
    assert_eq!(err.source.error_code(), ErrorCode::NotFound);
    assert_eq!(
        err.source.to_string(),
        format!("not found: file_location {source_location_id} is retired")
    );
}

#[tokio::test]
async fn policy_remux_root_ticket_carries_source_ids_and_operation_payload() {
    let mut fixture = ExecutorFixture::without_workers(0).await;
    fixture.seed_default_staging_root().await;
    // Envelope rendering resolves the pinned snapshot, so seed a real one
    // instead of the planner's placeholder id.
    let (source_file_version_id, _location_id) = fixture.seed_local_source().await;
    let snapshot_id = fixture.record_source_snapshot(source_file_version_id).await;
    fixture.plan = policy_remux_plan_for_snapshot(
        TargetRef::FileVersion {
            id: source_file_version_id,
        },
        snapshot_id,
    );
    let workflow_payload = fixture.first_ready_ticket_payload().await;

    assert_eq!(workflow_payload.operation, OperationKind::Remux);
    assert_eq!(workflow_payload.rendered_payload["operation"], "remux");
    assert_eq!(
        workflow_payload.rendered_payload["source_file_version_id"],
        source_file_version_id.0
    );
    assert_eq!(workflow_payload.rendered_payload["remux"]["type"], "remux");
    assert_eq!(
        workflow_payload.rendered_payload["remux"]["container"],
        "mkv"
    );
    assert_eq!(
        workflow_payload.rendered_payload["remux"]["track_order"],
        json!(["video", "audio"])
    );
}

#[tokio::test]
async fn policy_remux_file_location_target_carries_source_version_and_location() {
    let mut fixture = ExecutorFixture::without_workers(0).await;
    fixture.seed_default_staging_root().await;
    let (source_file_version_id, source_location_id) = fixture.seed_local_source().await;
    let snapshot_id = fixture.record_source_snapshot(source_file_version_id).await;
    fixture.plan = policy_remux_plan_for_snapshot(
        TargetRef::FileLocation {
            id: source_location_id,
        },
        snapshot_id,
    );
    let workflow_payload = fixture.first_ready_ticket_payload().await;

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
async fn policy_remux_file_location_target_rejects_retired_location() {
    let mut fixture = ExecutorFixture::without_workers(0).await;
    let (_source_file_version_id, source_location_id) = fixture.seed_local_source().await;
    fixture.retire_source_location(source_location_id).await;
    fixture.plan = policy_remux_plan(TargetRef::FileLocation {
        id: source_location_id,
    });

    let err = fixture.run().await.unwrap_err();

    // Both target shapes now route through the shared `select_location`, which
    // classifies a retired location as NotFound rather than a config error.
    assert_eq!(err.source.error_code(), ErrorCode::NotFound);
    assert_eq!(
        err.source.to_string(),
        format!("not found: file_location {source_location_id} is retired")
    );
}

#[tokio::test]
async fn malformed_policy_remux_payload_is_rejected_before_default_fallback() {
    let mut fixture = ExecutorFixture::without_workers(0).await;
    fixture.plan = policy_remux_plan_with_payload(
        TargetRef::FileVersion {
            id: voom_store::test_support::TEST_FILE_VERSION_ID,
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
            id: voom_store::test_support::TEST_FILE_VERSION_ID,
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
async fn byte_touching_root_node_without_a_target_fails_instead_of_rendering() {
    // remux opens bytes, so with no policy target it has no source to declare.
    // Rendering it anyway would create a ticket that names nothing.
    let mut fixture = ExecutorFixture::without_workers(0).await;
    fixture.plan = non_policy_remux_plan();

    let err = fixture.run().await.unwrap_err();

    assert_eq!(err.source.error_code(), ErrorCode::ConfigInvalid);
    let message = err.source.to_string();
    assert!(message.contains("remux"), "message was {message}");
    assert!(message.contains("storage source"), "message was {message}");
    assert_eq!(fixture.ticket_count().await, 0);
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

fn parse_test_time(value: &str) -> OffsetDateTime {
    OffsetDateTime::parse(
        value,
        &time::format_description::well_known::Iso8601::DEFAULT,
    )
    .unwrap()
}

#[tokio::test]
async fn await_with_lease_heartbeats_refreshes_workflow_lease_while_future_runs() {
    let fixture = ExecutorFixture::with_ready_tickets(1).await;
    let worker_id = fixture.worker_id();
    let (_unrelated_ticket_id, unrelated_lease_id) =
        fixture.create_heartbeat_test_lease(worker_id).await;
    let (_ticket_id, lease_id) = fixture.create_heartbeat_test_lease(worker_id).await;
    let mut options = WorkflowExecutorOptions::for_tests();
    options.timing.heartbeat_interval = Duration::from_millis(10);
    let operation_gate = Arc::new(tokio::sync::Notify::new());
    let operation_gate_in_task = operation_gate.clone();
    let control = fixture.cp.clone();
    let timing = options.timing.clone();
    let chaos = options.chaos.clone();
    let mut heartbeat_task = tokio::spawn(async move {
        let context = LeaseHeartbeatContext {
            control: &control,
            lease_id,
            timing: &timing,
            chaos: &chaos,
        };
        await_with_lease_heartbeats(context, OperationKind::HashFile, async move {
            operation_gate_in_task.notified().await;
            Ok::<_, VoomError>(())
        })
        .await
    });
    let (acquired_at, initial_heartbeat_at) = fixture.lease_heartbeat_window(lease_id).await;
    assert_eq!(initial_heartbeat_at, acquired_at);
    fixture.clock.advance(time::Duration::milliseconds(80));
    let observed_heartbeat_at =
        match wait_for_lease_heartbeat(&fixture, lease_id, acquired_at, Duration::from_secs(5))
            .await
        {
            Ok(observed) => observed,
            Err(diagnostic) => {
                operation_gate.notify_one();
                let cleanup =
                    tokio::time::timeout(Duration::from_secs(5), &mut heartbeat_task).await;
                if cleanup.is_err() {
                    heartbeat_task.abort();
                    let _ = heartbeat_task.await;
                }
                panic!("{diagnostic}; wrapper cleanup result: {cleanup:?}");
            }
        };

    operation_gate.notify_one();
    let Ok(joined) = tokio::time::timeout(Duration::from_secs(5), &mut heartbeat_task).await else {
        heartbeat_task.abort();
        let cleanup = heartbeat_task.await;
        panic!("heartbeat wrapper did not finish after release: {cleanup:?}");
    };
    joined.unwrap().unwrap();

    let (final_acquired_at, final_heartbeat_at) = fixture.lease_heartbeat_window(lease_id).await;
    let (unrelated_acquired_at, unrelated_heartbeat_at) =
        fixture.lease_heartbeat_window(unrelated_lease_id).await;
    assert_eq!(final_acquired_at, acquired_at);
    assert_eq!(unrelated_heartbeat_at, unrelated_acquired_at);
    assert!(
        final_heartbeat_at >= observed_heartbeat_at && final_heartbeat_at > acquired_at,
        "heartbeat wrapper must keep the outer workflow lease fresh: \
         acquired_at={acquired_at}, observed_heartbeat_at={observed_heartbeat_at}, \
         final_heartbeat_at={final_heartbeat_at}"
    );
}

#[tokio::test]
async fn heartbeat_write_does_not_block_operation_holding_sqlite_writer_lock() {
    let fixture = ExecutorFixture::with_ready_tickets(1).await;
    let worker_id = fixture.worker_id();
    let (_ticket_id, lease_id) = fixture.create_heartbeat_test_lease(worker_id).await;
    let mut options = WorkflowExecutorOptions::for_tests();
    options.timing.heartbeat_interval = Duration::from_millis(10);
    let context = LeaseHeartbeatContext {
        control: &fixture.cp,
        lease_id,
        timing: &options.timing,
        chaos: &options.chaos,
    };
    let operation = async {
        let mut transaction = fixture.cp.pool_for_test().begin().await.unwrap();
        sqlx::query("UPDATE leases SET last_heartbeat_at = last_heartbeat_at WHERE id = ?")
            .bind(i64::try_from(lease_id.0).unwrap())
            .execute(&mut *transaction)
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        transaction.commit().await.unwrap();
        Ok::<_, VoomError>(())
    };

    let result = tokio::time::timeout(
        Duration::from_secs(2),
        await_with_lease_heartbeats(context, OperationKind::HashFile, operation),
    )
    .await;

    assert!(
        matches!(result, Ok(Ok(()))),
        "operation stalled: {result:?}"
    );
}

async fn wait_for_lease_heartbeat(
    fixture: &ExecutorFixture,
    lease_id: LeaseId,
    acquired_at: OffsetDateTime,
    timeout: Duration,
) -> Result<OffsetDateTime, String> {
    let observation = async {
        loop {
            let (current_acquired_at, last_heartbeat_at) =
                fixture.lease_heartbeat_window(lease_id).await;
            if current_acquired_at != acquired_at {
                return Err(format!(
                    "lease {lease_id} acquired_at changed while observing heartbeats: \
                     expected={acquired_at}, actual={current_acquired_at}"
                ));
            }
            if last_heartbeat_at > acquired_at {
                return Ok(last_heartbeat_at);
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    };
    let Ok(result) = tokio::time::timeout(timeout, observation).await else {
        let (_, last_heartbeat_at) = fixture.lease_heartbeat_window(lease_id).await;
        return Err(format!(
            "timed out after {timeout:?} waiting for lease {lease_id} heartbeat: \
             acquired_at={acquired_at}, last_heartbeat_at={last_heartbeat_at}"
        ));
    };
    result
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

#[derive(Clone, Copy)]
enum TerminalWorkerIneligibility {
    Stale,
    Retired,
    Denied,
    Incapable,
    Ungranted,
}

impl TerminalWorkerIneligibility {
    const fn label(self) -> &'static str {
        match self {
            Self::Stale => "stale",
            Self::Retired => "retired",
            Self::Denied => "denied",
            Self::Incapable => "incapable",
            Self::Ungranted => "ungranted",
        }
    }
}

async fn seed_terminally_ineligible_worker(
    fixture: &ExecutorFixture,
    ineligibility: TerminalWorkerIneligibility,
) {
    let worker = fixture
        .cp
        .register_worker(crate::cases::workers::RegisterWorkerInput {
            name: format!("{}-worker", ineligibility.label()),
            kind: WorkerKind::Synthetic,
        })
        .await
        .unwrap();
    let operation = TicketOperation::from(OperationKind::HashFile);
    if !matches!(ineligibility, TerminalWorkerIneligibility::Incapable) {
        fixture
            .cp
            .record_capability(NewCapability {
                worker_id: worker.id,
                operation: operation.clone(),
                codecs: Vec::new(),
                hardware: Vec::new(),
                artifact_access: Vec::new(),
                extra: json!({}),
            })
            .await
            .unwrap();
    }
    if !matches!(ineligibility, TerminalWorkerIneligibility::Ungranted) {
        fixture
            .cp
            .record_grant(NewGrant {
                worker_id: worker.id,
                can_execute: vec![operation.clone()],
                can_access_read: Vec::new(),
                can_access_write: Vec::new(),
                denies: if matches!(ineligibility, TerminalWorkerIneligibility::Denied) {
                    vec![operation]
                } else {
                    Vec::new()
                },
                max_parallel: json!({}),
            })
            .await
            .unwrap();
    }
    match ineligibility {
        TerminalWorkerIneligibility::Stale => {
            sqlx::query("UPDATE workers SET status = 'stale' WHERE id = ?")
                .bind(i64::try_from(worker.id.0).unwrap())
                .execute(&fixture.cp.pool)
                .await
                .unwrap();
        }
        TerminalWorkerIneligibility::Retired => {
            fixture
                .cp
                .retire_worker(worker.id, worker.epoch, T0 + time::Duration::seconds(1))
                .await
                .unwrap();
        }
        TerminalWorkerIneligibility::Denied
        | TerminalWorkerIneligibility::Incapable
        | TerminalWorkerIneligibility::Ungranted => {}
    }
}

struct ExecutorFixture {
    cp: crate::ControlPlane,
    clock: Arc<ManualClock>,
    database_url: String,
    _tmp: voom_test_support::TempDatabase,
    plan: WorkflowPlan,
    registry: WorkerRuntimeRegistry,
    clients: HashMap<WorkerId, Arc<FakeClient>>,
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

    async fn capacity_deferred_then_free_worker() -> Self {
        let mut fixture = Self::without_workers(0).await;
        fixture.plan = capacity_deferred_mixed_plan();
        let hash_worker = fixture
            .register_worker(
                "hash-worker",
                OperationKind::HashFile,
                1,
                FakeBehavior::Hang,
            )
            .await;
        fixture
            .register_worker(
                "identify-worker",
                OperationKind::IdentifyMedia,
                1,
                FakeBehavior::Success,
            )
            .await;
        fixture.first_worker_id = Some(hash_worker);
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

    async fn two_workers(ticket_count: usize) -> Self {
        let mut fixture = Self::without_workers(ticket_count).await;
        let first = fixture
            .register_worker(
                "hash-worker-a",
                OperationKind::HashFile,
                1,
                FakeBehavior::Success,
            )
            .await;
        fixture
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
        let tmp = voom_test_support::TempDatabase::new().unwrap();
        let url = format!("sqlite://{}", tmp.path().display());
        let _ = voom_store::init(&url).await.unwrap();
        let pool = voom_store::connect(&url).await.unwrap();
        let pool_for_seed = pool.clone();
        let clock = Arc::new(ManualClock::new(T0));
        let cp = crate::ControlPlane::open_with_pool_and_rng(
            pool,
            clock.clone(),
            Arc::new(Mutex::new(FrozenRng::new(0))),
        )
        .await
        .unwrap();
        // Drive fenced commit intents to convergence from a simulated node.
        let node = voom_test_support::commit_node::SimulatedOwnerNode::new().unwrap();
        node.install(cp.pool_for_test()).await.unwrap();
        let _auto_driver =
            crate::artifact::commit::commit_test_support::spawn_auto_driver(&cp, &node);
        // Byte-touching plan nodes must name a live rooted location.
        voom_store::test_support::seed_test_rooted_location(&pool_for_seed)
            .await
            .unwrap();
        Self {
            cp,
            clock,
            database_url: url,
            _tmp: tmp,
            plan: independent_hash_plan(ticket_count),
            registry: WorkerRuntimeRegistry::new(),
            clients: HashMap::new(),
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
            client.clone(),
            WorkerCredentials {
                worker_id: worker,
                worker_epoch: 0,
                secret: SecretString::from("test-secret"),
            },
        );
        self.clients.insert(worker, client);
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
            .register_worker(crate::cases::workers::RegisterWorkerInput {
                name: name.to_owned(),
                kind: WorkerKind::Synthetic,
            })
            .await
            .unwrap();
        let operation_name = operation_name(operation);
        let operation = ticket_op(operation_name.clone());
        self.cp
            .record_capability(NewCapability {
                worker_id: worker.id,
                operation: operation.clone(),
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
                can_execute: vec![operation],
                can_access_read: Vec::new(),
                can_access_write: Vec::new(),
                denies: Vec::new(),
                max_parallel: json!({ operation_name: max_parallel }),
            })
            .await
            .unwrap();
        worker.id
    }

    async fn deny_worker_operation(&self, worker_id: WorkerId, operation: OperationKind) {
        self.cp
            .record_grant(NewGrant {
                worker_id,
                can_execute: Vec::new(),
                can_access_read: Vec::new(),
                can_access_write: Vec::new(),
                denies: vec![TicketOperation::from(operation)],
                max_parallel: json!({}),
            })
            .await
            .unwrap();
    }

    fn worker_id(&self) -> WorkerId {
        self.first_worker_id.unwrap()
    }

    fn worker_dispatch_count(&self, worker_id: WorkerId) -> u32 {
        self.clients[&worker_id].dispatch_count()
    }

    async fn second_control_plane(&self) -> crate::ControlPlane {
        let pool = voom_store::connect(&self.database_url).await.unwrap();
        crate::ControlPlane::open_with_pool_and_rng(
            pool,
            self.clock.clone(),
            Arc::new(Mutex::new(FrozenRng::new(0))),
        )
        .await
        .unwrap()
    }

    async fn occupy_worker_capacity(
        &self,
        control_plane: &crate::ControlPlane,
        worker_id: WorkerId,
        operation: OperationKind,
    ) -> LeaseId {
        let ticket = control_plane
            .create_ticket(NewTicket {
                job_id: None,
                kind: TicketOperation::from(operation),
                priority: 0,
                payload: json!({}),
                max_attempts: 1,
                created_at: T0,
            })
            .await
            .unwrap();
        control_plane
            .mark_ready_if_unblocked(ticket.id, T0)
            .await
            .unwrap();
        control_plane
            .acquire_lease(NewLease {
                ticket_id: ticket.id,
                worker_id,
                ttl: time::Duration::seconds(60),
                now: T0,
            })
            .await
            .unwrap()
            .id
    }

    async fn wait_for_workflow_ticket(&self) -> Ticket {
        for _ in 0..100 {
            if let Some(ticket) = self.optional_ready_workflow_ticket().await {
                return ticket;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!("workflow ticket was not created");
    }

    async fn first_workflow_ticket(&self) -> Ticket {
        let Some(ticket) = self.optional_first_workflow_ticket().await else {
            panic!("workflow ticket");
        };
        ticket
    }

    async fn optional_ready_workflow_ticket(&self) -> Option<Ticket> {
        let id: Option<i64> = sqlx::query_scalar(
            "SELECT id FROM tickets \
             WHERE kind LIKE 'synthetic.workflow.operation.%' AND state = 'ready' \
             ORDER BY id ASC LIMIT 1",
        )
        .fetch_optional(&self.cp.pool)
        .await
        .unwrap();
        self.workflow_ticket_by_id(id).await
    }

    async fn optional_first_workflow_ticket(&self) -> Option<Ticket> {
        let id: Option<i64> = sqlx::query_scalar(
            "SELECT id FROM tickets \
             WHERE kind LIKE 'synthetic.workflow.operation.%' \
             ORDER BY id ASC LIMIT 1",
        )
        .fetch_optional(&self.cp.pool)
        .await
        .unwrap();
        self.workflow_ticket_by_id(id).await
    }

    async fn workflow_ticket_by_id(&self, id: Option<i64>) -> Option<Ticket> {
        let id = id?;
        self.cp
            .tickets
            .get(TicketId(u64::try_from(id).unwrap()))
            .await
            .unwrap()
    }

    async fn ticket_event_count(&self, ticket_id: TicketId, kind: &str) -> i64 {
        sqlx::query_scalar(
            "SELECT COUNT(*) FROM events \
             WHERE subject_type = 'ticket' AND subject_id = ? AND kind = ?",
        )
        .bind(i64::try_from(ticket_id.0).unwrap())
        .bind(kind)
        .fetch_one(&self.cp.pool)
        .await
        .unwrap()
    }

    async fn run(
        &self,
    ) -> Result<WorkflowRunSummary, crate::workflow::execution::executor::WorkflowRunError> {
        self.run_with_options(WorkflowExecutorOptions::for_tests())
            .await
    }

    async fn run_with_policy(
        &self,
        concurrency: ConcurrencyPolicy,
    ) -> Result<WorkflowRunSummary, crate::workflow::execution::executor::WorkflowRunError> {
        let mut plan = self.plan.clone();
        plan.concurrency = concurrency;
        self.executor_with_options(WorkflowExecutorOptions::for_tests())
            .submit_and_run(plan)
            .await
    }

    async fn run_with_options(
        &self,
        options: WorkflowExecutorOptions,
    ) -> Result<WorkflowRunSummary, crate::workflow::execution::executor::WorkflowRunError> {
        self.executor_with_options(options)
            .submit_and_run(self.plan.clone())
            .await
    }

    async fn run_plan(
        &self,
        plan: WorkflowPlan,
    ) -> Result<WorkflowRunSummary, crate::workflow::execution::executor::WorkflowRunError> {
        self.executor_with_options(WorkflowExecutorOptions::for_tests())
            .submit_and_run(plan)
            .await
    }

    fn executor_with_options(&self, options: WorkflowExecutorOptions) -> WorkflowExecutor {
        WorkflowExecutor::with_options(self.cp.clone(), self.registry.clone(), options)
    }

    async fn lease_count(&self) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM leases")
            .fetch_one(&self.cp.pool)
            .await
            .unwrap()
    }

    async fn ticket_count_for_job(&self, job_id: JobId) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM tickets WHERE job_id = ?")
            .bind(i64::try_from(job_id.0).unwrap())
            .fetch_one(&self.cp.pool)
            .await
            .unwrap()
    }

    async fn ticket_lifecycle_event_count(&self) -> i64 {
        sqlx::query_scalar(
            "SELECT COUNT(*) FROM events \
             WHERE subject_type = 'ticket' \
               AND kind IN ('ticket.created', 'ticket.ready')",
        )
        .fetch_one(&self.cp.pool)
        .await
        .unwrap()
    }

    async fn create_heartbeat_test_lease(&self, worker_id: WorkerId) -> (TicketId, LeaseId) {
        let job_id = self.open_workflow_job().await;
        let operation = OperationKind::HashFile;
        let payload = WorkflowTicketPayload::new_for_test(
            "heartbeat-workflow",
            "heartbeat-plan",
            "hash",
            "root",
            operation,
            json!({
                "operation": operation_name(operation),
                "path": "/library/root.mkv",
            }),
        )
        .to_ticket_payload()
        .unwrap();
        let ticket = self
            .cp
            .create_ticket(NewTicket {
                job_id: Some(job_id),
                kind: workflow_ticket_op(operation),
                priority: 0,
                payload,
                max_attempts: 1,
                created_at: self.cp.clock().now(),
            })
            .await
            .unwrap();
        self.cp
            .mark_ready_if_unblocked(ticket.id, self.cp.clock().now())
            .await
            .unwrap();
        let lease = self
            .cp
            .acquire_lease(NewLease {
                ticket_id: ticket.id,
                worker_id,
                ttl: time::Duration::seconds(5),
                now: self.cp.clock().now(),
            })
            .await
            .unwrap();
        (ticket.id, lease.id)
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

    async fn job_state(&self, job_id: JobId) -> String {
        sqlx::query_scalar("SELECT state FROM jobs WHERE id = ?")
            .bind(i64::try_from(job_id.0).unwrap())
            .fetch_one(&self.cp.pool)
            .await
            .unwrap()
    }

    async fn job_state_and_epoch(&self, job_id: JobId) -> (String, i64) {
        sqlx::query_as("SELECT state, epoch FROM jobs WHERE id = ?")
            .bind(i64::try_from(job_id.0).unwrap())
            .fetch_one(&self.cp.pool)
            .await
            .unwrap()
    }

    async fn non_terminal_ticket_count(&self, job_id: JobId) -> i64 {
        sqlx::query_scalar(
            "SELECT COUNT(*) FROM tickets \
             WHERE job_id = ? AND state IN ('pending', 'ready', 'leased')",
        )
        .bind(i64::try_from(job_id.0).unwrap())
        .fetch_one(&self.cp.pool)
        .await
        .unwrap()
    }

    async fn leased_ticket_count(&self, job_id: JobId) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM tickets WHERE job_id = ? AND state = 'leased'")
            .bind(i64::try_from(job_id.0).unwrap())
            .fetch_one(&self.cp.pool)
            .await
            .unwrap()
    }

    async fn open_workflow_job(&self) -> JobId {
        self.cp
            .open_job(NewJob {
                kind: "synthetic.workflow".to_owned(),
                priority: 0,
                created_at: T0,
            })
            .await
            .unwrap()
            .id
    }

    async fn lease_heartbeat_window(&self, lease_id: LeaseId) -> (OffsetDateTime, OffsetDateTime) {
        let (acquired_at, last_heartbeat_at): (String, String) =
            sqlx::query_as("SELECT acquired_at, last_heartbeat_at FROM leases WHERE id = ?")
                .bind(i64::try_from(lease_id.0).unwrap())
                .fetch_one(&self.cp.pool)
                .await
                .unwrap();
        (
            parse_test_time(&acquired_at),
            parse_test_time(&last_heartbeat_at),
        )
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
        let source = TicketStorageSource::Location {
            storage_root_id: StorageRootId(3),
            file_location_id: FileLocationId(7),
        };
        let rendered_payload = json!({
            "operation": operation_name(operation),
            "branch_id": "other",
            "path": "/library/other.mkv",
            "duration_ms": 10_u64,
            "progress_interval_ms": 1_u64,
            "source_storage_root_id": 3_u64,
            "source_location_id": 7_u64,
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
            declared_artifact_access: declaration_for(operation, Some(&source)).unwrap(),
        }
        .to_ticket_payload()
        .unwrap();
        let ticket = self
            .cp
            .create_ticket(NewTicket {
                job_id: Some(job.id),
                kind: workflow_ticket_op(operation),
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

    async fn seed_malformed_ready_workflow_ticket(&self, job_id: JobId) {
        let operation = OperationKind::HashFile;
        let workflow_id = format!("workflow-{}-malformed-ready", job_id.0);
        let payload = WorkflowTicketPayload::new_for_test(
            &workflow_id,
            "executor-test-0",
            "hash-bad",
            "bad",
            operation,
            json!({
                "operation": operation_name(operation),
                "branch_id": "bad",
                "path": "/library/bad.mkv",
                "duration_ms": 10_u64,
                "progress_interval_ms": 1_u64,
            }),
        )
        .to_ticket_payload()
        .unwrap();
        let ticket = self
            .cp
            .create_ticket(NewTicket {
                job_id: Some(job_id),
                kind: workflow_ticket_op(operation),
                priority: 0,
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
        let malformed = json!({
            "workflow_id": workflow_id,
            "plan_id": "executor-test-0",
            "node_id": "hash-bad"
        });
        sqlx::query("UPDATE tickets SET payload = ? WHERE id = ?")
            .bind(malformed.to_string())
            .bind(i64::try_from(ticket.id.0).unwrap())
            .execute(&self.cp.pool)
            .await
            .unwrap();
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
        let location_value = path
            .canonicalize()
            .unwrap_or_else(|_| path.to_path_buf())
            .to_string_lossy()
            .into_owned();
        let outcome = self
            .cp
            .record_discovered_file(
                DiscoveredFile {
                    storage_root_id: voom_store::test_support::TEST_STORAGE_ROOT_ID,
                    provider_relative_locator: voom_store::test_support::test_relative_locator(
                        &location_value,
                    ),
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

    async fn retire_source_location(&self, source_location_id: voom_core::FileLocationId) {
        let result = sqlx::query("UPDATE file_locations SET retired_at = ? WHERE id = ?")
            .bind("1970-01-01T00:00:00Z")
            .bind(i64::try_from(source_location_id.0).unwrap())
            .execute(&self.cp.pool)
            .await
            .unwrap();
        assert_eq!(result.rows_affected(), 1);
    }

    async fn record_source_snapshot(&self, file_version_id: FileVersionId) -> MediaSnapshotId {
        self.record_source_snapshot_with_audio_channels(file_version_id, 2)
            .await
    }

    async fn record_source_snapshot_with_audio_channels(
        &self,
        file_version_id: FileVersionId,
        audio_channels: u32,
    ) -> MediaSnapshotId {
        self.cp
            .record_media_snapshot(
                file_version_id,
                None,
                json!({
                    "container": "mkv",
                    "video_codec": "h264",
                    "streams": [
                        {
                            "id": "stream-0",
                            "index": 0,
                            "kind": "video",
                            "codec_name": "h264",
                            "disposition": {"default": true}
                        },
                        {
                            "id": "stream-audio-1",
                            "index": 1,
                            "kind": "audio",
                            "codec_name": "aac",
                            "language": "eng",
                            "title": "Commentary",
                            "channels": audio_channels,
                            "disposition": {
                                "default": false,
                                "forced": false,
                                "commentary": true
                            }
                        }
                    ]
                }),
                T0,
            )
            .await
            .unwrap()
            .id
    }

    /// The shared test root ships with no default destinations; point staging
    /// at itself so planned media-dispatch outputs resolve.
    async fn seed_default_staging_root(&self) {
        sqlx::query("UPDATE library_roots SET default_staging_root_id = id WHERE id = ?")
            .bind(i64::try_from(voom_store::test_support::TEST_STORAGE_ROOT_ID.0).unwrap())
            .execute(&self.cp.pool)
            .await
            .unwrap();
    }

    /// Render the plan's single root ticket through the real executor path and
    /// return its parsed payload. A media ticket is never leased or failed by
    /// the bundled executor — it stays `ready` for its storage owner's agent —
    /// so callers assert on the rendered payload instead of a run outcome.
    async fn first_ready_ticket_payload(&mut self) -> WorkflowTicketPayload {
        let plan = self.plan.clone();
        let executor = self.executor_with_options(WorkflowExecutorOptions::for_tests());
        let job_id = self.open_workflow_job().await;
        let workflow_id = format!("workflow-{}", job_id.0);
        executor
            .create_root_tickets(&plan, &workflow_id, job_id, T0)
            .await
            .unwrap();
        let tickets = executor
            .ready_workflow_tickets(job_id, &workflow_id)
            .await
            .unwrap();
        assert_eq!(tickets.len(), 1);
        parse_payload(&tickets[0]).unwrap()
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
    dispatches: AtomicU32,
    active: Arc<AtomicU32>,
    max_active: AtomicU32,
}

impl FakeClient {
    fn new(worker_id: WorkerId, behavior: FakeBehavior) -> Self {
        Self {
            worker_id,
            behavior,
            dispatches: AtomicU32::new(0),
            active: Arc::new(AtomicU32::new(0)),
            max_active: AtomicU32::new(0),
        }
    }

    fn dispatch_count(&self) -> u32 {
        self.dispatches.load(Ordering::SeqCst)
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
}

#[async_trait]
impl ClientHandle for FakeClient {
    async fn handshake(&self, _offered: u32) -> Result<HandshakeResponse, ProtocolError> {
        Err(ProtocolError::InternalServerError)
    }

    async fn identity(
        &self,
        _credentials: &WorkerCredentials,
    ) -> Result<voom_worker_protocol::WorkerIdentityResponse, ProtocolError> {
        Err(ProtocolError::InternalServerError)
    }

    async fn dispatch(
        &self,
        _creds: &WorkerCredentials,
        _idempotency_key: &str,
        request: OperationRequest,
    ) -> Result<DispatchStream, ProtocolError> {
        assert_eq!(_creds.worker_id, self.worker_id);
        self.dispatches.fetch_add(1, Ordering::SeqCst);
        if matches!(self.behavior, FakeBehavior::DispatchError) {
            return Err(ProtocolError::InternalServerError);
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
                accepted_at: OffsetDateTime::now_utc(),
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
        FakeBehavior::Success => {
            write_frame(&mut writer, result_frame(&request, json!({"ok": true}))).await;
        }
        FakeBehavior::MalformedFrame => {
            let _ = writer.write_all(b"{not-json}\n").await;
        }
        FakeBehavior::Hang => {
            std::future::pending::<()>().await;
        }
        FakeBehavior::ProgressFlood => {
            for seq in 0..128 {
                write_frame(&mut writer, progress_frame(&request, seq)).await;
                tokio::task::yield_now().await;
            }
            std::future::pending::<()>().await;
        }
        FakeBehavior::Crash | FakeBehavior::DispatchError => {}
    }
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
        emitted_at: OffsetDateTime::now_utc(),
        payload,
    }
}

fn progress_frame(request: &OperationRequest, seq: u64) -> ProgressFrame {
    ProgressFrame::Progress {
        lease_id: request.lease_id,
        seq,
        emitted_at: OffsetDateTime::now_utc(),
        percent: Some(PercentBps::try_from(100).unwrap()),
        message: None,
        payload: None,
    }
}

fn independent_hash_plan(ticket_count: usize) -> WorkflowPlan {
    WorkflowPlan {
        id: format!("executor-test-{ticket_count}"),
        seed: 2,
        nodes: (0..ticket_count)
            .map(|index| OperationNode {
                id: format!("hash-{index}"),
                operation: OperationKind::HashFile,
                policy_target: fixture_policy_target(OperationKind::HashFile),
                operation_payload: Value::Null,
                depends_on: Vec::new(),
                depends_on_selected: Vec::new(),
                provides_selected: None,
            })
            .collect(),
        fan_out: crate::workflow::plan::model::FanOutPolicy { max_files: 3 },
        concurrency: ConcurrencyPolicy {
            max_in_flight_dispatches: 4,
        },
        timing: crate::workflow::plan::model::TimingPolicy {
            base_duration_ms: 10,
            jitter_ms: 0,
        },
    }
}

fn capacity_deferred_mixed_plan() -> WorkflowPlan {
    WorkflowPlan {
        id: "capacity-deferred-mixed-test".to_owned(),
        seed: 2,
        nodes: vec![
            simple_operation_node("hash-active", OperationKind::HashFile),
            simple_operation_node("hash-deferred", OperationKind::HashFile),
            simple_operation_node("identify-free", OperationKind::IdentifyMedia),
        ],
        fan_out: crate::workflow::plan::model::FanOutPolicy { max_files: 3 },
        concurrency: ConcurrencyPolicy {
            max_in_flight_dispatches: 4,
        },
        timing: crate::workflow::plan::model::TimingPolicy {
            base_duration_ms: 10,
            jitter_ms: 0,
        },
    }
}

/// The seeded fixture location, for a node whose operation opens bytes.
///
/// `ExecutorFixture::without_workers` seeds exactly this row, so a plan node can
/// name it without threading an id through every builder.
fn fixture_policy_target(operation: OperationKind) -> Option<TargetRef> {
    operation
        .is_byte_touching()
        .then_some(TargetRef::FileLocation {
            id: voom_store::test_support::TEST_FILE_LOCATION_ID,
        })
}

fn simple_operation_node(id: &str, operation: OperationKind) -> OperationNode {
    OperationNode {
        id: id.to_owned(),
        operation,
        policy_target: fixture_policy_target(operation),
        operation_payload: Value::Null,
        depends_on: Vec::new(),
        depends_on_selected: Vec::new(),
        provides_selected: None,
    }
}

async fn panicking_identify_fixture() -> (ExecutorFixture, WorkflowExecutorOptions) {
    let mut fixture = ExecutorFixture::without_workers(0).await;
    fixture.plan.nodes = vec![simple_operation_node(
        "identify",
        OperationKind::IdentifyMedia,
    )];
    fixture
        .register_worker(
            "identify-worker",
            OperationKind::IdentifyMedia,
            1,
            FakeBehavior::Success,
        )
        .await;
    let mut options = WorkflowExecutorOptions::for_tests();
    options.chaos.panic_after_lease_operation = Some(OperationKind::IdentifyMedia);
    (fixture, options)
}

async fn tracked_failed_dispatch(fixture: &ExecutorFixture, worker_id: WorkerId) -> RunLoopState {
    let (ticket_id, lease_id) = fixture.create_heartbeat_test_lease(worker_id).await;
    fixture
        .cp
        .fail_lease(
            lease_id,
            "prepared joined failure".to_owned(),
            FailureClass::WorkerCrash,
            fixture.cp.clock().now(),
        )
        .await
        .unwrap();
    let identity = DispatchIdentity {
        ticket_id,
        worker_id,
        lease_id,
        operation: OperationKind::HashFile,
    };
    let mut state = RunLoopState::new(JobId(900), Duration::ZERO);
    state.reservations.insert(worker_id, 1);
    let handle = state.active.spawn(async move {
        DispatchOutcome {
            ticket_id,
            worker_id,
            operation: OperationKind::HashFile,
            terminal: DispatchTerminal::Failure {
                source: VoomError::WorkerCrash("prepared joined failure".to_owned()),
            },
        }
    });
    state.active_identities.insert(handle.id(), identity);
    while !handle.is_finished() {
        tokio::task::yield_now().await;
    }
    state
}

fn successful_dispatch_outcome() -> DispatchOutcome {
    DispatchOutcome {
        ticket_id: TicketId(17),
        worker_id: WorkerId(23),
        operation: OperationKind::IdentifyMedia,
        terminal: DispatchTerminal::Success,
    }
}

fn assert_completion_bookkeeping_cleared(state: &RunLoopState) {
    assert!(state.active.is_empty());
    assert!(state.active_identities.is_empty());
    assert!(state.reservations.is_empty());
}

fn policy_transcode_plan(target: TargetRef) -> WorkflowPlan {
    // The resolved_profile is normally emitted by the planner (Task 5.2) and
    // threaded via binding.rs into the ticket payload. Here we supply it
    // directly so executor tests exercise the full dispatch path without
    // running the planner.
    let default_hevc = voom_worker_protocol::TranscodeVideoProfile::default_hevc();
    WorkflowPlan {
        id: "policy-transcode-test".to_owned(),
        seed: 12,
        nodes: vec![OperationNode {
            id: "policy-node_transcode".to_owned(),
            operation: OperationKind::TranscodeVideo,
            policy_target: Some(target),
            operation_payload: json!({
                "type": "transcode_video",
                "target_codec": "hevc",
                "container": "mkv",
                "profile": "default-hevc",
                "resolved_profile": serde_json::to_value(&default_hevc).unwrap(),
            }),
            depends_on: Vec::new(),
            depends_on_selected: Vec::new(),
            provides_selected: None,
        }],
        fan_out: crate::workflow::plan::model::FanOutPolicy { max_files: 1 },
        concurrency: ConcurrencyPolicy {
            max_in_flight_dispatches: 1,
        },
        timing: crate::workflow::plan::model::TimingPolicy {
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
        nodes: vec![OperationNode {
            id: "policy-node_remux".to_owned(),
            operation: OperationKind::Remux,
            policy_target: Some(target),
            operation_payload,
            depends_on: Vec::new(),
            depends_on_selected: Vec::new(),
            provides_selected: None,
        }],
        fan_out: crate::workflow::plan::model::FanOutPolicy { max_files: 1 },
        concurrency: ConcurrencyPolicy {
            max_in_flight_dispatches: 1,
        },
        timing: crate::workflow::plan::model::TimingPolicy {
            base_duration_ms: 10,
            jitter_ms: 0,
        },
    }
}

fn non_policy_remux_plan() -> WorkflowPlan {
    WorkflowPlan {
        id: "non-policy-remux-test".to_owned(),
        seed: 12,
        nodes: vec![OperationNode {
            id: "remux".to_owned(),
            operation: OperationKind::Remux,
            policy_target: None,
            operation_payload: Value::Null,
            depends_on: Vec::new(),
            depends_on_selected: Vec::new(),
            provides_selected: None,
        }],
        fan_out: crate::workflow::plan::model::FanOutPolicy { max_files: 1 },
        concurrency: ConcurrencyPolicy {
            max_in_flight_dispatches: 1,
        },
        timing: crate::workflow::plan::model::TimingPolicy {
            base_duration_ms: 10,
            jitter_ms: 0,
        },
    }
}

fn timeout_options() -> WorkflowExecutorOptions {
    let mut options = WorkflowExecutorOptions::for_tests();
    options.timing.progress_idle_timeout = Duration::ZERO;
    options.timing.heartbeat_timeout = Duration::from_millis(250);
    options.timing.heartbeat_interval = Duration::from_millis(10);
    options
}

fn operation_name(operation: OperationKind) -> String {
    operation.as_str().to_owned()
}

fn ticket_op(value: impl Into<String>) -> TicketOperation {
    TicketOperation::new(value).unwrap()
}

fn workflow_ticket_op(operation: OperationKind) -> TicketOperation {
    ticket_op(format!(
        "synthetic.workflow.operation.{}",
        operation_name(operation)
    ))
}

fn policy_hash_node(id: &str, depends_on: &[&str]) -> OperationNode {
    OperationNode {
        id: id.to_owned(),
        operation: OperationKind::HashFile,
        policy_target: fixture_policy_target(OperationKind::HashFile),
        operation_payload: Value::Null,
        depends_on: depends_on.iter().map(ToString::to_string).collect(),
        depends_on_selected: Vec::new(),
        provides_selected: None,
    }
}

fn policy_chain_plan(nodes: Vec<OperationNode>) -> WorkflowPlan {
    WorkflowPlan {
        id: "policy-node-expansion-test".to_owned(),
        seed: 7,
        nodes,
        fan_out: crate::workflow::plan::model::FanOutPolicy { max_files: 1 },
        concurrency: ConcurrencyPolicy {
            max_in_flight_dispatches: 4,
        },
        timing: crate::workflow::plan::model::TimingPolicy {
            base_duration_ms: 10,
            jitter_ms: 0,
        },
    }
}

#[tokio::test]
async fn expand_successful_ticket_handles_policy_node_ids() {
    let mut fixture = ExecutorFixture::without_workers(0).await;
    let worker_id = fixture
        .register_worker(
            "hash-worker",
            OperationKind::HashFile,
            4,
            FakeBehavior::Success,
        )
        .await;
    fixture.first_worker_id = Some(worker_id);

    let plan = policy_chain_plan(vec![
        policy_hash_node("policy-node_node_remux_first", &[]),
        policy_hash_node(
            "policy-node_node_remux_second",
            &["policy-node_node_remux_first"],
        ),
    ]);

    let summary = fixture.run_plan(plan).await.unwrap();

    // Both the root node (A) and its dependent (B) must run. Before the fix the
    // dependent node's ticket was never created, so only A ran.
    assert_eq!(summary.operation_count(OperationKind::HashFile), 2);
    assert_eq!(summary.dispatch_count, 2);
}

#[tokio::test]
async fn expand_successful_ticket_join_node_waits_for_all_parents() {
    let mut fixture = ExecutorFixture::without_workers(0).await;
    let worker_id = fixture
        .register_worker(
            "hash-worker",
            OperationKind::HashFile,
            1,
            FakeBehavior::Success,
        )
        .await;
    fixture.first_worker_id = Some(worker_id);

    // Diamond: A -> B, A -> C, (B, C) -> D. D is a join node and must be created
    // exactly once, only after BOTH B and C succeed.
    let plan = policy_chain_plan(vec![
        policy_hash_node("policy-node_a", &[]),
        policy_hash_node("policy-node_b", &["policy-node_a"]),
        policy_hash_node("policy-node_c", &["policy-node_a"]),
        policy_hash_node("policy-node_d", &["policy-node_b", "policy-node_c"]),
    ]);

    let summary = fixture.run_plan(plan).await.unwrap();

    // All four nodes run, and D runs exactly once (no duplicate join ticket).
    assert_eq!(summary.operation_count(OperationKind::HashFile), 4);
    assert_eq!(summary.dispatch_count, 4);

    let ticket_total: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM tickets WHERE json_extract(payload, '$.node_id') = ?",
    )
    .bind("policy-node_d")
    .fetch_one(&fixture.cp.pool)
    .await
    .unwrap();
    assert_eq!(ticket_total, 1);
}
#[test]
fn accelerator_unavailable_clocks_are_independent_and_reset_by_token() {
    let mut state = RunLoopState::new(JobId(1), Duration::ZERO);
    let now = Instant::now();
    state.accelerator_wait_started.insert(
        "nvidia:GPU-a".to_owned(),
        now.checked_sub(Duration::from_secs(20)).unwrap(),
    );
    state
        .accelerator_wait_started
        .insert("nvidia:GPU-b".to_owned(), now);

    assert_eq!(
        state.timed_out_accelerator(Duration::from_secs(10)),
        Some("nvidia:GPU-a")
    );

    let mut dispatch = DispatchReadyOutcome::default();
    dispatch
        .recovered_accelerators
        .insert("nvidia:GPU-a".to_owned());
    dispatch
        .recovered_accelerators
        .insert("nvidia:GPU-b".to_owned());
    state.update_accelerator_waits(&dispatch);

    assert!(!state.accelerator_wait_started.contains_key("nvidia:GPU-a"));
    assert!(!state.accelerator_wait_started.contains_key("nvidia:GPU-b"));
    assert_eq!(state.timed_out_accelerator(Duration::from_secs(10)), None);
}

#[tokio::test]
async fn envelope_bearing_media_tickets_route_to_owner_node_execution() {
    // ADR 0075: a byte-touching ticket whose payload carries the
    // `media_dispatch` envelope is never leased or pushed by the bundled
    // executor — even when a locally registered worker could execute it. It
    // stays `ready` for its storage owner's agent, and the run loop treats
    // the workflow as externally held while it waits.
    let mut fixture = ExecutorFixture::without_workers(0).await;
    fixture
        .register_worker(
            "ffmpeg-worker",
            OperationKind::TranscodeVideo,
            8,
            FakeBehavior::Success,
        )
        .await;
    let job_id = fixture.open_workflow_job().await;
    let operation = OperationKind::TranscodeVideo;
    let mut rendered_payload = json!({
        "operation": operation_name(operation),
        "branch_id": "root",
        "duration_ms": 10_u64,
        "progress_interval_ms": 1_u64,
        "media_dispatch": {"operation": "transcode_video"},
    });
    rendered_payload["source_storage_root_id"] = json!(3_u64);
    rendered_payload["source_location_id"] = json!(7_u64);
    let source = TicketStorageSource::Location {
        storage_root_id: StorageRootId(3),
        file_location_id: FileLocationId(7),
    };
    let payload = WorkflowTicketPayload {
        workflow_id: format!("workflow-{}", job_id.0),
        plan_id: "executor-test-0".to_owned(),
        node_id: "transcode".to_owned(),
        branch_id: "root".to_owned(),
        operation,
        rendered_payload,
        timing: EffectiveTiming::for_test(10, 1),
        source_file: None,
        declared_artifact_access: declaration_for(operation, Some(&source)).unwrap(),
    }
    .to_ticket_payload()
    .unwrap();
    let ticket = fixture
        .cp
        .create_ticket(NewTicket {
            job_id: Some(job_id),
            kind: workflow_ticket_op(operation),
            priority: 0,
            payload,
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

    let executor = fixture.executor_with_options(WorkflowExecutorOptions::for_tests());
    let mut state = RunLoopState::new(job_id, Duration::ZERO);
    let mut accelerator_runtimes = None;
    let outcome = executor
        .try_spawn_dispatch(&mut state, ticket.clone(), &mut accelerator_runtimes)
        .await
        .unwrap();

    assert!(matches!(outcome, SpawnOutcome::NodeLocalDispatched));
    assert_eq!(
        state.node_local_outstanding.get(&ticket.id),
        Some(&operation)
    );
    // No lease was minted and the bundled runtime was never consulted.
    assert_eq!(fixture.lease_count().await, 0);
    let stored = fixture.cp.tickets.get(ticket.id).await.unwrap().unwrap();
    assert_eq!(
        stored.state,
        voom_store::repo::execution::tickets::TicketState::Ready
    );

    // With every ready ticket held by an owner-node agent, the idle state is
    // externally-held work, not a stalled ready queue.
    let idle = executor
        .workflow_idle_state(job_id, &format!("workflow-{}", job_id.0))
        .await
        .unwrap();
    assert_eq!(idle, WorkflowIdleState::Leased);
}

#[tokio::test]
async fn owner_node_success_landing_before_the_finished_check_is_counted() {
    // Issue #545. ADR 0075 media tickets are observed by polling rather than
    // joined as dispatch tasks, so the storage owner's agent can succeed one
    // between the run loop's settlement pass and its `workflow_finished` read.
    // Concluding on that read alone loses the operation's success, leaving a
    // run summary that contradicts the very ticket the run committed — and the
    // phase-barrier coordinator then accumulates that zero across phases.
    let mut fixture = ExecutorFixture::without_workers(0).await;
    let (file_version_id, _location_id) = fixture.seed_local_source().await;
    let snapshot_id = fixture.record_source_snapshot(file_version_id).await;
    fixture.plan = policy_remux_plan_for_snapshot(
        TargetRef::FileVersion {
            id: file_version_id,
        },
        snapshot_id,
    );
    fixture.seed_default_staging_root().await;
    let agent_worker_id = fixture
        .register_worker(
            "owner-node-agent",
            OperationKind::Remux,
            1,
            FakeBehavior::Success,
        )
        .await;

    let observed = Arc::new(tokio::sync::Notify::new());
    let resume = Arc::new(tokio::sync::Notify::new());
    let mut options = WorkflowExecutorOptions::for_tests();
    options.node_local_settle_sync = Some(NodeLocalSettleTestSync {
        observed: Arc::clone(&observed),
        resume: Arc::clone(&resume),
        held: Arc::new(std::sync::atomic::AtomicBool::new(false)),
    });
    let executor = fixture.executor_with_options(options);
    let job_id = fixture.open_workflow_job().await;
    let plan = fixture.plan.clone();
    let run = tokio::spawn(async move {
        executor
            .submit_and_run_invocation_in_job(
                job_id,
                "phase-0",
                plan,
                super::RunFailureMode::ContinueIndependent,
            )
            .await
    });

    // The loop has finished one settlement pass and is holding before its
    // finished check: land the owner node's completion in exactly that window.
    // Both waits are bounded so a run that never reaches the hold, or never
    // concludes, fails the test instead of hanging the suite.
    tokio::time::timeout(Duration::from_secs(5), observed.notified())
        .await
        .unwrap_or_else(|_| panic!("the run loop must reach its settlement hold"));
    let ticket = fixture.first_workflow_ticket().await;
    let lease = fixture
        .cp
        .acquire_lease(NewLease {
            ticket_id: ticket.id,
            worker_id: agent_worker_id,
            ttl: time::Duration::seconds(5),
            now: fixture.cp.clock().now(),
        })
        .await
        .unwrap();
    fixture
        .cp
        .release_lease(lease.id, json!({}), fixture.cp.clock().now())
        .await
        .unwrap();
    resume.notify_one();

    let summary = tokio::time::timeout(Duration::from_secs(10), run)
        .await
        .unwrap_or_else(|_| panic!("the run must conclude once its ticket is terminal"))
        .unwrap()
        .unwrap_or_else(|error| panic!("the remux run must succeed: {}", error.source));
    assert_eq!(
        fixture.ticket_state(ticket.id).await,
        "succeeded",
        "the owner node's completion must be durably recorded"
    );
    assert_eq!(
        summary.operation_count(OperationKind::Remux),
        1,
        "a success settled after the finished check must still reach the run summary"
    );
}

#[tokio::test]
async fn envelope_backed_policy_media_tickets_render_dispatches_and_route_owner_node() {
    // ADR 0075 flip: a policy remux root ticket whose planning inputs are all
    // durably derivable (live rooted source, snapshot, configured staging
    // default) is created with a handle-shaped `media_dispatch` object that
    // decodes as the exact protocol envelope, and the executor routes it to
    // the storage owner's agent without minting a lease.
    let mut fixture = ExecutorFixture::without_workers(0).await;
    let (file_version_id, _location_id) = fixture.seed_local_source().await;
    let snapshot_id = fixture.record_source_snapshot(file_version_id).await;
    fixture.plan = policy_remux_plan_for_snapshot(
        TargetRef::FileVersion {
            id: file_version_id,
        },
        snapshot_id,
    );
    // The shared test root ships with no default destinations; point staging
    // at itself so the planned output resolves.
    sqlx::query("UPDATE library_roots SET default_staging_root_id = id WHERE id = ?")
        .bind(i64::try_from(voom_store::test_support::TEST_STORAGE_ROOT_ID.0).unwrap())
        .execute(&fixture.cp.pool)
        .await
        .unwrap();

    let plan = fixture.plan.clone();
    let executor = fixture.executor_with_options(WorkflowExecutorOptions::for_tests());
    let job_id = fixture.open_workflow_job().await;
    let workflow_id = format!("workflow-{}", job_id.0);
    executor
        .create_root_tickets(&plan, &workflow_id, job_id, T0)
        .await
        .unwrap();

    let tickets = executor
        .ready_workflow_tickets(job_id, &workflow_id)
        .await
        .unwrap();
    assert_eq!(tickets.len(), 1);
    let payload = parse_payload(&tickets[0]).unwrap();

    let Some(dispatch) = payload.rendered_payload.get("media_dispatch") else {
        panic!("policy remux ticket with fully derivable inputs must carry media_dispatch");
    };
    let decoded = voom_worker_protocol::decode_media_dispatch(dispatch).unwrap();
    match &decoded {
        voom_worker_protocol::MediaDispatch::Remux(remux) => {
            assert_eq!(
                remux.source.storage_root_id,
                voom_store::test_support::TEST_STORAGE_ROOT_ID
            );
            assert_eq!(
                remux.output.storage_root_id,
                voom_store::test_support::TEST_STORAGE_ROOT_ID
            );
            assert!(!remux.output.provider_relative_locator.as_str().is_empty());
            assert!(!remux.selection.default_streams.is_empty());
        }
        other => panic!("unexpected dispatch envelope: {other:?}"),
    }

    let mut accelerator_runtimes: Option<WorkerRuntimeRegistry> = None;
    let outcome = executor
        .try_spawn_dispatch(
            &mut RunLoopState::new(job_id, std::time::Duration::ZERO),
            tickets[0].clone(),
            &mut accelerator_runtimes,
        )
        .await
        .unwrap();
    assert!(matches!(outcome, SpawnOutcome::NodeLocalDispatched));
    // No lease was minted: the ticket waits for its storage owner's agent.
    assert_eq!(fixture.lease_count().await, 0);
}
