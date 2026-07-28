use super::*;

use std::path::Path;

use voom_core::{ErrorCode, LeaseId, TicketId};
use voom_plan::audio::{AudioBundleRole, AudioDispositionFact, SnapshotAudioStreamFact};

use super::super::selection::{ExtractAudioSelectionOutput, ExtractAudioSelectionPlan};

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
    let selection = legacy_selection("stream:audio/1");

    let targets = extract_target_paths(root.path(), Path::new("/library/Movie.mp4"), &selection)
        .await
        .unwrap();

    assert!(targets[0].ends_with("Movie.stream-audio-1.opus.ogg"));
}

#[tokio::test]
async fn extraction_target_ignores_title_language_and_provider_index() {
    let root = stage_tempdir();
    let selection = legacy_selection("sid");

    let targets = extract_target_paths(root.path(), Path::new("/library/Movie.mp4"), &selection)
        .await
        .unwrap();
    let target = &targets[0];

    assert!(target.ends_with("Movie.sid.opus.ogg"));
    assert!(!target.to_string_lossy().contains("English"));
    assert!(!target.to_string_lossy().contains("Commentary"));
    assert!(!target.to_string_lossy().contains(".7."));
}

#[tokio::test]
async fn planned_extraction_paths_preserve_suffix_order_and_use_operation_generation() {
    let root = stage_tempdir();
    let target_dir = root.path().join("targets");
    let staging_root = root.path().join("staging");
    let selection = plural_selection(["main.opus.ogg", "alt.opus.ogg"]);

    let targets = extract_target_paths(&target_dir, Path::new("/library/Movie.mp4"), &selection)
        .await
        .unwrap();
    let staging = prepare_extract_staging_paths(&staging_root, "abc123", 4, &targets)
        .await
        .unwrap();

    assert!(targets[0].ends_with("Movie.main.opus.ogg"));
    assert!(targets[1].ends_with("Movie.alt.opus.ogg"));
    assert!(staging.paths[0].ends_with("operation-abc123/generation-4/Movie.main.opus.ogg"));
    assert!(staging.paths[1].ends_with("operation-abc123/generation-4/Movie.alt.opus.ogg"));
}

#[tokio::test]
async fn planned_extraction_targets_are_isolated_by_operation() {
    let root = stage_tempdir();
    let mut first = plural_selection(["same.opus.ogg"]);
    first.operation_id = Some("node_extract_first".to_owned());
    let mut second = first.clone();
    second.operation_id = Some("node_extract_second".to_owned());

    let first_targets = extract_target_paths(root.path(), Path::new("/library/Movie.mp4"), &first)
        .await
        .unwrap();
    let second_targets =
        extract_target_paths(root.path(), Path::new("/library/Movie.mp4"), &second)
            .await
            .unwrap();

    assert_ne!(first_targets, second_targets);
    assert!(first_targets[0].ends_with("operation-node_extract_first/Movie.same.opus.ogg"));
    assert!(second_targets[0].ends_with("operation-node_extract_second/Movie.same.opus.ogg"));
}

#[tokio::test]
async fn planned_extraction_path_collision_fails_before_creating_directories() {
    let root = stage_tempdir();
    let target_dir = root.path().join("targets");
    let selection = plural_selection(["same.opus.ogg", "same.opus.ogg"]);

    let error = extract_target_paths(&target_dir, Path::new("/library/Movie.mp4"), &selection)
        .await
        .unwrap_err();

    assert_eq!(error.error_code(), ErrorCode::ConfigInvalid);
    assert!(error.to_string().contains("collision"));
    assert!(!target_dir.exists());
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

#[cfg(unix)]
#[tokio::test]
async fn staging_path_rejects_ticket_parent_symlink_before_creation() {
    let root = stage_tempdir();
    let root_path = root.path().canonicalize().unwrap();
    let real_parent = root_path.join("real-ticket");
    let ticket_link = root_path.join("ticket-10");
    std::fs::create_dir(&real_parent).unwrap();
    std::os::unix::fs::symlink(&real_parent, &ticket_link).unwrap();

    let err = prepare_transcode_staging_path(
        &root_path,
        TicketId(10),
        LeaseId(20),
        Path::new("/library/Movie.mp4"),
        "aac",
    )
    .await
    .unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::ConfigInvalid);
    assert!(err.to_string().contains("must not traverse a symlink"));
    assert!(!real_parent.join("lease-20").exists());
}

