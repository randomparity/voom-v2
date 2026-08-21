use serde_json::json;
use time::OffsetDateTime;
use voom_core::owner_access_evidence::{OwnerAccessEvidence, RootEpoch};
use voom_core::{
    ArtifactAccessDeclaration, ArtifactAccessEntry, ArtifactAccessRight, LeaseId, NodeId,
    StorageRootId, TicketId, WorkerId,
};

use super::*;

struct Fixture {
    repo: SqliteArtifactAccessPlanRepo,
    pool: sqlx::SqlitePool,
    lease_id: LeaseId,
    ticket_id: TicketId,
    worker_id: WorkerId,
    node_id: NodeId,
    _tmp: voom_test_support::TempDatabase,
}

fn owner_evidence() -> OwnerAccessEvidence {
    let declaration = ArtifactAccessDeclaration::new(vec![ArtifactAccessEntry {
        target: voom_core::ArtifactAccessTarget::StorageRoot(voom_core::StorageRootAccess {
            storage_root_id: StorageRootId(7),
        }),
        rights: vec![ArtifactAccessRight::Read],
    }])
    .unwrap();
    OwnerAccessEvidence::new(
        declaration,
        vec![RootEpoch {
            storage_root_id: StorageRootId(7),
            root_epoch: 3,
        }],
    )
    .unwrap()
}

impl Fixture {
    fn selected_input(&self, now: OffsetDateTime) -> NewArtifactAccessPlan {
        NewArtifactAccessPlan {
            lease_id: self.lease_id,
            ticket_id: self.ticket_id,
            worker_id: self.worker_id,
            node_id: self.node_id,
            owner_node_id: Some(self.node_id),
            access_evidence: Some(owner_evidence()),
            evidence: json!({"selected_by":"remote_acquire"}),
            now,
        }
    }

    fn absent_input(&self, now: OffsetDateTime) -> NewArtifactAccessPlan {
        NewArtifactAccessPlan {
            owner_node_id: None,
            access_evidence: None,
            ..self.selected_input(now)
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
    let tmp = voom_test_support::TempDatabase::new().unwrap();
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
        pool,
        lease_id,
        ticket_id,
        worker_id,
        node_id,
        _tmp: tmp,
    }
}

#[tokio::test]
async fn selected_plan_round_trips_owner_local_evidence() {
    let fixture = fixture().await;
    let now = OffsetDateTime::UNIX_EPOCH;
    let expected_evidence = owner_evidence();

    let plan = fixture.seed_selected_plan(now).await;

    assert_eq!(plan.status, ArtifactAccessPlanStatus::Selected);
    assert_eq!(plan.owner_node_id, Some(fixture.node_id));
    assert_eq!(
        plan.access_evidence.clone(),
        Some(expected_evidence.clone())
    );

    // Every lookup path decodes the same typed evidence.
    let by_lease = fixture
        .repo
        .get_by_lease(fixture.lease_id)
        .await
        .unwrap()
        .expect("selected plan is queryable by lease");
    assert_eq!(by_lease.access_evidence, Some(expected_evidence));
}

#[tokio::test]
async fn selected_plan_without_declared_byte_work_persists_absent_pair() {
    let fixture = fixture().await;
    let now = OffsetDateTime::UNIX_EPOCH;

    let plan = fixture
        .repo
        .create_selected(fixture.absent_input(now))
        .await
        .unwrap();

    assert_eq!(plan.owner_node_id, None);
    assert_eq!(plan.access_evidence, None);
    let reloaded = fixture
        .repo
        .get_by_lease(fixture.lease_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(reloaded.owner_node_id, None);
    assert_eq!(reloaded.access_evidence, None);
}

#[tokio::test]
async fn half_present_owner_pair_conflicts() {
    let fixture = fixture().await;
    let now = OffsetDateTime::UNIX_EPOCH;

    let mut input = fixture.selected_input(now);
    input.access_evidence = None;
    let err = fixture.repo.create_selected(input).await.unwrap_err();
    assert!(
        err.to_string()
            .contains("present together or absent together"),
        "{err}"
    );
}

#[tokio::test]
async fn owner_mismatching_acquiring_node_conflicts() {
    let fixture = fixture().await;
    let now = OffsetDateTime::UNIX_EPOCH;

    let mut input = fixture.selected_input(now);
    let other_node = NodeId(input.node_id.0 + 1);
    input.owner_node_id = Some(other_node);
    let err = fixture.repo.create_selected(input).await.unwrap_err();
    assert!(
        err.to_string().contains("does not match acquiring node"),
        "{err}"
    );
}

#[tokio::test]
async fn second_selected_plan_for_same_lease_conflicts() {
    let fixture = fixture().await;
    let now = OffsetDateTime::UNIX_EPOCH;
    fixture.seed_selected_plan(now).await;

    let err = fixture
        .repo
        .create_selected(fixture.selected_input(now))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("already has a selected plan"));
}

#[tokio::test]
async fn plan_status_transition_records_reason_and_evidence() {
    let fixture = fixture().await;
    let now = OffsetDateTime::UNIX_EPOCH;
    let original = fixture.seed_selected_plan(now).await;

    let consumed = fixture
        .repo
        .mark_status(
            original.id,
            ArtifactAccessPlanStatus::Consumed,
            Some("worker validated artifact access".to_owned()),
            json!({"validated": true}),
            now,
        )
        .await
        .unwrap();

    assert_eq!(consumed.status, ArtifactAccessPlanStatus::Consumed);
    assert_eq!(
        consumed.reason.as_deref(),
        Some("worker validated artifact access")
    );
    // Terminal transition keeps the selection-time owner-local proof intact.
    assert!(consumed.access_evidence.is_some());
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
            json!({"first": true}),
            now,
        )
        .await
        .unwrap();

