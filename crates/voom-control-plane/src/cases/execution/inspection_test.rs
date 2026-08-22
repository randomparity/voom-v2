use serde_json::json;
use sqlx::SqlitePool;
use time::OffsetDateTime;
use voom_core::{NodeId, TicketId, WorkerId};
use voom_store::repo::execution::scheduler_decisions::{
    NewSchedulerDecision, SchedulerDecisionFilter, SchedulerDecisionKind, SchedulerDecisionOutcome,
    SchedulerReasonCode, SchedulerRequestSource,
};

use crate::ControlPlane;

fn fresh_url() -> (voom_test_support::TempDatabase, String) {
    let tmp = voom_test_support::TempDatabase::new().unwrap();
    let url = format!("sqlite://{}", tmp.path().display());
    (tmp, url)
}

async fn seed_scheduler_decision_refs(pool: &SqlitePool) {
    sqlx::query(
        "INSERT INTO nodes \
         (id, name, kind, status, registered_at, last_seen_at, heartbeat_ttl_seconds, \
          auth_token_hash, auth_token_hint, metadata) \
         VALUES (1, 'node-1', 'remote', 'active', '1970-01-01T00:00:00Z', \
                 '1970-01-01T00:00:00Z', 60, 'hash', 'hint', '{}')",
    )
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO workers (id, name, kind, status, node_id, registered_at, last_seen_at) \
         VALUES (2, 'worker-2', 'remote', 'active', 1, \
                 '1970-01-01T00:00:00Z', '1970-01-01T00:00:00Z')",
    )
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO tickets \
         (id, job_id, kind, state, priority, payload, attempt, max_attempts, \
          next_eligible_at, created_at, state_changed_at) \
         VALUES (3, NULL, 'probe_file', 'ready', 0, '{}', 0, 3, \
                 '1970-01-01T00:00:00Z', '1970-01-01T00:00:00Z', \
                 '1970-01-01T00:00:00Z')",
    )
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO leases \
         (id, ticket_id, worker_id, state, acquired_at, expires_at, last_heartbeat_at, ttl_seconds) \
         VALUES (9, 3, 2, 'held', '1970-01-01T00:00:00Z', '1970-01-01T00:01:00Z', \
                 '1970-01-01T00:00:00Z', 60)",
    )
    .execute(pool)
    .await
    .unwrap();
}

fn test_scheduler_decision() -> NewSchedulerDecision {
    NewSchedulerDecision {
        decision_kind: SchedulerDecisionKind::LeaseAcquire,
        request_source: SchedulerRequestSource::RemoteAcquire,
        idempotency_key: Some("idem".to_owned()),
        request_node_id: Some(NodeId(1)),
        request_worker_id: Some(WorkerId(2)),
        ticket_id: Some(TicketId(3)),
        selected_worker_id: Some(WorkerId(2)),
        selected_node_id: Some(NodeId(1)),
        selected_lease_id: Some(voom_core::LeaseId(9)),
        outcome: SchedulerDecisionOutcome::Selected,
        reason_code: SchedulerReasonCode::Selected,
        summary: "selected".to_owned(),
        candidate_count: 1,
        selected_score: Some(100),
        suppression_key: None,
        access_evidence: None,
        explanation: json!({"scoring_version":1}),
        now: OffsetDateTime::UNIX_EPOCH,
    }
}

#[tokio::test]
async fn scheduler_decision_reads_are_exposed_without_exposing_repo_writes() {
    let (_keep, url) = fresh_url();
    voom_store::init(&url).await.unwrap();
    let pool = voom_store::connect(&url).await.unwrap();
    seed_scheduler_decision_refs(&pool).await;
    let repo =
        voom_store::repo::execution::scheduler_decisions::SqliteSchedulerDecisionRepo::new(pool);
    let inserted = repo.create(test_scheduler_decision()).await.unwrap();
    let cp = ControlPlane::open(&url).await.unwrap();

    let fetched = cp.scheduler_decision(inserted.id).await.unwrap().unwrap();
    let listed = cp
        .scheduler_decisions(SchedulerDecisionFilter {
            worker_id: Some(WorkerId(2)),
            ..SchedulerDecisionFilter::default()
        })
        .await
        .unwrap();

    assert_eq!(fetched.id, inserted.id);
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, inserted.id);
}
