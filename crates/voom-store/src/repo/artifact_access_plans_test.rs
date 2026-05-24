use serde_json::json;
use time::OffsetDateTime;
use voom_core::{LeaseId, NodeId, TicketId, WorkerId};

use super::*;

struct Fixture {
    repo: SqliteArtifactAccessPlanRepo,
    lease_id: LeaseId,
    ticket_id: TicketId,
    worker_id: WorkerId,
    node_id: NodeId,
    _tmp: tempfile::NamedTempFile,
}

impl Fixture {
    fn selected_input(&self, now: OffsetDateTime) -> NewArtifactAccessPlan {
        NewArtifactAccessPlan {
            lease_id: self.lease_id,
            ticket_id: self.ticket_id,
            worker_id: self.worker_id,
            node_id: self.node_id,
            input_handles: vec!["handle:input:1".to_owned()],
            output_handles: vec!["handle:output:1".to_owned()],
            selected_access_mode: ArtifactAccessMode::SharedMount,
            evidence: json!({"selected_by":"remote_acquire"}),
            now,
        }
    }

    async fn seed_selected_plan(&self, now: OffsetDateTime) -> ArtifactAccessPlan {
        self.repo
            .create_selected(self.selected_input(now))
            .await
            .unwrap()
    }
}

async fn fixture() -> Fixture {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let pool = crate::test_support::fresh_initialized_pool_at(tmp.path())
        .await
        .unwrap();
    let repo = SqliteArtifactAccessPlanRepo::new(pool.clone());

    let node_id = NodeId(
        sqlx::query(
            "INSERT INTO nodes \
             (name, kind, status, registered_at, last_seen_at, heartbeat_ttl_seconds, \
              auth_token_hash, auth_token_hint, metadata) \
             VALUES ('node-1', 'synthetic', 'registered', '1970-01-01T00:00:00Z', \
                     '1970-01-01T00:00:00Z', 60, 'token-hash', 'hint', '{}')",
        )
        .execute(&pool)
        .await
        .unwrap()
        .last_insert_rowid()
        .try_into()
        .unwrap(),
    );
    let worker_id = WorkerId(
        sqlx::query(
            "INSERT INTO workers (name, kind, status, node_id, registered_at, last_seen_at) \
             VALUES ('worker-1', 'remote', 'registered', ?, \
                     '1970-01-01T00:00:00Z', '1970-01-01T00:00:00Z')",
        )
        .bind(i64::try_from(node_id.0).unwrap())
        .execute(&pool)
        .await
        .unwrap()
        .last_insert_rowid()
        .try_into()
        .unwrap(),
    );
    let job_id: i64 = sqlx::query(
        "INSERT INTO jobs (kind, state, priority, created_at, updated_at) \
         VALUES ('artifact-access-test', 'open', 0, '1970-01-01T00:00:00Z', \
                 '1970-01-01T00:00:00Z')",
    )
    .execute(&pool)
    .await
    .unwrap()
    .last_insert_rowid();
    let ticket_id = TicketId(
        sqlx::query(
            "INSERT INTO tickets \
             (job_id, kind, state, priority, payload, attempt, max_attempts, next_eligible_at, \
              created_at, state_changed_at) \
             VALUES (?, 'artifact-access-test', 'leased', 0, '{}', 1, 3, \
                     '1970-01-01T00:00:00Z', '1970-01-01T00:00:00Z', \
                     '1970-01-01T00:00:00Z')",
        )
        .bind(job_id)
        .execute(&pool)
        .await
        .unwrap()
        .last_insert_rowid()
        .try_into()
        .unwrap(),
    );
    let lease_id = LeaseId(
        sqlx::query(
            "INSERT INTO leases \
             (ticket_id, worker_id, state, acquired_at, expires_at, last_heartbeat_at, \
              ttl_seconds) \
             VALUES (?, ?, 'held', '1970-01-01T00:00:00Z', '1970-01-01T00:01:00Z', \
                     '1970-01-01T00:00:00Z', 60)",
        )
        .bind(i64::try_from(ticket_id.0).unwrap())
        .bind(i64::try_from(worker_id.0).unwrap())
        .execute(&pool)
        .await
        .unwrap()
        .last_insert_rowid()
        .try_into()
        .unwrap(),
    );

    Fixture {
        repo,
        lease_id,
        ticket_id,
        worker_id,
        node_id,
        _tmp: tmp,
    }
}

