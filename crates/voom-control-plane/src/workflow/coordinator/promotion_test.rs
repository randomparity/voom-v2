use super::*;

async fn write(path: &Path, bytes: &[u8]) {
    tokio::fs::write(path, bytes).await.unwrap();
}

// --- files_have_equal_contents ---

#[tokio::test]
async fn equal_contents_true_for_identical_files() {
    let tmp = tempfile::TempDir::new().unwrap();
    let (a, b) = (tmp.path().join("a"), tmp.path().join("b"));
    write(&a, b"terminal-bytes").await;
    write(&b, b"terminal-bytes").await;
    assert!(files_have_equal_contents(&a, &b).await.unwrap());
}

#[tokio::test]
async fn equal_contents_false_for_same_size_different_bytes() {
    let tmp = tempfile::TempDir::new().unwrap();
    let (a, b) = (tmp.path().join("a"), tmp.path().join("b"));
    write(&a, b"aaaa").await;
    write(&b, b"bbbb").await;
    assert!(!files_have_equal_contents(&a, &b).await.unwrap());
}

#[tokio::test]
async fn equal_contents_false_for_different_size() {
    let tmp = tempfile::TempDir::new().unwrap();
    let (a, b) = (tmp.path().join("a"), tmp.path().join("b"));
    write(&a, b"short").await;
    write(&b, b"longer-content").await;
    assert!(!files_have_equal_contents(&a, &b).await.unwrap());
}

#[tokio::test]
async fn equal_contents_true_for_empty_files() {
    let tmp = tempfile::TempDir::new().unwrap();
    let (a, b) = (tmp.path().join("a"), tmp.path().join("b"));
    write(&a, b"").await;
    write(&b, b"").await;
    assert!(files_have_equal_contents(&a, &b).await.unwrap());
}

#[test]
fn promotion_layout_uses_input_root_for_primary_assets() {
    let relative = promotion_relative_dir(
        Some(Path::new("/library/show/S01")),
        Path::new("/library"),
        Some(Path::new("/library/show/S01")),
        Path::new("/stage/.committed/audio"),
        Path::new("/stage/.committed/audio"),
    );

    assert_eq!(relative, Path::new("show/S01"));
}

#[test]
fn promotion_layout_flattens_a_single_sidecar_operation_dir() {
    let relative = promotion_relative_dir(
        Some(Path::new("/stage/.committed/audio/v8")),
        Path::new("/library/show/S01"),
        Some(Path::new("/library/show/S01")),
        Path::new("/stage/.committed/audio/v8"),
        Path::new("/stage/.committed/audio"),
    );

    assert_eq!(relative, Path::new(""));
}

#[test]
fn promotion_layout_scopes_sidecar_to_branch_source_subtree() {
    let relative = promotion_relative_dir(
        Some(Path::new("/stage/.committed/audio/v8")),
        Path::new("/library"),
        Some(Path::new("/library/show/S01")),
        Path::new("/stage/.committed/audio/v8"),
        Path::new("/stage/.committed/audio"),
    );

    assert_eq!(relative, Path::new("show/S01"));
}

#[test]
fn promotion_layout_preserves_multiple_sidecar_operation_dirs() {
    let relative = promotion_relative_dir(
        Some(Path::new("/stage/.committed/audio/v8")),
        Path::new("/library/show/S01"),
        Some(Path::new("/library/show/S01")),
        Path::new("/stage/.committed/audio"),
        Path::new("/stage/.committed/audio"),
    );

    assert_eq!(relative, Path::new("v8"));
}

// --- copy_into_place ---

#[tokio::test]
async fn copy_terminal_artifact_moves_bytes_and_cleans_up() {
    let tmp = tempfile::TempDir::new().unwrap();
    let current = tmp.path().join("work").join("Movie.hevc.mkv");
    let dest = tmp.path().join("out").join("Movie.hevc.mkv");
    tokio::fs::create_dir_all(current.parent().unwrap())
        .await
        .unwrap();
    tokio::fs::create_dir_all(dest.parent().unwrap())
        .await
        .unwrap();
    write(&current, b"terminal-bytes").await;

    let temp = promotion_temp_path(&dest, FileLocationId(1)).unwrap();
    copy_terminal_artifact(&current, &dest, &temp)
        .await
        .unwrap();

    assert_eq!(tokio::fs::read(&dest).await.unwrap(), b"terminal-bytes");
    assert!(tokio::fs::symlink_metadata(&current).await.is_err());
    let leftovers = std::fs::read_dir(dest.parent().unwrap())
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with(".voom-promote.")
        })
        .count();
    assert_eq!(leftovers, 0);
}

