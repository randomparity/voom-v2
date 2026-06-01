use serde_json::json;
use time::OffsetDateTime;
use voom_core::{LeaseId, NodeId, TicketId, WorkerId};

use super::*;

const T0: OffsetDateTime = OffsetDateTime::UNIX_EPOCH;

async fn repo() -> (SqliteSchedulerDecisionRepo, tempfile::NamedTempFile) {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let pool = crate::test_support::fresh_initialized_pool_at(tmp.path())
        .await
        .unwrap();
    seed_scheduler_refs(&pool).await;
    (SqliteSchedulerDecisionRepo::new(pool), tmp)
}

async fn seed_scheduler_refs(pool: &sqlx::SqlitePool) {
    sqlx::query(
        "INSERT INTO nodes \
         (id, name, kind, status, registered_at, last_seen_at, heartbeat_ttl_seconds, \
          auth_token_hash, auth_token_hint, metadata) \
         VALUES (3, 'node-3', 'remote', 'active', '1970-01-01T00:00:00Z', \
                 '1970-01-01T00:00:00Z', 60, 'token-hash', 'hint', '{}')",
    )
    .execute(pool)
    .await
    .unwrap();

    sqlx::query(
        "INSERT INTO workers (id, name, kind, status, node_id, registered_at, last_seen_at) \
         VALUES (5, 'worker-5', 'remote', 'active', 3, \
                 '1970-01-01T00:00:00Z', '1970-01-01T00:00:00Z')",
    )
    .execute(pool)
    .await
    .unwrap();

    sqlx::query(
        "INSERT INTO tickets \
         (id, kind, state, priority, payload, attempt, max_attempts, next_eligible_at, \
          created_at, state_changed_at) \
         VALUES (7, 'probe_file', 'leased', 0, '{}', 1, 3, '1970-01-01T00:00:00Z', \
                 '1970-01-01T00:00:00Z', '1970-01-01T00:00:00Z')",
    )
    .execute(pool)
    .await
    .unwrap();

    sqlx::query(
        "INSERT INTO leases \
         (id, ticket_id, worker_id, state, acquired_at, expires_at, last_heartbeat_at, \
          ttl_seconds) \
         VALUES (11, 7, 5, 'held', '1970-01-01T00:00:00Z', '1970-01-01T00:01:00Z', \
                 '1970-01-01T00:00:00Z', 60)",
    )
    .execute(pool)
    .await
    .unwrap();

    sqlx::query(
        "INSERT INTO leases \
         (id, ticket_id, worker_id, state, acquired_at, expires_at, last_heartbeat_at, \
          ttl_seconds) \
         VALUES (12, 7, 5, 'held', '1970-01-01T00:00:00Z', '1970-01-01T00:01:00Z', \
                 '1970-01-01T00:00:00Z', 60)",
    )
    .execute(pool)
    .await
    .unwrap();

    sqlx::query(
        "INSERT INTO tickets \
         (id, kind, state, priority, payload, attempt, max_attempts, next_eligible_at, \
          created_at, state_changed_at) \
         VALUES (17, 'probe_file', 'leased', 0, '{}', 1, 3, '1970-01-01T00:00:00Z', \
                 '1970-01-01T00:00:00Z', '1970-01-01T00:00:00Z')",
    )
    .execute(pool)
    .await
    .unwrap();

    sqlx::query(
        "INSERT INTO leases \
         (id, ticket_id, worker_id, state, acquired_at, expires_at, last_heartbeat_at, \
          ttl_seconds) \
         VALUES (21, 17, 5, 'held', '1970-01-01T00:00:00Z', '1970-01-01T00:01:00Z', \
                 '1970-01-01T00:00:00Z', 60)",
    )
    .execute(pool)
    .await
    .unwrap();
}