#[tokio::test]
async fn selected_plan_is_queryable_by_lease_ticket_worker_node_mode_and_status() {
    let fixture = fixture().await;
    let now = OffsetDateTime::UNIX_EPOCH;
    let plan = fixture
        .repo
        .create_selected(NewArtifactAccessPlan {
            lease_id: fixture.lease_id,
            ticket_id: fixture.ticket_id,
            worker_id: fixture.worker_id,
            node_id: fixture.node_id,
            input_handles: vec!["handle:input:1".to_owned()],
            output_handles: vec!["handle:output:1".to_owned()],
            selected_access_mode: ArtifactAccessMode::SharedMount,
            evidence: serde_json::json!({"selected_by":"remote_acquire"}),
            now,
        })
        .await
        .unwrap();

    assert_eq!(plan.status, ArtifactAccessPlanStatus::Selected);
    assert_eq!(
        fixture
            .repo
            .get_by_lease(fixture.lease_id)
            .await
            .unwrap()
            .unwrap()
            .id,
        plan.id
    );
    assert_eq!(
        fixture
            .repo
            .list_by_ticket(fixture.ticket_id)
            .await
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        fixture
            .repo
            .list_by_worker(fixture.worker_id)
            .await
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        fixture
            .repo
            .list_by_node(fixture.node_id)
            .await
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        fixture
            .repo
            .list_by_mode_and_status(
                ArtifactAccessMode::SharedMount,
                ArtifactAccessPlanStatus::Selected
            )
            .await
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn second_selected_plan_for_same_lease_conflicts() {
    let fixture = fixture().await;
    let now = OffsetDateTime::UNIX_EPOCH;
    let original = fixture.seed_selected_plan(now).await;

    let mut duplicate = fixture.selected_input(now);
    duplicate.evidence = json!({"selected_by":"duplicate_attempt"});
    let err = fixture.repo.create_selected(duplicate).await.unwrap_err();

    assert_eq!(err.code(), "CONFLICT");
    let after = fixture
        .repo
        .get_by_lease(fixture.lease_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(after.id, original.id);
    assert_eq!(after.evidence, original.evidence);
    assert_eq!(after.status, ArtifactAccessPlanStatus::Selected);
}

#[tokio::test]
async fn plan_status_transition_records_reason_and_evidence() {
    let fixture = fixture().await;
    let now = OffsetDateTime::UNIX_EPOCH;
    let plan = fixture.seed_selected_plan(now).await;

    let consumed = fixture
        .repo
        .mark_status(
            plan.id,
            ArtifactAccessPlanStatus::Consumed,
            Some("synthetic worker validated shared mount".to_owned()),
            serde_json::json!({"validated":true}),
            now,
        )
        .await
        .unwrap();

    assert_eq!(consumed.status, ArtifactAccessPlanStatus::Consumed);
    assert_eq!(
        consumed.reason.as_deref(),
        Some("synthetic worker validated shared mount")
    );
    assert_eq!(consumed.evidence["validated"], true);
}

#[tokio::test]
async fn lease_lookup_in_tx_sees_uncommitted_plan_and_rollback_hides_it() {
    let fixture = fixture().await;
    let now = OffsetDateTime::UNIX_EPOCH;
    let mut tx = fixture.repo.pool.begin().await.unwrap();

    let plan = fixture
        .repo
        .create_selected_in_tx(&mut tx, fixture.selected_input(now))
        .await
        .unwrap();

    let in_tx = fixture
        .repo
        .get_by_lease_in_tx(&mut tx, fixture.lease_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(in_tx.id, plan.id);
    assert!(
        fixture
            .repo
            .get_by_lease(fixture.lease_id)
            .await
            .unwrap()
            .is_none()
    );

    tx.rollback().await.unwrap();

    assert!(
        fixture
            .repo
            .get_by_lease(fixture.lease_id)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn second_terminal_transition_conflicts_without_overwriting_original() {
    let fixture = fixture().await;
    let now = OffsetDateTime::UNIX_EPOCH;
    let plan = fixture.seed_selected_plan(now).await;
    let consumed = fixture
        .repo
        .mark_status(
            plan.id,
            ArtifactAccessPlanStatus::Consumed,
            Some("first terminal reason".to_owned()),
            json!({"first":true}),
            now,
        )
        .await
        .unwrap();

    let err = fixture
        .repo
        .mark_status(
            plan.id,
            ArtifactAccessPlanStatus::Failed,
            Some("second terminal reason".to_owned()),
            json!({"second":true}),
            now,
        )
        .await
        .unwrap_err();

    assert!(matches!(err, VoomError::Conflict(_)), "got {err:?}");
    let after = fixture
        .repo
        .get_by_lease(fixture.lease_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(after.status, ArtifactAccessPlanStatus::Consumed);
    assert_eq!(after.reason, consumed.reason);
    assert_eq!(after.evidence, consumed.evidence);
}

#[tokio::test]
async fn selected_status_transition_target_conflicts() {
    let fixture = fixture().await;
    let now = OffsetDateTime::UNIX_EPOCH;
    let plan = fixture.seed_selected_plan(now).await;

    let err = fixture
        .repo
        .mark_status(
            plan.id,
            ArtifactAccessPlanStatus::Selected,
            Some("no-op reset".to_owned()),
            json!({"reset":true}),
            now,
        )
        .await
        .unwrap_err();

    assert!(matches!(err, VoomError::Conflict(_)), "got {err:?}");
    let after = fixture
        .repo
        .get_by_lease(fixture.lease_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(after.status, ArtifactAccessPlanStatus::Selected);
    assert!(after.reason.is_none());
    assert_eq!(after.evidence, json!({"selected_by":"remote_acquire"}));
}

#[tokio::test]
async fn create_selected_rejects_ticket_worker_node_coherence_mismatches() {
    let fixture = fixture().await;
    let now = OffsetDateTime::UNIX_EPOCH;

    let mut wrong_ticket = fixture.selected_input(now);
    wrong_ticket.ticket_id = TicketId(fixture.ticket_id.0 + 10_000);
    let err = fixture
        .repo
        .create_selected(wrong_ticket)
        .await
        .unwrap_err();
    assert!(matches!(err, VoomError::Conflict(_)), "got {err:?}");

    let mut wrong_worker = fixture.selected_input(now);
    wrong_worker.worker_id = WorkerId(fixture.worker_id.0 + 10_000);
    let err = fixture
        .repo
        .create_selected(wrong_worker)
        .await
        .unwrap_err();
    assert!(matches!(err, VoomError::Conflict(_)), "got {err:?}");

    let mut wrong_node = fixture.selected_input(now);
    wrong_node.node_id = NodeId(fixture.node_id.0 + 10_000);
    let err = fixture.repo.create_selected(wrong_node).await.unwrap_err();
    assert!(matches!(err, VoomError::Conflict(_)), "got {err:?}");
}
