use super::*;

use serde_json::json;
use time::OffsetDateTime;
use voom_core::{FileAssetId, FileLocationId, FileVersionId, LeaseId, TicketId};

use crate::repo::execution::workers::{NewWorker, SqliteWorkerRepo, WorkerKind};
use crate::repo::media::identity::{
    FileAssetRepo, FileLocationKind, FileLocationRepo, FileVersionRepo, NewFileLocation,
    NewFileVersion, ProducedBy, SqliteIdentityRepo,
};

use crate::test_support::fresh_initialized_pool_at;

async fn pool() -> (sqlx::SqlitePool, voom_test_support::TempDatabase) {
    let tmp = voom_test_support::TempDatabase::new().unwrap();
    let p = fresh_initialized_pool_at(tmp.path()).await.unwrap();
    (p, tmp)
}

fn sample_new_handle() -> NewArtifactHandle {
    NewArtifactHandle {
        size_bytes: Some(1024),
        checksum: Some("abc".to_owned()),
        privacy_class: "internal".to_owned(),
        durability_class: "durable".to_owned(),
        allowed_access_modes: vec!["read".to_owned(), "write".to_owned()],
        mutability: "immutable".to_owned(),
        source_lineage: Some(json!({"src": "test"})),
        file_version_id: None,
        created_at: OffsetDateTime::UNIX_EPOCH,
    }
}

async fn source_version_and_location(pool: &sqlx::SqlitePool) -> (FileVersionId, FileLocationId) {
    let identity = SqliteIdentityRepo::new(pool.clone());
    let asset = identity
        .create_file_asset(OffsetDateTime::UNIX_EPOCH)
        .await
        .unwrap();
    let source = identity
        .create_file_version(NewFileVersion {
            file_asset_id: asset.id,
            content_hash: "source-hash".to_owned(),
            size_bytes: 1024,
            produced_by: ProducedBy::Ingest,
            produced_from_version_id: None,
            created_at: OffsetDateTime::UNIX_EPOCH,
        })
        .await
        .unwrap();
    let mut tx = pool.begin().await.unwrap();
    let source_location = identity
        .create_file_location_in_tx(
            &mut tx,
            NewFileLocation {
                file_version_id: source.id,
                kind: FileLocationKind::LocalPath,
                value: "/media/source.mkv".to_owned(),
                proof: None,
                observed_at: OffsetDateTime::UNIX_EPOCH,
            },
        )
        .await
        .unwrap();
    tx.commit().await.unwrap();
    (source.id, source_location.id)
}

async fn record_media_snapshot(pool: &sqlx::SqlitePool, version_id: FileVersionId) {
    sqlx::query(
        "INSERT INTO media_snapshots (file_version_id, probed_at, payload) \
         VALUES (?, '1970-01-01T00:00:00Z', '{}')",
    )
    .bind(i64::try_from(version_id.0).unwrap())
    .execute(pool)
    .await
    .unwrap();
}

async fn verification_worker(pool: &sqlx::SqlitePool) -> voom_core::WorkerId {
    let workers = SqliteWorkerRepo::new(pool.clone());
    workers
        .register(NewWorker {
            name: "verifier".to_owned(),
            kind: WorkerKind::Synthetic,
            registered_at: OffsetDateTime::UNIX_EPOCH,
            node_id: None,
        })
        .await
        .unwrap()
        .id
}

async fn create_staged_handle(
    repo: &SqliteArtifactRepo,
    source_version_id: FileVersionId,
) -> ArtifactHandle {
    let mut input = sample_new_handle();
    input.file_version_id = Some(source_version_id);
    repo.create_handle(input).await.unwrap()
}

async fn pending_record_fixture(
    pool: &sqlx::SqlitePool,
    repo: &SqliteArtifactRepo,
) -> ArtifactCommitRecord {
    let worker_id = verification_worker(pool).await;
    let (source_version_id, _) = source_version_and_location(pool).await;
    let handle = create_staged_handle(repo, source_version_id).await;
    let location = repo
        .record_location(NewArtifactLocation {
            artifact_handle_id: handle.id,
            kind: "staging".to_owned(),
            value: "/staging/report.mkv".to_owned(),
            observed_at: OffsetDateTime::UNIX_EPOCH,
        })
        .await
        .unwrap();
    let mut tx = pool.begin().await.unwrap();
    let verification = repo
        .record_verification_in_tx(
            &mut tx,
            successful_verification(
                handle.id,
                location.id,
                worker_id,
                &location.value,
                "report",
                1,
            ),
        )
        .await
        .unwrap();
    let pending = repo
        .create_pending_commit_in_tx(
            &mut tx,
            pending_commit(
                handle.id,
                source_version_id,
                verification.id,
                "/media/report.mkv",
            ),
        )
        .await
        .unwrap();
    tx.commit().await.unwrap();
    pending
}

async fn source_asset_id(
    identity: &SqliteIdentityRepo,
    source_version_id: FileVersionId,
) -> FileAssetId {
    identity
        .get_file_version(source_version_id)
        .await
        .unwrap()
        .unwrap()
        .file_asset_id
}

async fn dependency_produced_version_and_location(
    pool: &sqlx::SqlitePool,
    identity: &SqliteIdentityRepo,
    source_version_id: FileVersionId,
) -> (FileVersionId, FileLocationId) {
    let asset_id = source_asset_id(identity, source_version_id).await;
    let version = identity
        .create_file_version(NewFileVersion {
            file_asset_id: asset_id,
            content_hash: "produced-hash".to_owned(),
            size_bytes: 2048,
            produced_by: ProducedBy::Transcode,
            produced_from_version_id: Some(source_version_id),
            created_at: OffsetDateTime::UNIX_EPOCH,
        })
        .await
        .unwrap();
    let mut tx = pool.begin().await.unwrap();
    let location = identity
        .create_file_location_in_tx(
            &mut tx,
            NewFileLocation {
                file_version_id: version.id,
                kind: FileLocationKind::LocalPath,
                value: "/media/produced.mkv".to_owned(),
                proof: None,
                observed_at: OffsetDateTime::UNIX_EPOCH,
            },
        )
        .await
        .unwrap();
    tx.commit().await.unwrap();
    (version.id, location.id)
}

#[tokio::test]
async fn artifact_handles_carries_identity_link_columns() {
    let (pool, _tmp) = pool().await;
    let cols: Vec<(String,)> =
        sqlx::query_as("SELECT name FROM pragma_table_info('artifact_handles') ORDER BY cid")
            .fetch_all(&pool)
            .await
            .unwrap();
    let names: Vec<&str> = cols.iter().map(|c| c.0.as_str()).collect();
    for required in [
        "media_work_id",
        "media_variant_id",
        "asset_bundle_id",
        "file_asset_id",
        "file_version_id",
    ] {
        assert!(
            names.contains(&required),
            "M2 artifact_handles must carry the {required} identity-link column with an FK"
        );
    }
}

#[tokio::test]
async fn policy_target_resolution_creates_then_reuses_active_artifact() {
    let (pool, _tmp) = pool().await;
    let repo = SqliteArtifactRepo::new(pool.clone());
    let (version_id, location_id) = source_version_and_location(&pool).await;
    record_media_snapshot(&pool, version_id).await;

    let mut first_tx = pool.begin().await.unwrap();
    let first = repo
        .resolve_policy_artifact_target_in_tx(
            &mut first_tx,
            version_id,
            Some(location_id),
            OffsetDateTime::UNIX_EPOCH,
        )
        .await
        .unwrap();
    first_tx.commit().await.unwrap();

    let mut second_tx = pool.begin().await.unwrap();
    let second = repo
        .resolve_policy_artifact_target_in_tx(
            &mut second_tx,
            version_id,
            Some(location_id),
            OffsetDateTime::UNIX_EPOCH,
        )
        .await
        .unwrap();
    second_tx.commit().await.unwrap();

    assert!(first.created_handle.is_some());
    assert!(first.created_location.is_some());
    assert!(second.created_handle.is_none());
    assert!(second.created_location.is_none());
    assert_eq!(
        first.target.artifact_handle_id,
        second.target.artifact_handle_id
    );
    assert_eq!(
        first.target.artifact_location_id,
        second.target.artifact_location_id
    );
    assert_eq!(second.target.file_version_id, version_id);
    assert_eq!(second.target.file_location_id, location_id);
    assert_eq!(second.target.path, "/media/source.mkv");
    assert_eq!(second.target.size_bytes, 1024);
    assert_eq!(second.target.checksum, "source-hash");
}

#[tokio::test]
async fn policy_target_resolution_reuses_dependency_committed_handle() {
    let (pool, _tmp) = pool().await;
    let repo = SqliteArtifactRepo::new(pool.clone());
    let identity = SqliteIdentityRepo::new(pool.clone());
    let (source_version_id, _) = source_version_and_location(&pool).await;
    let (version_id, location_id) =
        dependency_produced_version_and_location(&pool, &identity, source_version_id).await;
    record_media_snapshot(&pool, version_id).await;
    let mut handle_input = sample_new_handle();
    handle_input.file_version_id = Some(source_version_id);
    handle_input.size_bytes = Some(2048);
    handle_input.checksum = Some("produced-hash".to_owned());
    let handle = repo.create_handle(handle_input).await.unwrap();
    let artifact_location = repo
        .record_location(NewArtifactLocation {
            artifact_handle_id: handle.id,
            kind: "staging".to_owned(),
            value: "/tmp/source.mkv".to_owned(),
            observed_at: OffsetDateTime::UNIX_EPOCH,
        })
        .await
        .unwrap();
    let worker_id = verification_worker(&pool).await;
    let mut verification_tx = pool.begin().await.unwrap();
    let verification = repo
        .record_verification_in_tx(
            &mut verification_tx,
            NewArtifactVerification {
                artifact_handle_id: handle.id,
                artifact_location_id: artifact_location.id,
                path: artifact_location.value,
                worker_id,
                workflow_ticket_id: None,
                workflow_lease_id: None,
                status: ArtifactVerificationStatus::Succeeded,
                expected_size_bytes: 2048,
                expected_checksum: "produced-hash".to_owned(),
                observed_size_bytes: Some(2048),
                observed_checksum: Some("produced-hash".to_owned()),
                failure_class: None,
                error_code: None,
                message: None,
                report: json!({}),
                started_at: OffsetDateTime::UNIX_EPOCH,
                finished_at: OffsetDateTime::UNIX_EPOCH,
            },
        )
        .await
        .unwrap();
    verification_tx.commit().await.unwrap();
    sqlx::query(
        "INSERT INTO artifact_commit_records \
         (artifact_handle_id, source_file_version_id, verification_id, target_path, \
          result_file_version_id, result_file_location_id, state, report, started_at, \
          promotion_started_at, finished_at) \
         VALUES (?, ?, ?, '/media/source.mkv', ?, ?, 'committed', '{}', \
                 '1970-01-01T00:00:00Z', '1970-01-01T00:00:00Z', '1970-01-01T00:00:00Z')",
    )
    .bind(i64::try_from(handle.id.0).unwrap())
    .bind(i64::try_from(source_version_id.0).unwrap())
    .bind(i64::try_from(verification.id.0).unwrap())
    .bind(i64::try_from(version_id.0).unwrap())
    .bind(i64::try_from(location_id.0).unwrap())
    .execute(&pool)
    .await
    .unwrap();

    let mut tx = pool.begin().await.unwrap();
    let resolved = repo
        .resolve_policy_artifact_target_in_tx(
            &mut tx,
            version_id,
            Some(location_id),
            OffsetDateTime::UNIX_EPOCH,
        )
        .await
        .unwrap();
    tx.commit().await.unwrap();

    assert_eq!(resolved.target.artifact_handle_id, handle.id);
    assert_eq!(resolved.target.file_version_id, version_id);
    assert_eq!(resolved.target.file_location_id, location_id);
    assert_eq!(resolved.target.size_bytes, 2048);
    assert_eq!(resolved.target.checksum, "produced-hash");
    assert!(resolved.created_handle.is_none());
    assert!(resolved.created_location.is_some());
}