fn selected_input() -> NewSchedulerDecision {
    NewSchedulerDecision {
        decision_kind: SchedulerDecisionKind::LeaseAcquire,
        request_source: SchedulerRequestSource::RemoteAcquire,
        idempotency_key: Some("idem-1".to_owned()),
        request_node_id: Some(NodeId(3)),
        request_worker_id: Some(WorkerId(5)),
        ticket_id: Some(TicketId(7)),
        selected_worker_id: Some(WorkerId(5)),
        selected_node_id: Some(NodeId(3)),
        selected_lease_id: None,
        outcome: SchedulerDecisionOutcome::Selected,
        reason_code: SchedulerReasonCode::Selected,
        summary: "selected worker 5 for ticket 7".to_owned(),
        candidate_count: 1,
        selected_score: Some(1700),
        suppression_key: None,
        explanation: json!({"scoring_version":1,"candidates":[]}),
        now: T0,
    }
}

#[tokio::test]
async fn create_selected_and_link_lease_round_trip() {
    let (repo, _tmp) = repo().await;

    let created = repo.create(selected_input()).await.unwrap();

    assert_eq!(created.outcome, SchedulerDecisionOutcome::Selected);
    assert_eq!(created.selected_lease_id, None);

    let linked = repo
        .link_selected_lease(created.id, LeaseId(11), T0)
        .await
        .unwrap();
    assert_eq!(linked.selected_lease_id, Some(LeaseId(11)));

    let fetched = repo.get(created.id).await.unwrap().unwrap();
    assert_eq!(fetched.selected_lease_id, Some(LeaseId(11)));
}

#[tokio::test]
async fn idle_decisions_are_suppressed_by_key() {
    let (repo, _tmp) = repo().await;
    let mut input = selected_input();
    input.decision_kind = SchedulerDecisionKind::Idle;
    input.ticket_id = None;
    input.selected_worker_id = None;
    input.selected_node_id = None;
    input.selected_score = None;
    input.outcome = SchedulerDecisionOutcome::Idle;
    input.reason_code = SchedulerReasonCode::NoReadyTicket;
    input.candidate_count = 0;
    input.suppression_key = Some("remote_acquire:worker:5:no_ready_ticket:0".to_owned());

    let first = repo.create_or_suppress(input.clone()).await.unwrap();
    let second = repo.create_or_suppress(input).await.unwrap();

    assert_eq!(second.id, first.id);
    assert_eq!(second.suppressed_count, 1);

    let rows = repo.list(SchedulerDecisionFilter::default()).await.unwrap();
    assert_eq!(rows.len(), 1);
}

#[tokio::test]
async fn suppression_key_keeps_selected_rows_separate_from_idle_rows() {
    let (repo, _tmp) = repo().await;
    let selected = repo.create(selected_input()).await.unwrap();
    let mut idle = selected_input();
    idle.decision_kind = SchedulerDecisionKind::Idle;
    idle.outcome = SchedulerDecisionOutcome::Idle;
    idle.reason_code = SchedulerReasonCode::NoReadyTicket;
    idle.suppression_key = Some("remote_acquire:worker:5:no_ready_ticket:0".to_owned());
    idle.ticket_id = None;
    idle.selected_worker_id = None;
    idle.selected_node_id = None;
    idle.selected_score = None;
    idle.candidate_count = 0;
    repo.create_or_suppress(idle.clone()).await.unwrap();
    repo.create_or_suppress(idle).await.unwrap();

    let rows = repo.list(SchedulerDecisionFilter::default()).await.unwrap();

    assert_eq!(rows.len(), 2);
    assert!(rows.iter().any(|row| row.id == selected.id));
    assert!(rows.iter().any(|row| row.suppressed_count == 1));
}

#[tokio::test]
async fn list_filters_by_request_worker_and_outcome() {
    let (repo, _tmp) = repo().await;

    repo.create(selected_input()).await.unwrap();

    let rows = repo
        .list(SchedulerDecisionFilter {
            worker_id: Some(WorkerId(5)),
            outcome: Some(SchedulerDecisionOutcome::Selected),
            limit: 10,
            ..SchedulerDecisionFilter::default()
        })
        .await
        .unwrap();

    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].request_worker_id, Some(WorkerId(5)));
}