#[tokio::test]
async fn copy_fallback_reclaims_partial_when_destination_appears() {
    let tmp = tempfile::TempDir::new().unwrap();
    let current = tmp.path().join("Movie.work.mkv");
    let dest = tmp.path().join("Movie.mkv");
    let temp = promotion_temp_path(&dest, FileLocationId(44)).unwrap();
    write(&current, b"terminal-bytes").await;
    write(&dest, b"terminal-bytes").await;
    write(&temp, b"stale-partial").await;

    let returned = copy_terminal_artifact(&current, &dest, &temp)
        .await
        .unwrap();

    assert_eq!(returned, dest);
    assert!(tokio::fs::symlink_metadata(&current).await.is_err());
    assert!(tokio::fs::symlink_metadata(&temp).await.is_err());
}

// --- move_terminal_artifact ---

#[tokio::test]
async fn resumed_copy_recovers_and_removes_source() {
    let tmp = tempfile::TempDir::new().unwrap();
    let current = tmp.path().join("Movie.work.mkv");
    let dest = tmp.path().join("Movie.mkv");
    write(&current, b"terminal-bytes").await;
    write(&dest, b"terminal-bytes").await; // copy-done, remove-failed

    let returned = move_terminal_artifact(&current, &dest, FileLocationId(2))
        .await
        .unwrap();

    assert_eq!(returned, dest);
    assert!(tokio::fs::symlink_metadata(&current).await.is_err());
    assert_eq!(tokio::fs::read(&dest).await.unwrap(), b"terminal-bytes");
}

#[tokio::test]
async fn genuine_collision_same_size_fails() {
    let tmp = tempfile::TempDir::new().unwrap();
    let current = tmp.path().join("Movie.work.mkv");
    let dest = tmp.path().join("Movie.mkv");
    write(&current, b"aaaaaaaaaaaaaa").await;
    write(&dest, b"bbbbbbbbbbbbbb").await;

    let err = move_terminal_artifact(&current, &dest, FileLocationId(3))
        .await
        .unwrap_err();

    assert!(
        err.to_string()
            .contains("promotion destination already exists"),
        "unexpected: {err}"
    );
    assert!(tokio::fs::symlink_metadata(&current).await.is_ok());
    assert_eq!(tokio::fs::read(&dest).await.unwrap(), b"bbbbbbbbbbbbbb");
}

#[tokio::test]
async fn genuine_collision_different_size_fails() {
    let tmp = tempfile::TempDir::new().unwrap();
    let current = tmp.path().join("Movie.work.mkv");
    let dest = tmp.path().join("Movie.mkv");
    write(&current, b"terminal-bytes").await;
    write(&dest, b"a-different-shorter").await;

    let err = move_terminal_artifact(&current, &dest, FileLocationId(4))
        .await
        .unwrap_err();

    assert!(
        err.to_string()
            .contains("promotion destination already exists")
    );
    assert!(tokio::fs::symlink_metadata(&current).await.is_ok());
}

#[tokio::test]
async fn directory_destination_fails() {
    let tmp = tempfile::TempDir::new().unwrap();
    let current = tmp.path().join("Movie.work.mkv");
    let dest = tmp.path().join("Movie.mkv");
    write(&current, b"terminal-bytes").await;
    tokio::fs::create_dir(&dest).await.unwrap();

    let err = move_terminal_artifact(&current, &dest, FileLocationId(5))
        .await
        .unwrap_err();

    assert!(
        err.to_string()
            .contains("promotion destination already exists")
    );
    assert!(tokio::fs::symlink_metadata(&current).await.is_ok());
}

#[tokio::test]
async fn already_moved_source_gone_repoints() {
    let tmp = tempfile::TempDir::new().unwrap();
    let current = tmp.path().join("Movie.work.mkv");
    let dest = tmp.path().join("Movie.mkv");
    write(&dest, b"terminal-bytes").await; // current absent

    let returned = move_terminal_artifact(&current, &dest, FileLocationId(6))
        .await
        .unwrap();

    assert_eq!(returned, dest);
    assert_eq!(tokio::fs::read(&dest).await.unwrap(), b"terminal-bytes");
}

