use super::*;

use serde_json::json;
use time::Duration;
use voom_core::rng_test_support::FrozenRng;
use voom_core::{FailureClass, LeaseId, NodeId, TicketId, TicketOperation, VoomError, WorkerId};

use crate::repo::execution::jobs::{NewJob, SqliteJobRepo};
use crate::repo::execution::nodes::{NewNode, NodeKind, SqliteNodeRepo};
use crate::repo::execution::tickets::{NewTicket, SqliteTicketRepo, TicketState};
use crate::repo::execution::workers::{
    NewCapability, NewGrant, NewWorker, SqliteWorkerRepo, WorkerKind,
};
use crate::test_support::{T0, fresh_initialized_pool_at};

/// Jitter floor — `FrozenRng::new(0)` makes `default_backoff` return
/// `Duration::seconds(0)`, so `next_eligible_at == now`.
fn floor_rng() -> FrozenRng {
    FrozenRng::new(0)
}

/// Jitter ceiling — `FrozenRng::new(u32::MAX)` makes `default_backoff`
/// return the capped window (e.g. `min(cap, base * 2^attempt)` seconds).
fn ceiling_rng() -> FrozenRng {
    FrozenRng::new(u32::MAX)
}

fn ticket_op(value: &str) -> TicketOperation {
    TicketOperation::new(value).unwrap()
}

/// Returns the pool, the three repos, the seeded ticket id, the seeded
/// worker id, and the tempfile (caller must bind it to keep the `SQLite`
/// file alive for the duration of the test; `_tmp` underscore-binding
/// in the caller silences the unused-variable warning).
async fn setup() -> (
    sqlx::SqlitePool,
    SqliteTicketRepo,
    SqliteWorkerRepo,
    SqliteLeaseRepo,
    TicketId,
    WorkerId,
    voom_test_support::TempDatabase,
) {
    let tmp = voom_test_support::TempDatabase::new().unwrap();
    let pool = fresh_initialized_pool_at(tmp.path()).await.unwrap();
    let trepo = SqliteTicketRepo::new(pool.clone());
    let wrepo = SqliteWorkerRepo::new(pool.clone());
    let lrepo = SqliteLeaseRepo::new(pool.clone());
    let (tid, wid) = seed_ticket_and_worker(&trepo, &wrepo).await;
    (pool, trepo, wrepo, lrepo, tid, wid, tmp)
}

/// Seed one ready `noop` ticket and one worker eligible to run it.
async fn seed_ticket_and_worker(
    trepo: &SqliteTicketRepo,
    wrepo: &SqliteWorkerRepo,
) -> (TicketId, WorkerId) {
    let t = trepo
        .create(NewTicket {
            job_id: None,
            kind: ticket_op("noop"),
            priority: 0,
            payload: json!({}),
            max_attempts: 3,
            created_at: T0,
        })
        .await
        .unwrap();
    trepo.mark_ready_if_unblocked(t.id, T0).await.unwrap();
    let w = wrepo
        .register(NewWorker {
            name: "w-1".to_owned(),
            kind: WorkerKind::Synthetic,
            registered_at: T0,
            node_id: None,
        })
        .await
        .unwrap();
    make_worker_eligible(wrepo, w.id, ticket_op("noop")).await;
    (t.id, w.id)
}