#[tokio::test]
async fn selected_decisions_cannot_be_suppressed() {
    let (repo, _tmp) = repo().await;
    let mut input = selected_input();
    input.suppression_key = Some("bad:selected".to_owned());

    let err = repo.create_or_suppress(input).await.unwrap_err();

    assert_eq!(err.error_code(), voom_core::ErrorCode::ConfigInvalid);
}

#[tokio::test]
async fn selected_lease_id_must_be_linked_after_create() {
    let (repo, _tmp) = repo().await;
    let mut input = selected_input();
    input.selected_lease_id = Some(LeaseId(21));

    let err = repo.create(input).await.unwrap_err();

    assert_eq!(err.error_code(), voom_core::ErrorCode::ConfigInvalid);
}

#[tokio::test]
async fn impossible_decision_shapes_are_rejected() {
    let (repo, _tmp) = repo().await;
    let mut idle_selected = selected_input();
    idle_selected.decision_kind = SchedulerDecisionKind::Idle;
    let err = repo.create(idle_selected).await.unwrap_err();
    assert_eq!(err.error_code(), voom_core::ErrorCode::ConfigInvalid);

    let mut selected_without_tuple = selected_input();
    selected_without_tuple.ticket_id = None;
    let err = repo.create(selected_without_tuple).await.unwrap_err();
    assert_eq!(err.error_code(), voom_core::ErrorCode::ConfigInvalid);

    let mut idle_with_ticket = selected_input();
    idle_with_ticket.decision_kind = SchedulerDecisionKind::Idle;
    idle_with_ticket.outcome = SchedulerDecisionOutcome::Idle;
    idle_with_ticket.reason_code = SchedulerReasonCode::NoReadyTicket;
    idle_with_ticket.selected_worker_id = None;
    idle_with_ticket.selected_node_id = None;
    idle_with_ticket.selected_score = None;
    let err = repo.create(idle_with_ticket).await.unwrap_err();
    assert_eq!(err.error_code(), voom_core::ErrorCode::ConfigInvalid);

    let mut idle_with_candidates = selected_input();
    idle_with_candidates.decision_kind = SchedulerDecisionKind::Idle;
    idle_with_candidates.outcome = SchedulerDecisionOutcome::Idle;
    idle_with_candidates.reason_code = SchedulerReasonCode::NoReadyTicket;
    idle_with_candidates.ticket_id = None;
    idle_with_candidates.selected_worker_id = None;
    idle_with_candidates.selected_node_id = None;
    idle_with_candidates.selected_score = None;
    let err = repo.create(idle_with_candidates).await.unwrap_err();
    assert_eq!(err.error_code(), voom_core::ErrorCode::ConfigInvalid);

    let mut no_candidate_selected_reason = selected_input();
    no_candidate_selected_reason.decision_kind = SchedulerDecisionKind::NoCandidate;
    no_candidate_selected_reason.outcome = SchedulerDecisionOutcome::NoEligibleCandidate;
    no_candidate_selected_reason.ticket_id = None;
    no_candidate_selected_reason.selected_worker_id = None;
    no_candidate_selected_reason.selected_node_id = None;
    no_candidate_selected_reason.selected_score = None;
    let err = repo.create(no_candidate_selected_reason).await.unwrap_err();
    assert_eq!(err.error_code(), voom_core::ErrorCode::ConfigInvalid);
}

#[tokio::test]
async fn remote_acquire_decisions_require_request_context() {
    let (repo, _tmp) = repo().await;
    let mut input = selected_input();
    input.request_node_id = None;
    let err = repo.create(input).await.unwrap_err();
    assert_eq!(err.error_code(), voom_core::ErrorCode::ConfigInvalid);

    let mut input = selected_input();
    input.selected_worker_id = Some(WorkerId(6));
    let err = repo.create(input).await.unwrap_err();
    assert_eq!(err.error_code(), voom_core::ErrorCode::ConfigInvalid);
}