#[cfg(unix)]
#[tokio::test]
async fn staging_path_rejects_lease_parent_symlink_before_creation() {
    let root = stage_tempdir();
    let root_path = root.path().canonicalize().unwrap();
    let ticket_parent = root_path.join("ticket-10");
    let real_parent = root_path.join("real-lease");
    let lease_link = ticket_parent.join("lease-20");
    std::fs::create_dir(&ticket_parent).unwrap();
    std::fs::create_dir(&real_parent).unwrap();
    std::os::unix::fs::symlink(&real_parent, &lease_link).unwrap();

    let err = prepare_transcode_staging_path(
        &root_path,
        TicketId(10),
        LeaseId(20),
        Path::new("/library/Movie.mp4"),
        "aac",
    )
    .await
    .unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::ConfigInvalid);
    assert!(err.to_string().contains("must not traverse a symlink"));
}

#[cfg(unix)]
#[tokio::test]
async fn private_mode_verification_rejects_group_accessible_directory() {
    use std::os::unix::fs::PermissionsExt;

    let root = stage_tempdir();
    let path = root.path().join("public");
    std::fs::create_dir(&path).unwrap();
    let mut permissions = std::fs::metadata(&path).unwrap().permissions();
    permissions.set_mode(0o750);
    std::fs::set_permissions(&path, permissions).unwrap();

    let err = verify_private_dir_mode(&path, "audio staging root")
        .await
        .unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::ConfigInvalid);
    assert!(err.to_string().contains("must be private"));
}

fn stage_tempdir() -> tempfile::TempDir {
    tempfile::TempDir::new_in(std::env::current_dir().unwrap()).unwrap()
}

fn plural_selection<const N: usize>(suffixes: [&str; N]) -> ExtractAudioSelectionPlan {
    ExtractAudioSelectionPlan {
        operation_id: Some("node_extract_audio_test".to_owned()),
        outputs: suffixes
            .into_iter()
            .enumerate()
            .map(|(index, suffix)| ExtractAudioSelectionOutput {
                output_id: Some(format!("output-{index}")),
                name_suffix: Some(suffix.to_owned()),
                stream: voom_worker_protocol::AudioStreamRef {
                    snapshot_stream_id: format!("a-{index}"),
                    provider_stream_index: u32::try_from(index).unwrap(),
                },
                source: SnapshotAudioStreamFact {
                    snapshot_stream_id: format!("a-{index}"),
                    provider_stream_index: u32::try_from(index).unwrap(),
                    codec: Some("aac".to_owned()),
                    language: None,
                    title: None,
                    channels: Some(2),
                    default: false,
                    disposition: AudioDispositionFact {
                        default: false,
                        forced: false,
                        commentary: Some(false),
                    },
                    commentary: Some(false),
                },
                role: AudioBundleRole::ExternalAudio,
            })
            .collect(),
        target_codec: "opus".to_owned(),
        container: "ogg".to_owned(),
    }
}

fn legacy_selection(snapshot_stream_id: &str) -> ExtractAudioSelectionPlan {
    let mut selection = plural_selection(["unused"]);
    selection.operation_id = None;
    selection.outputs[0].output_id = None;
    selection.outputs[0].name_suffix = None;
    selection.outputs[0].stream.snapshot_stream_id = snapshot_stream_id.to_owned();
    selection
}