async fn make_worker_eligible(
    workers: &SqliteWorkerRepo,
    worker_id: WorkerId,
    operation: TicketOperation,
) {
    workers
        .record_capability(NewCapability {
            worker_id,
            operation: operation.clone(),
            codecs: Vec::new(),
            hardware: Vec::new(),
            artifact_access: Vec::new(),
            extra: json!({}),
        })
        .await
        .unwrap();
    workers
        .record_grant(NewGrant {
            worker_id,
            can_execute: vec![operation],
            can_access_read: Vec::new(),
            can_access_write: Vec::new(),
            denies: Vec::new(),
            max_parallel: json!({}),
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn keyset_list_is_newest_first_filters_and_pages() {
    let (pool, trepo, wrepo, lrepo, tid, wid, _tmp) = setup().await;
    wrepo
        .record_grant(NewGrant {
            worker_id: wid,
            can_execute: vec![ticket_op("noop")],
            can_access_read: Vec::new(),
            can_access_write: Vec::new(),
            denies: Vec::new(),
            max_parallel: json!({"noop": 2}),
        })
        .await
        .unwrap();
    // `setup` seeds one ready ticket; add a second so two leases can coexist.
    let t2 = trepo
        .create(NewTicket {
            job_id: None,
            kind: ticket_op("noop"),
            priority: 0,
            payload: json!({}),
            max_attempts: 3,
            created_at: T0,
        })
        .await
        .unwrap();
    trepo.mark_ready_if_unblocked(t2.id, T0).await.unwrap();

    let first = lrepo
        .acquire(NewLease {
            ticket_id: tid,
            worker_id: wid,
            ttl: Duration::seconds(60),
            now: T0,
        })
        .await
        .unwrap();
    let second = lrepo
        .acquire(NewLease {
            ticket_id: t2.id,
            worker_id: wid,
            ttl: Duration::seconds(60),
            now: T0,
        })
        .await
        .unwrap();

    // Newest first (id DESC), ADR 0031.
    let all = lrepo.list(LeaseFilter::default(), None, 10).await.unwrap();
    assert_eq!(
        all.iter().map(|l| l.id).collect::<Vec<_>>(),
        vec![second.id, first.id]
    );

    // `after_id` continues into the older lease.
    let page = lrepo
        .list(LeaseFilter::default(), Some(second.id.0), 10)
        .await
        .unwrap();
    assert_eq!(
        page.iter().map(|l| l.id).collect::<Vec<_>>(),
        vec![first.id]
    );

    // State filter: both are held, none released.
    let held = lrepo
        .list(
            LeaseFilter {
                state: Some(LeaseState::Held),
            },
            None,
            10,
        )
        .await
        .unwrap();
    assert_eq!(held.len(), 2);
    let released = lrepo
        .list(
            LeaseFilter {
                state: Some(LeaseState::Released),
            },
            None,
            10,
        )
        .await
        .unwrap();
    assert!(released.is_empty());
    drop(pool);
}

#[tokio::test]
async fn acquire_promotes_ticket_to_leased_and_bumps_attempt() {
    let (pool, trepo, _wrepo, lrepo, tid, wid, _tmp) = setup().await;
    let lease = lrepo
        .acquire(NewLease {
            ticket_id: tid,
            worker_id: wid,
            ttl: Duration::seconds(60),
            now: T0,
        })
        .await
        .unwrap();
    assert_eq!(lease.state, LeaseState::Held);
    assert_eq!(lease.ttl_seconds, 60);
    let t = trepo.get(tid).await.unwrap().unwrap();
    assert_eq!(t.state, TicketState::Leased);
    assert_eq!(t.attempt, 1);
    drop(pool);
}

#[tokio::test]
async fn dispatch_context_projects_held_lease_worker_epoch_and_expiry() {
    let (pool, _trepo, _wrepo, lrepo, ticket_id, worker_id, _tmp) = setup().await;
    sqlx::query("UPDATE workers SET epoch = 7 WHERE id = ?")
        .bind(i64::try_from(worker_id.0).unwrap())
        .execute(&pool)
        .await
        .unwrap();
    let lease = lrepo
        .acquire(NewLease {
            ticket_id,
            worker_id,
            ttl: Duration::seconds(60),
            now: T0,
        })
        .await
        .unwrap();

    assert_eq!(
        lrepo.dispatch_context(lease.id).await.unwrap(),
        Some(LeaseDispatchContext {
            worker_id,
            worker_epoch: 7,
            expires_at: T0 + Duration::seconds(60),
        })
    );

    lrepo
        .release(lease.id, json!({}), T0 + Duration::seconds(1))
        .await
        .unwrap();
    assert_eq!(lrepo.dispatch_context(lease.id).await.unwrap(), None);
}

#[tokio::test]
async fn dispatch_context_returns_none_when_the_worker_row_is_missing() {
    let (pool, _trepo, _wrepo, lrepo, ticket_id, worker_id, _tmp) = setup().await;
    let lease = lrepo
        .acquire(NewLease {
            ticket_id,
            worker_id,
            ttl: Duration::seconds(60),
            now: T0,
        })
        .await
        .unwrap();
    let mut connection = pool.acquire().await.unwrap();
    sqlx::query("PRAGMA foreign_keys = OFF")
        .execute(&mut *connection)
        .await
        .unwrap();
    sqlx::query("DELETE FROM workers WHERE id = ?")
        .bind(i64::try_from(worker_id.0).unwrap())
        .execute(&mut *connection)
        .await
        .unwrap();
    drop(connection);

    assert_eq!(lrepo.dispatch_context(lease.id).await.unwrap(), None);
}

#[tokio::test]
async fn dispatch_context_rejects_negative_worker_epoch_as_database_error() {
    let (pool, _trepo, _wrepo, lrepo, ticket_id, worker_id, _tmp) = setup().await;
    let lease = lrepo
        .acquire(NewLease {
            ticket_id,
            worker_id,
            ttl: Duration::seconds(60),
            now: T0,
        })
        .await
        .unwrap();
    sqlx::query("UPDATE workers SET epoch = -1 WHERE id = ?")
        .bind(i64::try_from(worker_id.0).unwrap())
        .execute(&pool)
        .await
        .unwrap();

    let error = lrepo.dispatch_context(lease.id).await.unwrap_err();

    assert!(matches!(error, VoomError::Database { .. }));
    assert!(error.to_string().contains("epoch"));
}

#[tokio::test]
async fn dispatch_context_rejects_lease_id_above_sqlite_integer_range() {
    let (_pool, _trepo, _wrepo, lrepo, _ticket_id, _worker_id, _tmp) = setup().await;

    let error = lrepo.dispatch_context(LeaseId(u64::MAX)).await.unwrap_err();

    assert!(matches!(error, VoomError::Config(_)));
    assert!(error.to_string().contains("lease id"));
}

#[tokio::test]
async fn acquire_resolves_namespaced_workflow_ticket_kind_to_worker_operation() {
    let (pool, _trepo, wrepo, lrepo, ticket_id, worker_id, _tmp) = setup().await;
    make_worker_eligible(&wrepo, worker_id, ticket_op("probe_file")).await;
    sqlx::query("UPDATE tickets SET kind = 'synthetic.workflow.operation.probe_file' WHERE id = ?")
        .bind(i64::try_from(ticket_id.0).unwrap())
        .execute(&pool)
        .await
        .unwrap();

    let lease = lrepo
        .acquire(NewLease {
            ticket_id,
            worker_id,
            ttl: Duration::seconds(60),
            now: T0,
        })
        .await
        .unwrap();

    assert_eq!(lease.ticket_id, ticket_id);
}

#[tokio::test]
async fn acquire_fails_closed_on_an_unknown_namespaced_ticket_kind() {
    // `acquire_guarded` handles exactly one ticket, so it is safe to raise here —
    // unlike the capability lookups, which run inside a candidate loop.
    for kind in [
        "synthetic.workflow.operation.bogus",
        "synthetic.workflow.operation.",
    ] {
        let (pool, _trepo, _wrepo, lrepo, ticket_id, worker_id, _tmp) = setup().await;
        sqlx::query("UPDATE tickets SET kind = ? WHERE id = ?")
            .bind(kind)
            .bind(i64::try_from(ticket_id.0).unwrap())
            .execute(&pool)
            .await
            .unwrap();

        let error = lrepo
            .acquire(NewLease {
                ticket_id,
                worker_id,
                ttl: Duration::seconds(60),
                now: T0,
            })
            .await
            .unwrap_err();

        assert!(
            matches!(error, VoomError::Database { .. }),
            "acquire of {kind} returned {error:?}"
        );
        let message = error.to_string();
        assert!(message.contains("ticket kind"), "message was {message}");
        assert!(message.contains(kind), "message was {message}");
    }
}

#[tokio::test]
async fn acquire_still_accepts_an_exact_custom_local_ticket_kind() {
    // `noop` is outside every reserved namespace, so it stays exactly itself.
    let (_pool, _trepo, _wrepo, lrepo, ticket_id, worker_id, _tmp) = setup().await;

    let lease = lrepo
        .acquire(NewLease {
            ticket_id,
            worker_id,
            ttl: Duration::seconds(60),
            now: T0,
        })
        .await
        .unwrap();

    assert_eq!(lease.ticket_id, ticket_id);
}

#[tokio::test]
async fn acquire_rejects_when_ticket_not_ready() {
    let (_pool, _trepo, _wrepo, lrepo, tid, wid, _tmp) = setup().await;
    lrepo
        .acquire(NewLease {
            ticket_id: tid,
            worker_id: wid,
            ttl: Duration::seconds(60),
            now: T0,
        })
        .await
        .unwrap();
    // Second acquire on the same ticket — ticket is now leased.
    let err = lrepo
        .acquire(NewLease {
            ticket_id: tid,
            worker_id: wid,
            ttl: Duration::seconds(60),
            now: T0,
        })
        .await
        .unwrap_err();
    assert!(matches!(err, VoomError::Conflict(_)));
}

#[derive(Debug, Clone, Copy)]
enum IneligibleWorkerState {
    Missing,
    Stale,
    Retired,
    MissingCapability,
    MissingGrant,
    Denied,
}

impl IneligibleWorkerState {
    const ALL: [Self; 6] = [
        Self::Missing,
        Self::Stale,
        Self::Retired,
        Self::MissingCapability,
        Self::MissingGrant,
        Self::Denied,
    ];

    const fn error_fragment(self) -> &'static str {
        match self {
            Self::Missing => "not found",
            Self::Stale => "stale",
            Self::Retired => "retired",
            Self::MissingCapability => "capability",
            Self::MissingGrant => "grant",
            Self::Denied => "denied",
        }
    }
}

#[tokio::test]
async fn acquire_rejects_ineffective_worker_without_partial_durable_state() {
    for state in IneligibleWorkerState::ALL {
        let (pool, trepo, wrepo, lrepo, ticket_id, worker_id, _tmp) = setup().await;
        make_worker_ineligible(&pool, &wrepo, worker_id, state).await;
        let before = trepo.get(ticket_id).await.unwrap().unwrap();
        let mut tx = pool.begin().await.unwrap();

        let err = lrepo
            .acquire_in_tx(
                &mut tx,
                NewLease {
                    ticket_id,
                    worker_id,
                    ttl: Duration::seconds(60),
                    now: T0,
                },
            )
            .await
            .unwrap_err();
        tx.commit().await.unwrap();

        assert!(
            err.to_string().contains(state.error_fragment()),
            "state={state:?}, error={err}"
        );
        let after = trepo.get(ticket_id).await.unwrap().unwrap();
        assert_eq!(after.state, TicketState::Ready, "state={state:?}");
        assert_eq!(after.attempt, before.attempt, "state={state:?}");
        assert_eq!(after.epoch, before.epoch, "state={state:?}");
        let lease_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM leases")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(lease_count, 0, "state={state:?}");
    }
}

async fn make_worker_ineligible(
    pool: &sqlx::SqlitePool,
    workers: &SqliteWorkerRepo,
    worker_id: WorkerId,
    state: IneligibleWorkerState,
) {
    match state {
        IneligibleWorkerState::Missing => {
            sqlx::query("DELETE FROM workers WHERE id = ?")
                .bind(i64::try_from(worker_id.0).unwrap())
                .execute(pool)
                .await
                .unwrap();
        }
        IneligibleWorkerState::Stale => {
            sqlx::query("UPDATE workers SET status = 'stale' WHERE id = ?")
                .bind(i64::try_from(worker_id.0).unwrap())
                .execute(pool)
                .await
                .unwrap();
        }
        IneligibleWorkerState::Retired => {
            sqlx::query("UPDATE workers SET status = 'retired', retired_at = ? WHERE id = ?")
                .bind("1970-01-01T00:00:00Z")
                .bind(i64::try_from(worker_id.0).unwrap())
                .execute(pool)
                .await
                .unwrap();
        }
        IneligibleWorkerState::MissingCapability => {
            sqlx::query("DELETE FROM worker_capabilities WHERE worker_id = ?")
                .bind(i64::try_from(worker_id.0).unwrap())
                .execute(pool)
                .await
                .unwrap();
        }
        IneligibleWorkerState::MissingGrant => {
            sqlx::query("DELETE FROM worker_grants WHERE worker_id = ?")
                .bind(i64::try_from(worker_id.0).unwrap())
                .execute(pool)
                .await
                .unwrap();
        }
        IneligibleWorkerState::Denied => {
            workers
                .record_grant(NewGrant {
                    worker_id,
                    can_execute: Vec::new(),
                    can_access_read: Vec::new(),
                    can_access_write: Vec::new(),
                    denies: vec![ticket_op("noop")],
                    max_parallel: json!({}),
                })
                .await
                .unwrap();
        }
    }
}

#[tokio::test]
async fn heartbeat_extends_expires_at() {
    let (_pool, _trepo, _wrepo, lrepo, tid, wid, _tmp) = setup().await;
    let l1 = lrepo
        .acquire(NewLease {
            ticket_id: tid,
            worker_id: wid,
            ttl: Duration::seconds(60),
            now: T0,
        })
        .await
        .unwrap();
    let l2 = lrepo
        .heartbeat(l1.id, Duration::seconds(60), T0 + Duration::seconds(30))
        .await
        .unwrap();
    assert!(l2.expires_at > l1.expires_at);
    assert_eq!(l2.last_heartbeat_at, T0 + Duration::seconds(30));
}

#[tokio::test]
async fn heartbeat_rejects_expired_held_lease_without_reviving_it() {
    let (_pool, _trepo, _wrepo, lrepo, tid, wid, _tmp) = setup().await;
    let lease = lrepo
        .acquire(NewLease {
            ticket_id: tid,
            worker_id: wid,
            ttl: Duration::seconds(60),
            now: T0,
        })
        .await
        .unwrap();

    let error = lrepo
        .heartbeat(lease.id, Duration::seconds(60), lease.expires_at)
        .await
        .unwrap_err();
    assert_eq!(error.error_code(), voom_core::ErrorCode::Conflict);

    let persisted = lrepo.get(lease.id).await.unwrap().unwrap();
    assert_eq!(persisted.state, LeaseState::Held);
    assert_eq!(persisted.expires_at, lease.expires_at);
    assert_eq!(persisted.last_heartbeat_at, lease.last_heartbeat_at);
    assert_eq!(persisted.epoch, lease.epoch);
}

#[tokio::test]
async fn heartbeat_never_shortens_expires_at_but_still_records_beat() {
    // M5 regression: a heartbeat carrying a shorter TTL than the lease's
    // current deadline must NOT move expires_at backwards (a shortened
    // deadline could let expire_due reap a lease whose worker just proved
    // it is alive). The heartbeat time must still be recorded — the worker
    // did beat, and dropping that signal is its own bug.
    let (_pool, _trepo, _wrepo, lrepo, tid, wid, _tmp) = setup().await;
    let l1 = lrepo
        .acquire(NewLease {
            ticket_id: tid,
            worker_id: wid,
            ttl: Duration::seconds(60),
            now: T0,
        })
        .await
        .unwrap();
    // Heartbeat 5s in with a 10s TTL → candidate deadline T0+15, which is
    // *earlier* than the current T0+60. The deadline must stay at T0+60.
    let l2 = lrepo
        .heartbeat(l1.id, Duration::seconds(10), T0 + Duration::seconds(5))
        .await
        .unwrap();
    assert_eq!(
        l2.expires_at, l1.expires_at,
        "shortening heartbeat must not move expires_at backwards"
    );
    assert_eq!(
        l2.last_heartbeat_at,
        T0 + Duration::seconds(5),
        "the heartbeat must still be recorded even when the deadline is unchanged"
    );
}

#[tokio::test]
async fn release_transitions_lease_and_ticket_to_succeeded() {
    let (_pool, trepo, _wrepo, lrepo, tid, wid, _tmp) = setup().await;
    let l = lrepo
        .acquire(NewLease {
            ticket_id: tid,
            worker_id: wid,
            ttl: Duration::seconds(60),
            now: T0,
        })
        .await
        .unwrap();
    lrepo
        .release(l.id, json!({"ok": true}), T0 + Duration::seconds(5))
        .await
        .unwrap();
    let lease = lrepo.get(l.id).await.unwrap().unwrap();
    assert_eq!(lease.state, LeaseState::Released);
    assert_eq!(lease.release_reason, Some(ReleaseReason::Released));
    let t = trepo.get(tid).await.unwrap().unwrap();
    assert_eq!(t.state, TicketState::Succeeded);
    assert_eq!(t.result.unwrap(), json!({"ok": true}));
}

#[tokio::test]
async fn get_held_for_worker_returns_held_lease_for_matching_worker() {
    let (_pool, _trepo, _wrepo, lrepo, tid, wid, _tmp) = setup().await;
    let lease = lrepo
        .acquire(NewLease {
            ticket_id: tid,
            worker_id: wid,
            ttl: Duration::seconds(60),
            now: T0,
        })
        .await
        .unwrap();

    let found = lrepo.get_held_for_worker(lease.id, wid).await.unwrap();

    assert_eq!(found.id, lease.id);
    assert_eq!(found.worker_id, wid);
    assert_eq!(found.state, LeaseState::Held);
}

#[tokio::test]
async fn get_held_for_worker_returns_conflict_for_wrong_worker() {
    let (_pool, _trepo, wrepo, lrepo, tid, wid, _tmp) = setup().await;
    let other = wrepo
        .register(NewWorker {
            name: "w-2".to_owned(),
            kind: WorkerKind::Synthetic,
            registered_at: T0,
            node_id: None,
        })
        .await
        .unwrap();
    let lease = lrepo
        .acquire(NewLease {
            ticket_id: tid,
            worker_id: wid,
            ttl: Duration::seconds(60),
            now: T0,
        })
        .await
        .unwrap();

    let err = lrepo
        .get_held_for_worker(lease.id, other.id)
        .await
        .unwrap_err();

    assert!(matches!(err, VoomError::Conflict(_)), "got: {err:?}");
}

#[tokio::test]
async fn get_held_for_worker_returns_conflict_for_non_held_lease() {
    let (_pool, _trepo, _wrepo, lrepo, tid, wid, _tmp) = setup().await;
    let lease = lrepo
        .acquire(NewLease {
            ticket_id: tid,
            worker_id: wid,
            ttl: Duration::seconds(60),
            now: T0,
        })
        .await
        .unwrap();
    lrepo
        .release(lease.id, json!({"ok": true}), T0 + Duration::seconds(5))
        .await
        .unwrap();

    let err = lrepo.get_held_for_worker(lease.id, wid).await.unwrap_err();

    assert!(matches!(err, VoomError::Conflict(_)), "got: {err:?}");
}

#[tokio::test]
async fn get_held_for_worker_returns_not_found_for_missing_lease() {
    let (_pool, _trepo, _wrepo, lrepo, _tid, wid, _tmp) = setup().await;

    let err = lrepo
        .get_held_for_worker(LeaseId(99_999), wid)
        .await
        .unwrap_err();

    assert!(matches!(err, VoomError::NotFound(_)), "got: {err:?}");
}

#[tokio::test]
async fn fail_retriable_requeues_ticket_and_sets_backoff() {
    let (_pool, trepo, _wrepo, lrepo, tid, wid, _tmp) = setup().await;
    let l = lrepo
        .acquire(NewLease {
            ticket_id: tid,
            worker_id: wid,
            ttl: Duration::seconds(60),
            now: T0,
        })
        .await
        .unwrap();
    // Ceiling jitter (FrozenRng(u32::MAX)) makes the backoff window
    // exactly `min(cap, base * 2^attempt)`. attempt=1 here, base=5s,
    // cap=300s → window = 10s.
    lrepo
        .fail(
            l.id,
            FailureClass::WorkerTimeout,
            T0 + Duration::seconds(10),
            &mut ceiling_rng(),
        )
        .await
        .unwrap();
    let t = trepo.get(tid).await.unwrap().unwrap();
    assert_eq!(t.state, TicketState::Ready);
    assert_eq!(t.attempt, 1, "attempt not bumped on requeue");
    assert_eq!(
        t.next_eligible_at,
        T0 + Duration::seconds(10) + Duration::seconds(10),
        "ceiling jitter for attempt=1 should give a 10s window"
    );
}

#[tokio::test]
async fn fail_retriable_with_floor_rng_sets_next_eligible_to_now() {
    let (_pool, trepo, _wrepo, lrepo, tid, wid, _tmp) = setup().await;
    let l = lrepo
        .acquire(NewLease {
            ticket_id: tid,
            worker_id: wid,
            ttl: Duration::seconds(60),
            now: T0,
        })
        .await
        .unwrap();
    lrepo
        .fail(
            l.id,
            FailureClass::WorkerTimeout,
            T0 + Duration::seconds(10),
            &mut floor_rng(),
        )
        .await
        .unwrap();
    let t = trepo.get(tid).await.unwrap().unwrap();
    assert_eq!(t.state, TicketState::Ready);
    // Floor jitter — backoff is 0s, so next_eligible_at == now.
    assert_eq!(t.next_eligible_at, T0 + Duration::seconds(10));
}

#[tokio::test]
async fn fail_with_non_retriable_class_skips_requeue_even_when_attempts_remain() {
    let (_pool, trepo, _wrepo, lrepo, tid, wid, _tmp) = setup().await;
    let l = lrepo
        .acquire(NewLease {
            ticket_id: tid,
            worker_id: wid,
            ttl: Duration::seconds(60),
            now: T0,
        })
        .await
        .unwrap();
    // attempt=1, max_attempts=3 → would have requeued for a retriable
    // class. MalformedWorkerResult (non-retriable) must transition
    // straight to terminal failure regardless of remaining attempts.
    lrepo
        .fail(
            l.id,
            FailureClass::MalformedWorkerResult,
            T0 + Duration::seconds(5),
            &mut floor_rng(),
        )
        .await
        .unwrap();
    let t = trepo.get(tid).await.unwrap().unwrap();
    assert_eq!(t.state, TicketState::Failed);
    assert_eq!(t.attempt, 1, "attempt should not have advanced past 1");
}

#[tokio::test]
async fn fail_terminal_marks_ticket_failed() {
    let (_pool, trepo, _wrepo, lrepo, tid, wid, _tmp) = setup().await;
    // Burn through 3 attempts.
    for i in 0..3 {
        let l = lrepo
            .acquire(NewLease {
                ticket_id: tid,
                worker_id: wid,
                ttl: Duration::seconds(60),
                now: T0 + Duration::seconds(60 * i),
            })
            .await
            .unwrap();
        lrepo
            .fail(
                l.id,
                FailureClass::WorkerTimeout,
                T0 + Duration::seconds(60 * i + 1),
                &mut floor_rng(),
            )
            .await
            .unwrap();
        if i < 2 {
            // ready again for the next acquire
            assert_eq!(
                trepo.get(tid).await.unwrap().unwrap().state,
                TicketState::Ready
            );
        }
    }
    let t = trepo.get(tid).await.unwrap().unwrap();
    assert_eq!(t.state, TicketState::Failed);
    assert_eq!(t.attempt, 3);
}

#[tokio::test]
async fn expire_due_requeues_overdue_leases() {
    let (_pool, trepo, _wrepo, lrepo, tid, wid, _tmp) = setup().await;
    let l = lrepo
        .acquire(NewLease {
            ticket_id: tid,
            worker_id: wid,
            ttl: Duration::seconds(10),
            now: T0,
        })
        .await
        .unwrap();
    let report = lrepo.expire_due(T0 + Duration::seconds(11)).await.unwrap();
    assert_eq!(report.expired_leases, vec![l.id]);
    assert_eq!(report.requeued_tickets, vec![tid]);
    assert!(report.failed_expiries.is_empty());
    let t = trepo.get(tid).await.unwrap().unwrap();
    assert_eq!(t.state, TicketState::Ready);
}

#[tokio::test]
async fn expire_due_requeue_resets_next_eligible_at() {
    // A retriable failure leaves a future next_eligible_at (backoff). When
    // the *next* lease later expires, expire_due must reset next_eligible_at
    // to the expiry `now` — like force_release and fail_retriable do — so the
    // requeued ticket is immediately eligible and never carries a stale value.
    let (_pool, trepo, _wrepo, lrepo, tid, wid, _tmp) = setup().await;

    // First attempt: fail retriable at T0+10 with ceiling jitter → ticket
    // requeued with next_eligible_at = T0+20 (a 10s backoff window).
    let l1 = lrepo
        .acquire(NewLease {
            ticket_id: tid,
            worker_id: wid,
            ttl: Duration::seconds(10),
            now: T0,
        })
        .await
        .unwrap();
    lrepo
        .fail(
            l1.id,
            FailureClass::WorkerTimeout,
            T0 + Duration::seconds(10),
            &mut ceiling_rng(),
        )
        .await
        .unwrap();
    let backed_off = trepo.get(tid).await.unwrap().unwrap();
    assert_eq!(
        backed_off.next_eligible_at,
        T0 + Duration::seconds(20),
        "precondition: retriable failure set a future backoff"
    );

    // Second attempt: acquire once eligible (acquire does not touch
    // next_eligible_at), then let this lease expire.
    let _l2 = lrepo
        .acquire(NewLease {
            ticket_id: tid,
            worker_id: wid,
            ttl: Duration::seconds(10),
            now: T0 + Duration::seconds(20),
        })
        .await
        .unwrap();
    let expire_now = T0 + Duration::seconds(31);
    let report = lrepo.expire_due(expire_now).await.unwrap();
    assert_eq!(report.requeued_tickets, vec![tid]);

    let requeued = trepo.get(tid).await.unwrap().unwrap();
    assert_eq!(requeued.state, TicketState::Ready);
    assert_eq!(
        requeued.next_eligible_at, expire_now,
        "expire_due must reset next_eligible_at to now, not keep the stale backoff"
    );
}

#[tokio::test]
async fn expire_due_second_call_is_a_no_op() {
    let (_pool, _trepo, _wrepo, lrepo, tid, wid, _tmp) = setup().await;
    let _l = lrepo
        .acquire(NewLease {
            ticket_id: tid,
            worker_id: wid,
            ttl: Duration::seconds(10),
            now: T0,
        })
        .await
        .unwrap();
    let _first = lrepo.expire_due(T0 + Duration::seconds(11)).await.unwrap();
    let second = lrepo.expire_due(T0 + Duration::seconds(11)).await.unwrap();
    assert!(second.expired_leases.is_empty());
    assert!(second.requeued_tickets.is_empty());
}

/// A deferred `BEGIN` that reads and then writes — ADR 0083's exact hazard,
/// run against the test's own pool so no production code gains a test hook.
///
/// While another connection holds the write lock this must be refused, and
/// refused *immediately*: `SQLite` declines the read→write upgrade without ever
/// consulting `busy_timeout`. That makes it a timing-free probe for "the write
/// lock is held right now" — the property a `sleep` can only guess at.
///
/// It is an assertion in both directions. If the upgrade *succeeds*, the lock
/// was not held, the window under test never existed, and the caller is told so
/// rather than passing vacuously.
async fn assert_deferred_upgrade_is_refused(pool: &sqlx::SqlitePool, when: &str) {
    let mut probe = pool
        .begin()
        .await
        .expect("a deferred BEGIN takes no lock, so it cannot block");
    sqlx::query("SELECT id FROM leases LIMIT 1")
        .fetch_optional(&mut *probe)
        .await
        .expect("the control read must succeed — it is what takes the read snapshot");
    // Matches no row, so even a succeeding control arm cannot perturb the
    // treatment's assertions. The write lock is requested when the statement
    // begins, before it knows that.
    let outcome = sqlx::query("UPDATE leases SET epoch = epoch WHERE id = -1")
        .execute(&mut *probe)
        .await;
    let error = outcome.expect_err(&format!(
        "control arm {when}: the deferred upgrade succeeded, so the write lock was \
         NOT held and this test proves nothing about contention"
    ));
    assert!(
        error.to_string().contains("database is locked"),
        "control arm {when}: expected the upgrade to be refused, got {error}"
    );
}

/// `expire_due` scans candidates and then updates them, so its transaction
/// must take the write lock at `BEGIN`. Under a deferred `BEGIN` the lock is
/// only requested at the first UPDATE, and `SQLite` refuses that lock upgrade
/// with `SQLITE_BUSY` *without* consulting `busy_timeout` (upgrading would
/// deadlock the two readers), so a concurrent writer made the whole call fail
/// instead of waiting its turn. Regression for the `database is locked` flake
/// in `voom-node-agent`'s `delayed_acquire_replay_never_dispatches`.
///
/// The writer is released by an ordered sequence, not a timer. A timed release
/// passes on any host slow enough that `expire_due` has not reached its first
/// `UPDATE` before the timer fires — a false green that grows more likely on
/// exactly the loaded CI where the flake lives.
///
/// **Residual.** Nothing exposes "this connection is now waiting on the write
/// lock", so step 4 cannot prove the treatment reached its `BEGIN`. If it has
/// not, the writer is released and the treatment runs uncontended — a vacuous
/// pass, failing toward a green treatment. The window is two fail-fast
/// round-trips rather than 200 ms. Owned by issue #588.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn expire_due_waits_out_a_concurrent_writer() {
    let (pool, trepo, _wrepo, lrepo, tid, wid, _tmp) = setup().await;
    let lease = lrepo
        .acquire(NewLease {
            ticket_id: tid,
            worker_id: wid,
            ttl: Duration::seconds(10),
            now: T0,
        })
        .await
        .unwrap();
    let competing_writer = pool.begin_with("BEGIN IMMEDIATE").await.unwrap();

    // 1. The lock is held and the window is real.
    assert_deferred_upgrade_is_refused(&pool, "before the treatment started").await;

    // 2. Start the treatment against that held lock, and wait until its task is
    //    actually running rather than merely spawned.
    let running = std::sync::Arc::new(tokio::sync::Notify::new());
    let treatment = tokio::spawn({
        let lrepo = lrepo.clone();
        let running = std::sync::Arc::clone(&running);
        async move {
            running.notify_one();
            lrepo.expire_due(T0 + Duration::seconds(11)).await
        }
    });
    running.notified().await;

    // 3. The lock is still held now that the treatment is under way, and each
    //    probe is real database work — so a host slow enough to delay the
    //    treatment delays these equally. That is the property a `sleep` lacks.
    //    One probe is not enough: measured against a deferred `expire_due`, the
    //    treatment had not yet reached its first UPDATE on the first pass.
    for probe in 0..4 {
        assert_deferred_upgrade_is_refused(&pool, "after the treatment started").await;
        // 4. A deferred `expire_due` fails at its first UPDATE without ever
        //    waiting, so finishing here means it did not wait. Unwrap inside
        //    the assertion so the panic carries its own error rather than a
        //    message about task state.
        assert!(
            !treatment.is_finished(),
            "expire_due returned while the write lock was held (probe {probe}): {:?}",
            treatment.await.expect("treatment task panicked")
        );
    }

    // 5. Release. The treatment must now succeed, having waited rather than failed.
    competing_writer.commit().await.unwrap();
    let report = treatment
        .await
        .expect("treatment task panicked")
        .expect("expire_due must wait out the writer, not fail with SQLITE_BUSY");
    assert_eq!(report.expired_leases, vec![lease.id]);
    assert_eq!(report.requeued_tickets, vec![tid]);
    assert_eq!(
        trepo.get(tid).await.unwrap().unwrap().state,
        TicketState::Ready
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pool_saturation_queues_heartbeats_until_writer_releases() {
    use std::future::Future as _;

    const CALLERS: usize = 12;
    let (pool, _trepo, _wrepo, lrepo, tid, wid, _tmp) = setup().await;
    let lease = lrepo
        .acquire(NewLease {
            ticket_id: tid,
            worker_id: wid,
            ttl: Duration::seconds(60),
            now: T0,
        })
        .await
        .unwrap();
    let writer = pool.begin_with("BEGIN IMMEDIATE").await.unwrap();
    let lease_id = lease.id;
    let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(CALLERS + 1));
    let first_pending = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let heartbeat_at = T0 + Duration::seconds(10);
    let mut handles = Vec::with_capacity(CALLERS);
    for _ in 0..CALLERS {
        let lrepo = lrepo.clone();
        let barrier = std::sync::Arc::clone(&barrier);
        let first_pending = std::sync::Arc::clone(&first_pending);
        handles.push(tokio::spawn(async move {
            barrier.wait().await;
            let heartbeat = lrepo.heartbeat(lease_id, Duration::seconds(60), heartbeat_at);
            tokio::pin!(heartbeat);
            let completed = std::future::poll_fn(|cx| match heartbeat.as_mut().poll(cx) {
                std::task::Poll::Pending => {
                    first_pending.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    std::task::Poll::Ready(None)
                }
                std::task::Poll::Ready(result) => std::task::Poll::Ready(Some(result)),
            })
            .await;
            match completed {
                Some(result) => result,
                None => heartbeat.await,
            }
        }));
    }
    barrier.wait().await;

    let saturated = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if first_pending.load(std::sync::atomic::Ordering::SeqCst) == CALLERS
                && pool.size() == 8
                && pool.num_idle() == 0
            {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await;
    let finished_while_locked = handles.iter().filter(|handle| handle.is_finished()).count();

    let writer_result = writer.commit().await;
    let mut heartbeat_results = Vec::with_capacity(CALLERS);
    for handle in handles {
        heartbeat_results.push(handle.await);
    }

    writer_result.expect("held writer must commit before heartbeat assertions");
    saturated.unwrap_or_else(|error| {
        panic!(
            "pool did not saturate while the writer was held: pending={}, size={}, idle={}, \
             finished={finished_while_locked}, error={error}",
            first_pending.load(std::sync::atomic::Ordering::SeqCst),
            pool.size(),
            pool.num_idle()
        )
    });
    let available_beside_writer = usize::try_from(pool.size()).unwrap() - 1;
    assert!(
        CALLERS > available_beside_writer,
        "{CALLERS} callers must exceed {available_beside_writer} non-writer connections"
    );
    assert_eq!(finished_while_locked, 0);
    for result in heartbeat_results {
        let heartbeat = result
            .expect("heartbeat task panicked")
            .expect("heartbeat must wait for the writer and succeed");
        assert_eq!(heartbeat.id, lease_id);
        assert_eq!(heartbeat.state, LeaseState::Held);
    }

    let stored = lrepo.get(lease_id).await.unwrap().unwrap();
    assert_eq!(stored.state, LeaseState::Held);
    assert!(stored.expires_at >= lease.expires_at);
    assert_eq!(stored.last_heartbeat_at, heartbeat_at);
    assert_eq!(stored.epoch, lease.epoch + u64::try_from(CALLERS).unwrap());

    let converged = lrepo
        .heartbeat(lease_id, Duration::seconds(60), T0 + Duration::seconds(20))
        .await
        .unwrap();
    assert_eq!(converged.epoch, stored.epoch + 1);
}

/// A pool whose connections refuse to wait, so contention is an immediate error
/// rather than a 30-second block. Every pooled connection is pragma'd, because
/// `busy_timeout` is connection-local and the one that matters is whichever the
/// call under test happens to draw.
async fn zero_busy_timeout_pool(path: &std::path::Path) -> sqlx::SqlitePool {
    let pool = fresh_initialized_pool_at(path).await.unwrap();
    let mut held = Vec::new();
    for _ in 0..8 {
        let mut conn = pool.acquire().await.unwrap();
        sqlx::query("PRAGMA busy_timeout = 0")
            .execute(&mut *conn)
            .await
            .unwrap();
        held.push(conn);
    }
    drop(held);
    pool
}

/// The mode itself, with no concurrency and no timing: under a zero
/// `busy_timeout` a held write lock makes `expire_due` fail, and *where* it
/// fails names the opener it used.
///
/// `BEGIN IMMEDIATE` asks for the lock at the opener, so the error carries the
/// opener's context. A deferred `BEGIN` gets past the opener and is refused at
/// the first `UPDATE`, carrying that statement's context instead. Reverting
/// #546 flips this assertion deterministically — where
/// `expire_due_waits_out_a_concurrent_writer` depends on the treatment reaching
/// its transaction while the lock is held, this depends on nothing but the
/// ordering `SQLite` guarantees.
#[tokio::test]
async fn expire_due_asks_for_the_write_lock_at_its_opener() {
    let tmp = voom_test_support::TempDatabase::new().unwrap();
    let pool = zero_busy_timeout_pool(tmp.path()).await;
    let trepo = SqliteTicketRepo::new(pool.clone());
    let wrepo = SqliteWorkerRepo::new(pool.clone());
    let lrepo = SqliteLeaseRepo::new(pool.clone());
    let (tid, wid) = seed_ticket_and_worker(&trepo, &wrepo).await;
    let lease = lrepo
        .acquire(NewLease {
            ticket_id: tid,
            worker_id: wid,
            ttl: Duration::seconds(10),
            now: T0,
        })
        .await
        .unwrap();
    let competing_writer = pool.begin_with("BEGIN IMMEDIATE").await.unwrap();

    let due = T0 + Duration::seconds(11);
    let error = lrepo
        .expire_due(due)
        .await
        .expect_err("a held write lock and a zero busy_timeout must fail expire_due");
    competing_writer.rollback().await.unwrap();

    let text = error.to_string();
    assert!(
        text.contains("leases: expire_due"),
        "expire_due must contend at its opener, not at a later statement: {text}"
    );
    assert!(
        !text.contains("lease expire"),
        "contention reported at the first UPDATE means the opener was deferred: {text}"
    );
    assert_eq!(
        lrepo.get(lease.id).await.unwrap().unwrap().state,
        lease.state,
        "a refused expire_due must leave the lease alone"
    );
    // Anti-vacuity: the same call, uncontended, must actually expire it. An
    // empty batch would make every assertion above pass without the opener
    // ever asking for the write lock.
    let report = lrepo.expire_due(due).await.unwrap();
    assert_eq!(
        report.expired_leases,
        vec![lease.id],
        "the fixture must present a genuinely overdue lease"
    );
}

#[tokio::test]
async fn expire_due_fails_terminal_when_no_retries_remain() {
    let (_pool, trepo, _wrepo, lrepo, tid, wid, _tmp) = setup().await;
    // First 2 expire_due cycles requeue; third should mark terminal.
    for i in 0..3 {
        let l = lrepo
            .acquire(NewLease {
                ticket_id: tid,
                worker_id: wid,
                ttl: Duration::seconds(10),
                now: T0 + Duration::seconds(20 * i),
            })
            .await
            .unwrap();
        let _ = l;
        let _ = lrepo
            .expire_due(T0 + Duration::seconds(20 * i + 11))
            .await
            .unwrap();
    }
    let t = trepo.get(tid).await.unwrap().unwrap();
    assert_eq!(t.state, TicketState::Failed);
    assert_eq!(t.attempt, 3);
}

#[tokio::test]
async fn force_release_with_requeue() {
    // setup() seeds max_attempts = 3, so after one acquire attempts remain
    // (1 < 3). also_requeue + attempts_remain → ticket goes back to ready.
    let (_pool, trepo, _wrepo, lrepo, tid, wid, _tmp) = setup().await;
    let l = lrepo
        .acquire(NewLease {
            ticket_id: tid,
            worker_id: wid,
            ttl: Duration::seconds(60),
            now: T0,
        })
        .await
        .unwrap();
    let outcome = lrepo
        .force_release(l.id, /*also_requeue=*/ true, T0 + Duration::seconds(1))
        .await
        .unwrap();
    assert!(
        outcome.ticket_requeued,
        "attempts remain, requeue requested → outcome.ticket_requeued"
    );
    assert_eq!(outcome.attempt, 1);
    assert_eq!(outcome.max_attempts, 3);
    let lease = lrepo.get(l.id).await.unwrap().unwrap();
    assert_eq!(lease.state, LeaseState::ForceReleased);
    let t = trepo.get(tid).await.unwrap().unwrap();
    assert_eq!(t.state, TicketState::Ready);
}

#[tokio::test]
async fn force_release_with_requeue_rejects_when_attempts_exhausted() {
    // A max_attempts=1 ticket whose only attempt was consumed by acquire
    // cannot be requeued — acquire's `attempt < max_attempts` predicate
    // would refuse it forever and no held lease remains to expire. The
    // spec's revised contract: refuse the call outright with Conflict,
    // leaving the lease/ticket/event log untouched. The operator must
    // explicitly retry with also_requeue=false if they intend a
    // terminal force-release.
    let tmp = voom_test_support::TempDatabase::new().unwrap();
    let pool = fresh_initialized_pool_at(tmp.path()).await.unwrap();
    let trepo = SqliteTicketRepo::new(pool.clone());
    let wrepo = SqliteWorkerRepo::new(pool.clone());
    let lrepo = SqliteLeaseRepo::new(pool.clone());
    let t = trepo
        .create(NewTicket {
            job_id: None,
            kind: ticket_op("noop"),
            priority: 0,
            payload: json!({}),
            max_attempts: 1,
            created_at: T0,
        })
        .await
        .unwrap();
    trepo.mark_ready_if_unblocked(t.id, T0).await.unwrap();
    let w = wrepo
        .register(NewWorker {
            name: "w-1".to_owned(),
            kind: WorkerKind::Synthetic,
            registered_at: T0,
            node_id: None,
        })
        .await
        .unwrap();
    make_worker_eligible(&wrepo, w.id, ticket_op("noop")).await;
    let l = lrepo
        .acquire(NewLease {
            ticket_id: t.id,
            worker_id: w.id,
            ttl: Duration::seconds(60),
            now: T0,
        })
        .await
        .unwrap();
    // After acquire: attempt = 1, max_attempts = 1, no attempts remain.
    let err = lrepo
        .force_release(l.id, /*also_requeue=*/ true, T0 + Duration::seconds(1))
        .await
        .unwrap_err();
    assert!(matches!(err, VoomError::Conflict(_)), "got: {err:?}");
    // No side effects: lease still held, ticket still leased.
    let lease_after = lrepo.get(l.id).await.unwrap().unwrap();
    assert_eq!(lease_after.state, LeaseState::Held);
    let ticket_after = trepo.get(t.id).await.unwrap().unwrap();
    assert_eq!(ticket_after.state, TicketState::Leased);

    // Same fixture with also_requeue = false succeeds.
    let outcome = lrepo
        .force_release(
            l.id,
            /*also_requeue=*/ false,
            T0 + Duration::seconds(2),
        )
        .await
        .unwrap();
    assert!(!outcome.ticket_requeued);
    let ticket = trepo.get(t.id).await.unwrap().unwrap();
    assert_eq!(ticket.state, TicketState::Failed);
}

#[tokio::test]
async fn force_release_with_requeue_marks_ready_when_attempts_remain() {
    // max_attempts = 2, one consumed by acquire (attempt = 1 < 2) → requeue
    // succeeds, ticket returns to ready for the next attempt.
    let tmp = voom_test_support::TempDatabase::new().unwrap();
    let pool = fresh_initialized_pool_at(tmp.path()).await.unwrap();
    let trepo = SqliteTicketRepo::new(pool.clone());
    let wrepo = SqliteWorkerRepo::new(pool.clone());
    let lrepo = SqliteLeaseRepo::new(pool.clone());
    let t = trepo
        .create(NewTicket {
            job_id: None,
            kind: ticket_op("noop"),
            priority: 0,
            payload: json!({}),
            max_attempts: 2,
            created_at: T0,
        })
        .await
        .unwrap();
    trepo.mark_ready_if_unblocked(t.id, T0).await.unwrap();
    let w = wrepo
        .register(NewWorker {
            name: "w-1".to_owned(),
            kind: WorkerKind::Synthetic,
            registered_at: T0,
            node_id: None,
        })
        .await
        .unwrap();
    make_worker_eligible(&wrepo, w.id, ticket_op("noop")).await;
    let l = lrepo
        .acquire(NewLease {
            ticket_id: t.id,
            worker_id: w.id,
            ttl: Duration::seconds(60),
            now: T0,
        })
        .await
        .unwrap();
    let outcome = lrepo
        .force_release(l.id, /*also_requeue=*/ true, T0 + Duration::seconds(1))
        .await
        .unwrap();
    assert!(outcome.ticket_requeued);
    assert_eq!(outcome.attempt, 1);
    assert_eq!(outcome.max_attempts, 2);
    let ticket = trepo.get(t.id).await.unwrap().unwrap();
    assert_eq!(ticket.state, TicketState::Ready);
}

#[tokio::test]
async fn force_release_without_requeue_fails_ticket() {
    let (_pool, trepo, _wrepo, lrepo, tid, wid, _tmp) = setup().await;
    let l = lrepo
        .acquire(NewLease {
            ticket_id: tid,
            worker_id: wid,
            ttl: Duration::seconds(60),
            now: T0,
        })
        .await
        .unwrap();
    lrepo
        .force_release(
            l.id,
            /*also_requeue=*/ false,
            T0 + Duration::seconds(1),
        )
        .await
        .unwrap();
    let t = trepo.get(tid).await.unwrap().unwrap();
    assert_eq!(t.state, TicketState::Failed);
}

#[test]
fn default_backoff_floor_is_zero_and_ceiling_caps_at_window() {
    // Floor (FrozenRng(0)) — always 0 seconds.
    let mut rng_floor = FrozenRng::new(0);
    assert_eq!(
        SqliteTicketRepo::default_backoff(0, &mut rng_floor),
        Duration::seconds(0)
    );
    assert_eq!(
        SqliteTicketRepo::default_backoff(5, &mut rng_floor),
        Duration::seconds(0)
    );

    // Ceiling (FrozenRng(u32::MAX)) — matches `min(cap, base * 2^attempt)`.
    let mut rng_ceil = FrozenRng::new(u32::MAX);
    // attempt=0: base*2^0 = 5s, < cap → 5s.
    assert_eq!(
        SqliteTicketRepo::default_backoff(0, &mut rng_ceil),
        Duration::seconds(5)
    );
    // attempt=1: 10s.
    assert_eq!(
        SqliteTicketRepo::default_backoff(1, &mut rng_ceil),
        Duration::seconds(10)
    );
    // attempt=2: 20s.
    assert_eq!(
        SqliteTicketRepo::default_backoff(2, &mut rng_ceil),
        Duration::seconds(20)
    );
    // attempt=20: base*2^20 = ~5M s, clamps to cap=300s.
    assert_eq!(
        SqliteTicketRepo::default_backoff(20, &mut rng_ceil),
        Duration::seconds(300)
    );
}

// --- rows_affected gates on lifecycle methods -----------------------------
//
// These tests use direct SQL to force the ticket out of the expected state
// between the read-lease and the update-ticket inside each lifecycle method.
// The row-count gate must surface this as Conflict and roll back the
// transaction, leaving the lease and ticket states untouched.

#[tokio::test]
async fn release_returns_conflict_when_ticket_no_longer_leased() {
    let (pool, _trepo, _wrepo, lrepo, tid, wid, _tmp) = setup().await;
    let l = lrepo
        .acquire(NewLease {
            ticket_id: tid,
            worker_id: wid,
            ttl: Duration::seconds(60),
            now: T0,
        })
        .await
        .unwrap();
    // Force the ticket out of 'leased' via direct SQL.
    sqlx::query("UPDATE tickets SET state = 'ready' WHERE id = ?")
        .bind(i64::try_from(tid.0).unwrap())
        .execute(&pool)
        .await
        .unwrap();
    let err = lrepo
        .release(l.id, json!({}), T0 + Duration::seconds(1))
        .await
        .unwrap_err();
    assert!(matches!(err, VoomError::Conflict(_)), "got: {err:?}");
    // Lease must NOT have transitioned (tx rolled back).
    let lease = lrepo.get(l.id).await.unwrap().unwrap();
    assert_eq!(lease.state, LeaseState::Held);
}

#[tokio::test]
async fn fail_retriable_returns_conflict_when_ticket_no_longer_leased() {
    let (pool, _trepo, _wrepo, lrepo, tid, wid, _tmp) = setup().await;
    let l = lrepo
        .acquire(NewLease {
            ticket_id: tid,
            worker_id: wid,
            ttl: Duration::seconds(60),
            now: T0,
        })
        .await
        .unwrap();
    sqlx::query("UPDATE tickets SET state = 'ready' WHERE id = ?")
        .bind(i64::try_from(tid.0).unwrap())
        .execute(&pool)
        .await
        .unwrap();
    // retriable + attempts remain → would take the requeue branch
    let err = lrepo
        .fail(
            l.id,
            FailureClass::WorkerTimeout,
            T0 + Duration::seconds(1),
            &mut floor_rng(),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, VoomError::Conflict(_)), "got: {err:?}");
    let lease = lrepo.get(l.id).await.unwrap().unwrap();
    assert_eq!(lease.state, LeaseState::Held);
}

#[tokio::test]
async fn fail_terminal_returns_conflict_when_ticket_no_longer_leased() {
    let (pool, trepo, _wrepo, lrepo, tid, wid, _tmp) = setup().await;
    // Burn through two attempts so the next fail goes terminal.
    let l1 = lrepo
        .acquire(NewLease {
            ticket_id: tid,
            worker_id: wid,
            ttl: Duration::seconds(60),
            now: T0,
        })
        .await
        .unwrap();
    lrepo
        .fail(
            l1.id,
            FailureClass::WorkerTimeout,
            T0 + Duration::seconds(1),
            &mut floor_rng(),
        )
        .await
        .unwrap();
    let now2 = trepo.get(tid).await.unwrap().unwrap().next_eligible_at + Duration::seconds(1);
    let l2 = lrepo
        .acquire(NewLease {
            ticket_id: tid,
            worker_id: wid,
            ttl: Duration::seconds(60),
            now: now2,
        })
        .await
        .unwrap();
    lrepo
        .fail(
            l2.id,
            FailureClass::WorkerTimeout,
            now2 + Duration::seconds(1),
            &mut floor_rng(),
        )
        .await
        .unwrap();
    let now3 = trepo.get(tid).await.unwrap().unwrap().next_eligible_at + Duration::seconds(1);
    let l3 = lrepo
        .acquire(NewLease {
            ticket_id: tid,
            worker_id: wid,
            ttl: Duration::seconds(60),
            now: now3,
        })
        .await
        .unwrap();
    // Knock the ticket out of 'leased'.
    sqlx::query("UPDATE tickets SET state = 'ready' WHERE id = ?")
        .bind(i64::try_from(tid.0).unwrap())
        .execute(&pool)
        .await
        .unwrap();
    // A retriable class with attempts exhausted hits the terminal branch.
    let err = lrepo
        .fail(
            l3.id,
            FailureClass::WorkerTimeout,
            now3 + Duration::seconds(2),
            &mut floor_rng(),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, VoomError::Conflict(_)), "got: {err:?}");
    let lease = lrepo.get(l3.id).await.unwrap().unwrap();
    assert_eq!(lease.state, LeaseState::Held);
}

#[tokio::test]
async fn expire_due_returns_conflict_when_ticket_no_longer_leased() {
    let (pool, _trepo, _wrepo, lrepo, tid, wid, _tmp) = setup().await;
    let _l = lrepo
        .acquire(NewLease {
            ticket_id: tid,
            worker_id: wid,
            ttl: Duration::seconds(10),
            now: T0,
        })
        .await
        .unwrap();
    // Flip the ticket out of 'leased' before expire_due runs.
    sqlx::query("UPDATE tickets SET state = 'ready' WHERE id = ?")
        .bind(i64::try_from(tid.0).unwrap())
        .execute(&pool)
        .await
        .unwrap();
    let err = lrepo
        .expire_due(T0 + Duration::seconds(11))
        .await
        .unwrap_err();
    assert!(matches!(err, VoomError::Conflict(_)), "got: {err:?}");
    // The whole tx rolled back: no lease transitioned to expired.
    let rows: Vec<(String,)> = sqlx::query_as("SELECT state FROM leases")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert!(
        rows.iter().all(|(s,)| s == "held"),
        "no lease should have transitioned after Conflict abort: {rows:?}"
    );
}

#[tokio::test]
async fn force_release_returns_conflict_when_ticket_no_longer_leased() {
    let (pool, _trepo, _wrepo, lrepo, tid, wid, _tmp) = setup().await;
    let l = lrepo
        .acquire(NewLease {
            ticket_id: tid,
            worker_id: wid,
            ttl: Duration::seconds(60),
            now: T0,
        })
        .await
        .unwrap();
    sqlx::query("UPDATE tickets SET state = 'ready' WHERE id = ?")
        .bind(i64::try_from(tid.0).unwrap())
        .execute(&pool)
        .await
        .unwrap();
    let err = lrepo
        .force_release(l.id, /*also_requeue=*/ true, T0 + Duration::seconds(1))
        .await
        .unwrap_err();
    assert!(matches!(err, VoomError::Conflict(_)), "got: {err:?}");
    let lease = lrepo.get(l.id).await.unwrap().unwrap();
    assert_eq!(lease.state, LeaseState::Held);
}

/// Regression for the unbounded `expire_due` candidate scan: with a
/// backlog larger than `LEASE_BATCH_LIMIT` a single call must cap the
/// processed set at the limit, and a follow-up call must drain the
/// remainder. Mirrors the M3 `reanchor_on_move_drains_past_batch_limit`
/// integration test but exercises the repo directly so the bound is
/// pinned at the SQL layer, not at the case handler.
#[tokio::test]
async fn expire_due_caps_at_lease_batch_limit_and_drains_remainder() {
    let tmp = voom_test_support::TempDatabase::new().unwrap();
    let pool = fresh_initialized_pool_at(tmp.path()).await.unwrap();
    let trepo = SqliteTicketRepo::new(pool.clone());
    let wrepo = SqliteWorkerRepo::new(pool.clone());
    let lrepo = SqliteLeaseRepo::new(pool.clone());
    let w = wrepo
        .register(NewWorker {
            name: "w-cap".to_owned(),
            kind: WorkerKind::Synthetic,
            registered_at: T0,
            node_id: None,
        })
        .await
        .unwrap();

    let limit = usize::try_from(LEASE_BATCH_LIMIT).unwrap();
    let total = limit + 1;
    let operations = (0..total)
        .map(|i| ticket_op(&format!("k-{i}")))
        .collect::<Vec<_>>();
    for operation in &operations {
        wrepo
            .record_capability(NewCapability {
                worker_id: w.id,
                operation: operation.clone(),
                codecs: Vec::new(),
                hardware: Vec::new(),
                artifact_access: Vec::new(),
                extra: json!({}),
            })
            .await
            .unwrap();
    }
    wrepo
        .record_grant(NewGrant {
            worker_id: w.id,
            can_execute: operations.clone(),
            can_access_read: Vec::new(),
            can_access_write: Vec::new(),
            denies: Vec::new(),
            max_parallel: json!({}),
        })
        .await
        .unwrap();
    for operation in operations {
        let t = trepo
            .create(NewTicket {
                job_id: None,
                kind: operation,
                priority: 0,
                payload: json!({}),
                max_attempts: 3,
                created_at: T0,
            })
            .await
            .unwrap();
        trepo.mark_ready_if_unblocked(t.id, T0).await.unwrap();
        let _l = lrepo
            .acquire(NewLease {
                ticket_id: t.id,
                worker_id: w.id,
                ttl: Duration::seconds(10),
                now: T0,
            })
            .await
            .unwrap();
    }

    let first = lrepo.expire_due(T0 + Duration::seconds(11)).await.unwrap();
    assert_eq!(
        first.expired_leases.len(),
        limit,
        "first call must cap at LEASE_BATCH_LIMIT"
    );

    let second = lrepo.expire_due(T0 + Duration::seconds(11)).await.unwrap();
    assert_eq!(
        second.expired_leases.len(),
        total - limit,
        "second call must process the remainder"
    );

    let third = lrepo.expire_due(T0 + Duration::seconds(11)).await.unwrap();
    assert!(
        third.expired_leases.is_empty(),
        "no candidates remain after the drain"
    );
}

#[tokio::test]
async fn held_lease_probe_only_matches_the_requested_ticket() {
    let (pool, _trepo, _wrepo, lrepo, ticket_id, worker_id, _tmp) = setup().await;
    let lease = lrepo
        .acquire(NewLease {
            ticket_id,
            worker_id,
            ttl: Duration::seconds(60),
            now: T0,
        })
        .await
        .unwrap();
    let mut tx = pool.begin().await.unwrap();

    assert!(
        lrepo
            .has_held_for_ticket_in_tx(&mut tx, ticket_id)
            .await
            .unwrap()
    );
    assert!(
        !lrepo
            .has_held_for_ticket_in_tx(&mut tx, TicketId(ticket_id.0 + 1))
            .await
            .unwrap()
    );
    lrepo
        .release_in_tx(&mut tx, lease.id, json!({}), T0 + Duration::seconds(1))
        .await
        .unwrap();
    assert!(
        !lrepo
            .has_held_for_ticket_in_tx(&mut tx, ticket_id)
            .await
            .unwrap()
    );
    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn active_count_for_node_counts_only_held_leases_for_that_node() {
    let (pool, _trepo, _wrepo, lrepo, ticket_id, worker_id, _tmp) = setup().await;
    let nodes = SqliteNodeRepo::new(pool.clone());
    let mut tx = pool.begin().await.unwrap();
    let node = nodes
        .register_in_tx(
            &mut tx,
            NewNode {
                name: "lease-node".to_owned(),
                kind: NodeKind::Synthetic,
                registered_at: T0,
                heartbeat_ttl_seconds: 60,
                auth_token_hash: "voom-node-token-sha256-v1:lease-node".to_owned(),
                auth_token_hint: "lease-node".to_owned(),
                metadata: json!({}),
            },
        )
        .await
        .unwrap();
    sqlx::query("UPDATE workers SET node_id = ? WHERE id = ?")
        .bind(i64::try_from(node.id.0).unwrap())
        .bind(i64::try_from(worker_id.0).unwrap())
        .execute(&mut *tx)
        .await
        .unwrap();
    let lease = lrepo
        .acquire_in_tx(
            &mut tx,
            NewLease {
                ticket_id,
                worker_id,
                ttl: Duration::seconds(60),
                now: T0,
            },
        )
        .await
        .unwrap();

    assert_eq!(
        lrepo
            .active_count_for_node_in_tx(&mut tx, node.id)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        lrepo
            .active_count_for_node_in_tx(&mut tx, NodeId(node.id.0 + 1))
            .await
            .unwrap(),
        0
    );
    lrepo
        .release_in_tx(&mut tx, lease.id, json!({}), T0 + Duration::seconds(1))
        .await
        .unwrap();
    assert_eq!(
        lrepo
            .active_count_for_node_in_tx(&mut tx, node.id)
            .await
            .unwrap(),
        0
    );
    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn timeline_for_job_returns_held_and_released_worker_intervals_in_order() {
    let (pool, trepo, wrepo, lrepo, first_ticket_id, first_worker_id, _tmp) = setup().await;
    let job = SqliteJobRepo::new(pool.clone())
        .create(NewJob {
            kind: "workflow".to_owned(),
            priority: 0,
            created_at: T0,
        })
        .await
        .unwrap();
    sqlx::query("UPDATE tickets SET job_id = ? WHERE id = ?")
        .bind(i64::try_from(job.id.0).unwrap())
        .bind(i64::try_from(first_ticket_id.0).unwrap())
        .execute(&pool)
        .await
        .unwrap();
    let second_ticket = trepo
        .create(NewTicket {
            job_id: Some(job.id),
            kind: ticket_op("noop"),
            priority: 0,
            payload: json!({}),
            max_attempts: 1,
            created_at: T0,
        })
        .await
        .unwrap();
    trepo
        .mark_ready_if_unblocked(second_ticket.id, T0)
        .await
        .unwrap();
    let second_worker = wrepo
        .register(NewWorker {
            name: "w-2".to_owned(),
            kind: WorkerKind::Synthetic,
            registered_at: T0,
            node_id: None,
        })
        .await
        .unwrap();
    make_worker_eligible(&wrepo, second_worker.id, ticket_op("noop")).await;
    let first_lease = lrepo
        .acquire(NewLease {
            ticket_id: first_ticket_id,
            worker_id: first_worker_id,
            ttl: Duration::seconds(60),
            now: T0,
        })
        .await
        .unwrap();
    lrepo
        .release(first_lease.id, json!({}), T0 + Duration::seconds(10))
        .await
        .unwrap();
    lrepo
        .acquire(NewLease {
            ticket_id: second_ticket.id,
            worker_id: second_worker.id,
            ttl: Duration::seconds(60),
            now: T0 + Duration::seconds(5),
        })
        .await
        .unwrap();

    let timeline = lrepo.timeline_for_job(job.id).await.unwrap();

    assert_eq!(
        timeline,
        vec![
            LeaseInterval {
                worker_id: first_worker_id,
                acquired_at: T0,
                released_at: Some(T0 + Duration::seconds(10)),
            },
            LeaseInterval {
                worker_id: second_worker.id,
                acquired_at: T0 + Duration::seconds(5),
                released_at: None,
            },
        ]
    );
}

#[tokio::test]
async fn try_acquire_reports_a_not_ready_ticket_as_a_structured_outcome() {
    let (pool, trepo, _wrepo, lrepo, tid, wid, _tmp) = setup().await;
    lrepo
        .acquire(NewLease {
            ticket_id: tid,
            worker_id: wid,
            ttl: Duration::seconds(60),
            now: T0,
        })
        .await
        .unwrap();
    let leased = trepo.get(tid).await.unwrap().unwrap();

    let mut tx = pool.begin().await.unwrap();
    let outcome = lrepo
        .try_acquire_in_tx(
            &mut tx,
            NewLease {
                ticket_id: tid,
                worker_id: wid,
                ttl: Duration::seconds(60),
                now: T0,
            },
        )
        .await
        .unwrap();
    tx.commit().await.unwrap();

    match outcome {
        LeaseAcquireOutcome::TicketNotReady { ticket_id } => assert_eq!(ticket_id, tid),
        other => panic!("expected TicketNotReady, got {other:?}"),
    }
    // The rejected attempt mutated nothing: the ticket keeps its leased state
    // and attempt count from the successful acquisition, and no second lease
    // row exists.
    let after = trepo.get(tid).await.unwrap().unwrap();
    assert_eq!(after.state, TicketState::Leased);
    assert_eq!(after.attempt, leased.attempt);
    assert_eq!(after.epoch, leased.epoch);
    let lease_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM leases")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(lease_count, 1);
}

#[tokio::test]
async fn try_acquire_maps_every_ineligible_worker_state_to_a_structured_reason() {
    for (state, expected_reason) in [
        (
            IneligibleWorkerState::Missing,
            LeaseIneligibilityReason::WorkerMissing,
        ),
        (
            IneligibleWorkerState::Stale,
            LeaseIneligibilityReason::WorkerStale,
        ),
        (
            IneligibleWorkerState::Retired,
            LeaseIneligibilityReason::WorkerRetired,
        ),
        (
            IneligibleWorkerState::MissingCapability,
            LeaseIneligibilityReason::MissingCapability,
        ),
        (
            IneligibleWorkerState::MissingGrant,
            LeaseIneligibilityReason::MissingGrant,
        ),
        (
            IneligibleWorkerState::Denied,
            LeaseIneligibilityReason::OperationDenied,
        ),
    ] {
        let (pool, trepo, wrepo, lrepo, tid, wid, _tmp) = setup().await;
        make_worker_ineligible(&pool, &wrepo, wid, state).await;
        let before = trepo.get(tid).await.unwrap().unwrap();
        let mut tx = pool.begin().await.unwrap();
        let outcome = lrepo
            .try_acquire_in_tx(
                &mut tx,
                NewLease {
                    ticket_id: tid,
                    worker_id: wid,
                    ttl: Duration::seconds(60),
                    now: T0,
                },
            )
            .await
            .unwrap();
        tx.commit().await.unwrap();

        match outcome {
            LeaseAcquireOutcome::WorkerIneligible {
                worker_id,
                operation,
                reason,
            } => {
                assert_eq!(worker_id, wid, "state={state:?}");
                assert_eq!(operation, ticket_op("noop"), "state={state:?}");
                assert_eq!(reason, expected_reason, "state={state:?}");
            }
            other => panic!("state={state:?}, expected WorkerIneligible, got {other:?}"),
        }
        // The savepoint rolled the provisional ticket transition back.
        let after = trepo.get(tid).await.unwrap().unwrap();
        assert_eq!(after.state, TicketState::Ready, "state={state:?}");
        assert_eq!(after.attempt, before.attempt, "state={state:?}");
        assert_eq!(after.epoch, before.epoch, "state={state:?}");
        let lease_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM leases")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(lease_count, 0, "state={state:?}");
    }
}

#[tokio::test]
async fn try_acquire_preserves_the_capacity_full_outcome() {
    let (pool, trepo, wrepo, lrepo, tid, wid, _tmp) = setup().await;
    // The single grant slot is already consumed by a held lease.
    wrepo
        .record_grant(NewGrant {
            worker_id: wid,
            can_execute: vec![ticket_op("noop")],
            can_access_read: Vec::new(),
            can_access_write: Vec::new(),
            denies: Vec::new(),
            max_parallel: json!({"*": 1}),
        })
        .await
        .unwrap();
    lrepo
        .acquire(NewLease {
            ticket_id: tid,
            worker_id: wid,
            ttl: Duration::seconds(60),
            now: T0,
        })
        .await
        .unwrap();
    // A second ready ticket wants the same single-slot operation.
    let second = trepo
        .create(NewTicket {
            job_id: None,
            kind: ticket_op("noop"),
            priority: 0,
            payload: json!({}),
            max_attempts: 3,
            created_at: T0,
        })
        .await
        .unwrap();
    trepo.mark_ready_if_unblocked(second.id, T0).await.unwrap();

    let mut tx = pool.begin().await.unwrap();
    let outcome = lrepo
        .try_acquire_in_tx(
            &mut tx,
            NewLease {
                ticket_id: second.id,
                worker_id: wid,
                ttl: Duration::seconds(60),
                now: T0,
            },
        )
        .await
        .unwrap();
    tx.commit().await.unwrap();

    match outcome {
        LeaseAcquireOutcome::CapacityFull(saturation) => {
            assert_eq!(saturation.worker_id, wid);
            assert_eq!(saturation.operation, ticket_op("noop"));
            assert_eq!(saturation.active_leases, 1);
            assert_eq!(saturation.max_parallel, 1);
        }
        other => panic!("expected CapacityFull, got {other:?}"),
    }
    // Only the first acquisition's lease exists; the saturated attempt
    // rolled back.
    let lease_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM leases")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(lease_count, 1);
}

#[test]
fn into_lease_result_preserves_the_legacy_error_classification_for_changed_gates() {
    let not_ready = LeaseAcquireOutcome::TicketNotReady {
        ticket_id: TicketId(7),
    }
    .into_lease_result()
    .unwrap_err();
    match not_ready {
        VoomError::Conflict(message) => assert_eq!(
            message,
            "acquire rejected for ticket 7: not ready, not eligible, \
             parent job not open, or out of attempts"
        ),
        other => panic!("expected Conflict, got {other:?}"),
    }

    for (reason, expected) in [
        (
            LeaseIneligibilityReason::WorkerMissing,
            VoomError::NotFound("worker 9".to_owned()),
        ),
        (
            LeaseIneligibilityReason::WorkerStale,
            VoomError::Conflict("acquire rejected: worker 9 stale".to_owned()),
        ),
        (
            LeaseIneligibilityReason::WorkerRetired,
            VoomError::Conflict("acquire rejected: worker 9 retired".to_owned()),
        ),
        (
            LeaseIneligibilityReason::OperationDenied,
            VoomError::Conflict("acquire rejected: worker 9 denied operation noop".to_owned()),
        ),
        (
            LeaseIneligibilityReason::MissingCapability,
            VoomError::Conflict("acquire rejected: worker 9 missing capability noop".to_owned()),
        ),
        (
            LeaseIneligibilityReason::MissingGrant,
            VoomError::Conflict("acquire rejected: worker 9 missing grant noop".to_owned()),
        ),
    ] {
        let err = LeaseAcquireOutcome::WorkerIneligible {
            worker_id: WorkerId(9),
            operation: ticket_op("noop"),
            reason,
        }
        .into_lease_result()
        .unwrap_err();
        assert_eq!(err.to_string(), expected.to_string(), "reason={reason:?}");
    }

    let capacity = LeaseAcquireOutcome::CapacityFull(WorkerCapacitySaturation {
        worker_id: WorkerId(9),
        operation: ticket_op("noop"),
        active_leases: 2,
        max_parallel: 1,
    })
    .into_lease_result()
    .unwrap_err();
    assert!(
        matches!(capacity, VoomError::NoEligibleWorker(_)),
        "expected NoEligibleWorker, got {capacity:?}"
    );
}
