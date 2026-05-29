use super::*;

use voom_core::{LeaseId, TicketId};

#[test]
fn target_file_name_includes_profile_identity_codec_and_container() {
    assert_eq!(
        output_file_name("/lib/Movie.mkv", "hevc-archive", "hevc", "mkv"),
        "Movie.hevc-archive.hevc.mkv"
    );
    assert_eq!(
        output_file_name("/lib/Movie.mkv", "inline-ab12cd34ef56", "av1", "mp4"),
        "Movie.inline-ab12cd34ef56.av1.mp4"
    );
}

#[test]
fn default_hevc_profile_name_produces_expected_file_name() {
    assert_eq!(
        output_file_name("Movie.mp4", "default-hevc", "hevc", "mkv"),
        "Movie.default-hevc.hevc.mkv"
    );
}

#[test]
fn profile_identity_is_sanitized_for_filenames() {
    // No path separators or spaces survive into the file name.
    let name = output_file_name("/lib/Movie.mkv", "weird/name here", "hevc", "mkv");
    assert!(!name.contains('/'));
    assert!(!name.contains(' '));
    // Slashes and spaces are replaced with dashes.
    assert_eq!(name, "Movie.weird-name-here.hevc.mkv");
}

#[test]
fn two_profiles_same_codec_and_container_produce_distinct_targets() {
    let a = output_file_name("Movie.mkv", "hevc-archive", "hevc", "mkv");
    let b = output_file_name("Movie.mkv", "default-hevc", "hevc", "mkv");
    assert_ne!(a, b);
}

#[test]
fn fallback_stem_when_source_has_no_stem() {
    // An empty source path uses "output" as the stem.
    let name = output_file_name("", "default-hevc", "hevc", "mkv");
    assert_eq!(name, "output.default-hevc.hevc.mkv");
}

#[tokio::test]
async fn staging_path_uses_ticket_and_lease_scoped_parent() {
    let dir = tempfile::TempDir::new().unwrap();

    let path = staging_path(
        dir.path(),
        TicketId(7),
        LeaseId(9),
        "Movie.mkv",
        "default-hevc",
        "hevc",
        "mkv",
    )
    .await
    .unwrap();

    assert!(path.ends_with("ticket-7/lease-9/Movie.default-hevc.hevc.mkv"));
    assert!(path.parent().unwrap().is_dir());
}

#[tokio::test]
async fn staging_path_is_retry_unique_by_lease() {
    let dir = tempfile::TempDir::new().unwrap();

    let first = staging_path(
        dir.path(),
        TicketId(7),
        LeaseId(9),
        "Movie.mkv",
        "default-hevc",
        "hevc",
        "mkv",
    )
    .await
    .unwrap();
    let second = staging_path(
        dir.path(),
        TicketId(7),
        LeaseId(10),
        "Movie.mkv",
        "default-hevc",
        "hevc",
        "mkv",
    )
    .await
    .unwrap();

    assert_ne!(first, second);
}

#[tokio::test]
async fn existing_ticket_lease_parent_is_rejected() {
    let dir = tempfile::TempDir::new().unwrap();
    let _first = staging_path(
        dir.path(),
        TicketId(7),
        LeaseId(9),
        "Movie.mkv",
        "default-hevc",
        "hevc",
        "mkv",
    )
    .await
    .unwrap();

    let err = staging_path(
        dir.path(),
        TicketId(7),
        LeaseId(9),
        "Movie.mkv",
        "default-hevc",
        "hevc",
        "mkv",
    )
    .await
    .unwrap_err();

    assert_eq!(err.error_code(), voom_core::ErrorCode::ConfigInvalid);
}

#[tokio::test]
async fn target_path_rejects_existing_target_for_same_profile() {
    let dir = tempfile::TempDir::new().unwrap();

    let first = target_path(dir.path(), "Movie.mkv", "default-hevc", "hevc", "mkv")
        .await
        .unwrap();
    std::fs::write(&first, b"committed output").unwrap();

    // A second run of the SAME profile (identical source/profile/codec/container)
    // collides with the committed target → CONFIG_INVALID.
    let err = target_path(dir.path(), "Movie.mkv", "default-hevc", "hevc", "mkv")
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

    let err = staging_path(
        &link,
        TicketId(7),
        LeaseId(9),
        "Movie.mkv",
        "default-hevc",
        "hevc",
        "mkv",
    )
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
        "default-hevc",
        "hevc",
        "mkv",
    )
    .await
    .unwrap();

    let canonical_real_parent = std::fs::canonicalize(&real_parent).unwrap();
    assert!(path.ends_with("ticket-7/lease-9/Movie.default-hevc.hevc.mkv"));
    assert!(path.starts_with(canonical_real_parent));
}
