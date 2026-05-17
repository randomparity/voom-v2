use super::*;

use time::OffsetDateTime;
use voom_core::VoomError;

use crate::test_support::fresh_initialized_pool_at;

async fn pool() -> (sqlx::SqlitePool, tempfile::NamedTempFile) {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let p = fresh_initialized_pool_at(tmp.path()).await.unwrap();
    (p, tmp)
}

fn sample_new_ticket() -> NewTicket {
    NewTicket {
        job_id: None,
        kind: "ingest.scan".to_owned(),
        priority: 0,
        payload: serde_json::json!({"path": "/tmp/x"}),
        max_attempts: 3,
        created_at: OffsetDateTime::UNIX_EPOCH,
    }
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
    use crate::repo::leases::{LeaseRepo, NewLease, SqliteLeaseRepo};
    use crate::repo::workers::{NewWorker, SqliteWorkerRepo, WorkerKind, WorkerRepo};
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