#[tokio::test]
async fn rejected_decisions_are_persistable_without_selected_tuple() {
    let (repo, _tmp) = repo().await;
    let mut input = selected_input();
    input.outcome = SchedulerDecisionOutcome::Rejected;
    input.reason_code = SchedulerReasonCode::WorkerCapacityFull;
    input.selected_worker_id = None;
    input.selected_node_id = None;
    input.selected_score = None;

    let row = repo.create(input).await.unwrap();

    assert_eq!(row.decision_kind, SchedulerDecisionKind::LeaseAcquire);
    assert_eq!(row.outcome, SchedulerDecisionOutcome::Rejected);
    assert_eq!(row.reason_code, SchedulerReasonCode::WorkerCapacityFull);
}

#[tokio::test]
async fn suppression_key_reuse_requires_equivalent_decision() {
    let (repo, _tmp) = repo().await;
    let mut input = selected_input();
    input.decision_kind = SchedulerDecisionKind::Idle;
    input.ticket_id = None;
    input.selected_worker_id = None;
    input.selected_node_id = None;
    input.selected_score = None;
    input.outcome = SchedulerDecisionOutcome::Idle;
    input.reason_code = SchedulerReasonCode::NoReadyTicket;
    input.candidate_count = 0;
    input.suppression_key = Some("remote_acquire:worker:5:no_ready_ticket:0".to_owned());
    repo.create_or_suppress(input.clone()).await.unwrap();

    input.request_node_id = Some(NodeId(4));
    let err = repo.create_or_suppress(input).await.unwrap_err();

    assert_eq!(err.error_code(), voom_core::ErrorCode::Conflict);
    assert!(
        err.to_string()
            .contains("already belongs to a different decision")
    );
}

#[tokio::test]
async fn link_selected_lease_rejects_incoherent_rows() {
    let (repo, _tmp) = repo().await;
    let selected = repo.create(selected_input()).await.unwrap();
    let err = repo
        .link_selected_lease(selected.id, LeaseId(21), T0)
        .await
        .unwrap_err();
    assert_eq!(err.error_code(), voom_core::ErrorCode::Conflict);

    let mut idle = selected_input();
    idle.decision_kind = SchedulerDecisionKind::Idle;
    idle.ticket_id = None;
    idle.selected_worker_id = None;
    idle.selected_node_id = None;
    idle.selected_score = None;
    idle.outcome = SchedulerDecisionOutcome::Idle;
    idle.reason_code = SchedulerReasonCode::NoReadyTicket;
    idle.candidate_count = 0;
    let idle = repo.create(idle).await.unwrap();
    let err = repo
        .link_selected_lease(idle.id, LeaseId(11), T0)
        .await
        .unwrap_err();
    assert_eq!(err.error_code(), voom_core::ErrorCode::Conflict);
}

#[tokio::test]
async fn link_selected_lease_is_idempotent_but_not_replaceable() {
    let (repo, _tmp) = repo().await;
    let selected = repo.create(selected_input()).await.unwrap();

    let first = repo
        .link_selected_lease(selected.id, LeaseId(11), T0)
        .await
        .unwrap();
    let replay = repo
        .link_selected_lease(selected.id, LeaseId(11), T0)
        .await
        .unwrap();
    assert_eq!(first.selected_lease_id, Some(LeaseId(11)));
    assert_eq!(replay.selected_lease_id, Some(LeaseId(11)));

    let err = repo
        .link_selected_lease(selected.id, LeaseId(12), T0)
        .await
        .unwrap_err();
    assert_eq!(err.error_code(), voom_core::ErrorCode::Conflict);
}
