use super::*;

use serde_json::json;
use time::Duration;
use voom_core::{TicketId, VoomError, WorkerId};

use crate::repo::tickets::{NewTicket, SqliteTicketRepo, TicketRepo, TicketState};
use crate::repo::workers::{NewWorker, SqliteWorkerRepo, WorkerKind, WorkerRepo};
use crate::test_support::{T0, fresh_initialized_pool_at};

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
            kind: "noop".to_owned(),
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
    lrepo
        .fail(l.id, true, T0 + Duration::seconds(10))
        .await
        .unwrap();
    let t = trepo.get(tid).await.unwrap().unwrap();
    assert_eq!(t.state, TicketState::Ready);
    assert_eq!(t.attempt, 1, "attempt not bumped on requeue");
    // Backoff = 5s * attempt where attempt is the one we just dispatched (1).
    assert_eq!(
        t.next_eligible_at,
        T0 + Duration::seconds(10) + Duration::seconds(5)
    );
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
            .fail(l.id, true, T0 + Duration::seconds(60 * i + 1))
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
    assert!(report.failed_tickets.is_empty());
    let t = trepo.get(tid).await.unwrap().unwrap();
    assert_eq!(t.state, TicketState::Ready);
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
        .force_release(l.id, /*also_requeue=*/ true, T0 + Duration::seconds(1))
        .await
        .unwrap();
    let lease = lrepo.get(l.id).await.unwrap().unwrap();
    assert_eq!(lease.state, LeaseState::ForceReleased);
    let t = trepo.get(tid).await.unwrap().unwrap();
    assert_eq!(t.state, TicketState::Ready);
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
fn backoff_matches_5s_times_attempt() {
    assert_eq!(backoff(1), Duration::seconds(5));
    assert_eq!(backoff(2), Duration::seconds(10));
    assert_eq!(backoff(3), Duration::seconds(15));
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
        .fail(l.id, true, T0 + Duration::seconds(1))
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
        .fail(l1.id, true, T0 + Duration::seconds(1))
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
        .fail(l2.id, true, now2 + Duration::seconds(1))
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
    // !retriable would also take the terminal branch; using retriable=true here
    // hits the terminal branch via the attempts-exhausted condition.
    let err = lrepo
        .fail(l3.id, true, now3 + Duration::seconds(2))
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