#[tokio::test]
async fn policy_target_resolution_rejects_superseded_version() {
    let (pool, _tmp) = pool().await;
    let repo = SqliteArtifactRepo::new(pool.clone());
    let identity = SqliteIdentityRepo::new(pool.clone());
    let (version_id, location_id) = source_version_and_location(&pool).await;
    let asset_id = source_asset_id(&identity, version_id).await;
    identity
        .create_file_version(NewFileVersion {
            file_asset_id: asset_id,
            content_hash: "new-hash".to_owned(),
            size_bytes: 2048,
            produced_by: ProducedBy::Remux,
            produced_from_version_id: Some(version_id),
            created_at: OffsetDateTime::UNIX_EPOCH,
        })
        .await
        .unwrap();

    let mut tx = pool.begin().await.unwrap();
    let error = repo
        .resolve_policy_artifact_target_in_tx(
            &mut tx,
            version_id,
            Some(location_id),
            OffsetDateTime::UNIX_EPOCH,
        )
        .await
        .unwrap_err();

    assert!(matches!(error, VoomError::Conflict(_)));
    assert!(error.to_string().contains("superseded"));
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM artifact_handles")
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    assert_eq!(count, 0);
}

#[tokio::test]
async fn policy_target_resolution_rejects_retired_or_mismatched_location() {
    let (pool, _tmp) = pool().await;
    let repo = SqliteArtifactRepo::new(pool.clone());
    let (version_id, location_id) = source_version_and_location(&pool).await;
    let (_, other_location_id) = source_version_and_location(&pool).await;
    record_media_snapshot(&pool, version_id).await;

    let mut mismatched_tx = pool.begin().await.unwrap();
    let mismatched = repo
        .resolve_policy_artifact_target_in_tx(
            &mut mismatched_tx,
            version_id,
            Some(other_location_id),
            OffsetDateTime::UNIX_EPOCH,
        )
        .await
        .unwrap_err();
    mismatched_tx.rollback().await.unwrap();
    assert!(matches!(mismatched, VoomError::Conflict(_)));

    sqlx::query("UPDATE file_locations SET retired_at = '1970-01-01T00:00:01Z' WHERE id = ?")
        .bind(i64::try_from(location_id.0).unwrap())
        .execute(&pool)
        .await
        .unwrap();
    let mut retired_tx = pool.begin().await.unwrap();
    let retired = repo
        .resolve_policy_artifact_target_in_tx(
            &mut retired_tx,
            version_id,
            Some(location_id),
            OffsetDateTime::UNIX_EPOCH,
        )
        .await
        .unwrap_err();
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM artifact_handles")
        .fetch_one(&mut *retired_tx)
        .await
        .unwrap();

    assert!(matches!(retired, VoomError::Config(_)));
    assert_eq!(count, 0);
}

#[tokio::test]
async fn policy_target_resolution_rejects_ambiguous_unpinned_local_path() {
    let (pool, _tmp) = pool().await;
    let repo = SqliteArtifactRepo::new(pool.clone());
    let identity = SqliteIdentityRepo::new(pool.clone());
    let (version_id, _) = source_version_and_location(&pool).await;
    record_media_snapshot(&pool, version_id).await;
    let mut location_tx = pool.begin().await.unwrap();
    identity
        .create_file_location_in_tx(
            &mut location_tx,
            NewFileLocation {
                file_version_id: version_id,
                kind: FileLocationKind::LocalPath,
                value: "/media/duplicate-source.mkv".to_owned(),
                proof: None,
                observed_at: OffsetDateTime::UNIX_EPOCH,
            },
        )
        .await
        .unwrap();
    location_tx.commit().await.unwrap();

    let mut tx = pool.begin().await.unwrap();
    let error = repo
        .resolve_policy_artifact_target_in_tx(&mut tx, version_id, None, OffsetDateTime::UNIX_EPOCH)
        .await
        .unwrap_err();

    assert!(matches!(error, VoomError::Config(_)));
    assert!(error.to_string().contains("found 2"));
}

#[tokio::test]
async fn create_handle_returns_id() {
    let (pool, _tmp) = pool().await;
    let repo = SqliteArtifactRepo::new(pool.clone());
    let h = repo.create_handle(sample_new_handle()).await.unwrap();
    assert!(h.id.0 > 0);
}

#[tokio::test]
async fn list_handle_ids_pages_newest_first_by_exclusive_id() {
    let (pool, _tmp) = pool().await;
    let repo = SqliteArtifactRepo::new(pool);
    let first = repo.create_handle(sample_new_handle()).await.unwrap();
    let second = repo.create_handle(sample_new_handle()).await.unwrap();
    let third = repo.create_handle(sample_new_handle()).await.unwrap();

    assert_eq!(
        repo.list_handle_ids(None, Some(2)).await.unwrap(),
        vec![third.id, second.id]
    );
    assert_eq!(
        repo.list_handle_ids(Some(second.id.0), None).await.unwrap(),
        vec![first.id]
    );
}

#[tokio::test]
async fn handle_facts_preserves_optional_inspection_values() {
    let (pool, _tmp) = pool().await;
    let repo = SqliteArtifactRepo::new(pool);
    let mut input = sample_new_handle();
    input.size_bytes = None;
    input.checksum = None;
    let handle = repo.create_handle(input).await.unwrap();

    let facts = repo.handle_facts(handle.id).await.unwrap();

    assert_eq!(
        facts,
        ArtifactHandleFacts {
            handle,
            size_bytes: None,
            checksum: None,
        }
    );
}

#[tokio::test]
async fn handle_facts_rejects_negative_persisted_source_version_id() {
    let (pool, _tmp) = pool().await;
    let repo = SqliteArtifactRepo::new(pool.clone());
    let handle = repo.create_handle(sample_new_handle()).await.unwrap();
    let mut connection = pool.acquire().await.unwrap();
    sqlx::query("PRAGMA foreign_keys = OFF")
        .execute(&mut *connection)
        .await
        .unwrap();
    sqlx::query("UPDATE artifact_handles SET file_version_id = -1 WHERE id = ?")
        .bind(i64::try_from(handle.id.0).unwrap())
        .execute(&mut *connection)
        .await
        .unwrap();
    sqlx::query("PRAGMA foreign_keys = ON")
        .execute(&mut *connection)
        .await
        .unwrap();
    drop(connection);

    let error = repo.handle_facts(handle.id).await.unwrap_err();

    assert!(matches!(error, VoomError::Database { .. }));
    assert!(error.to_string().contains("file_version_id"));
}