#[tokio::test]
async fn normal_move_dest_absent_places_and_removes_source() {
    let tmp = tempfile::TempDir::new().unwrap();
    let current = tmp.path().join("Movie.work.mkv");
    let dest = tmp.path().join("Movie.mkv");
    write(&current, b"terminal-bytes").await;

    let returned = move_terminal_artifact(&current, &dest, FileLocationId(7))
        .await
        .unwrap();

    assert_eq!(returned, dest);
    assert!(tokio::fs::symlink_metadata(&current).await.is_err());
    assert_eq!(tokio::fs::read(&dest).await.unwrap(), b"terminal-bytes");
}

#[tokio::test]
async fn interrupted_copy_temp_is_reclaimed_before_retry() {
    let tmp = tempfile::TempDir::new().unwrap();
    let current = tmp.path().join("Movie.work.mkv");
    let dest = tmp.path().join("Movie.mkv");
    let location_id = FileLocationId(41);
    let temp = promotion_temp_path(&dest, location_id).unwrap();
    write(&current, b"terminal-bytes").await;
    write(&temp, b"interrupted").await;

    let returned = move_terminal_artifact(&current, &dest, location_id)
        .await
        .unwrap();

    assert_eq!(returned, dest);
    assert_eq!(tokio::fs::read(&dest).await.unwrap(), b"terminal-bytes");
    assert!(tokio::fs::symlink_metadata(&temp).await.is_err());
}

#[tokio::test]
async fn concurrent_moves_never_replace_the_winning_destination() {
    let tmp = tempfile::TempDir::new().unwrap();
    let first = tmp.path().join("first.work.mkv");
    let second = tmp.path().join("second.work.mkv");
    let dest = tmp.path().join("Movie.mkv");
    write(&first, b"first-terminal").await;
    write(&second, b"second-output").await;

    let (first_result, second_result) = tokio::join!(
        move_terminal_artifact(&first, &dest, FileLocationId(8)),
        move_terminal_artifact(&second, &dest, FileLocationId(9))
    );

    assert_ne!(first_result.is_ok(), second_result.is_ok());
    let bytes = tokio::fs::read(&dest).await.unwrap();
    assert!(
        bytes == b"first-terminal" || bytes == b"second-output",
        "the destination must contain one complete contender"
    );
}

#[tokio::test]
async fn same_location_contender_waits_for_temp_ownership() {
    let tmp = tempfile::TempDir::new().unwrap();
    let current = tmp.path().join("Movie.work.mkv");
    let dest = tmp.path().join("Movie.mkv");
    let location_id = FileLocationId(42);
    let temp = promotion_temp_path(&dest, location_id).unwrap();
    write(&current, b"terminal-bytes").await;
    write(&temp, b"interrupted").await;
    let owner = PromotionTempOwnership::acquire(&temp).await.unwrap();

    let mut contender = tokio::spawn({
        let current = current.clone();
        let dest = dest.clone();
        let contender_temp = temp.clone();
        async move { copy_terminal_artifact(&current, &dest, &contender_temp).await }
    });
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(50), &mut contender)
            .await
            .is_err(),
        "a same-location contender must wait for exclusive temp ownership"
    );

    drop(owner);
    assert_eq!(contender.await.unwrap().unwrap(), dest);
    assert_eq!(tokio::fs::read(&dest).await.unwrap(), b"terminal-bytes");
    assert!(tokio::fs::symlink_metadata(&temp).await.is_err());
}

#[tokio::test]
async fn stale_waiter_rejects_replaced_temp_path() {
    let tmp = tempfile::TempDir::new().unwrap();
    let dest = tmp.path().join("Movie.mkv");
    let temp = promotion_temp_path(&dest, FileLocationId(43)).unwrap();
    write(&temp, b"first-partial").await;
    let owner = PromotionTempOwnership::acquire(&temp).await.unwrap();
    let stale_file = open_promotion_temp(&temp).unwrap();

    tokio::fs::remove_file(&temp).await.unwrap();
    write(&temp, b"replacement-partial").await;
    let validation = tokio::spawn({
        let temp = temp.clone();
        async move { PromotionTempOwnership::lock_and_validate(&temp, stale_file).await }
    });
    drop(owner);

    assert!(
        validation.await.unwrap().unwrap().is_none(),
        "a waiter holding the detached inode must reopen the replacement path"
    );
    assert_eq!(
        tokio::fs::read(&temp).await.unwrap(),
        b"replacement-partial"
    );
}

