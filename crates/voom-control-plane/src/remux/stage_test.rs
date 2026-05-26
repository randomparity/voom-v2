use super::*;

use std::path::Path;

use voom_core::{ErrorCode, LeaseId, TicketId};

#[tokio::test]
async fn staging_path_includes_ticket_and_lease() {
    let root = tempfile::tempdir().unwrap();
    let source = Path::new("/library/Movie.mp4");

    let path = staging_path(root.path(), TicketId(10), LeaseId(20), source)
        .await
        .unwrap();

    assert!(path.starts_with(root.path().canonicalize().unwrap()));
    assert!(path.to_string_lossy().contains("ticket-10"));
    assert!(path.to_string_lossy().contains("lease-20"));
    assert!(path.ends_with("Movie.remux.mkv"));
}

#[tokio::test]
async fn staging_path_rejects_existing_output() {
    let root = tempfile::tempdir().unwrap();
    let path = staging_path(
        root.path(),
        TicketId(10),
        LeaseId(20),
        Path::new("/library/Movie.mp4"),
    )
    .await
    .unwrap();
    tokio::fs::write(&path, b"stale").await.unwrap();

    let err = staging_path(
        root.path(),
        TicketId(10),
        LeaseId(20),
        Path::new("/library/Movie.mp4"),
    )
    .await
    .unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::ConfigInvalid);
    assert!(err.to_string().contains("staging path already exists"));
}

#[tokio::test]
async fn target_path_rejects_existing_output() {
    let root = tempfile::tempdir().unwrap();
    let target = root.path().join("Movie.remux.mkv");
    tokio::fs::write(&target, b"existing").await.unwrap();

    let err = target_path(root.path(), Path::new("/library/Movie.mp4"))
        .await
        .unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::ConfigInvalid);
    assert!(err.to_string().contains("target path already exists"));
}

#[cfg(unix)]
#[tokio::test]
async fn staging_path_rejects_dangling_symlink_output() {
    let root = tempfile::tempdir().unwrap();
    let path = staging_path(
        root.path(),
        TicketId(10),
        LeaseId(20),
        Path::new("/library/Movie.mp4"),
    )
    .await
    .unwrap();
    std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(root.path().join("missing"), &path).unwrap();

    let err = staging_path(
        root.path(),
        TicketId(10),
        LeaseId(20),
        Path::new("/library/Movie.mp4"),
    )
    .await
    .unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::ConfigInvalid);
    assert!(err.to_string().contains("staging path already exists"));
}

#[cfg(unix)]
#[tokio::test]
async fn staging_path_rejects_symlink_ancestor_before_creation() {
    let root = tempfile::tempdir().unwrap();
    let real = root.path().join("real");
    let linked = root.path().join("linked");
    std::fs::create_dir(&real).unwrap();
    std::os::unix::fs::symlink(&real, &linked).unwrap();

    let err = staging_path(
        &linked.join("nested"),
        TicketId(10),
        LeaseId(20),
        Path::new("/library/Movie.mp4"),
    )
    .await
    .unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::ConfigInvalid);
    assert!(err.to_string().contains("symlink"));
    assert!(!real.join("nested").exists());
}

#[cfg(unix)]
#[tokio::test]
async fn staging_path_rejects_lexical_symlink_escape_after_missing_component() {
    let root = tempfile::tempdir().unwrap();
    let real = root.path().join("real");
    let linked = root.path().join("linked");
    let path = root.path().join("missing/../linked/nested");
    std::fs::create_dir(&real).unwrap();
    std::os::unix::fs::symlink(&real, &linked).unwrap();

    let err = staging_path(
        &path,
        TicketId(10),
        LeaseId(20),
        Path::new("/library/Movie.mp4"),
    )
    .await
    .unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::ConfigInvalid);
    assert!(err.to_string().contains("symlink"));
    assert!(!real.join("nested").exists());
    assert!(!root.path().join("missing").exists());
}

#[cfg(unix)]
#[tokio::test]
async fn target_path_rejects_dangling_symlink_output() {
    let root = tempfile::tempdir().unwrap();
    let target = root.path().join("Movie.remux.mkv");
    std::os::unix::fs::symlink(root.path().join("missing"), &target).unwrap();

    let err = target_path(root.path(), Path::new("/library/Movie.mp4"))
        .await
        .unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::ConfigInvalid);
    assert!(err.to_string().contains("target path already exists"));
}

#[cfg(unix)]
#[tokio::test]
async fn target_path_rejects_symlink_ancestor_before_creation() {
    let root = tempfile::tempdir().unwrap();
    let real = root.path().join("real");
    let linked = root.path().join("linked");
    std::fs::create_dir(&real).unwrap();
    std::os::unix::fs::symlink(&real, &linked).unwrap();

    let err = target_path(&linked.join("nested"), Path::new("/library/Movie.mp4"))
        .await
        .unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::ConfigInvalid);
    assert!(err.to_string().contains("symlink"));
    assert!(!real.join("nested").exists());
}

#[cfg(unix)]
#[tokio::test]
async fn target_path_rejects_lexical_symlink_escape_after_missing_component() {
    let root = tempfile::tempdir().unwrap();
    let real = root.path().join("real");
    let linked = root.path().join("linked");
    let path = root.path().join("missing/../linked/nested");
    std::fs::create_dir(&real).unwrap();
    std::os::unix::fs::symlink(&real, &linked).unwrap();

    let err = target_path(&path, Path::new("/library/Movie.mp4"))
        .await
        .unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::ConfigInvalid);
    assert!(err.to_string().contains("symlink"));
    assert!(!real.join("nested").exists());
    assert!(!root.path().join("missing").exists());
}
