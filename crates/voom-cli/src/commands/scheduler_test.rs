use serde_json::json;
use time::OffsetDateTime;
use voom_core::{NodeId, TicketId, WorkerId};
use voom_store::repo::scheduler_decisions::{
    NewSchedulerDecision, SchedulerDecisionKind, SchedulerDecisionOutcome, SchedulerDecisionRepo,
    SchedulerReasonCode, SchedulerRequestSource,
};

use super::*;

#[tokio::test]
async fn decision_data_maps_full_record() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let url = voom_store::test_support::sqlite_url_for(tmp.path());
    voom_store::init(&url).await.unwrap();
    let pool = voom_store::connect(&url).await.unwrap();
    seed_refs(&pool).await;
    let repo = voom_store::repo::scheduler_decisions::SqliteSchedulerDecisionRepo::new(pool);
    let created = repo
        .create(NewSchedulerDecision {
            decision_kind: SchedulerDecisionKind::LeaseAcquire,
            request_source: SchedulerRequestSource::RemoteAcquire,
            idempotency_key: Some("idem".to_owned()),
            request_node_id: Some(NodeId(1)),
            request_worker_id: Some(WorkerId(2)),
            ticket_id: Some(TicketId(3)),
            selected_worker_id: Some(WorkerId(2)),
            selected_node_id: Some(NodeId(1)),
            selected_lease_id: None,
            outcome: SchedulerDecisionOutcome::Selected,
            reason_code: SchedulerReasonCode::Selected,
            summary: "selected".to_owned(),
            candidate_count: 1,
            selected_score: Some(100),
            suppression_key: None,
            explanation: json!({"scoring_version":1}),
            now: OffsetDateTime::UNIX_EPOCH,
        })
        .await
        .unwrap();

    let data = DecisionData::from(created);

    assert_eq!(data.id, 1);
    assert_eq!(data.outcome, "selected");
    assert_eq!(data.explanation_json, json!({"scoring_version":1}));
}

async fn seed_refs(pool: &sqlx::SqlitePool) {
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
}