#[tokio::test]
async fn interrupted_intermediate_cleanup_retires_a_location_after_file_is_already_gone() {
    use voom_store::repo::media::identity::{DiscoveredFile, FileLocationRepo, IngestOutcome};

    let (cp, _db) = crate::cases::cp().await;
    let tmp = tempfile::TempDir::new().unwrap();
    let path = tmp.path().join("intermediate.mkv");
    write(&path, b"intermediate").await;
    let IngestOutcome::NewFileAsset {
        file_location_id, ..
    } = cp
        .record_discovered_file(
            DiscoveredFile {
                storage_root_id: voom_store::test_support::TEST_STORAGE_ROOT_ID,
                provider_relative_locator: voom_store::test_support::test_relative_locator(
                    &path.display().to_string(),
                ),
                content_hash: "cleanup-replay".to_owned(),
                size_bytes: 12,
                observed_at: time::OffsetDateTime::UNIX_EPOCH,
                proof: None,
            },
            None,
        )
        .await
        .unwrap()
    else {
        panic!("cleanup fixture was not created");
    };
    let location = cp
        .identity()
        .get_file_location(file_location_id)
        .await
        .unwrap()
        .unwrap();
    tokio::fs::remove_file(&path).await.unwrap();

    cp.reclaim_intermediate_location(&location, &path)
        .await
        .unwrap();

    assert!(
        cp.identity()
            .get_file_location(location.id)
            .await
            .unwrap()
            .unwrap()
            .retired_at
            .is_some(),
        "replay must retire the durable location after an interrupted delete"
    );
}

#[tokio::test]
async fn cleanup_failure_before_delete_keeps_location_live() {
    use voom_store::repo::media::identity::{DiscoveredFile, FileLocationRepo, IngestOutcome};

    let (cp, _db) = crate::cases::cp().await;
    let tmp = tempfile::TempDir::new().unwrap();
    let path = tmp.path().join("not-a-file");
    tokio::fs::create_dir(&path).await.unwrap();
    let IngestOutcome::NewFileAsset {
        file_location_id, ..
    } = cp
        .record_discovered_file(
            DiscoveredFile {
                storage_root_id: voom_store::test_support::TEST_STORAGE_ROOT_ID,
                provider_relative_locator: voom_store::test_support::test_relative_locator(
                    &path.display().to_string(),
                ),
                content_hash: "cleanup-failure".to_owned(),
                size_bytes: 0,
                observed_at: time::OffsetDateTime::UNIX_EPOCH,
                proof: None,
            },
            None,
        )
        .await
        .unwrap()
    else {
        panic!("cleanup fixture was not created");
    };
    let location = cp
        .identity()
        .get_file_location(file_location_id)
        .await
        .unwrap()
        .unwrap();

    cp.reclaim_intermediate_location(&location, &path)
        .await
        .unwrap_err();

    assert!(
        cp.identity()
            .get_file_location(location.id)
            .await
            .unwrap()
            .unwrap()
            .retired_at
            .is_none(),
        "failed deletion must not retire a still-present location"
    );
}

// --- placement state (ADR 0103) ---

const T0: time::OffsetDateTime = time::OffsetDateTime::UNIX_EPOCH;

/// Ingest a source file beside `path` and return its version.
async fn source_version(cp: &ControlPlane, path: &Path) -> FileVersionId {
    use voom_store::repo::media::identity::{DiscoveredFile, IngestOutcome};
    let source_path = path.with_extension("source");
    write(&source_path, b"source bytes").await;
    let IngestOutcome::NewFileAsset {
        file_version_id, ..
    } = cp
        .record_discovered_file(
            DiscoveredFile {
                storage_root_id: voom_store::test_support::TEST_STORAGE_ROOT_ID,
                provider_relative_locator: voom_store::test_support::test_relative_locator(
                    &source_path.display().to_string(),
                ),
                content_hash: format!("source-{}", path.display()),
                size_bytes: 12,
                observed_at: T0,
                proof: None,
            },
            None,
        )
        .await
        .unwrap()
    else {
        panic!("source fixture was not created");
    };
    file_version_id
}

