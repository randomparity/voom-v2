use super::*;

use voom_core::{LeaseId, TicketId};

fn default_output() -> OutputName<'static> {
    OutputName {
        source_path: "Movie.mkv",
        profile_id: "default-hevc",
        codec: "hevc",
        container: "mkv",
    }
}

#[test]
fn target_file_name_includes_profile_identity_codec_and_container() {
    assert_eq!(
        output_file_name(&OutputName {
            source_path: "/lib/Movie.mkv",
            profile_id: "hevc-archive",
            codec: "hevc",
            container: "mkv",
        }),
        "Movie.hevc-archive.hevc.mkv"
    );
    assert_eq!(
        output_file_name(&OutputName {
            source_path: "/lib/Movie.mkv",
            profile_id: "inline-ab12cd34ef56",
            codec: "av1",
            container: "mp4",
        }),
        "Movie.inline-ab12cd34ef56.av1.mp4"
    );
}

#[test]
fn default_hevc_profile_name_produces_expected_file_name() {
    assert_eq!(
        output_file_name(&OutputName {
            source_path: "Movie.mp4",
            profile_id: "default-hevc",
            codec: "hevc",
            container: "mkv",
        }),
        "Movie.default-hevc.hevc.mkv"
    );
}

#[test]
fn profile_identity_is_sanitized_for_filenames() {
    // No path separators or spaces survive into the file name.
    let name = output_file_name(&OutputName {
        source_path: "/lib/Movie.mkv",
        profile_id: "weird/name here",
        codec: "hevc",
        container: "mkv",
    });
    assert!(!name.contains('/'));
    assert!(!name.contains(' '));
    // Slashes and spaces are replaced with dashes.
    assert_eq!(name, "Movie.weird-name-here.hevc.mkv");
}

#[test]
fn two_profiles_same_codec_and_container_produce_distinct_targets() {
    let a = output_file_name(&OutputName {
        source_path: "Movie.mkv",
        profile_id: "hevc-archive",
        codec: "hevc",
        container: "mkv",
    });
    let b = output_file_name(&default_output());
    assert_ne!(a, b);
}

#[test]
fn fallback_stem_when_source_has_no_stem() {
    // An empty source path uses "output" as the stem.
    let name = output_file_name(&OutputName {
        source_path: "",
        profile_id: "default-hevc",
        codec: "hevc",
        container: "mkv",
    });
    assert_eq!(name, "output.default-hevc.hevc.mkv");
}

#[tokio::test]
async fn staging_path_uses_ticket_and_lease_scoped_parent() {
    let dir = stage_tempdir();

    let path = staging_path(dir.path(), TicketId(7), LeaseId(9), &default_output())
        .await
        .unwrap();

    assert!(path.ends_with("ticket-7/lease-9/Movie.default-hevc.hevc.mkv"));
    assert!(path.parent().unwrap().is_dir());
}

#[tokio::test]
async fn staging_path_is_retry_unique_by_lease() {
    let dir = stage_tempdir();

    let first = staging_path(dir.path(), TicketId(7), LeaseId(9), &default_output())
        .await
        .unwrap();
    let second = staging_path(dir.path(), TicketId(7), LeaseId(10), &default_output())
        .await
        .unwrap();

    assert_ne!(first, second);
}

#[tokio::test]
async fn staging_path_rejects_existing_output() {
    let dir = stage_tempdir();
    let first = staging_path(dir.path(), TicketId(7), LeaseId(9), &default_output())
        .await
        .unwrap();
    tokio::fs::write(&first, b"stale").await.unwrap();

    let err = staging_path(dir.path(), TicketId(7), LeaseId(9), &default_output())
        .await
        .unwrap_err();

    assert_eq!(err.error_code(), voom_core::ErrorCode::ConfigInvalid);
    assert!(err.to_string().contains("staging path already exists"));
}

#[cfg(unix)]
#[tokio::test]
async fn staging_path_makes_lease_directory_private() {
    use std::os::unix::fs::PermissionsExt;

    let dir = stage_tempdir();

    let path = staging_path(dir.path(), TicketId(7), LeaseId(9), &default_output())
        .await
        .unwrap();

    let mode = std::fs::metadata(path.parent().unwrap())
        .unwrap()
        .permissions()
        .mode()
        & 0o777;

    assert_eq!(mode, 0o700);
}

#[tokio::test]
async fn target_path_rejects_existing_target_for_same_profile() {
    let dir = stage_tempdir();

    let first = target_path(dir.path(), &default_output()).await.unwrap();
    std::fs::write(&first, b"committed output").unwrap();

    // A second run of the SAME profile (identical source/profile/codec/container)
    // collides with the committed target → CONFIG_INVALID.
    let err = target_path(dir.path(), &default_output())
        .await
        .unwrap_err();

    assert_eq!(err.error_code(), voom_core::ErrorCode::ConfigInvalid);
}

#[cfg(unix)]
#[tokio::test]
async fn staging_root_symlink_is_rejected() {
    let dir = stage_tempdir();
    let real = dir.path().join("real");
    let link = dir.path().join("link");
    std::fs::create_dir(&real).unwrap();
    std::os::unix::fs::symlink(&real, &link).unwrap();

    let err = staging_path(&link, TicketId(7), LeaseId(9), &default_output())
        .await
        .unwrap_err();

    assert_eq!(err.error_code(), voom_core::ErrorCode::ConfigInvalid);
}

#[cfg(unix)]
#[tokio::test]
async fn staging_path_rejects_symlink_ancestor_before_creation() {
    let dir = stage_tempdir();
    let root = dir.path().canonicalize().unwrap();
    let real_parent = root.join("real-parent");
    let link_parent = root.join("link-parent");
    std::fs::create_dir(&real_parent).unwrap();
    std::os::unix::fs::symlink(&real_parent, &link_parent).unwrap();

    let err = staging_path(
        &link_parent.join("stage"),
        TicketId(7),
        LeaseId(9),
        &default_output(),
    )
    .await
    .unwrap_err();

    assert_eq!(err.error_code(), voom_core::ErrorCode::ConfigInvalid);
    assert!(err.to_string().contains("symlink"));
    assert!(!real_parent.join("stage").exists());
}

#[cfg(unix)]
#[tokio::test]
async fn target_path_rejects_dangling_symlink_output() {
    let dir = stage_tempdir();
    let root = dir.path().canonicalize().unwrap();
    let target = root.join("Movie.default-hevc.hevc.mkv");
    std::os::unix::fs::symlink(root.join("missing"), &target).unwrap();

    let err = target_path(&root, &default_output()).await.unwrap_err();

    assert_eq!(err.error_code(), voom_core::ErrorCode::ConfigInvalid);
    assert!(err.to_string().contains("target path already exists"));
}

#[cfg(unix)]
#[tokio::test]
async fn target_path_rejects_symlink_ancestor_before_creation() {
    let dir = stage_tempdir();
    let root = dir.path().canonicalize().unwrap();
    let real = root.join("real");
    let linked = root.join("linked");
    std::fs::create_dir(&real).unwrap();
    std::os::unix::fs::symlink(&real, &linked).unwrap();

    let err = target_path(&linked.join("nested"), &default_output())
        .await
        .unwrap_err();

    assert_eq!(err.error_code(), voom_core::ErrorCode::ConfigInvalid);
    assert!(err.to_string().contains("symlink"));
    assert!(!real.join("nested").exists());
}

fn stage_tempdir() -> tempfile::TempDir {
    tempfile::TempDir::new_in(std::env::current_dir().unwrap()).unwrap()
}
