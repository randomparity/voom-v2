use std::path::Path;

use super::*;

#[tokio::test]
async fn observe_file_facts_reports_size_hash_and_mtime_for_regular_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("artifact.bin");
    tokio::fs::write(&path, b"verified bytes").await.unwrap();

    let observed = observe_file_facts(&path).await.unwrap();

    assert_eq!(observed.size_bytes, 14);
    assert_eq!(
        observed.content_hash,
        format!("blake3:{}", blake3::hash(b"verified bytes").to_hex())
    );
    assert!(observed.modified_at.is_some());
    assert_eq!(observed.local_file_key, None);
}

#[tokio::test]
async fn observe_file_facts_rejects_missing_paths() {
    let dir = tempfile::tempdir().unwrap();

    let err = observe_file_facts(&dir.path().join("missing.bin"))
        .await
        .unwrap_err();

    assert!(err.to_string().contains("artifact unavailable"));
}

#[tokio::test]
async fn observe_file_facts_rejects_non_regular_files_without_following_symlinks() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("target.bin");
    tokio::fs::write(&target, b"target").await.unwrap();
    let link = dir.path().join("link.bin");
    make_symlink(&target, &link);

    let err = observe_file_facts(&link).await.unwrap_err();

    assert!(err.to_string().contains("not a regular file"));
}

#[cfg(unix)]
#[tokio::test]
async fn no_follow_open_rejects_leaf_symlink_after_path_check() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("target.bin");
    tokio::fs::write(&target, b"target").await.unwrap();
    let link = dir.path().join("link.bin");
    make_symlink(&target, &link);

    let err = open_regular_file_no_follow(&link).await.unwrap_err();

    assert!(err.to_string().contains("artifact unavailable"));
}

#[cfg(unix)]
fn make_symlink(target: &Path, link: &Path) {
    std::os::unix::fs::symlink(target, link).unwrap();
}

#[cfg(windows)]
fn make_symlink(target: &Path, link: &Path) {
    std::os::windows::fs::symlink_file(target, link).unwrap();
}