/// A staged artifact handle derived from `source` with a succeeded verification.
async fn verified_handle(
    cp: &ControlPlane,
    source: FileVersionId,
    name: &str,
) -> (
    voom_core::ArtifactHandleId,
    voom_core::ids::ArtifactVerificationId,
) {
    use voom_store::repo::media::artifacts::{
        ArtifactHandleAccessMode, ArtifactLocationKind, ArtifactVerificationStatus,
        NewArtifactHandle, NewArtifactLocation, NewArtifactVerification,
    };
    let worker = cp
        .register_worker(crate::cases::workers::RegisterWorkerInput {
            name: format!("placement-{name}"),
            kind: voom_core::WorkerKind::Synthetic,
        })
        .await
        .unwrap();
    let handle = cp
        .create_artifact_handle(NewArtifactHandle {
            size_bytes: Some(14),
            checksum: Some("terminal".to_owned()),
            privacy_class: "internal".to_owned(),
            durability_class: "staging".to_owned(),
            allowed_access_modes: vec![ArtifactHandleAccessMode::LocalPath],
            mutability: "immutable".to_owned(),
            source_lineage: Some(serde_json::json!({"kind": "test"})),
            file_version_id: Some(source),
            created_at: T0,
        })
        .await
        .unwrap();
    let staging = cp
        .record_artifact_location(NewArtifactLocation {
            artifact_handle_id: handle.id,
            kind: ArtifactLocationKind::Staging,
            value: format!("/staging/{}.mkv", handle.id.0),
            observed_at: T0,
        })
        .await
        .unwrap();
    let mut tx = voom_store::tx::begin_read_then_write(&cp.pool, "promotion_test: verification")
        .await
        .unwrap();
    let verification = cp
        .artifacts()
        .record_verification_in_tx(
            &mut tx,
            NewArtifactVerification {
                artifact_handle_id: handle.id,
                artifact_location_id: staging.id,
                path: staging.value.clone(),
                worker_id: worker.id,
                workflow_ticket_id: None,
                workflow_lease_id: None,
                status: ArtifactVerificationStatus::Succeeded,
                expected_size_bytes: 14,
                expected_checksum: "terminal".to_owned(),
                observed_size_bytes: Some(14),
                observed_checksum: Some("terminal".to_owned()),
                failure_class: None,
                error_code: None,
                message: None,
                report: serde_json::json!({}),
                started_at: T0,
                finished_at: T0,
            },
        )
        .await
        .unwrap();
    commit_tx(tx).await.unwrap();
    (handle.id, verification.id)
}

/// A file at `path` whose live location is the result of a committed record
/// with `intent`, shaped as `promote_terminal_artifacts` hands it over.
async fn committed_result(
    cp: &ControlPlane,
    path: &Path,
    intent: voom_store::repo::media::artifacts::CommitPlacementIntent,
) -> WorkingDirArtifact {
    use voom_store::repo::media::artifacts::{NewArtifactCommitRecord, NewSidecarArtifactCommit};
    use voom_store::repo::media::identity::FileLocationRepo;
    use voom_store::test_support::{TEST_STORAGE_ROOT_ID, test_relative_locator};

    write(path, b"terminal bytes").await;
    let source = source_version(cp, path).await;
    let target_path = path.display().to_string();
    let (handle, verification) = verified_handle(cp, source, &target_path).await;
    let locator = test_relative_locator(&target_path);
    let mut tx = begin_write_first(&cp.pool, "promotion_test: committed_result")
        .await
        .unwrap();
    let pending = cp
        .artifacts()
        .create_pending_commit_in_tx(
            &mut tx,
            NewArtifactCommitRecord {
                artifact_handle_id: handle,
                source_file_version_id: source,
                verification_id: verification,
                target_path: target_path.clone(),
                temp_path: None,
                report: serde_json::json!({
                    "rooted_target": {
                        "storage_root_id": TEST_STORAGE_ROOT_ID.0,
                        "provider_relative_locator": locator.as_str(),
                    },
                }),
                started_at: T0,
                placement_intent: intent,
            },
        )
        .await
        .unwrap();
    let committed = cp
        .artifacts()
        .record_verified_sidecar_commit_rows_in_tx(
            &mut tx,
            NewSidecarArtifactCommit {
                commit_record_id: pending.id,
                target_path,
                storage_root_id: TEST_STORAGE_ROOT_ID,
                provider_relative_locator: locator.clone(),
                content_hash: "terminal".to_owned(),
                size_bytes: 14,
                observed_at: T0,
                finished_at: T0,
            },
        )
        .await
        .unwrap();
    commit_tx(tx).await.unwrap();
    let epoch = cp
        .identity()
        .get_file_location(committed.file_location_id)
        .await
        .unwrap()
        .unwrap()
        .epoch;
    WorkingDirArtifact {
        location_id: committed.file_location_id,
        asset_id: committed.file_asset_id,
        storage_root_id: TEST_STORAGE_ROOT_ID,
        provider_relative_locator: locator,
        epoch,
    }
}

