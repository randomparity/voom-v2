use super::*;

use voom_core::{LeaseId, TicketId};

#[tokio::test]
async fn staging_path_uses_ticket_and_lease_scoped_parent() {
    let dir = tempfile::TempDir::new().unwrap();

    let path = staging_path(dir.path(), TicketId(7), LeaseId(9), "Movie.mkv")
        .await
        .unwrap();

    assert!(path.ends_with("ticket-7/lease-9/Movie.hevc.mkv"));
    assert!(path.parent().unwrap().is_dir());
}

#[tokio::test]
async fn staging_path_is_retry_unique_by_lease() {
    let dir = tempfile::TempDir::new().unwrap();

    let first = staging_path(dir.path(), TicketId(7), LeaseId(9), "Movie.mkv")
        .await
        .unwrap();
    let second = staging_path(dir.path(), TicketId(7), LeaseId(10), "Movie.mkv")
        .await
        .unwrap();

    assert_ne!(first, second);
}

#[tokio::test]
async fn existing_ticket_lease_parent_is_rejected() {
    let dir = tempfile::TempDir::new().unwrap();
    let _first = staging_path(dir.path(), TicketId(7), LeaseId(9), "Movie.mkv")
        .await
        .unwrap();

    let err = staging_path(dir.path(), TicketId(7), LeaseId(9), "Movie.mkv")
        .await
        .unwrap_err();

    assert_eq!(err.error_code(), voom_core::ErrorCode::ConfigInvalid);
}

#[cfg(unix)]
#[tokio::test]
async fn staging_root_symlink_is_rejected() {
    let dir = tempfile::TempDir::new().unwrap();
    let real = dir.path().join("real");
    let link = dir.path().join("link");
    std::fs::create_dir(&real).unwrap();
    std::os::unix::fs::symlink(&real, &link).unwrap();

    let err = staging_path(&link, TicketId(7), LeaseId(9), "Movie.mkv")
        .await
        .unwrap_err();

    assert_eq!(err.error_code(), voom_core::ErrorCode::ConfigInvalid);
}

#[cfg(unix)]
#[tokio::test]
async fn staging_root_may_have_symlink_ancestor() {
    let dir = tempfile::TempDir::new().unwrap();
    let real_parent = dir.path().join("real-parent");
    let link_parent = dir.path().join("link-parent");
    std::fs::create_dir(&real_parent).unwrap();
    std::os::unix::fs::symlink(&real_parent, &link_parent).unwrap();

    let path = staging_path(
        &link_parent.join("stage"),
        TicketId(7),
        LeaseId(9),
        "Movie.mkv",
    )
    .await
    .unwrap();

    let canonical_real_parent = std::fs::canonicalize(&real_parent).unwrap();
    assert!(path.ends_with("ticket-7/lease-9/Movie.hevc.mkv"));
    assert!(path.starts_with(canonical_real_parent));
}
