use super::*;

use time::{Duration, OffsetDateTime};
use voom_core::{FileVersionId, JobId, TicketOperation, VoomError};

use crate::repo::execution::jobs::{NewJob, SqliteJobRepo};
use crate::test_support::fresh_initialized_pool_at;

async fn pool() -> (sqlx::SqlitePool, voom_test_support::TempDatabase) {
    let tmp = voom_test_support::TempDatabase::new().unwrap();
    let p = fresh_initialized_pool_at(tmp.path()).await.unwrap();
    (p, tmp)
}

struct TicketFixture {
    repo: SqliteTicketRepo,
    _tmp: voom_test_support::TempDatabase,
    now: OffsetDateTime,
}

async fn ticket_fixture() -> TicketFixture {
    let (pool, tmp) = pool().await;
    let repo = SqliteTicketRepo::new(pool);
    TicketFixture {
        repo,
        _tmp: tmp,
        now: OffsetDateTime::UNIX_EPOCH + Duration::seconds(30),
    }
}

impl TicketFixture {
    async fn ready_ticket(&self, kind: &str, priority: i64, eligible_after_secs: i64) -> Ticket {
        let ticket = self
            .repo
            .create(NewTicket {
                job_id: None,
                kind: ticket_op(kind),
                priority,
                payload: serde_json::json!({}),
                max_attempts: 3,
                created_at: OffsetDateTime::UNIX_EPOCH + Duration::seconds(eligible_after_secs),
            })
            .await
            .unwrap();
        self.repo
            .mark_ready_if_unblocked(ticket.id, self.now)
            .await
            .unwrap()
            .pop()
            .unwrap()
    }
}

fn sample_new_ticket() -> NewTicket {
    NewTicket {
        job_id: None,
        kind: ticket_op("ingest.scan"),
        priority: 0,
        payload: serde_json::json!({"path": "/tmp/x"}),
        max_attempts: 3,
        created_at: OffsetDateTime::UNIX_EPOCH,
    }
}

fn ticket_op(value: &str) -> TicketOperation {
    TicketOperation::new(value).unwrap()
}

#[tokio::test]
async fn succeeded_results_for_job_and_operation_selects_succeeded_rows_in_ticket_order() {
    let (pool, _tmp) = pool().await;
    let jobs = SqliteJobRepo::new(pool.clone());
    let repo = SqliteTicketRepo::new(pool.clone());
    let job = jobs.create(sample_job()).await.unwrap();
    let other_job = jobs.create(sample_job()).await.unwrap();
    let operation = ticket_op("synthetic.workflow.operation.extract_audio");
    let first = create_ticket_for_job(&repo, job.id, operation.clone()).await;
    let second = create_ticket_for_job(&repo, job.id, operation.clone()).await;
    let wrong_operation = create_ticket_for_job(&repo, job.id, ticket_op("other")).await;
    let wrong_job = create_ticket_for_job(&repo, other_job.id, operation.clone()).await;
    let failed = create_ticket_for_job(&repo, job.id, operation.clone()).await;

    for (ticket, state, result) in [
        (&first, "succeeded", serde_json::json!({"sequence": 1})),
        (&second, "succeeded", serde_json::json!({"sequence": 2})),
        (
            &wrong_operation,
            "succeeded",
            serde_json::json!({"sequence": 3}),
        ),
        (&wrong_job, "succeeded", serde_json::json!({"sequence": 4})),
        (&failed, "failed", serde_json::json!({"sequence": 5})),
    ] {
        sqlx::query("UPDATE tickets SET state = ?, result = ? WHERE id = ?")
            .bind(state)
            .bind(result.to_string())
            .bind(i64::try_from(ticket.id.0).unwrap())
            .execute(&pool)
            .await
            .unwrap();
    }

    let results = repo
        .succeeded_results_for_job_and_operation(job.id, operation)
        .await
        .unwrap();

    assert_eq!(results.len(), 2);
    assert_eq!(results[0].ticket_id, first.id);
    assert_eq!(results[0].result, serde_json::json!({"sequence": 1}));
    assert_eq!(results[1].ticket_id, second.id);
    assert_eq!(results[1].result, serde_json::json!({"sequence": 2}));
}