async fn placement_fixture() -> (
    ControlPlane,
    voom_test_support::TempDatabase,
    tempfile::TempDir,
) {
    let (cp, db) = crate::cases::cp().await;
    voom_store::test_support::set_test_storage_root_self_defaults(&cp.pool)
        .await
        .unwrap();
    let tmp = tempfile::TempDir::new().unwrap();
    tokio::fs::create_dir_all(tmp.path().join("working"))
        .await
        .unwrap();
    (cp, db, tmp)
}

async fn placement_of(
    cp: &ControlPlane,
    location: FileLocationId,
) -> Option<voom_store::repo::media::artifacts::CommitPlacementState> {
    cp.artifacts()
        .get_commit_record_by_result_location(location)
        .await
        .unwrap()
        .and_then(|record| record.placement_state)
}

async fn locator_of(cp: &ControlPlane, location: FileLocationId) -> String {
    use voom_store::repo::media::identity::FileLocationRepo;
    let location = cp
        .identity()
        .get_file_location(location)
        .await
        .unwrap()
        .unwrap();
    location.rooted_address().unwrap().1.as_str().to_owned()
}

#[tokio::test]
async fn promotion_marks_a_staged_result_placed() {
    use voom_store::repo::media::artifacts::{CommitPlacementIntent, CommitPlacementState};
    let (cp, _db, tmp) = placement_fixture().await;
    let path = tmp.path().join("working/out.mkv");
    let dest_dir = tmp.path().join("output");
    let artifact = committed_result(&cp, &path, CommitPlacementIntent::Staged).await;

    cp.promote_artifact(&artifact, &path, &dest_dir)
        .await
        .unwrap();

    assert_eq!(
        placement_of(&cp, artifact.location_id).await,
        Some(CommitPlacementState::Placed)
    );
    assert!(dest_dir.join("out.mkv").exists());
}

#[tokio::test]
async fn promotion_refuses_a_retained_result_before_moving_bytes() {
    use voom_store::repo::media::artifacts::{CommitPlacementIntent, CommitPlacementState};
    let (cp, _db, tmp) = placement_fixture().await;
    let path = tmp.path().join("working/out.mkv");
    let dest_dir = tmp.path().join("output");
    let artifact = committed_result(&cp, &path, CommitPlacementIntent::Retained).await;
    let before = locator_of(&cp, artifact.location_id).await;

    let error = cp
        .promote_artifact(&artifact, &path, &dest_dir)
        .await
        .unwrap_err();

    assert_eq!(error.error_code(), voom_core::ErrorCode::Conflict);
    assert!(path.exists(), "a retained result's bytes must not move");
    assert!(!dest_dir.join("out.mkv").exists());
    assert_eq!(locator_of(&cp, artifact.location_id).await, before);
    assert_eq!(
        placement_of(&cp, artifact.location_id).await,
        Some(CommitPlacementState::Retained)
    );
}