    let err = fixture
        .repo
        .mark_status(
            consumed.id,
            ArtifactAccessPlanStatus::Failed,
            Some("second terminal reason".to_owned()),
            json!({"second": true}),
            now,
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("already consumed"));

    let after = fixture
        .repo
        .get_by_lease(fixture.lease_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(after.status, ArtifactAccessPlanStatus::Consumed);
    assert_eq!(after.reason.as_deref(), Some("first terminal reason"));
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
            json!({"reset": true}),
            now,
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("cannot transition to selected"));
}

#[tokio::test]
async fn corrupted_access_evidence_is_a_database_error() {
    let fixture = fixture().await;
    let now = OffsetDateTime::UNIX_EPOCH;
    let plan = fixture.seed_selected_plan(now).await;
    let id = i64::try_from(plan.id).unwrap();

    for (sql, label) in [
        (
            "UPDATE artifact_access_plans SET access_evidence = 'not-json' WHERE id = ?",
            "non-JSON",
        ),
        (
            r#"UPDATE artifact_access_plans SET access_evidence =
               '{"declaration":[{"target":{"kind":"storage_root","storage_root_id":7},
               "rights":["read"]}],"root_epochs":[],"mount":"/mnt/evil"}' WHERE id = ?"#,
            "unknown field",
        ),
        (
            r#"UPDATE artifact_access_plans SET access_evidence =
               '{"declaration":[{"target":{"kind":"storage_root","storage_root_id":7},
               "rights":["read"]}],"root_epochs":[{"storage_root_id":8,"root_epoch":1}]}'
               WHERE id = ?"#,
            "epoch set mismatch",
        ),
        (
            r#"UPDATE artifact_access_plans SET access_evidence =
               '{"declaration":[{"target":{"kind":"storage_root","storage_root_id":7},
               "rights":["read"]}],"root_epochs":[{"storage_root_id":7,"root_epoch":-2}]}'
               WHERE id = ?"#,
            "negative epoch",
        ),
        (
            "UPDATE artifact_access_plans SET access_evidence = NULL, \
             owner_node_id = NULL WHERE id = ?",
            "half-present pair via raw write",
        ),
    ] {
        // The column CHECK rejects some malformed payloads at write time —
        // that guard firing is also correct behavior; only a write the schema
        // accepted must then fail typed decode.
        if sqlx::query(sql)
            .bind(id)
            .execute(&fixture.pool)
            .await
            .is_err()
        {
            continue;
        }

        if label == "half-present pair via raw write" {
            continue; // NULL/NULL is the legal absent shape, not corruption.
        }
        let err = fixture
            .repo
            .get_by_lease(fixture.lease_id)
            .await
            .unwrap_err();
        assert!(
            matches!(err, VoomError::Database { .. }),
            "{label}: expected database error, got {err:?}"
        );
        assert!(
            err.to_string().contains("access_evidence"),
            "{label}: {err}"
        );
    }
}

#[tokio::test]
async fn lease_lookup_in_tx_sees_uncommitted_plan_and_rollback_hides_it() {
    let fixture = fixture().await;
    let now = OffsetDateTime::UNIX_EPOCH;

    let mut tx = fixture.pool.begin().await.unwrap();
    let created = fixture
        .repo
        .create_selected_in_tx(&mut tx, fixture.selected_input(now))
        .await
        .unwrap();
    let seen = fixture
        .repo
        .get_by_lease_in_tx(&mut tx, fixture.lease_id)
        .await
        .unwrap();
    assert_eq!(seen.map(|seen| seen.id), Some(created.id));
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