#[tokio::test]
async fn succeeded_results_for_job_and_operation_rejects_malformed_result_json() {
    let (pool, _tmp) = pool().await;
    let jobs = SqliteJobRepo::new(pool.clone());
    let repo = SqliteTicketRepo::new(pool.clone());
    let job = jobs.create(sample_job()).await.unwrap();
    let operation = ticket_op("synthetic.workflow.operation.extract_audio");
    let ticket = create_ticket_for_job(&repo, job.id, operation.clone()).await;
    let mut connection = pool.acquire().await.unwrap();
    sqlx::query("PRAGMA ignore_check_constraints = TRUE")
        .execute(&mut *connection)
        .await
        .unwrap();
    sqlx::query("UPDATE tickets SET state = 'succeeded', result = '{' WHERE id = ?")
        .bind(i64::try_from(ticket.id.0).unwrap())
        .execute(&mut *connection)
        .await
        .unwrap();
    sqlx::query("PRAGMA ignore_check_constraints = FALSE")
        .execute(&mut *connection)
        .await
        .unwrap();

    let error = repo
        .succeeded_results_for_job_and_operation(job.id, operation)
        .await
        .unwrap_err();

    assert!(error.to_string().contains("succeeded ticket result"));
}

async fn create_ticket_for_job(
    repo: &SqliteTicketRepo,
    job_id: JobId,
    kind: TicketOperation,
) -> Ticket {
    repo.create(NewTicket {
        job_id: Some(job_id),
        kind,
        priority: 0,
        payload: serde_json::json!({}),
        max_attempts: 1,
        created_at: OffsetDateTime::UNIX_EPOCH,
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn keyset_list_is_newest_first_and_pages_by_after_id() {
    let (pool, _tmp) = pool().await;
    let repo = SqliteTicketRepo::new(pool.clone());
    let first = repo.create(sample_new_ticket()).await.unwrap();
    let second = repo.create(sample_new_ticket()).await.unwrap();
    let third = repo.create(sample_new_ticket()).await.unwrap();

    // Newest first (id DESC), ADR 0031.
    let all = repo.list(TicketFilter::default(), None, 10).await.unwrap();
    assert_eq!(
        all.iter().map(|t| t.id).collect::<Vec<_>>(),
        vec![third.id, second.id, first.id]
    );

    let page2 = repo
        .list(TicketFilter::default(), Some(second.id.0), 10)
        .await
        .unwrap();
    assert_eq!(
        page2.iter().map(|t| t.id).collect::<Vec<_>>(),
        vec![first.id]
    );

    // State filter composes with the keyset window.
    let pending = repo
        .list(
            TicketFilter {
                state: Some(TicketState::Pending),
            },
            None,
            10,
        )
        .await
        .unwrap();
    assert_eq!(pending.len(), 3);
    let leased = repo
        .list(
            TicketFilter {
                state: Some(TicketState::Leased),
            },
            None,
            10,
        )
        .await
        .unwrap();
    assert!(leased.is_empty());
}

#[tokio::test]
async fn create_starts_in_pending_state() {
    let (pool, _tmp) = pool().await;
    let repo = SqliteTicketRepo::new(pool.clone());
    let t = repo.create(sample_new_ticket()).await.unwrap();
    assert!(t.id.0 > 0);
    assert_eq!(t.state, TicketState::Pending);
    assert_eq!(t.attempt, 0);
    assert_eq!(t.max_attempts, 3);
}

#[tokio::test]
async fn next_ready_for_operations_orders_by_priority_next_eligible_and_ticket_id() {
    let fixture = ticket_fixture().await;
    let low = fixture.ready_ticket("transcode_video", 1, 10).await;
    let high_late = fixture.ready_ticket("transcode_video", 10, 20).await;
    let high_early = fixture.ready_ticket("transcode_video", 10, 5).await;

    let selected = fixture
        .repo
        .next_ready_for_operations(&[ticket_op("transcode_video")], fixture.now)
        .await
        .unwrap()
        .unwrap();

    assert_eq!(selected.id, high_early.id);
    assert_ne!(selected.id, low.id);
    assert_ne!(selected.id, high_late.id);
}

#[tokio::test]
async fn next_ready_for_operations_uses_ticket_id_as_final_tiebreaker() {
    let fixture = ticket_fixture().await;
    let first = fixture.ready_ticket("transcode_video", 10, 5).await;
    let second = fixture.ready_ticket("transcode_video", 10, 5).await;

    let selected = fixture
        .repo
        .next_ready_for_operations(&[ticket_op("transcode_video")], fixture.now)
        .await
        .unwrap()
        .unwrap();

    assert_eq!(selected.id, first.id);
    assert_ne!(selected.id, second.id);
}

#[tokio::test]
async fn ready_for_operations_excludes_cancelled_job_tickets() {
    let (pool, _tmp) = pool().await;
    let jobs = SqliteJobRepo::new(pool.clone());
    let tickets = SqliteTicketRepo::new(pool.clone());
    let open_job = jobs.create(sample_job()).await.unwrap();
    let cancelled_job = jobs.create(sample_job()).await.unwrap();
    jobs.cancel(cancelled_job.id, OffsetDateTime::UNIX_EPOCH)
        .await
        .unwrap();
    let open = ready_ticket_for_job(&tickets, Some(open_job.id)).await;
    let cancelled = ready_ticket_for_job(&tickets, Some(cancelled_job.id)).await;
    let jobless = ready_ticket_for_job(&tickets, None).await;

    let mut tx = pool.begin().await.unwrap();
    let selected = tickets
        .ready_for_operations_in_tx(
            &mut tx,
            &[ticket_op("transcode_video")],
            OffsetDateTime::UNIX_EPOCH,
        )
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let ids = selected
        .into_iter()
        .map(|ticket| ticket.id)
        .collect::<Vec<_>>();
    assert!(ids.contains(&open.id));
    assert!(ids.contains(&jobless.id));
    assert!(!ids.contains(&cancelled.id));
}

fn sample_job() -> NewJob {
    NewJob {
        kind: "policy_execute".to_owned(),
        priority: 0,
        created_at: OffsetDateTime::UNIX_EPOCH,
    }
}

async fn ready_ticket_for_job(tickets: &SqliteTicketRepo, job_id: Option<JobId>) -> Ticket {
    let ticket = tickets
        .create(NewTicket {
            job_id,
            kind: ticket_op("transcode_video"),
            priority: 0,
            payload: serde_json::json!({}),
            max_attempts: 1,
            created_at: OffsetDateTime::UNIX_EPOCH,
        })
        .await
        .unwrap();
    tickets
        .mark_ready_if_unblocked(ticket.id, OffsetDateTime::UNIX_EPOCH)
        .await
        .unwrap()
        .pop()
        .unwrap()
}

#[tokio::test]
async fn mark_ready_if_unblocked_promotes_pending_with_no_deps_to_ready() {
    let (pool, _tmp) = pool().await;
    let repo = SqliteTicketRepo::new(pool.clone());
    let t = repo.create(sample_new_ticket()).await.unwrap();
    let promoted = repo
        .mark_ready_if_unblocked(t.id, OffsetDateTime::UNIX_EPOCH)
        .await
        .unwrap();
    assert_eq!(promoted.len(), 1, "target ticket promoted");
    assert_eq!(promoted[0].id, t.id);
    assert_eq!(promoted[0].state, TicketState::Ready);
}

#[tokio::test]
async fn mark_ready_keeps_pending_when_unsucceeded_dep_remains() {
    let (pool, _tmp) = pool().await;
    let repo = SqliteTicketRepo::new(pool.clone());
    let a = repo.create(sample_new_ticket()).await.unwrap();
    let b = repo.create(sample_new_ticket()).await.unwrap();
    repo.add_dependency(b.id, a.id).await.unwrap();
    let promoted = repo
        .mark_ready_if_unblocked(b.id, OffsetDateTime::UNIX_EPOCH)
        .await
        .unwrap();
    assert!(promoted.is_empty(), "blocked by upstream");
    let fetched = repo.get(b.id).await.unwrap().unwrap();
    assert_eq!(fetched.state, TicketState::Pending);
}

#[tokio::test]
async fn mark_ready_cascades_to_dependents_when_target_was_already_succeeded() {
    // The intended usage is: a -> b (b depends on a). When a succeeds and a
    // caller invokes mark_ready_if_unblocked(b, now), b should promote IF its
    // remaining unsucceeded deps are gone. The cascade case is when calling
    // mark_ready_if_unblocked on an upstream ticket that's already ready —
    // dependents whose only blocker is the *upstream's* succeeded state should
    // be promoted in the same call.
    //
    // This test pins the contract for the no-cascade case at the repo level
    // (target alone). Cascade-on-success is exercised at the ControlPlane
    // layer via release_lease in Task 14's tests.
    let (pool, _tmp) = pool().await;
    let repo = SqliteTicketRepo::new(pool.clone());
    let a = repo.create(sample_new_ticket()).await.unwrap();
    let b = repo.create(sample_new_ticket()).await.unwrap();
    repo.add_dependency(b.id, a.id).await.unwrap();
    // a has no deps -> promotes.
    let promoted_a = repo
        .mark_ready_if_unblocked(a.id, OffsetDateTime::UNIX_EPOCH)
        .await
        .unwrap();
    assert_eq!(promoted_a.len(), 1);
    assert_eq!(promoted_a[0].id, a.id);
    // b still blocked because a is only `ready`, not `succeeded`.
    let promoted_b = repo
        .mark_ready_if_unblocked(b.id, OffsetDateTime::UNIX_EPOCH)
        .await
        .unwrap();
    assert!(promoted_b.is_empty());
}

#[tokio::test]
async fn add_dependency_rejects_self_reference() {
    let (pool, _tmp) = pool().await;
    let repo = SqliteTicketRepo::new(pool.clone());
    let a = repo.create(sample_new_ticket()).await.unwrap();
    let err = repo.add_dependency(a.id, a.id).await.unwrap_err();
    assert!(matches!(err, VoomError::DependencyCycle(_)));
}

#[tokio::test]
async fn add_dependency_detects_cycle_via_multi_edge_walk() {
    // a -> b -> c, then attempt c -> a (would form cycle)
    let (pool, _tmp) = pool().await;
    let repo = SqliteTicketRepo::new(pool.clone());
    let a = repo.create(sample_new_ticket()).await.unwrap();
    let b = repo.create(sample_new_ticket()).await.unwrap();
    let c = repo.create(sample_new_ticket()).await.unwrap();
    repo.add_dependency(a.id, b.id).await.unwrap();
    repo.add_dependency(b.id, c.id).await.unwrap();
    let err = repo.add_dependency(c.id, a.id).await.unwrap_err();
    assert!(matches!(err, VoomError::DependencyCycle(_)), "got: {err:?}");
}

#[tokio::test]
async fn add_dependency_accepts_dag() {
    // a -> b, c -> b (diamond top: b has two dependents) is fine
    let (pool, _tmp) = pool().await;
    let repo = SqliteTicketRepo::new(pool.clone());
    let a = repo.create(sample_new_ticket()).await.unwrap();
    let b = repo.create(sample_new_ticket()).await.unwrap();
    let c = repo.create(sample_new_ticket()).await.unwrap();
    repo.add_dependency(a.id, b.id).await.unwrap();
    repo.add_dependency(c.id, b.id).await.unwrap();
}

#[tokio::test]
async fn add_dependency_rejects_ready_dependent() {
    // Once the dependent has crossed the readiness gate, a late edge does
    // not demote it back to pending — and acquire only checks `state =
    // 'ready'`. The gate must surface this as Conflict, not silently
    // insert.
    let (pool, _tmp) = pool().await;
    let repo = SqliteTicketRepo::new(pool.clone());
    let a = repo.create(sample_new_ticket()).await.unwrap();
    let b = repo.create(sample_new_ticket()).await.unwrap();
    let _ = repo
        .mark_ready_if_unblocked(a.id, OffsetDateTime::UNIX_EPOCH)
        .await
        .unwrap();
    let err = repo.add_dependency(a.id, b.id).await.unwrap_err();
    let msg = match &err {
        VoomError::Conflict(s) => s.clone(),
        other => panic!("expected Conflict, got: {other:?}"),
    };
    assert!(
        msg.contains(&a.id.to_string()) && msg.contains("ready"),
        "Conflict message must name the ticket and its state, got: {msg}"
    );
}

#[tokio::test]
async fn add_dependency_rejects_leased_dependent() {
    // A leased ticket is mid-execution — adding a new blocker now would
    // pretend it had been gated on the new edge all along. Reject it.
    use crate::repo::execution::leases::{NewLease, SqliteLeaseRepo};
    use crate::repo::execution::workers::{
        NewCapability, NewGrant, NewWorker, SqliteWorkerRepo, WorkerKind,
    };
    use time::Duration;

    let (pool, _tmp) = pool().await;
    let trepo = SqliteTicketRepo::new(pool.clone());
    let wrepo = SqliteWorkerRepo::new(pool.clone());
    let lrepo = SqliteLeaseRepo::new(pool.clone());
    let a = trepo.create(sample_new_ticket()).await.unwrap();
    let b = trepo.create(sample_new_ticket()).await.unwrap();
    trepo
        .mark_ready_if_unblocked(a.id, OffsetDateTime::UNIX_EPOCH)
        .await
        .unwrap();
    let w = wrepo
        .register(NewWorker {
            name: "w".to_owned(),
            kind: WorkerKind::Synthetic,
            registered_at: OffsetDateTime::UNIX_EPOCH,
            node_id: None,
        })
        .await
        .unwrap();
    wrepo
        .record_capability(NewCapability {
            worker_id: w.id,
            operation: a.kind.clone(),
            codecs: Vec::new(),
            hardware: Vec::new(),
            artifact_access: Vec::new(),
            extra: serde_json::json!({}),
        })
        .await
        .unwrap();
    wrepo
        .record_grant(NewGrant {
            worker_id: w.id,
            can_execute: vec![a.kind.clone()],
            can_access_read: Vec::new(),
            can_access_write: Vec::new(),
            denies: Vec::new(),
            max_parallel: serde_json::json!({}),
        })
        .await
        .unwrap();
    lrepo
        .acquire(NewLease {
            ticket_id: a.id,
            worker_id: w.id,
            ttl: Duration::seconds(60),
            now: OffsetDateTime::UNIX_EPOCH,
        })
        .await
        .unwrap();
    let err = trepo.add_dependency(a.id, b.id).await.unwrap_err();
    let msg = match &err {
        VoomError::Conflict(s) => s.clone(),
        other => panic!("expected Conflict, got: {other:?}"),
    };
    assert!(
        msg.contains(&a.id.to_string()) && msg.contains("leased"),
        "Conflict message must name the ticket and its state, got: {msg}"
    );
}

#[tokio::test]
async fn add_dependency_rejects_missing_dependent() {
    // A non-existent dependent must surface NotFound — previously the
    // function returned Ok(()) after the cycle check (the dependent's id
    // was never read), masking caller bugs.
    use voom_core::TicketId;

    let (pool, _tmp) = pool().await;
    let repo = SqliteTicketRepo::new(pool.clone());
    let b = repo.create(sample_new_ticket()).await.unwrap();
    let missing = TicketId(99_999);
    let err = repo.add_dependency(missing, b.id).await.unwrap_err();
    assert!(matches!(err, VoomError::NotFound(_)), "got: {err:?}");
}

#[tokio::test]
async fn list_dependents_returns_tickets_that_depend_on_this_one() {
    let (pool, _tmp) = pool().await;
    let repo = SqliteTicketRepo::new(pool.clone());
    let a = repo.create(sample_new_ticket()).await.unwrap();
    let b = repo.create(sample_new_ticket()).await.unwrap();
    let c = repo.create(sample_new_ticket()).await.unwrap();
    repo.add_dependency(a.id, c.id).await.unwrap();
    repo.add_dependency(b.id, c.id).await.unwrap();
    let dependents = repo.list_dependents(c.id).await.unwrap();
    let ids: Vec<_> = dependents.iter().map(|t| t.id).collect();
    assert!(ids.contains(&a.id));
    assert!(ids.contains(&b.id));
}

#[tokio::test]
async fn pre_lease_failure_transition_returns_terminal_and_retry_ticket_rows() {
    let fixture = ticket_fixture().await;
    let terminal_source = fixture.ready_ticket("terminal", 0, 0).await;
    let retry_source = fixture.ready_ticket("retry", 0, 0).await;
    let terminal_at = fixture.now + Duration::seconds(1);
    let retry_at = fixture.now + Duration::seconds(30);
    let mut tx = fixture.repo.pool.begin().await.unwrap();

    let terminal = fixture
        .repo
        .transition_ready_before_lease_failure_in_tx(
            &mut tx,
            terminal_source.id,
            terminal_source.attempt,
            1,
            PreLeaseFailureTransition::Terminal,
            terminal_at,
        )
        .await
        .unwrap();
    let retry = fixture
        .repo
        .transition_ready_before_lease_failure_in_tx(
            &mut tx,
            retry_source.id,
            retry_source.attempt,
            1,
            PreLeaseFailureTransition::RetryAt(retry_at),
            terminal_at,
        )
        .await
        .unwrap();
    tx.commit().await.unwrap();

    assert_eq!(terminal.state, TicketState::Failed);
    assert_eq!(terminal.attempt, 1);
    assert_eq!(terminal.state_changed_at, terminal_at);
    assert_eq!(retry.state, TicketState::Ready);
    assert_eq!(retry.attempt, 1);
    assert_eq!(retry.next_eligible_at, retry_at);
    assert_eq!(retry.state_changed_at, terminal_at);
}

#[tokio::test]
async fn pre_lease_failure_transition_rejects_a_changed_previous_attempt() {
    let fixture = ticket_fixture().await;
    let ticket = fixture.ready_ticket("stale-attempt", 0, 0).await;
    let mut tx = fixture.repo.pool.begin().await.unwrap();

    let err = fixture
        .repo
        .transition_ready_before_lease_failure_in_tx(
            &mut tx,
            ticket.id,
            ticket.attempt + 1,
            1,
            PreLeaseFailureTransition::Terminal,
            fixture.now,
        )
        .await
        .unwrap_err();
    tx.rollback().await.unwrap();

    assert!(matches!(err, VoomError::Conflict(_)));
}

#[tokio::test]
async fn succeeded_workflow_node_ids_are_ordered_and_deduplicated() {
    let (pool, _tmp) = pool().await;
    let repo = SqliteTicketRepo::new(pool.clone());
    let job = SqliteJobRepo::new(pool).create(sample_job()).await.unwrap();
    create_workflow_ticket(&repo, job.id, "wf", "root", "zeta", TicketState::Succeeded).await;
    create_workflow_ticket(&repo, job.id, "wf", "root", "alpha", TicketState::Succeeded).await;
    create_workflow_ticket(
        &repo,
        job.id,
        "wf",
        "other",
        "alpha",
        TicketState::Succeeded,
    )
    .await;
    create_workflow_ticket(
        &repo,
        job.id,
        "other",
        "root",
        "ignored",
        TicketState::Succeeded,
    )
    .await;

    let node_ids = repo
        .succeeded_workflow_node_ids(job.id, "wf")
        .await
        .unwrap();

    assert_eq!(node_ids, vec!["alpha", "zeta"]);
}

#[tokio::test]
async fn workflow_ticket_exists_in_tx_uses_full_node_identity() {
    let (pool, _tmp) = pool().await;
    let repo = SqliteTicketRepo::new(pool.clone());
    let jobs = SqliteJobRepo::new(pool.clone());
    let job = jobs.create(sample_job()).await.unwrap();
    let other_job = jobs.create(sample_job()).await.unwrap();
    create_workflow_ticket(&repo, job.id, "wf", "branch", "node", TicketState::Pending).await;
    create_workflow_ticket(
        &repo,
        other_job.id,
        "wf",
        "branch",
        "node",
        TicketState::Pending,
    )
    .await;
    let mut tx = pool.begin().await.unwrap();

    assert!(
        repo.workflow_ticket_exists_in_tx(&mut tx, job.id, "wf", "branch", "node")
            .await
            .unwrap()
    );
    assert!(
        !repo
            .workflow_ticket_exists_in_tx(&mut tx, job.id, "other", "branch", "node")
            .await
            .unwrap()
    );
    assert!(
        !repo
            .workflow_ticket_exists_in_tx(&mut tx, job.id, "wf", "other", "node")
            .await
            .unwrap()
    );
    assert!(
        !repo
            .workflow_ticket_exists_in_tx(&mut tx, job.id, "wf", "branch", "other")
            .await
            .unwrap()
    );
    assert!(
        !repo
            .workflow_ticket_exists_in_tx(
                &mut tx,
                JobId(other_job.id.0 + 1),
                "wf",
                "branch",
                "node",
            )
            .await
            .unwrap()
    );
    tx.commit().await.unwrap();
}

#[tokio::test]
async fn ready_workflow_tickets_are_typed_and_use_scheduling_order() {
    let (pool, _tmp) = pool().await;
    let repo = SqliteTicketRepo::new(pool.clone());
    let job = SqliteJobRepo::new(pool).create(sample_job()).await.unwrap();
    let first = create_workflow_ticket(&repo, job.id, "wf", "a", "first", TicketState::Ready).await;
    let second =
        create_workflow_ticket(&repo, job.id, "wf", "b", "second", TicketState::Ready).await;
    let third = create_workflow_ticket(&repo, job.id, "wf", "c", "third", TicketState::Ready).await;
    create_workflow_ticket(&repo, job.id, "other", "d", "ignored", TicketState::Ready).await;
    set_ticket_schedule(&repo, first.id, 5, 10).await;
    set_ticket_schedule(&repo, second.id, 5, 5).await;
    set_ticket_schedule(&repo, third.id, 10, 10).await;

    let ready = repo
        .ready_workflow_tickets(
            job.id,
            "wf",
            OffsetDateTime::UNIX_EPOCH + Duration::seconds(20),
            10,
        )
        .await
        .unwrap();

    assert_eq!(
        ready.iter().map(|ticket| ticket.id).collect::<Vec<_>>(),
        vec![third.id, second.id, first.id]
    );
}

#[tokio::test]
async fn workflow_ticket_facts_distinguish_each_execution_state() {
    let (pool, _tmp) = pool().await;
    let repo = SqliteTicketRepo::new(pool.clone());
    let job = SqliteJobRepo::new(pool).create(sample_job()).await.unwrap();
    for (node_id, state) in [
        ("pending", TicketState::Pending),
        ("ready", TicketState::Ready),
        ("leased", TicketState::Leased),
        ("failed", TicketState::Failed),
        ("succeeded", TicketState::Succeeded),
    ] {
        create_workflow_ticket(&repo, job.id, "wf", "branch", node_id, state).await;
    }
    create_workflow_ticket(
        &repo,
        job.id,
        "other",
        "branch",
        "ready",
        TicketState::Ready,
    )
    .await;

    let facts = repo.workflow_ticket_facts(job.id, "wf").await.unwrap();

    assert_eq!(
        facts,
        WorkflowTicketFacts {
            unfinished: 3,
            ready: 1,
            leased: 1,
            failed: 1,
        }
    );
}

#[tokio::test]
async fn first_failed_workflow_ticket_uses_lowest_ticket_id() {
    let (pool, _tmp) = pool().await;
    let repo = SqliteTicketRepo::new(pool.clone());
    let job = SqliteJobRepo::new(pool).create(sample_job()).await.unwrap();
    let first =
        create_workflow_ticket(&repo, job.id, "wf", "a", "first", TicketState::Failed).await;
    create_workflow_ticket(&repo, job.id, "other", "b", "ignored", TicketState::Failed).await;
    create_workflow_ticket(&repo, job.id, "wf", "c", "second", TicketState::Failed).await;

    let failed = repo
        .first_failed_workflow_ticket(job.id, "wf")
        .await
        .unwrap()
        .unwrap();

    assert_eq!(failed.id, first.id);
    assert_eq!(failed.state, TicketState::Failed);
}

#[tokio::test]
async fn retry_eligible_at_returns_earliest_future_ready_timestamp() {
    let (pool, _tmp) = pool().await;
    let repo = SqliteTicketRepo::new(pool.clone());
    let job = SqliteJobRepo::new(pool).create(sample_job()).await.unwrap();
    let past = create_workflow_ticket(&repo, job.id, "wf", "a", "past", TicketState::Ready).await;
    let later = create_workflow_ticket(&repo, job.id, "wf", "b", "later", TicketState::Ready).await;
    let earlier =
        create_workflow_ticket(&repo, job.id, "wf", "c", "earlier", TicketState::Ready).await;
    let failed =
        create_workflow_ticket(&repo, job.id, "wf", "d", "failed", TicketState::Failed).await;
    set_ticket_schedule(&repo, past.id, 0, 5).await;
    set_ticket_schedule(&repo, later.id, 0, 30).await;
    set_ticket_schedule(&repo, earlier.id, 0, 20).await;
    set_ticket_schedule(&repo, failed.id, 0, 15).await;

    let eligible_at = repo
        .retry_eligible_at(
            job.id,
            "wf",
            OffsetDateTime::UNIX_EPOCH + Duration::seconds(10),
        )
        .await
        .unwrap();

    assert_eq!(
        eligible_at,
        Some(OffsetDateTime::UNIX_EPOCH + Duration::seconds(20))
    );
}

#[tokio::test]
async fn find_workflow_ticket_id_in_tx_uses_full_phase_identity() {
    let (pool, _tmp) = pool().await;
    let repo = SqliteTicketRepo::new(pool.clone());
    let jobs = SqliteJobRepo::new(pool.clone());
    let job = jobs.create(sample_job()).await.unwrap();
    let other_job = jobs.create(sample_job()).await.unwrap();
    let target = create_identity_ticket(&repo, job.id, "wf", "branch", "node", Some(7)).await;
    create_identity_ticket(&repo, other_job.id, "wf", "branch", "node", Some(7)).await;
    create_identity_ticket(&repo, job.id, "other", "branch", "node", Some(7)).await;
    create_identity_ticket(&repo, job.id, "wf", "other", "node", Some(7)).await;
    create_identity_ticket(&repo, job.id, "wf", "branch", "other", Some(7)).await;
    create_identity_ticket(&repo, job.id, "wf", "branch", "node", Some(8)).await;
    let mut tx = pool.begin().await.unwrap();

    let found = repo
        .find_workflow_ticket_id_in_tx(
            &mut tx,
            WorkflowTicketIdentity {
                job_id: job.id,
                workflow_id: "wf",
                branch_id: "branch",
                node_id: "node",
                source_file_version_id: Some(FileVersionId(7)),
            },
        )
        .await
        .unwrap();
    tx.commit().await.unwrap();

    assert_eq!(found, Some(target.id));
}

#[tokio::test]
async fn dependency_exists_in_tx_matches_both_ticket_ids() {
    let (pool, _tmp) = pool().await;
    let repo = SqliteTicketRepo::new(pool.clone());
    let dependent = repo.create(sample_new_ticket()).await.unwrap();
    let dependency = repo.create(sample_new_ticket()).await.unwrap();
    let other = repo.create(sample_new_ticket()).await.unwrap();
    repo.add_dependency(dependent.id, dependency.id)
        .await
        .unwrap();
    let mut tx = pool.begin().await.unwrap();

    assert!(
        repo.dependency_exists_in_tx(&mut tx, dependent.id, dependency.id)
            .await
            .unwrap()
    );
    assert!(
        !repo
            .dependency_exists_in_tx(&mut tx, dependent.id, other.id)
            .await
            .unwrap()
    );
    tx.commit().await.unwrap();
}

#[tokio::test]
async fn list_for_job_returns_all_typed_tickets_in_id_order() {
    let (pool, _tmp) = pool().await;
    let repo = SqliteTicketRepo::new(pool.clone());
    let jobs = SqliteJobRepo::new(pool);
    let job = jobs.create(sample_job()).await.unwrap();
    let other_job = jobs.create(sample_job()).await.unwrap();
    let first =
        create_workflow_ticket(&repo, job.id, "wf", "a", "first", TicketState::Pending).await;
    create_workflow_ticket(
        &repo,
        other_job.id,
        "wf",
        "ignored",
        "ignored",
        TicketState::Pending,
    )
    .await;
    let second =
        create_workflow_ticket(&repo, job.id, "wf", "b", "second", TicketState::Succeeded).await;

    let tickets = repo.list_for_job(job.id).await.unwrap();

    assert_eq!(
        tickets.iter().map(|ticket| ticket.id).collect::<Vec<_>>(),
        vec![first.id, second.id]
    );
    assert_eq!(tickets[1].state, TicketState::Succeeded);
}

async fn create_identity_ticket(
    repo: &SqliteTicketRepo,
    job_id: JobId,
    workflow_id: &str,
    branch_id: &str,
    node_id: &str,
    source_file_version_id: Option<u64>,
) -> Ticket {
    let mut rendered_payload = serde_json::json!({"operation": "test"});
    if let Some(source_file_version_id) = source_file_version_id {
        rendered_payload["source_file_version_id"] = serde_json::json!(source_file_version_id);
    }
    repo.create(NewTicket {
        job_id: Some(job_id),
        kind: ticket_op("synthetic.workflow.operation.test"),
        priority: 0,
        payload: serde_json::json!({
            "workflow_id": workflow_id,
            "branch_id": branch_id,
            "node_id": node_id,
            "rendered_payload": rendered_payload
        }),
        max_attempts: 1,
        created_at: OffsetDateTime::UNIX_EPOCH,
    })
    .await
    .unwrap()
}

async fn set_ticket_schedule(
    repo: &SqliteTicketRepo,
    ticket_id: voom_core::TicketId,
    priority: i64,
    eligible_at_seconds: i64,
) {
    let eligible_at = OffsetDateTime::UNIX_EPOCH + Duration::seconds(eligible_at_seconds);
    sqlx::query("UPDATE tickets SET priority = ?, next_eligible_at = ? WHERE id = ?")
        .bind(priority)
        .bind(iso8601(eligible_at).unwrap())
        .bind(i64::try_from(ticket_id.0).unwrap())
        .execute(&repo.pool)
        .await
        .unwrap();
}

async fn create_workflow_ticket(
    repo: &SqliteTicketRepo,
    job_id: JobId,
    workflow_id: &str,
    branch_id: &str,
    node_id: &str,
    state: TicketState,
) -> Ticket {
    let ticket = repo
        .create(NewTicket {
            job_id: Some(job_id),
            kind: ticket_op("synthetic.workflow.operation.test"),
            priority: 0,
            payload: serde_json::json!({
                "workflow_id": workflow_id,
                "branch_id": branch_id,
                "node_id": node_id,
                "rendered_payload": {"operation": "test"}
            }),
            max_attempts: 3,
            created_at: OffsetDateTime::UNIX_EPOCH,
        })
        .await
        .unwrap();
    sqlx::query("UPDATE tickets SET state = ? WHERE id = ?")
        .bind(state.as_str())
        .bind(i64::try_from(ticket.id.0).unwrap())
        .execute(&repo.pool)
        .await
        .unwrap();
    repo.get(ticket.id).await.unwrap().unwrap()
}