/// A discovered file at `path` that is no commit record's result.
async fn recordless_result(cp: &ControlPlane, path: &Path) -> WorkingDirArtifact {
    use voom_store::repo::media::identity::{DiscoveredFile, FileLocationRepo, IngestOutcome};
    write(path, b"no commit record").await;
    let locator = voom_store::test_support::test_relative_locator(&path.display().to_string());
    let IngestOutcome::NewFileAsset {
        file_asset_id,
        file_location_id,
        ..
    } = cp
        .record_discovered_file(
            DiscoveredFile {
                storage_root_id: voom_store::test_support::TEST_STORAGE_ROOT_ID,
                provider_relative_locator: locator.clone(),
                content_hash: "no-commit-record".to_owned(),
                size_bytes: 16,
                observed_at: time::OffsetDateTime::UNIX_EPOCH,
                proof: None,
            },
            None,
        )
        .await
        .unwrap()
    else {
        panic!("record-less fixture was not created");
    };
    let epoch = cp
        .identity()
        .get_file_location(file_location_id)
        .await
        .unwrap()
        .unwrap()
        .epoch;
    WorkingDirArtifact {
        location_id: file_location_id,
        asset_id: file_asset_id,
        storage_root_id: voom_store::test_support::TEST_STORAGE_ROOT_ID,
        provider_relative_locator: locator,
        epoch,
    }
}

