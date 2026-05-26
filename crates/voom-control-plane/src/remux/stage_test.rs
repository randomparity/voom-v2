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
