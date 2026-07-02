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

    copy_into_place(&current, &dest).await.unwrap();

    assert_eq!(tokio::fs::read(&dest).await.unwrap(), b"terminal-bytes");
    assert!(tokio::fs::symlink_metadata(&current).await.is_err());
    let temp = dest.with_file_name(".voom-promote.Movie.hevc.mkv.partial");
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

    let returned = move_terminal_artifact(&current, &dest).await.unwrap();

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

    let err = move_terminal_artifact(&current, &dest).await.unwrap_err();

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

    let err = move_terminal_artifact(&current, &dest).await.unwrap_err();

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

    let err = move_terminal_artifact(&current, &dest).await.unwrap_err();

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

    let returned = move_terminal_artifact(&current, &dest).await.unwrap();

    assert_eq!(returned, dest);
    assert_eq!(tokio::fs::read(&dest).await.unwrap(), b"terminal-bytes");
}

#[tokio::test]
async fn normal_move_dest_absent_places_and_removes_source() {
    let tmp = tempfile::TempDir::new().unwrap();
    let current = tmp.path().join("Movie.work.mkv");
    let dest = tmp.path().join("Movie.mkv");
    write(&current, b"terminal-bytes").await;

    let returned = move_terminal_artifact(&current, &dest).await.unwrap();

    assert_eq!(returned, dest);
    assert!(tokio::fs::symlink_metadata(&current).await.is_err());
    assert_eq!(tokio::fs::read(&dest).await.unwrap(), b"terminal-bytes");
}
