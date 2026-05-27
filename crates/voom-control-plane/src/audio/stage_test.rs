use super::*;

use std::path::Path;

use voom_core::{ErrorCode, LeaseId, TicketId};

#[tokio::test]
async fn staging_path_includes_ticket_and_lease_under_canonical_root() {
    let root = stage_tempdir();
    let root_path = root.path().canonicalize().unwrap();

    let staging = prepare_transcode_staging_path(
        &root_path,
        TicketId(10),
        LeaseId(20),
        Path::new("/library/Movie.mp4"),
        "aac",
    )
    .await
    .unwrap();

    assert_eq!(staging.canonical_root, root_path);
    assert!(staging.path.starts_with(&staging.canonical_root));
    assert!(staging.path.to_string_lossy().contains("ticket-10"));
    assert!(staging.path.to_string_lossy().contains("lease-20"));
}

#[tokio::test]
async fn transcode_target_is_source_stem_audio_codec_mkv() {
    let root = stage_tempdir();

    let target = transcode_target_path(root.path(), Path::new("/library/Movie.mp4"), "opus")
        .await
        .unwrap();

    assert!(target.ends_with("Movie.audio-opus.mkv"));
}

#[tokio::test]
async fn extraction_target_is_source_stem_sanitized_stream_id_codec_ogg() {
    let root = stage_tempdir();

    let target = extract_target_path(
        root.path(),
        Path::new("/library/Movie.mp4"),
        "stream:audio/1",
        "opus",
    )
    .await
    .unwrap();

    assert!(target.ends_with("Movie.stream-audio-1.opus.ogg"));
}

#[tokio::test]
async fn extraction_target_ignores_title_language_and_provider_index() {
    let root = stage_tempdir();

    let target = extract_target_path(root.path(), Path::new("/library/Movie.mp4"), "sid", "opus")
        .await
        .unwrap();

    assert!(target.ends_with("Movie.sid.opus.ogg"));
    assert!(!target.to_string_lossy().contains("English"));
    assert!(!target.to_string_lossy().contains("Commentary"));
    assert!(!target.to_string_lossy().contains(".7."));
}

#[tokio::test]
async fn existing_staging_and_target_paths_fail_with_config_invalid() {
    let root = stage_tempdir();
    let staging = prepare_transcode_staging_path(
        root.path(),
        TicketId(10),
        LeaseId(20),
        Path::new("/library/Movie.mp4"),
        "aac",
    )
    .await
    .unwrap();
    tokio::fs::write(&staging.path, b"stale").await.unwrap();

    let err = prepare_transcode_staging_path(
        root.path(),
        TicketId(10),
        LeaseId(20),
        Path::new("/library/Movie.mp4"),
        "aac",
    )
    .await
    .unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::ConfigInvalid);

    let target_dir = stage_tempdir();
    let target = target_dir.path().join("Movie.audio-aac.mkv");
    tokio::fs::write(&target, b"existing").await.unwrap();

    let err = transcode_target_path(target_dir.path(), Path::new("/library/Movie.mp4"), "aac")
        .await
        .unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::ConfigInvalid);
}

fn stage_tempdir() -> tempfile::TempDir {
    tempfile::TempDir::new_in(std::env::current_dir().unwrap()).unwrap()
}