#[tokio::test]
async fn require_expected_facts_returns_typed_values_inside_and_outside_transaction() {
    let (pool, _tmp) = pool().await;
    let repo = SqliteArtifactRepo::new(pool.clone());
    let (source_version_id, _) = source_version_and_location(&pool).await;
    let handle = create_staged_handle(&repo, source_version_id).await;
    let expected = ArtifactExpectedFacts {
        source_file_version_id: Some(source_version_id),
        size_bytes: 1024,
        checksum: "abc".to_owned(),
    };

    assert_eq!(
        repo.require_expected_facts(handle.id).await.unwrap(),
        expected
    );
    let mut tx = pool.begin().await.unwrap();
    assert_eq!(
        repo.require_expected_facts_in_tx(&mut tx, handle.id)
            .await
            .unwrap(),
        expected
    );
    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn require_expected_facts_rejects_missing_size_and_checksum_with_context() {
    let (pool, _tmp) = pool().await;
    let repo = SqliteArtifactRepo::new(pool);
    for (size_bytes, checksum, missing_field) in [
        (None, Some("abc".to_owned()), "size_bytes"),
        (Some(1024), None, "checksum"),
        (None, None, "size_bytes"),
    ] {
        let mut input = sample_new_handle();
        input.size_bytes = size_bytes;
        input.checksum = checksum;
        let handle = repo.create_handle(input).await.unwrap();

        let error = repo.require_expected_facts(handle.id).await.unwrap_err();

        assert!(matches!(error, VoomError::Config(_)));
        assert!(error.to_string().contains(&handle.id.to_string()));
        assert!(error.to_string().contains(missing_field));
    }
}

#[tokio::test]
async fn require_expected_facts_rejects_negative_persisted_size_as_database_error() {
    let (pool, _tmp) = pool().await;
    let repo = SqliteArtifactRepo::new(pool.clone());
    let handle = repo.create_handle(sample_new_handle()).await.unwrap();
    sqlx::query("UPDATE artifact_handles SET size_bytes = -1 WHERE id = ?")
        .bind(i64::try_from(handle.id.0).unwrap())
        .execute(&pool)
        .await
        .unwrap();

    let error = repo.require_expected_facts(handle.id).await.unwrap_err();

    assert!(matches!(error, VoomError::Database { .. }));
    assert!(error.to_string().contains("size_bytes"));
}

#[tokio::test]
async fn require_expected_facts_rejects_negative_size_before_missing_checksum() {
    let (pool, _tmp) = pool().await;
    let repo = SqliteArtifactRepo::new(pool.clone());
    let handle = repo.create_handle(sample_new_handle()).await.unwrap();
    sqlx::query("UPDATE artifact_handles SET size_bytes = -1, checksum = NULL WHERE id = ?")
        .bind(i64::try_from(handle.id.0).unwrap())
        .execute(&pool)
        .await
        .unwrap();

    let error = repo.require_expected_facts(handle.id).await.unwrap_err();

    assert!(matches!(error, VoomError::Database { .. }));
    assert!(error.to_string().contains("size_bytes"));
}

#[tokio::test]
async fn artifact_projections_reject_ids_above_sqlite_integer_range() {
    let (pool, _tmp) = pool().await;
    let repo = SqliteArtifactRepo::new(pool.clone());
    let invalid_handle_id = ArtifactHandleId(u64::MAX);
    let invalid_location_id = ArtifactLocationId(u64::MAX);

    assert!(matches!(
        repo.handle_facts(invalid_handle_id).await.unwrap_err(),
        VoomError::Internal(_)
    ));
    assert!(matches!(
        repo.require_expected_facts(invalid_handle_id)
            .await
            .unwrap_err(),
        VoomError::Internal(_)
    ));
    let mut tx = pool.begin().await.unwrap();
    assert!(matches!(
        repo.live_location_of_kind_in_tx(&mut tx, invalid_handle_id, "staging")
            .await
            .unwrap_err(),
        VoomError::Internal(_)
    ));
    assert!(matches!(
        repo.require_live_location_in_tx(
            &mut tx,
            ArtifactHandleId(1),
            invalid_location_id,
            "staging",
            "/staging/invalid.mkv",
        )
        .await
        .unwrap_err(),
        VoomError::Internal(_)
    ));
    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn live_location_of_kind_selects_the_exact_single_live_location() {
    let (pool, _tmp) = pool().await;
    let repo = SqliteArtifactRepo::new(pool.clone());
    let handle = repo.create_handle(sample_new_handle()).await.unwrap();
    let location = repo
        .record_location(NewArtifactLocation {
            artifact_handle_id: handle.id,
            kind: "staging".to_owned(),
            value: "/staging/exact.mkv".to_owned(),
            observed_at: OffsetDateTime::UNIX_EPOCH,
        })
        .await
        .unwrap();
    let mut tx = pool.begin().await.unwrap();

    assert_eq!(
        repo.live_location_of_kind_in_tx(&mut tx, handle.id, "staging")
            .await
            .unwrap(),
        Some(LiveArtifactLocation {
            id: location.id,
            kind: "staging".to_owned(),
            value: location.value,
        })
    );
    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn live_location_of_kind_conflicts_when_multiple_locations_are_live() {
    let (pool, _tmp) = pool().await;
    let repo = SqliteArtifactRepo::new(pool.clone());
    let handle = repo.create_handle(sample_new_handle()).await.unwrap();
    for value in ["/staging/first.mkv", "/staging/second.mkv"] {
        repo.record_location(NewArtifactLocation {
            artifact_handle_id: handle.id,
            kind: "staging".to_owned(),
            value: value.to_owned(),
            observed_at: OffsetDateTime::UNIX_EPOCH,
        })
        .await
        .unwrap();
    }
    let mut tx = pool.begin().await.unwrap();

    let error = repo
        .live_location_of_kind_in_tx(&mut tx, handle.id, "staging")
        .await
        .unwrap_err();

    assert!(matches!(error, VoomError::Conflict(_)));
    assert!(error.to_string().contains("found 2"));
    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn require_live_location_rejects_retired_or_replaced_rows() {
    let (pool, _tmp) = pool().await;
    let repo = SqliteArtifactRepo::new(pool.clone());
    let handle = repo.create_handle(sample_new_handle()).await.unwrap();
    let old = repo
        .record_location(NewArtifactLocation {
            artifact_handle_id: handle.id,
            kind: "staging".to_owned(),
            value: "/staging/old.mkv".to_owned(),
            observed_at: OffsetDateTime::UNIX_EPOCH,
        })
        .await
        .unwrap();
    repo.retire_location(old.id, OffsetDateTime::UNIX_EPOCH)
        .await
        .unwrap();
    repo.record_location(NewArtifactLocation {
        artifact_handle_id: handle.id,
        kind: "staging".to_owned(),
        value: "/staging/replacement.mkv".to_owned(),
        observed_at: OffsetDateTime::UNIX_EPOCH,
    })
    .await
    .unwrap();
    let mut tx = pool.begin().await.unwrap();

    let error = repo
        .require_live_location_in_tx(&mut tx, handle.id, old.id, "staging", "/staging/old.mkv")
        .await
        .unwrap_err();

    assert!(matches!(error, VoomError::Config(_)));
    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn require_live_location_rejects_wrong_owner_kind_and_value() {
    let (pool, _tmp) = pool().await;
    let repo = SqliteArtifactRepo::new(pool.clone());
    let handle = repo.create_handle(sample_new_handle()).await.unwrap();
    let other = repo.create_handle(sample_new_handle()).await.unwrap();
    let location = repo
        .record_location(NewArtifactLocation {
            artifact_handle_id: handle.id,
            kind: "staging".to_owned(),
            value: "/staging/exact.mkv".to_owned(),
            observed_at: OffsetDateTime::UNIX_EPOCH,
        })
        .await
        .unwrap();
    let mut tx = pool.begin().await.unwrap();

    for (owner, kind, value) in [
        (other.id, "staging", "/staging/exact.mkv"),
        (handle.id, "local_path", "/staging/exact.mkv"),
        (handle.id, "staging", "/staging/wrong.mkv"),
    ] {
        let error = repo
            .require_live_location_in_tx(&mut tx, owner, location.id, kind, value)
            .await
            .unwrap_err();
        assert!(matches!(error, VoomError::Conflict(_)));
    }
    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn pending_commit_report_update_participates_in_caller_transaction() {
    let (pool, _tmp) = pool().await;
    let repo = SqliteArtifactRepo::new(pool.clone());
    let pending = pending_record_fixture(&pool, &repo).await;
    let mut tx = pool.begin().await.unwrap();
    repo.update_pending_commit_report_in_tx(&mut tx, pending.id, &json!({"phase": "changed"}))
        .await
        .unwrap();
    tx.rollback().await.unwrap();

    let reloaded = repo.get_commit_record(pending.id).await.unwrap().unwrap();
    assert_eq!(reloaded.report, pending.report);

    let mut tx = pool.begin().await.unwrap();
    repo.update_pending_commit_report_in_tx(&mut tx, pending.id, &json!({"phase": "saved"}))
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let reloaded = repo.get_commit_record(pending.id).await.unwrap().unwrap();
    assert_eq!(reloaded.report, json!({"phase": "saved"}));
}

#[tokio::test]
async fn create_staged_handle_links_to_source_file_version() {
    let (pool, _tmp) = pool().await;
    let (source_version_id, _source_location_id) = source_version_and_location(&pool).await;
    let repo = SqliteArtifactRepo::new(pool.clone());

    let mut input = sample_new_handle();
    input.file_version_id = Some(source_version_id);
    input.source_lineage = Some(json!({
        "kind": "staged_commit_source",
        "source_file_version_id": source_version_id.0,
    }));
    let handle = repo.create_handle(input).await.unwrap();

    assert_eq!(handle.file_version_id, Some(source_version_id));
    let got = repo.get_handle(handle.id).await.unwrap().unwrap();
    assert_eq!(got.file_version_id, Some(source_version_id));
}

#[tokio::test]
async fn record_verification_persists_success_and_failure_rows() {
    let (pool, _tmp) = pool().await;
    let repo = SqliteArtifactRepo::new(pool.clone());
    let worker_id = verification_worker(&pool).await;
    let handle = repo.create_handle(sample_new_handle()).await.unwrap();
    let location = repo
        .record_location(NewArtifactLocation {
            artifact_handle_id: handle.id,
            kind: "staging".to_owned(),
            value: "/staging/out.mkv".to_owned(),
            observed_at: OffsetDateTime::UNIX_EPOCH,
        })
        .await
        .unwrap();
    let mut tx = pool.begin().await.unwrap();

    let succeeded = repo
        .record_verification_in_tx(
            &mut tx,
            NewArtifactVerification {
                artifact_handle_id: handle.id,
                artifact_location_id: location.id,
                path: "/staging/out.mkv".to_owned(),
                worker_id,
                workflow_ticket_id: None,
                workflow_lease_id: None,
                status: ArtifactVerificationStatus::Succeeded,
                expected_size_bytes: 1024,
                expected_checksum: "abc".to_owned(),
                observed_size_bytes: Some(1024),
                observed_checksum: Some("abc".to_owned()),
                failure_class: None,
                error_code: None,
                message: None,
                report: json!({"hash": "matched"}),
                started_at: OffsetDateTime::UNIX_EPOCH,
                finished_at: OffsetDateTime::UNIX_EPOCH + time::Duration::seconds(1),
            },
        )
        .await
        .unwrap();
    let failed = repo
        .record_verification_in_tx(
            &mut tx,
            NewArtifactVerification {
                artifact_handle_id: handle.id,
                artifact_location_id: location.id,
                path: "/staging/out.mkv".to_owned(),
                worker_id,
                workflow_ticket_id: None,
                workflow_lease_id: None,
                status: ArtifactVerificationStatus::Failed,
                expected_size_bytes: 1024,
                expected_checksum: "abc".to_owned(),
                observed_size_bytes: None,
                observed_checksum: None,
                failure_class: Some("io".to_owned()),
                error_code: Some("READ_FAILED".to_owned()),
                message: Some("read failed".to_owned()),
                report: json!({"attempt": 2}),
                started_at: OffsetDateTime::UNIX_EPOCH + time::Duration::seconds(2),
                finished_at: OffsetDateTime::UNIX_EPOCH + time::Duration::seconds(3),
            },
        )
        .await
        .unwrap();
    tx.commit().await.unwrap();

    assert_eq!(succeeded.status, ArtifactVerificationStatus::Succeeded);
    assert_eq!(failed.status, ArtifactVerificationStatus::Failed);
    let rows = repo.list_verifications(handle.id).await.unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].report, json!({"hash": "matched"}));
    assert_eq!(rows[1].error_code.as_deref(), Some("READ_FAILED"));
}

#[tokio::test]
async fn latest_successful_verification_uses_live_staging_location() {
    let (pool, _tmp) = pool().await;
    let repo = SqliteArtifactRepo::new(pool.clone());
    let worker_id = verification_worker(&pool).await;
    let handle = repo.create_handle(sample_new_handle()).await.unwrap();
    let old_location = repo
        .record_location(NewArtifactLocation {
            artifact_handle_id: handle.id,
            kind: "staging".to_owned(),
            value: "/staging/old.mkv".to_owned(),
            observed_at: OffsetDateTime::UNIX_EPOCH,
        })
        .await
        .unwrap();
    repo.retire_location(
        old_location.id,
        OffsetDateTime::UNIX_EPOCH + time::Duration::seconds(10),
    )
    .await
    .unwrap();
    let live_location = repo
        .record_location(NewArtifactLocation {
            artifact_handle_id: handle.id,
            kind: "staging".to_owned(),
            value: "/staging/live.mkv".to_owned(),
            observed_at: OffsetDateTime::UNIX_EPOCH + time::Duration::seconds(11),
        })
        .await
        .unwrap();
    let mut tx = pool.begin().await.unwrap();
    let retired_success = repo
        .record_verification_in_tx(
            &mut tx,
            successful_verification(
                handle.id,
                old_location.id,
                worker_id,
                &old_location.value,
                "retired",
                20,
            ),
        )
        .await
        .unwrap();
    let live_success = repo
        .record_verification_in_tx(
            &mut tx,
            successful_verification(
                handle.id,
                live_location.id,
                worker_id,
                &live_location.value,
                "live-old",
                21,
            ),
        )
        .await
        .unwrap();
    let latest_live_success = repo
        .record_verification_in_tx(
            &mut tx,
            successful_verification(
                handle.id,
                live_location.id,
                worker_id,
                &live_location.value,
                "live-new",
                22,
            ),
        )
        .await
        .unwrap();
    let _ignored_failure = repo
        .record_verification_in_tx(
            &mut tx,
            failed_verification(
                handle.id,
                live_location.id,
                worker_id,
                &live_location.value,
                "VERIFY_FAILED",
                23,
            ),
        )
        .await
        .unwrap();

    let latest = repo
        .latest_successful_verification_for_live_staging_in_tx(&mut tx, handle.id)
        .await
        .unwrap()
        .unwrap();
    tx.commit().await.unwrap();

    assert!(retired_success.id.0 < live_success.id.0);
    assert_eq!(latest.id, latest_live_success.id);
    assert_eq!(latest.report, json!({"label": "live-new"}));
}

#[tokio::test]
async fn workflow_verification_is_unique_per_lease_but_allows_ticket_retry() {
    let (pool, _tmp) = pool().await;
    let repo = SqliteArtifactRepo::new(pool.clone());
    let worker_id = verification_worker(&pool).await;
    let handle = repo.create_handle(sample_new_handle()).await.unwrap();
    let location = repo
        .record_location(NewArtifactLocation {
            artifact_handle_id: handle.id,
            kind: "local_path".to_owned(),
            value: "/media/retry.mkv".to_owned(),
            observed_at: OffsetDateTime::UNIX_EPOCH,
        })
        .await
        .unwrap();
    seed_ticket_and_leases(&pool, worker_id).await;

    let mut first = successful_verification(
        handle.id,
        location.id,
        worker_id,
        &location.value,
        "first",
        1,
    );
    first.workflow_ticket_id = Some(TicketId(1));
    first.workflow_lease_id = Some(LeaseId(1));
    let mut half_owned = first.clone();
    half_owned.workflow_lease_id = None;
    let mut invalid_tx = pool.begin().await.unwrap();
    let invalid = repo
        .record_verification_in_tx(&mut invalid_tx, half_owned)
        .await
        .unwrap_err();
    assert!(matches!(invalid, VoomError::Config(_)));
    invalid_tx.rollback().await.unwrap();

    let mut tx = pool.begin().await.unwrap();
    let first_verification = repo
        .record_verification_in_tx(&mut tx, first.clone())
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let half_owned_update =
        sqlx::query("UPDATE artifact_verifications SET workflow_lease_id = NULL WHERE id = ?")
            .bind(i64::try_from(first_verification.id.0).unwrap())
            .execute(&pool)
            .await
            .unwrap_err();
    assert!(
        half_owned_update
            .to_string()
            .contains("CHECK constraint failed")
    );

    let mut duplicate_tx = pool.begin().await.unwrap();
    let duplicate = repo
        .record_verification_in_tx(&mut duplicate_tx, first)
        .await
        .unwrap_err();
    assert!(matches!(duplicate, voom_core::VoomError::Database { .. }));
    duplicate_tx.rollback().await.unwrap();

    let mut retry = successful_verification(
        handle.id,
        location.id,
        worker_id,
        &location.value,
        "retry",
        2,
    );
    retry.workflow_ticket_id = Some(TicketId(1));
    retry.workflow_lease_id = Some(LeaseId(2));
    let mut retry_tx = pool.begin().await.unwrap();
    repo.record_verification_in_tx(&mut retry_tx, retry)
        .await
        .unwrap();
    retry_tx.commit().await.unwrap();

    assert_eq!(repo.list_verifications(handle.id).await.unwrap().len(), 2);
}

async fn seed_ticket_and_leases(pool: &sqlx::SqlitePool, worker_id: WorkerId) {
    sqlx::query(
        "INSERT INTO tickets \
         (id, kind, state, priority, payload, attempt, max_attempts, \
          next_eligible_at, created_at, state_changed_at) \
         VALUES (1, 'verify', 'succeeded', 0, '{}', 1, 3, \
          '1970-01-01T00:00:00Z', '1970-01-01T00:00:00Z', '1970-01-01T00:00:00Z')",
    )
    .execute(pool)
    .await
    .unwrap();
    for id in [1_i64, 2] {
        sqlx::query(
            "INSERT INTO leases \
             (id, ticket_id, worker_id, state, acquired_at, expires_at, \
              last_heartbeat_at, ttl_seconds, release_reason, released_at) \
             VALUES (?, 1, ?, 'released', '1970-01-01T00:00:00Z', \
             '1970-01-01T00:00:01Z', '1970-01-01T00:00:00Z', 1, \
             'test', '1970-01-01T00:00:01Z')",
        )
        .bind(id)
        .bind(i64::try_from(worker_id.0).unwrap())
        .execute(pool)
        .await
        .unwrap();
    }
}

#[tokio::test]
async fn verification_location_must_belong_to_same_handle() {
    let (pool, _tmp) = pool().await;
    let repo = SqliteArtifactRepo::new(pool.clone());
    let worker_id = verification_worker(&pool).await;
    let handle = repo.create_handle(sample_new_handle()).await.unwrap();
    let other_handle = repo.create_handle(sample_new_handle()).await.unwrap();
    let other_location = repo
        .record_location(NewArtifactLocation {
            artifact_handle_id: other_handle.id,
            kind: "staging".to_owned(),
            value: "/staging/other.mkv".to_owned(),
            observed_at: OffsetDateTime::UNIX_EPOCH,
        })
        .await
        .unwrap();
    let mut tx = pool.begin().await.unwrap();

    let err = repo
        .record_verification_in_tx(
            &mut tx,
            successful_verification(
                handle.id,
                other_location.id,
                worker_id,
                &other_location.value,
                "mismatch",
                30,
            ),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, voom_core::VoomError::Conflict(_)));

    let latest = repo
        .latest_successful_verification_for_live_staging_in_tx(&mut tx, handle.id)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    assert!(
        latest.is_none(),
        "a verification cannot borrow another handle's live staging location"
    );
}

#[tokio::test]
async fn verification_path_must_match_location_value() {
    let (pool, _tmp) = pool().await;
    let repo = SqliteArtifactRepo::new(pool.clone());
    let worker_id = verification_worker(&pool).await;
    let handle = repo.create_handle(sample_new_handle()).await.unwrap();
    let location = repo
        .record_location(NewArtifactLocation {
            artifact_handle_id: handle.id,
            kind: "staging".to_owned(),
            value: "/staging/live.mkv".to_owned(),
            observed_at: OffsetDateTime::UNIX_EPOCH,
        })
        .await
        .unwrap();
    let mut tx = pool.begin().await.unwrap();

    let err = repo
        .record_verification_in_tx(
            &mut tx,
            successful_verification(
                handle.id,
                location.id,
                worker_id,
                "/staging/other.mkv",
                "wrong-path",
                31,
            ),
        )
        .await
        .unwrap_err();
    tx.commit().await.unwrap();

    assert!(matches!(err, voom_core::VoomError::Conflict(_)));
}

#[tokio::test]
async fn commit_records_move_through_terminal_states() {
    let (pool, _tmp) = pool().await;
    let repo = SqliteArtifactRepo::new(pool.clone());
    let identity = SqliteIdentityRepo::new(pool.clone());
    let worker_id = verification_worker(&pool).await;
    let (source_version_id, _source_location_id) = source_version_and_location(&pool).await;
    let source_asset_id = identity
        .get_file_version(source_version_id)
        .await
        .unwrap()
        .unwrap()
        .file_asset_id;
    let handle = create_staged_handle(&repo, source_version_id).await;
    let failed_handle = create_staged_handle(&repo, source_version_id).await;
    let recovery_handle = create_staged_handle(&repo, source_version_id).await;
    let staging_location = repo
        .record_location(NewArtifactLocation {
            artifact_handle_id: handle.id,
            kind: "staging".to_owned(),
            value: "/staging/out.mkv".to_owned(),
            observed_at: OffsetDateTime::UNIX_EPOCH,
        })
        .await
        .unwrap();
    let failed_staging_location = repo
        .record_location(NewArtifactLocation {
            artifact_handle_id: failed_handle.id,
            kind: "staging".to_owned(),
            value: "/staging/failed.mkv".to_owned(),
            observed_at: OffsetDateTime::UNIX_EPOCH,
        })
        .await
        .unwrap();
    let recovery_staging_location = repo
        .record_location(NewArtifactLocation {
            artifact_handle_id: recovery_handle.id,
            kind: "staging".to_owned(),
            value: "/staging/recovery.mkv".to_owned(),
            observed_at: OffsetDateTime::UNIX_EPOCH,
        })
        .await
        .unwrap();
    let mut tx = pool.begin().await.unwrap();
    let verification = repo
        .record_verification_in_tx(
            &mut tx,
            successful_verification(
                handle.id,
                staging_location.id,
                worker_id,
                &staging_location.value,
                "ok",
                1,
            ),
        )
        .await
        .unwrap();
    let failed_verification = repo
        .record_verification_in_tx(
            &mut tx,
            successful_verification(
                failed_handle.id,
                failed_staging_location.id,
                worker_id,
                &failed_staging_location.value,
                "failed-ok",
                2,
            ),
        )
        .await
        .unwrap();
    let recovery_verification = repo
        .record_verification_in_tx(
            &mut tx,
            successful_verification(
                recovery_handle.id,
                recovery_staging_location.id,
                worker_id,
                &recovery_staging_location.value,
                "recovery-ok",
                3,
            ),
        )
        .await
        .unwrap();
    let committed_version = identity
        .create_file_version_in_tx(
            &mut tx,
            NewFileVersion {
                file_asset_id: source_asset_id,
                content_hash: "committed-hash".to_owned(),
                size_bytes: 1024,
                produced_by: ProducedBy::StagedCommit,
                produced_from_version_id: Some(source_version_id),
                created_at: OffsetDateTime::UNIX_EPOCH + time::Duration::seconds(2),
            },
        )
        .await
        .unwrap();
    let committed_location = identity
        .create_file_location_in_tx(
            &mut tx,
            NewFileLocation {
                file_version_id: committed_version.id,
                kind: FileLocationKind::LocalPath,
                value: "/media/committed.mkv".to_owned(),
                proof: None,
                observed_at: OffsetDateTime::UNIX_EPOCH + time::Duration::seconds(3),
            },
        )
        .await
        .unwrap();

    let committed = repo
        .create_pending_commit_in_tx(
            &mut tx,
            pending_commit(
                handle.id,
                source_version_id,
                verification.id,
                "/media/committed.mkv",
            ),
        )
        .await
        .unwrap();
    let committed = repo
        .mark_commit_committed_in_tx(
            &mut tx,
            committed.id,
            committed_version.id,
            committed_location.id,
            OffsetDateTime::UNIX_EPOCH + time::Duration::seconds(4),
            OffsetDateTime::UNIX_EPOCH + time::Duration::seconds(5),
        )
        .await
        .unwrap();

    let failed = repo
        .create_pending_commit_in_tx(
            &mut tx,
            pending_commit(
                failed_handle.id,
                source_version_id,
                failed_verification.id,
                "/media/retry.mkv",
            ),
        )
        .await
        .unwrap();
    let failed = repo
        .mark_commit_failed_in_tx(
            &mut tx,
            failed.id,
            commit_failure("RENAME_FAILED", "rename failed", 6),
        )
        .await
        .unwrap();

    let recovery_required = repo
        .create_pending_commit_in_tx(
            &mut tx,
            pending_commit(
                recovery_handle.id,
                source_version_id,
                recovery_verification.id,
                "/media/recovery.mkv",
            ),
        )
        .await
        .unwrap();
    let recovery_required = repo
        .mark_commit_recovery_required_in_tx(
            &mut tx,
            recovery_required.id,
            commit_failure("PARTIAL_PROMOTION", "promotion uncertain", 7),
            "operator must inspect target".to_owned(),
        )
        .await
        .unwrap();
    tx.commit().await.unwrap();

    assert_eq!(committed.state, ArtifactCommitState::Committed);
    assert_eq!(committed.result_file_version_id, Some(committed_version.id));
    assert_eq!(
        committed.result_file_location_id,
        Some(committed_location.id)
    );
    assert_eq!(failed.state, ArtifactCommitState::Failed);
    assert_eq!(failed.error_code.as_deref(), Some("RENAME_FAILED"));
    assert_eq!(
        recovery_required.state,
        ArtifactCommitState::RecoveryRequired
    );
    assert_eq!(
        recovery_required.recovery_reason.as_deref(),
        Some("operator must inspect target")
    );

    // The safety gate consults this: a recovery-required commit for the source
    // version is present; an unrelated version has none.
    assert!(
        repo.has_recovery_required_for_source_version(source_version_id)
            .await
            .unwrap()
    );
    assert!(
        !repo
            .has_recovery_required_for_source_version(FileVersionId(999_999))
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn commit_pending_uniqueness_blocks_second_owner_but_failed_can_retry() {
    let (pool, _tmp) = pool().await;
    let repo = SqliteArtifactRepo::new(pool.clone());
    let worker_id = verification_worker(&pool).await;
    let (source_version_id, _source_location_id) = source_version_and_location(&pool).await;
    let handle = create_staged_handle(&repo, source_version_id).await;
    let other_handle = create_staged_handle(&repo, source_version_id).await;
    let location = repo
        .record_location(NewArtifactLocation {
            artifact_handle_id: handle.id,
            kind: "staging".to_owned(),
            value: "/staging/out.mkv".to_owned(),
            observed_at: OffsetDateTime::UNIX_EPOCH,
        })
        .await
        .unwrap();
    let other_location = repo
        .record_location(NewArtifactLocation {
            artifact_handle_id: other_handle.id,
            kind: "staging".to_owned(),
            value: "/staging/other.mkv".to_owned(),
            observed_at: OffsetDateTime::UNIX_EPOCH,
        })
        .await
        .unwrap();
    let mut tx = pool.begin().await.unwrap();
    let verification = repo
        .record_verification_in_tx(
            &mut tx,
            successful_verification(handle.id, location.id, worker_id, &location.value, "ok", 1),
        )
        .await
        .unwrap();
    let other_verification = repo
        .record_verification_in_tx(
            &mut tx,
            successful_verification(
                other_handle.id,
                other_location.id,
                worker_id,
                &other_location.value,
                "other",
                2,
            ),
        )
        .await
        .unwrap();

    let pending = repo
        .create_pending_commit_in_tx(
            &mut tx,
            pending_commit(
                handle.id,
                source_version_id,
                verification.id,
                "/media/out.mkv",
            ),
        )
        .await
        .unwrap();
    let same_artifact_err = repo
        .create_pending_commit_in_tx(
            &mut tx,
            pending_commit(
                handle.id,
                source_version_id,
                verification.id,
                "/media/out-2.mkv",
            ),
        )
        .await
        .unwrap_err();
    let same_target_err = repo
        .create_pending_commit_in_tx(
            &mut tx,
            pending_commit(
                other_handle.id,
                source_version_id,
                other_verification.id,
                "/media/out.mkv",
            ),
        )
        .await
        .unwrap_err();
    let failed = repo
        .mark_commit_failed_in_tx(
            &mut tx,
            pending.id,
            commit_failure("RENAME_FAILED", "rename failed", 3),
        )
        .await
        .unwrap();
    let retry = repo
        .create_pending_commit_in_tx(
            &mut tx,
            pending_commit(
                handle.id,
                source_version_id,
                verification.id,
                "/media/out.mkv",
            ),
        )
        .await
        .unwrap();
    tx.commit().await.unwrap();

    assert!(matches!(
        same_artifact_err,
        voom_core::VoomError::Conflict(_)
    ));
    assert!(matches!(same_target_err, voom_core::VoomError::Conflict(_)));
    assert_eq!(failed.state, ArtifactCommitState::Failed);
    assert_eq!(retry.state, ArtifactCommitState::Pending);
    let records = repo.list_commit_records(handle.id).await.unwrap();
    assert_eq!(records.len(), 2);
}

#[tokio::test]
async fn pending_commit_requires_successful_live_staging_verification_for_same_handle() {
    let (pool, _tmp) = pool().await;
    let repo = SqliteArtifactRepo::new(pool.clone());
    let worker_id = verification_worker(&pool).await;
    let (source_version_id, _source_location_id) = source_version_and_location(&pool).await;
    let (other_source_version_id, _other_source_location_id) =
        source_version_and_location(&pool).await;
    let handle = create_staged_handle(&repo, source_version_id).await;
    let unlinked_handle = repo.create_handle(sample_new_handle()).await.unwrap();
    let other_handle = create_staged_handle(&repo, source_version_id).await;
    let live_location = repo
        .record_location(NewArtifactLocation {
            artifact_handle_id: handle.id,
            kind: "staging".to_owned(),
            value: "/staging/live.mkv".to_owned(),
            observed_at: OffsetDateTime::UNIX_EPOCH,
        })
        .await
        .unwrap();
    let retired_location = repo
        .record_location(NewArtifactLocation {
            artifact_handle_id: handle.id,
            kind: "staging".to_owned(),
            value: "/staging/retired.mkv".to_owned(),
            observed_at: OffsetDateTime::UNIX_EPOCH,
        })
        .await
        .unwrap();
    repo.retire_location(
        retired_location.id,
        OffsetDateTime::UNIX_EPOCH + time::Duration::seconds(1),
    )
    .await
    .unwrap();
    let other_location = repo
        .record_location(NewArtifactLocation {
            artifact_handle_id: other_handle.id,
            kind: "staging".to_owned(),
            value: "/staging/other.mkv".to_owned(),
            observed_at: OffsetDateTime::UNIX_EPOCH,
        })
        .await
        .unwrap();
    let unlinked_location = repo
        .record_location(NewArtifactLocation {
            artifact_handle_id: unlinked_handle.id,
            kind: "staging".to_owned(),
            value: "/staging/unlinked.mkv".to_owned(),
            observed_at: OffsetDateTime::UNIX_EPOCH,
        })
        .await
        .unwrap();
    let mut tx = pool.begin().await.unwrap();
    let failed = repo
        .record_verification_in_tx(
            &mut tx,
            failed_verification(
                handle.id,
                live_location.id,
                worker_id,
                &live_location.value,
                "VERIFY_FAILED",
                1,
            ),
        )
        .await
        .unwrap();
    let live_success = repo
        .record_verification_in_tx(
            &mut tx,
            successful_verification(
                handle.id,
                live_location.id,
                worker_id,
                &live_location.value,
                "live",
                5,
            ),
        )
        .await
        .unwrap();
    let older_live_success = repo
        .record_verification_in_tx(
            &mut tx,
            successful_verification(
                handle.id,
                live_location.id,
                worker_id,
                &live_location.value,
                "older-live",
                6,
            ),
        )
        .await
        .unwrap();
    let latest_live_success = repo
        .record_verification_in_tx(
            &mut tx,
            successful_verification(
                handle.id,
                live_location.id,
                worker_id,
                &live_location.value,
                "latest-live",
                7,
            ),
        )
        .await
        .unwrap();
    let retired = repo
        .record_verification_in_tx(
            &mut tx,
            successful_verification(
                handle.id,
                retired_location.id,
                worker_id,
                &retired_location.value,
                "retired",
                2,
            ),
        )
        .await
        .unwrap();
    let other = repo
        .record_verification_in_tx(
            &mut tx,
            successful_verification(
                other_handle.id,
                other_location.id,
                worker_id,
                &other_location.value,
                "other",
                3,
            ),
        )
        .await
        .unwrap();
    let unlinked = repo
        .record_verification_in_tx(
            &mut tx,
            successful_verification(
                unlinked_handle.id,
                unlinked_location.id,
                worker_id,
                &unlinked_location.value,
                "unlinked",
                4,
            ),
        )
        .await
        .unwrap();

    for (verification_id, target) in [
        (failed.id, "/media/failed.mkv"),
        (retired.id, "/media/retired.mkv"),
        (other.id, "/media/other.mkv"),
        (older_live_success.id, "/media/stale-verification.mkv"),
    ] {
        let err = repo
            .create_pending_commit_in_tx(
                &mut tx,
                pending_commit(handle.id, source_version_id, verification_id, target),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, voom_core::VoomError::Conflict(_)));
    }
    for (artifact_handle_id, source_file_version_id, verification_id, target) in [
        (
            handle.id,
            other_source_version_id,
            live_success.id,
            "/media/source-mismatch.mkv",
        ),
        (
            unlinked_handle.id,
            source_version_id,
            unlinked.id,
            "/media/unlinked.mkv",
        ),
    ] {
        let err = repo
            .create_pending_commit_in_tx(
                &mut tx,
                pending_commit(
                    artifact_handle_id,
                    source_file_version_id,
                    verification_id,
                    target,
                ),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, voom_core::VoomError::Conflict(_)));
    }
    let _ok = repo
        .create_pending_commit_in_tx(
            &mut tx,
            pending_commit(
                handle.id,
                source_version_id,
                latest_live_success.id,
                "/media/latest-ok.mkv",
            ),
        )
        .await
        .unwrap();

    tx.commit().await.unwrap();
}

#[tokio::test]
async fn pending_commit_rejects_retired_source_file_version() {
    let (pool, _tmp) = pool().await;
    let repo = SqliteArtifactRepo::new(pool.clone());
    let identity = SqliteIdentityRepo::new(pool.clone());
    let worker_id = verification_worker(&pool).await;
    let (source_version_id, _source_location_id) = source_version_and_location(&pool).await;
    let handle = create_staged_handle(&repo, source_version_id).await;
    let location = repo
        .record_location(NewArtifactLocation {
            artifact_handle_id: handle.id,
            kind: "staging".to_owned(),
            value: "/staging/source-retired.mkv".to_owned(),
            observed_at: OffsetDateTime::UNIX_EPOCH,
        })
        .await
        .unwrap();
    let mut tx = pool.begin().await.unwrap();
    let verification = repo
        .record_verification_in_tx(
            &mut tx,
            successful_verification(
                handle.id,
                location.id,
                worker_id,
                &location.value,
                "source-retired",
                1,
            ),
        )
        .await
        .unwrap();
    identity
        .retire_file_version_in_tx(
            &mut tx,
            source_version_id,
            OffsetDateTime::UNIX_EPOCH + time::Duration::seconds(2),
            0,
        )
        .await
        .unwrap();

    let err = repo
        .create_pending_commit_in_tx(
            &mut tx,
            pending_commit(
                handle.id,
                source_version_id,
                verification.id,
                "/media/source-retired.mkv",
            ),
        )
        .await
        .unwrap_err();
    tx.commit().await.unwrap();

    assert!(matches!(err, voom_core::VoomError::Conflict(_)));
}

#[tokio::test]
async fn committed_record_requires_result_location_on_staged_commit_child() {
    let (pool, _tmp) = pool().await;
    let repo = SqliteArtifactRepo::new(pool.clone());
    let identity = SqliteIdentityRepo::new(pool.clone());
    let worker_id = verification_worker(&pool).await;
    let (source_version_id, _source_location_id) = source_version_and_location(&pool).await;
    let source_asset_id = identity
        .get_file_version(source_version_id)
        .await
        .unwrap()
        .unwrap()
        .file_asset_id;
    let handle = create_staged_handle(&repo, source_version_id).await;
    let staging_location = repo
        .record_location(NewArtifactLocation {
            artifact_handle_id: handle.id,
            kind: "staging".to_owned(),
            value: "/staging/out.mkv".to_owned(),
            observed_at: OffsetDateTime::UNIX_EPOCH,
        })
        .await
        .unwrap();
    let mut tx = pool.begin().await.unwrap();
    let verification = repo
        .record_verification_in_tx(
            &mut tx,
            successful_verification(
                handle.id,
                staging_location.id,
                worker_id,
                &staging_location.value,
                "ok",
                1,
            ),
        )
        .await
        .unwrap();
    let pending = repo
        .create_pending_commit_in_tx(
            &mut tx,
            pending_commit(
                handle.id,
                source_version_id,
                verification.id,
                "/media/out.mkv",
            ),
        )
        .await
        .unwrap();
    let staged_child = identity
        .create_file_version_in_tx(
            &mut tx,
            NewFileVersion {
                file_asset_id: source_asset_id,
                content_hash: "child-hash".to_owned(),
                size_bytes: 1024,
                produced_by: ProducedBy::StagedCommit,
                produced_from_version_id: Some(source_version_id),
                created_at: OffsetDateTime::UNIX_EPOCH,
            },
        )
        .await
        .unwrap();
    let wrong_child = identity
        .create_file_version_in_tx(
            &mut tx,
            NewFileVersion {
                file_asset_id: source_asset_id,
                content_hash: "wrong-child-hash".to_owned(),
                size_bytes: 1024,
                produced_by: ProducedBy::StagedCommit,
                produced_from_version_id: Some(source_version_id),
                created_at: OffsetDateTime::UNIX_EPOCH,
            },
        )
        .await
        .unwrap();
    let wrong_location = identity
        .create_file_location_in_tx(
            &mut tx,
            NewFileLocation {
                file_version_id: wrong_child.id,
                kind: FileLocationKind::LocalPath,
                value: "/media/wrong.mkv".to_owned(),
                proof: None,
                observed_at: OffsetDateTime::UNIX_EPOCH,
            },
        )
        .await
        .unwrap();
    let wrong_path_location = identity
        .create_file_location_in_tx(
            &mut tx,
            NewFileLocation {
                file_version_id: staged_child.id,
                kind: FileLocationKind::LocalPath,
                value: "/media/wrong-target.mkv".to_owned(),
                proof: None,
                observed_at: OffsetDateTime::UNIX_EPOCH,
            },
        )
        .await
        .unwrap();
    let wrong_kind_location = identity
        .create_file_location_in_tx(
            &mut tx,
            NewFileLocation {
                file_version_id: staged_child.id,
                kind: FileLocationKind::Historical,
                value: "/media/out.mkv".to_owned(),
                proof: None,
                observed_at: OffsetDateTime::UNIX_EPOCH,
            },
        )
        .await
        .unwrap();

    for location_id in [
        wrong_location.id,
        wrong_path_location.id,
        wrong_kind_location.id,
    ] {
        let err = repo
            .mark_commit_committed_in_tx(
                &mut tx,
                pending.id,
                staged_child.id,
                location_id,
                OffsetDateTime::UNIX_EPOCH,
                OffsetDateTime::UNIX_EPOCH,
            )
            .await
            .unwrap_err();
        assert!(matches!(err, voom_core::VoomError::Conflict(_)));
    }
    tx.commit().await.unwrap();
}

#[tokio::test]
async fn committed_record_rejects_retired_result_file_version() {
    let (pool, _tmp) = pool().await;
    let repo = SqliteArtifactRepo::new(pool.clone());
    let identity = SqliteIdentityRepo::new(pool.clone());
    let worker_id = verification_worker(&pool).await;
    let (source_version_id, _source_location_id) = source_version_and_location(&pool).await;
    let source_asset_id = identity
        .get_file_version(source_version_id)
        .await
        .unwrap()
        .unwrap()
        .file_asset_id;
    let handle = create_staged_handle(&repo, source_version_id).await;
    let staging_location = repo
        .record_location(NewArtifactLocation {
            artifact_handle_id: handle.id,
            kind: "staging".to_owned(),
            value: "/staging/result-retired.mkv".to_owned(),
            observed_at: OffsetDateTime::UNIX_EPOCH,
        })
        .await
        .unwrap();
    let mut tx = pool.begin().await.unwrap();
    let verification = repo
        .record_verification_in_tx(
            &mut tx,
            successful_verification(
                handle.id,
                staging_location.id,
                worker_id,
                &staging_location.value,
                "result-retired",
                1,
            ),
        )
        .await
        .unwrap();
    let pending = repo
        .create_pending_commit_in_tx(
            &mut tx,
            pending_commit(
                handle.id,
                source_version_id,
                verification.id,
                "/media/result-retired.mkv",
            ),
        )
        .await
        .unwrap();
    let result_version = identity
        .create_file_version_in_tx(
            &mut tx,
            NewFileVersion {
                file_asset_id: source_asset_id,
                content_hash: "result-retired-hash".to_owned(),
                size_bytes: 1024,
                produced_by: ProducedBy::StagedCommit,
                produced_from_version_id: Some(source_version_id),
                created_at: OffsetDateTime::UNIX_EPOCH,
            },
        )
        .await
        .unwrap();
    let result_location = identity
        .create_file_location_in_tx(
            &mut tx,
            NewFileLocation {
                file_version_id: result_version.id,
                kind: FileLocationKind::LocalPath,
                value: "/media/result-retired.mkv".to_owned(),
                proof: None,
                observed_at: OffsetDateTime::UNIX_EPOCH,
            },
        )
        .await
        .unwrap();
    identity
        .retire_file_version_in_tx(
            &mut tx,
            result_version.id,
            OffsetDateTime::UNIX_EPOCH + time::Duration::seconds(2),
            0,
        )
        .await
        .unwrap();

    let err = repo
        .mark_commit_committed_in_tx(
            &mut tx,
            pending.id,
            result_version.id,
            result_location.id,
            OffsetDateTime::UNIX_EPOCH,
            OffsetDateTime::UNIX_EPOCH,
        )
        .await
        .unwrap_err();
    tx.commit().await.unwrap();

    assert!(matches!(err, voom_core::VoomError::Conflict(_)));
}

#[tokio::test]
async fn sidecar_commit_helper_links_staged_version_to_source_and_finalizes_pending_record() {
    let (pool, _tmp) = pool().await;
    let repo = SqliteArtifactRepo::new(pool.clone());
    let identity = SqliteIdentityRepo::new(pool.clone());
    let worker_id = verification_worker(&pool).await;
    let (source_version_id, _source_location_id) = source_version_and_location(&pool).await;
    let source_asset_id = source_asset_id(&identity, source_version_id).await;
    let handle = create_staged_handle(&repo, source_version_id).await;
    let staging_location = repo
        .record_location(NewArtifactLocation {
            artifact_handle_id: handle.id,
            kind: "staging".to_owned(),
            value: "/staging/audio.ogg".to_owned(),
            observed_at: OffsetDateTime::UNIX_EPOCH,
        })
        .await
        .unwrap();
    let mut tx = pool.begin().await.unwrap();
    let verification = repo
        .record_verification_in_tx(
            &mut tx,
            successful_verification(
                handle.id,
                staging_location.id,
                worker_id,
                &staging_location.value,
                "sidecar",
                1,
            ),
        )
        .await
        .unwrap();
    let pending = repo
        .create_pending_commit_in_tx(
            &mut tx,
            pending_commit(
                handle.id,
                source_version_id,
                verification.id,
                "/media/movie.eng.opus.ogg",
            ),
        )
        .await
        .unwrap();

    let committed = repo
        .record_verified_sidecar_commit_rows_in_tx(
            &mut tx,
            NewSidecarArtifactCommit {
                commit_record_id: pending.id,
                target_path: "/media/movie.eng.opus.ogg".to_owned(),
                content_hash: "sidecar-hash".to_owned(),
                size_bytes: 2048,
                observed_at: OffsetDateTime::UNIX_EPOCH + time::Duration::seconds(2),
                finished_at: OffsetDateTime::UNIX_EPOCH + time::Duration::seconds(3),
            },
        )
        .await
        .unwrap();
    tx.commit().await.unwrap();

    assert_eq!(
        committed.commit_record.state,
        ArtifactCommitState::Committed
    );
    assert_eq!(
        committed.commit_record.result_file_version_id,
        Some(committed.file_version_id)
    );
    assert_eq!(
        committed.commit_record.result_file_location_id,
        Some(committed.file_location_id)
    );
    assert_eq!(committed.commit_record.promotion_started_at, None);

    let sidecar_version = identity
        .get_file_version(committed.file_version_id)
        .await
        .unwrap()
        .unwrap();
    assert_ne!(sidecar_version.file_asset_id, source_asset_id);
    assert_eq!(sidecar_version.file_asset_id, committed.file_asset_id);
    assert_eq!(sidecar_version.produced_by, ProducedBy::StagedCommit);
    assert_eq!(
        sidecar_version.produced_from_version_id,
        Some(source_version_id)
    );

    let locations = identity
        .list_file_locations_by_version(committed.file_version_id)
        .await
        .unwrap();
    assert_eq!(locations.len(), 1);
    assert_eq!(locations[0].id, committed.file_location_id);
    assert_eq!(locations[0].kind, FileLocationKind::LocalPath);
    assert_eq!(locations[0].value, "/media/movie.eng.opus.ogg");
}

#[tokio::test]
async fn sidecar_commit_helper_requires_existing_pending_lineage_record() {
    let (pool, _tmp) = pool().await;
    let repo = SqliteArtifactRepo::new(pool.clone());
    let worker_id = verification_worker(&pool).await;
    let (source_version_id, _source_location_id) = source_version_and_location(&pool).await;
    let handle = create_staged_handle(&repo, source_version_id).await;
    let staging_location = repo
        .record_location(NewArtifactLocation {
            artifact_handle_id: handle.id,
            kind: "staging".to_owned(),
            value: "/staging/audio-no-pending.ogg".to_owned(),
            observed_at: OffsetDateTime::UNIX_EPOCH,
        })
        .await
        .unwrap();
    let mut tx = pool.begin().await.unwrap();
    let _verification = repo
        .record_verification_in_tx(
            &mut tx,
            successful_verification(
                handle.id,
                staging_location.id,
                worker_id,
                &staging_location.value,
                "sidecar-no-pending",
                1,
            ),
        )
        .await
        .unwrap();

    let err = repo
        .record_verified_sidecar_commit_rows_in_tx(
            &mut tx,
            NewSidecarArtifactCommit {
                commit_record_id: voom_core::ids::ArtifactCommitRecordId(404),
                target_path: "/media/no-pending.opus.ogg".to_owned(),
                content_hash: "sidecar-no-pending-hash".to_owned(),
                size_bytes: 2048,
                observed_at: OffsetDateTime::UNIX_EPOCH,
                finished_at: OffsetDateTime::UNIX_EPOCH,
            },
        )
        .await
        .unwrap_err();
    tx.commit().await.unwrap();

    assert!(matches!(err, VoomError::Conflict(_)), "got: {err:?}");
}

#[tokio::test]
async fn sidecar_commit_helper_requires_target_path_from_commit_path() {
    let (pool, _tmp) = pool().await;
    let repo = SqliteArtifactRepo::new(pool.clone());
    let worker_id = verification_worker(&pool).await;
    let (source_version_id, _source_location_id) = source_version_and_location(&pool).await;
    let handle = create_staged_handle(&repo, source_version_id).await;
    let staging_location = repo
        .record_location(NewArtifactLocation {
            artifact_handle_id: handle.id,
            kind: "staging".to_owned(),
            value: "/staging/audio-target-mismatch.ogg".to_owned(),
            observed_at: OffsetDateTime::UNIX_EPOCH,
        })
        .await
        .unwrap();
    let mut tx = pool.begin().await.unwrap();
    let verification = repo
        .record_verification_in_tx(
            &mut tx,
            successful_verification(
                handle.id,
                staging_location.id,
                worker_id,
                &staging_location.value,
                "sidecar-target-mismatch",
                1,
            ),
        )
        .await
        .unwrap();
    let pending = repo
        .create_pending_commit_in_tx(
            &mut tx,
            pending_commit(
                handle.id,
                source_version_id,
                verification.id,
                "/media/expected.opus.ogg",
            ),
        )
        .await
        .unwrap();

    let err = repo
        .record_verified_sidecar_commit_rows_in_tx(
            &mut tx,
            NewSidecarArtifactCommit {
                commit_record_id: pending.id,
                target_path: "/media/other.opus.ogg".to_owned(),
                content_hash: "sidecar-target-mismatch-hash".to_owned(),
                size_bytes: 2048,
                observed_at: OffsetDateTime::UNIX_EPOCH,
                finished_at: OffsetDateTime::UNIX_EPOCH,
            },
        )
        .await
        .unwrap_err();
    tx.commit().await.unwrap();

    assert!(matches!(err, VoomError::Conflict(_)), "got: {err:?}");
}

fn successful_verification(
    artifact_handle_id: voom_core::ArtifactHandleId,
    artifact_location_id: voom_core::ArtifactLocationId,
    worker_id: voom_core::WorkerId,
    path: &str,
    label: &str,
    second: i64,
) -> NewArtifactVerification {
    NewArtifactVerification {
        artifact_handle_id,
        artifact_location_id,
        path: path.to_owned(),
        worker_id,
        workflow_ticket_id: None,
        workflow_lease_id: None,
        status: ArtifactVerificationStatus::Succeeded,
        expected_size_bytes: 1024,
        expected_checksum: "abc".to_owned(),
        observed_size_bytes: Some(1024),
        observed_checksum: Some("abc".to_owned()),
        failure_class: None,
        error_code: None,
        message: None,
        report: json!({"label": label}),
        started_at: OffsetDateTime::UNIX_EPOCH + time::Duration::seconds(second),
        finished_at: OffsetDateTime::UNIX_EPOCH + time::Duration::seconds(second + 1),
    }
}

fn failed_verification(
    artifact_handle_id: voom_core::ArtifactHandleId,
    artifact_location_id: voom_core::ArtifactLocationId,
    worker_id: voom_core::WorkerId,
    path: &str,
    error_code: &str,
    second: i64,
) -> NewArtifactVerification {
    NewArtifactVerification {
        artifact_handle_id,
        artifact_location_id,
        path: path.to_owned(),
        worker_id,
        workflow_ticket_id: None,
        workflow_lease_id: None,
        status: ArtifactVerificationStatus::Failed,
        expected_size_bytes: 1024,
        expected_checksum: "abc".to_owned(),
        observed_size_bytes: None,
        observed_checksum: None,
        failure_class: Some("verification".to_owned()),
        error_code: Some(error_code.to_owned()),
        message: Some("verification failed".to_owned()),
        report: json!({"error": error_code}),
        started_at: OffsetDateTime::UNIX_EPOCH + time::Duration::seconds(second),
        finished_at: OffsetDateTime::UNIX_EPOCH + time::Duration::seconds(second + 1),
    }
}

fn pending_commit(
    artifact_handle_id: voom_core::ArtifactHandleId,
    source_file_version_id: FileVersionId,
    verification_id: voom_core::ids::ArtifactVerificationId,
    target_path: &str,
) -> NewArtifactCommitRecord {
    NewArtifactCommitRecord {
        artifact_handle_id,
        source_file_version_id,
        verification_id,
        target_path: target_path.to_owned(),
        temp_path: Some(format!("{target_path}.tmp")),
        report: json!({"target_path": target_path}),
        started_at: OffsetDateTime::UNIX_EPOCH,
    }
}

fn commit_failure(error_code: &str, message: &str, second: i64) -> ArtifactCommitFailure {
    ArtifactCommitFailure {
        failure_class: "io".to_owned(),
        error_code: error_code.to_owned(),
        message: message.to_owned(),
        finished_at: OffsetDateTime::UNIX_EPOCH + time::Duration::seconds(second),
    }
}

#[tokio::test]
async fn record_location_attaches_to_handle() {
    let (pool, _tmp) = pool().await;
    let repo = SqliteArtifactRepo::new(pool.clone());
    let h = repo.create_handle(sample_new_handle()).await.unwrap();
    let loc = repo
        .record_location(NewArtifactLocation {
            artifact_handle_id: h.id,
            kind: "local_path".to_owned(),
            value: "/tmp/x".to_owned(),
            observed_at: OffsetDateTime::UNIX_EPOCH,
        })
        .await
        .unwrap();
    assert!(loc.id.0 > 0);
    let locs = repo.list_locations_for_handle(h.id).await.unwrap();
    assert_eq!(locs.len(), 1);
}

#[tokio::test]
async fn retire_location_sets_retired_at() {
    let (pool, _tmp) = pool().await;
    let repo = SqliteArtifactRepo::new(pool.clone());
    let h = repo.create_handle(sample_new_handle()).await.unwrap();
    let loc = repo
        .record_location(NewArtifactLocation {
            artifact_handle_id: h.id,
            kind: "local_path".to_owned(),
            value: "/tmp/x".to_owned(),
            observed_at: OffsetDateTime::UNIX_EPOCH,
        })
        .await
        .unwrap();
    let when = OffsetDateTime::UNIX_EPOCH + time::Duration::days(1);
    repo.retire_location(loc.id, when).await.unwrap();
    let live = repo.list_locations_for_handle(h.id).await.unwrap();
    assert_eq!(
        live.len(),
        0,
        "retired locations excluded from live listing"
    );
}

#[tokio::test]
async fn record_lineage_links_two_handles() {
    let (pool, _tmp) = pool().await;
    let repo = SqliteArtifactRepo::new(pool.clone());
    let parent = repo.create_handle(sample_new_handle()).await.unwrap();
    let child = repo.create_handle(sample_new_handle()).await.unwrap();
    let edge = repo
        .record_lineage(NewArtifactLineage {
            parent_artifact_id: parent.id,
            child_artifact_id: child.id,
            operation: "transcode".to_owned(),
            recorded_at: OffsetDateTime::UNIX_EPOCH,
        })
        .await
        .unwrap();
    assert!(edge.id > 0);
}

#[tokio::test]
async fn record_lineage_rejects_self_edge() {
    let (pool, _tmp) = pool().await;
    let repo = SqliteArtifactRepo::new(pool.clone());
    let h = repo.create_handle(sample_new_handle()).await.unwrap();
    let err = repo
        .record_lineage(NewArtifactLineage {
            parent_artifact_id: h.id,
            child_artifact_id: h.id,
            operation: "noop".to_owned(),
            recorded_at: OffsetDateTime::UNIX_EPOCH,
        })
        .await
        .unwrap_err();
    // CHECK constraint rejects self-references; surfaces as Database.
    assert!(matches!(err, voom_core::VoomError::Database { .. }));
}

#[tokio::test]
async fn committed_ticket_evidence_decodes_typed_joined_facts() {
    let (pool, _tmp) = pool().await;
    seed_workflow_evidence(&pool).await;
    let repo = SqliteArtifactRepo::new(pool);

    let rows = repo
        .committed_ticket_evidence(&[TicketId(1)])
        .await
        .unwrap();

    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    assert_eq!(row.ticket_id, TicketId(1));
    assert_eq!(row.ticket_job_id, Some(voom_core::JobId(1)));
    assert_eq!(row.ticket_payload["branch_id"], "alpha");
    assert_eq!(row.result["commit_record_id"], 1);
    assert_eq!(
        row.commit.as_ref().unwrap().state,
        ArtifactCommitState::Committed
    );
    assert_eq!(
        row.verification.as_ref().unwrap().status,
        ArtifactVerificationStatus::Succeeded
    );
    assert_eq!(
        row.result_lease.as_ref().unwrap().state,
        crate::repo::execution::leases::LeaseState::Released
    );
    assert_eq!(row.source_file_asset_id, Some(FileAssetId(1)));
    assert_eq!(row.result_file_asset_id, Some(FileAssetId(1)));
    assert_eq!(row.location_file_version_id, Some(FileVersionId(2)));
    assert_eq!(row.snapshot_file_version_id, Some(FileVersionId(2)));
}

#[tokio::test]
async fn committed_ticket_evidence_preserves_absent_joins_and_rejects_corruption() {
    let (pool, _tmp) = pool().await;
    seed_workflow_evidence(&pool).await;
    let repo = SqliteArtifactRepo::new(pool.clone());
    sqlx::query(
        "UPDATE tickets SET result = json_set(result, '$.commit_record_id', 999) WHERE id = 1",
    )
    .execute(&pool)
    .await
    .unwrap();
    let row = repo
        .committed_ticket_evidence(&[TicketId(1)])
        .await
        .unwrap()
        .pop()
        .unwrap();
    assert!(row.commit.is_none());

    let mut connection = pool.acquire().await.unwrap();
    sqlx::query("PRAGMA ignore_check_constraints = TRUE")
        .execute(&mut *connection)
        .await
        .unwrap();
    sqlx::query("UPDATE tickets SET result = '{' WHERE id = 1")
        .execute(&mut *connection)
        .await
        .unwrap();
    let error = repo
        .committed_ticket_evidence(&[TicketId(1)])
        .await
        .unwrap_err();
    assert!(error.to_string().contains("ticket result"));
}

#[tokio::test]
async fn verified_ticket_evidence_decodes_verification_and_optional_location() {
    let (pool, _tmp) = pool().await;
    seed_workflow_evidence(&pool).await;
    let repo = SqliteArtifactRepo::new(pool);

    let evidence = repo
        .verified_ticket_evidence(
            TicketId(1),
            voom_core::ids::ArtifactVerificationId(1),
            voom_core::ArtifactHandleId(1),
            FileLocationId(1),
        )
        .await
        .unwrap()
        .unwrap();

    assert_eq!(
        evidence.verification.id,
        voom_core::ids::ArtifactVerificationId(1)
    );
    assert_eq!(evidence.file_version_id, Some(FileVersionId(2)));
    assert_eq!(evidence.location_value.as_deref(), Some("/output.mkv"));

    let without_location = repo
        .verified_ticket_evidence(
            TicketId(1),
            voom_core::ids::ArtifactVerificationId(1),
            voom_core::ArtifactHandleId(1),
            FileLocationId(999),
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(without_location.file_version_id, None);
    assert_eq!(without_location.location_value, None);
}

#[tokio::test]
async fn verified_ticket_evidence_exposes_mismatched_ticket_for_caller_validation() {
    let (pool, _tmp) = pool().await;
    seed_workflow_evidence(&pool).await;
    let repo = SqliteArtifactRepo::new(pool);

    let evidence = repo
        .verified_ticket_evidence(
            TicketId(999),
            voom_core::ids::ArtifactVerificationId(1),
            voom_core::ArtifactHandleId(1),
            FileLocationId(1),
        )
        .await
        .unwrap()
        .unwrap();

    assert_eq!(evidence.verification.workflow_ticket_id, Some(TicketId(1)));
}

#[tokio::test]
async fn verified_ticket_evidence_exposes_mismatched_handle_for_caller_validation() {
    let (pool, _tmp) = pool().await;
    seed_workflow_evidence(&pool).await;
    let repo = SqliteArtifactRepo::new(pool);

    let evidence = repo
        .verified_ticket_evidence(
            TicketId(1),
            voom_core::ids::ArtifactVerificationId(1),
            voom_core::ArtifactHandleId(2),
            FileLocationId(1),
        )
        .await
        .unwrap()
        .unwrap();

    assert_eq!(
        evidence.verification.artifact_handle_id,
        voom_core::ArtifactHandleId(1)
    );
}

#[tokio::test]
async fn verified_ticket_evidence_rejects_negative_persisted_ticket_id() {
    let (pool, _tmp) = pool().await;
    seed_workflow_evidence(&pool).await;
    let mut connection = pool.acquire().await.unwrap();
    sqlx::query("PRAGMA foreign_keys = OFF")
        .execute(&mut *connection)
        .await
        .unwrap();
    sqlx::query("UPDATE artifact_verifications SET workflow_ticket_id = -1 WHERE id = 1")
        .execute(&mut *connection)
        .await
        .unwrap();
    drop(connection);
    let repo = SqliteArtifactRepo::new(pool);

    let error = repo
        .verified_ticket_evidence(
            TicketId(1),
            voom_core::ids::ArtifactVerificationId(1),
            voom_core::ArtifactHandleId(1),
            FileLocationId(1),
        )
        .await
        .unwrap_err();

    assert!(matches!(error, VoomError::Database { .. }));
    assert!(error.to_string().contains("workflow_ticket_id"));
}

#[tokio::test]
async fn verified_ticket_evidence_rejects_negative_persisted_handle_id() {
    let (pool, _tmp) = pool().await;
    seed_workflow_evidence(&pool).await;
    let mut connection = pool.acquire().await.unwrap();
    sqlx::query("PRAGMA foreign_keys = OFF")
        .execute(&mut *connection)
        .await
        .unwrap();
    sqlx::query("UPDATE artifact_verifications SET artifact_handle_id = -1 WHERE id = 1")
        .execute(&mut *connection)
        .await
        .unwrap();
    drop(connection);
    let repo = SqliteArtifactRepo::new(pool);

    let error = repo
        .verified_ticket_evidence(
            TicketId(1),
            voom_core::ids::ArtifactVerificationId(1),
            voom_core::ArtifactHandleId(1),
            FileLocationId(1),
        )
        .await
        .unwrap_err();

    assert!(matches!(error, VoomError::Database { .. }));
    assert!(error.to_string().contains("artifact_handle_id"));
}

#[tokio::test]
async fn verified_ticket_evidence_rejects_lease_owned_by_different_ticket() {
    let (pool, _tmp) = pool().await;
    seed_workflow_evidence(&pool).await;
    sqlx::query(
        "INSERT INTO tickets \
         (id, job_id, kind, state, priority, payload, result, max_attempts, next_eligible_at, \
          created_at, state_changed_at) \
         SELECT 2, job_id, kind, state, priority, payload, result, max_attempts, next_eligible_at, \
                created_at, state_changed_at FROM tickets WHERE id = 1",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("UPDATE leases SET ticket_id = 2 WHERE id = 1")
        .execute(&pool)
        .await
        .unwrap();
    let repo = SqliteArtifactRepo::new(pool);

    let error = repo
        .verified_ticket_evidence(
            TicketId(1),
            voom_core::ids::ArtifactVerificationId(1),
            voom_core::ArtifactHandleId(1),
            FileLocationId(1),
        )
        .await
        .unwrap_err();

    assert!(matches!(error, VoomError::Database { .. }));
    assert!(error.to_string().contains("workflow lease ticket mismatch"));
}

#[tokio::test]
async fn verified_ticket_evidence_rejects_missing_workflow_lease() {
    let (pool, _tmp) = pool().await;
    seed_workflow_evidence(&pool).await;
    let mut connection = pool.acquire().await.unwrap();
    sqlx::query("PRAGMA foreign_keys = OFF")
        .execute(&mut *connection)
        .await
        .unwrap();
    sqlx::query("UPDATE artifact_verifications SET workflow_lease_id = 999 WHERE id = 1")
        .execute(&mut *connection)
        .await
        .unwrap();
    drop(connection);
    let repo = SqliteArtifactRepo::new(pool);

    let error = repo
        .verified_ticket_evidence(
            TicketId(1),
            voom_core::ids::ArtifactVerificationId(1),
            voom_core::ArtifactHandleId(1),
            FileLocationId(1),
        )
        .await
        .unwrap_err();

    assert!(matches!(error, VoomError::Database { .. }));
    assert!(error.to_string().contains("workflow lease ticket mismatch"));
}

#[tokio::test]
async fn committed_ticket_evidence_expands_outputs_and_preserves_sidecar_assets() {
    let (pool, _tmp) = pool().await;
    seed_workflow_evidence(&pool).await;
    sqlx::query("UPDATE tickets SET result = ? WHERE id = 1")
        .bind(
            json!({
                "job_id": 1,
                "ticket_id": 1,
                "lease_id": 1,
                "outputs": [
                    {
                        "source_file_version_id": 1,
                        "staged_artifact_handle_id": 1,
                        "verification_id": 1,
                        "commit_record_id": 1,
                        "result_file_version_id": 2,
                        "result_file_location_id": 1,
                        "result_media_snapshot_id": 1
                    },
                    {
                        "source_file_version_id": 1,
                        "staged_artifact_handle_id": 2,
                        "verification_id": 2,
                        "commit_record_id": 2,
                        "result_file_version_id": 3,
                        "result_file_location_id": 2,
                        "result_media_snapshot_id": 2
                    }
                ]
            })
            .to_string(),
        )
        .execute(&pool)
        .await
        .unwrap();
    let repo = SqliteArtifactRepo::new(pool);

    let rows = repo
        .committed_ticket_evidence(&[TicketId(1)])
        .await
        .unwrap();

    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].result_file_asset_id, Some(FileAssetId(1)));
    assert_eq!(rows[1].result_file_asset_id, Some(FileAssetId(2)));
    assert_eq!(rows[0].commit.as_ref().unwrap().id.0, 1);
    assert_eq!(rows[1].commit.as_ref().unwrap().id.0, 2);
}

#[tokio::test]
async fn committed_ticket_evidence_rejects_each_corrupt_durable_kind() {
    for (statement, expected) in [
        (
            "UPDATE tickets SET payload = '{' WHERE id = 1",
            "ticket_payload",
        ),
        (
            "UPDATE artifact_commit_records SET state = 'unknown' WHERE id = 1",
            "artifact_commit_records.state",
        ),
        (
            "UPDATE artifact_verifications SET status = 'unknown' WHERE id = 1",
            "artifact_verifications.status",
        ),
        (
            "UPDATE leases SET state = 'unknown' WHERE id = 1",
            "leases.state",
        ),
        (
            "UPDATE artifact_commit_records SET started_at = 'bad' WHERE id = 1",
            "parse iso8601",
        ),
    ] {
        let (pool, _tmp) = pool().await;
        seed_workflow_evidence(&pool).await;
        let mut connection = pool.acquire().await.unwrap();
        sqlx::query("PRAGMA ignore_check_constraints = TRUE")
            .execute(&mut *connection)
            .await
            .unwrap();
        sqlx::query(statement)
            .execute(&mut *connection)
            .await
            .unwrap();
        drop(connection);
        let error = SqliteArtifactRepo::new(pool)
            .committed_ticket_evidence(&[TicketId(1)])
            .await
            .unwrap_err();
        assert!(error.to_string().contains(expected), "got: {error}");
    }
}

async fn seed_workflow_evidence(pool: &sqlx::SqlitePool) {
    for statement in [
        "INSERT INTO jobs (id, kind, state, priority, created_at, updated_at) VALUES \
         (1, 'workflow', 'open', 0, '1970-01-01T00:00:00Z', '1970-01-01T00:00:00Z')",
        "INSERT INTO tickets (id, job_id, kind, state, priority, payload, result, max_attempts, \
         next_eligible_at, created_at, state_changed_at) VALUES \
         (1, 1, 'synthetic.workflow.operation.test', 'succeeded', 0, \
          '{\"workflow_id\":\"workflow-1-phase-0\",\"branch_id\":\"alpha\"}', \
          '{\"job_id\":1,\"ticket_id\":1,\"lease_id\":1,\"source_file_version_id\":1,\
           \"staged_artifact_handle_id\":1,\"verification_id\":1,\"commit_record_id\":1,\
           \"result_file_version_id\":2,\"result_file_location_id\":1,\
           \"result_media_snapshot_id\":1}', 1, '1970-01-01T00:00:00Z', \
          '1970-01-01T00:00:00Z', '1970-01-01T00:00:00Z')",
        "INSERT INTO workers (id, name, kind, status, registered_at, last_seen_at) VALUES \
         (1, 'worker', 'synthetic', 'active', '1970-01-01T00:00:00Z', \
          '1970-01-01T00:00:00Z')",
        "INSERT INTO leases (id, ticket_id, worker_id, state, acquired_at, expires_at, \
         last_heartbeat_at, ttl_seconds, release_reason, released_at) VALUES \
         (1, 1, 1, 'released', '1970-01-01T00:00:00Z', '1970-01-01T00:01:00Z', \
          '1970-01-01T00:00:00Z', 60, 'released', '1970-01-01T00:00:00Z')",
        "INSERT INTO file_assets (id, created_at) VALUES (1, '1970-01-01T00:00:00Z')",
        "INSERT INTO file_assets (id, created_at) VALUES (2, '1970-01-01T00:00:00Z')",
        "INSERT INTO file_versions (id, file_asset_id, content_hash, size_bytes, produced_by, \
         created_at) VALUES (1, 1, 'source', 10, 'ingest', '1970-01-01T00:00:00Z')",
        "INSERT INTO file_versions (id, file_asset_id, content_hash, size_bytes, produced_by, \
         produced_from_version_id, created_at) VALUES \
         (2, 1, 'result', 10, 'staged_commit', 1, '1970-01-01T00:00:00Z')",
        "INSERT INTO file_locations (id, file_version_id, kind, value, observed_at) VALUES \
         (1, 2, 'local_path', '/output.mkv', '1970-01-01T00:00:00Z')",
        "INSERT INTO file_versions (id, file_asset_id, content_hash, size_bytes, produced_by, \
         produced_from_version_id, created_at) VALUES \
         (3, 2, 'sidecar', 2, 'staged_commit', 1, '1970-01-01T00:00:00Z')",
        "INSERT INTO file_locations (id, file_version_id, kind, value, observed_at) VALUES \
         (2, 3, 'local_path', '/sidecar.srt', '1970-01-01T00:00:00Z')",
        "INSERT INTO media_snapshots (id, file_version_id, probed_at, payload) VALUES \
         (1, 2, '1970-01-01T00:00:00Z', '{}')",
        "INSERT INTO media_snapshots (id, file_version_id, probed_at, payload) VALUES \
         (2, 3, '1970-01-01T00:00:00Z', '{}')",
        "INSERT INTO artifact_handles (id, privacy_class, durability_class, allowed_access_modes, \
         mutability, file_version_id, created_at) VALUES \
         (1, 'internal', 'staging', '[]', 'immutable', 1, '1970-01-01T00:00:00Z')",
        "INSERT INTO artifact_handles (id, privacy_class, durability_class, allowed_access_modes, \
         mutability, file_version_id, created_at) VALUES \
         (2, 'internal', 'staging', '[]', 'immutable', 1, '1970-01-01T00:00:00Z')",
        "INSERT INTO artifact_locations (id, artifact_handle_id, kind, value, observed_at) VALUES \
         (1, 1, 'staging', '/staging.mkv', '1970-01-01T00:00:00Z')",
        "INSERT INTO artifact_locations (id, artifact_handle_id, kind, value, observed_at) VALUES \
         (2, 2, 'staging', '/staging.srt', '1970-01-01T00:00:00Z')",
        "INSERT INTO artifact_verifications (id, artifact_handle_id, artifact_location_id, path, \
         worker_id, workflow_ticket_id, workflow_lease_id, status, expected_size_bytes, \
         expected_checksum, observed_size_bytes, observed_checksum, report, started_at, \
         finished_at) VALUES (1, 1, 1, '/staging.mkv', 1, 1, 1, 'succeeded', 10, 'sum', \
         10, 'sum', '{}', '1970-01-01T00:00:00Z', '1970-01-01T00:00:00Z')",
        "INSERT INTO artifact_verifications (id, artifact_handle_id, artifact_location_id, path, \
         worker_id, status, expected_size_bytes, expected_checksum, observed_size_bytes, \
         observed_checksum, report, started_at, finished_at) VALUES \
         (2, 2, 2, '/staging.srt', 1, 'succeeded', 2, 'sidecar', 2, 'sidecar', '{}', \
          '1970-01-01T00:00:00Z', '1970-01-01T00:00:00Z')",
        "INSERT INTO artifact_commit_records (id, artifact_handle_id, source_file_version_id, \
         verification_id, target_path, result_file_version_id, result_file_location_id, state, \
         report, started_at, promotion_started_at, finished_at) VALUES \
         (1, 1, 1, 1, '/output.mkv', 2, 1, 'committed', '{}', '1970-01-01T00:00:00Z', \
          '1970-01-01T00:00:00Z', '1970-01-01T00:00:00Z')",
        "INSERT INTO artifact_commit_records (id, artifact_handle_id, source_file_version_id, \
         verification_id, target_path, result_file_version_id, result_file_location_id, state, \
         report, started_at, promotion_started_at, finished_at) VALUES \
         (2, 2, 1, 2, '/sidecar.srt', 3, 2, 'committed', '{}', '1970-01-01T00:00:00Z', \
          '1970-01-01T00:00:00Z', '1970-01-01T00:00:00Z')",
    ] {
        sqlx::query(statement).execute(pool).await.unwrap();
    }
}
