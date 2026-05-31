use super::*;

use serde_json::json;
use time::OffsetDateTime;
use voom_core::{FileAssetId, FileLocationId, FileVersionId};

use crate::repo::execution::workers::{NewWorker, SqliteWorkerRepo, WorkerKind, WorkerRepo};
use crate::repo::media::identity::{
    FileLocationKind, IdentityRepo, NewFileLocation, NewFileVersion, ProducedBy, SqliteIdentityRepo,
};

use crate::test_support::fresh_initialized_pool_at;

async fn pool() -> (sqlx::SqlitePool, tempfile::NamedTempFile) {
    let tmp = tempfile::NamedTempFile::new().unwrap();
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
async fn create_handle_returns_id() {
    let (pool, _tmp) = pool().await;
    let repo = SqliteArtifactRepo::new(pool.clone());
    let h = repo.create_handle(sample_new_handle()).await.unwrap();
    assert!(h.id.0 > 0);
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
    assert!(matches!(err, voom_core::VoomError::Database(_)));
}
