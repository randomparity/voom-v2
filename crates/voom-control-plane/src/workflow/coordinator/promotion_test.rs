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
async fn copy_into_place_moves_bytes_and_cleans_up() {
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
    copy_into_place(&current, &dest, &temp).await.unwrap();

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
async fn interrupted_intermediate_cleanup_retires_a_location_after_file_is_already_gone() {
    use voom_store::repo::identity::{
        DiscoveredFile, FileLocationKind, IdentityRepo, IngestOutcome,
    };

    let (cp, _db) = crate::cases::cp().await;
    let tmp = tempfile::TempDir::new().unwrap();
    let path = tmp.path().join("intermediate.mkv");
    write(&path, b"intermediate").await;
    let IngestOutcome::NewFileAsset {
        file_location_id, ..
    } = cp
        .record_discovered_file(
            DiscoveredFile {
                location_kind: FileLocationKind::LocalPath,
                location_value: path.display().to_string(),
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

    cp.reclaim_intermediate_location(&location).await.unwrap();

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
    use voom_store::repo::identity::{
        DiscoveredFile, FileLocationKind, IdentityRepo, IngestOutcome,
    };

    let (cp, _db) = crate::cases::cp().await;
    let tmp = tempfile::TempDir::new().unwrap();
    let path = tmp.path().join("not-a-file");
    tokio::fs::create_dir(&path).await.unwrap();
    let IngestOutcome::NewFileAsset {
        file_location_id, ..
    } = cp
        .record_discovered_file(
            DiscoveredFile {
                location_kind: FileLocationKind::LocalPath,
                location_value: path.display().to_string(),
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

    cp.reclaim_intermediate_location(&location)
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