#[tokio::test]
async fn promotion_of_a_result_without_a_commit_record_writes_no_placement() {
    let (cp, _db, tmp) = placement_fixture().await;
    let path = tmp.path().join("working/out.mkv");
    let dest_dir = tmp.path().join("output");
    let artifact = recordless_result(&cp, &path).await;

    cp.promote_artifact(&artifact, &path, &dest_dir)
        .await
        .unwrap();

    assert!(dest_dir.join("out.mkv").exists());
    assert!(
        cp.artifacts()
            .get_commit_record_by_result_location(artifact.location_id)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn placement_failure_rolls_back_the_address_repoint() {
    use voom_store::repo::media::artifacts::{CommitPlacementIntent, CommitPlacementState};
    let (cp, _db, tmp) = placement_fixture().await;
    let path = tmp.path().join("working/out.mkv");
    let dest_dir = tmp.path().join("output");
    let artifact = committed_result(&cp, &path, CommitPlacementIntent::Staged).await;
    let before = locator_of(&cp, artifact.location_id).await;
    sqlx::query(
        "CREATE TRIGGER test_refuse_placed BEFORE UPDATE OF placement_state \
         ON artifact_commit_records WHEN NEW.placement_state = 'placed' \
         BEGIN SELECT RAISE(ABORT, 'test refuses placed'); END",
    )
    .execute(&cp.pool)
    .await
    .unwrap();

    cp.promote_artifact(&artifact, &path, &dest_dir)
        .await
        .unwrap_err();

    // The `placed` write and the address repoint share one transaction: the
    // refused write leaves the location where it was. (The bytes already moved:
    // the accepted pre-transaction window, spec Failure model 3.)
    assert_eq!(locator_of(&cp, artifact.location_id).await, before);
    assert_eq!(
        placement_of(&cp, artifact.location_id).await,
        Some(CommitPlacementState::Staged)
    );
}

#[derive(Clone, Default)]
struct LogBuffer(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

struct LogWriter(LogBuffer);

impl std::io::Write for LogWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        (self.0.0)
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'writer> tracing_subscriber::fmt::MakeWriter<'writer> for LogBuffer {
    type Writer = LogWriter;

    fn make_writer(&'writer self) -> Self::Writer {
        LogWriter(self.clone())
    }
}

impl LogBuffer {
    /// Installs this buffer as the current thread's subscriber.
    ///
    /// tracing-core caches each callsite's interest process-wide. While exactly one dispatcher
    /// is registered, it computes that interest from the default subscriber of whichever thread
    /// first reaches the callsite, so a parallel test without a subscriber can cache the warning
    /// as never-enabled and starve this capture (#609). The returned peer dispatcher keeps a
    /// second dispatcher live so registration consults every dispatcher, this capture included.
    fn capture(&self) -> (tracing::subscriber::DefaultGuard, tracing::Dispatch) {
        let interest_peer = tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default());
        let subscriber = tracing_subscriber::fmt()
            .without_time()
            .with_ansi(false)
            .with_writer(self.clone())
            .finish();
        (tracing::subscriber::set_default(subscriber), interest_peer)
    }

    fn text(&self) -> String {
        String::from_utf8(
            self.0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone(),
        )
        .unwrap()
    }
}

/// Run `promote_terminal_artifacts` over `artifact` with one working dir
/// (`<tmp>/working`) paired to `<tmp>/output`, returning the warnings logged.
async fn promote_scoped(
    cp: &ControlPlane,
    tmp: &Path,
    artifact: &WorkingDirArtifact,
) -> (Result<(), VoomError>, String) {
    use crate::cases::policy::compliance::PromotionPair;
    let plan = PromotionPlan {
        pairs: vec![PromotionPair {
            working_dir: tmp.join("working"),
            output_dir: tmp.join("output"),
        }],
    };
    let logs = LogBuffer::default();
    let _capture = logs.capture();
    let result = cp
        .promote_terminal_artifacts(&plan, &[artifact.location_id], tmp, None)
        .await;
    (result, logs.text())
}

async fn unmatched_tip(
    intent: voom_store::repo::media::artifacts::CommitPlacementIntent,
) -> (
    ControlPlane,
    voom_test_support::TempDatabase,
    tempfile::TempDir,
    WorkingDirArtifact,
    PathBuf,
) {
    let (cp, db, tmp) = placement_fixture().await;
    tokio::fs::create_dir_all(tmp.path().join("elsewhere"))
        .await
        .unwrap();
    let path = tmp.path().join("elsewhere/out.mkv");
    let artifact = committed_result(&cp, &path, intent).await;
    (cp, db, tmp, artifact, path)
}

#[tokio::test]
async fn unmatched_staged_tip_warns_once_naming_record_and_location() {
    use voom_store::repo::media::artifacts::CommitPlacementIntent;
    let (cp, _db, tmp, artifact, path) = unmatched_tip(CommitPlacementIntent::Staged).await;
    let record = cp
        .artifacts()
        .get_commit_record_by_result_location(artifact.location_id)
        .await
        .unwrap()
        .unwrap();

    let (result, logs) = promote_scoped(&cp, tmp.path(), &artifact).await;

    result.unwrap();
    assert_eq!(logs.matches("WARN").count(), 1, "{logs}");
    assert!(
        logs.contains(&format!("commit_record={}", record.id)),
        "{logs}"
    );
    assert!(logs.contains(&path.display().to_string()), "{logs}");
    assert!(path.exists(), "the skipped tip stays where it was");
}

#[tokio::test]
async fn unmatched_retained_tip_is_silent() {
    use voom_store::repo::media::artifacts::CommitPlacementIntent;
    let (cp, _db, tmp, artifact, _path) = unmatched_tip(CommitPlacementIntent::Retained).await;

    let (result, logs) = promote_scoped(&cp, tmp.path(), &artifact).await;

    result.unwrap();
    assert!(!logs.contains("WARN"), "{logs}");
}

#[tokio::test]
async fn unmatched_tip_without_a_commit_record_is_silent() {
    let (cp, _db, tmp) = placement_fixture().await;
    let path = tmp.path().join("elsewhere/out.mkv");
    tokio::fs::create_dir_all(tmp.path().join("elsewhere"))
        .await
        .unwrap();
    let artifact = recordless_result(&cp, &path).await;

    let (result, logs) = promote_scoped(&cp, tmp.path(), &artifact).await;

    result.unwrap();
    assert!(!logs.contains("WARN"), "{logs}");
}

#[tokio::test]
async fn resumed_promotion_of_a_placed_tip_is_silent() {
    use voom_store::repo::media::artifacts::{CommitPlacementIntent, CommitPlacementState};
    let (cp, _db, tmp) = placement_fixture().await;
    let path = tmp.path().join("working/out.mkv");
    let artifact = committed_result(&cp, &path, CommitPlacementIntent::Staged).await;

    let (first, first_logs) = promote_scoped(&cp, tmp.path(), &artifact).await;
    let (resume, resume_logs) = promote_scoped(&cp, tmp.path(), &artifact).await;

    first.unwrap();
    resume.unwrap();
    assert!(!first_logs.contains("WARN"), "{first_logs}");
    assert!(!resume_logs.contains("WARN"), "{resume_logs}");
    assert_eq!(
        placement_of(&cp, artifact.location_id).await,
        Some(CommitPlacementState::Placed)
    );
    assert!(tmp.path().join("output/out.mkv").exists());
}
