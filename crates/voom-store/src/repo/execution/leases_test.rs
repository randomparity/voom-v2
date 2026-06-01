use super::*;

use serde_json::json;
use time::Duration;
use voom_core::clock_test_support::FrozenClock;
use voom_core::rng_test_support::FrozenRng;
use voom_core::{FailureClass, LeaseId, TicketId, TicketOperation, VoomError, WorkerId};

use crate::repo::execution::tickets::{NewTicket, SqliteTicketRepo, TicketRepo, TicketState};
use crate::repo::execution::workers::{NewWorker, SqliteWorkerRepo, WorkerKind, WorkerRepo};
use crate::test_support::{T0, fresh_initialized_pool_at};

/// Helper: build a (clock, rng) pair pinned to `T0` and the jitter
/// floor. Pinning jitter to 0 (`FrozenRng::new(0)`) makes the
/// `next_eligible_at` math exact: `now + 0`.
fn test_clock() -> FrozenClock {
    FrozenClock::new(T0)
}

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
    tempfile::NamedTempFile,
) {
    let tmp = tempfile::NamedTempFile::new().unwrap();
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
    (pool, trepo, wrepo, lrepo, t.id, w.id, tmp)
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
            &test_clock(),
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
            &test_clock(),
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
            &test_clock(),
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
                &test_clock(),
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
            &test_clock(),
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
    let tmp = tempfile::NamedTempFile::new().unwrap();
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
    let tmp = tempfile::NamedTempFile::new().unwrap();
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
        SqliteTicketRepo::default_backoff(0, &test_clock(), &mut rng_floor),
        Duration::seconds(0)
    );
    assert_eq!(
        SqliteTicketRepo::default_backoff(5, &test_clock(), &mut rng_floor),
        Duration::seconds(0)
    );

    // Ceiling (FrozenRng(u32::MAX)) — matches `min(cap, base * 2^attempt)`.
    let mut rng_ceil = FrozenRng::new(u32::MAX);
    // attempt=0: base*2^0 = 5s, < cap → 5s.
    assert_eq!(
        SqliteTicketRepo::default_backoff(0, &test_clock(), &mut rng_ceil),
        Duration::seconds(5)
    );
    // attempt=1: 10s.
    assert_eq!(
        SqliteTicketRepo::default_backoff(1, &test_clock(), &mut rng_ceil),
        Duration::seconds(10)
    );
    // attempt=2: 20s.
    assert_eq!(
        SqliteTicketRepo::default_backoff(2, &test_clock(), &mut rng_ceil),
        Duration::seconds(20)
    );
    // attempt=20: base*2^20 = ~5M s, clamps to cap=300s.
    assert_eq!(
        SqliteTicketRepo::default_backoff(20, &test_clock(), &mut rng_ceil),
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
            &test_clock(),
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
            &test_clock(),
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
            &test_clock(),
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
            &test_clock(),
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
    let tmp = tempfile::NamedTempFile::new().unwrap();
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
    for i in 0..total {
        let t = trepo
            .create(NewTicket {
                job_id: None,
                kind: ticket_op(&format!("k-{i}")),
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
