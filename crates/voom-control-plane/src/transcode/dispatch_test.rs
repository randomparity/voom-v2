use super::*;

use std::ffi::{OsStr, OsString};
use std::path::PathBuf;

use voom_core::{FileAssetId, FileLocationId, FileVersionId};
use voom_store::repo::identity::{FileLocation, FileLocationKind, FileVersion, ProducedBy};
use voom_worker_protocol::{
    TranscodeVideoExpectedFacts, TranscodeVideoInput, TranscodeVideoObservedFacts,
    TranscodeVideoOutput, TranscodeVideoProfile, TranscodeVideoStatus,
};

#[test]
fn default_ffmpeg_worker_command_prefers_current_exe_sibling() {
    let dir = tempfile::tempdir().unwrap();
    let current_exe = dir.path().join("voom");
    let worker = dir.path().join("voom-ffmpeg-worker");
    std::fs::write(&worker, b"").unwrap();

    let command = bundled_ffmpeg_worker_command_from(None, Ok(current_exe));

    assert_eq!(command.program, worker.as_os_str());
    assert_eq!(command.env, Vec::<(OsString, OsString)>::new());
}

#[test]
fn default_ffmpeg_worker_command_searches_profile_dir_from_test_deps_dir() {
    let dir = tempfile::tempdir().unwrap();
    let deps_dir = dir.path().join("deps");
    std::fs::create_dir(&deps_dir).unwrap();
    let current_exe = deps_dir.join("transcode_dispatch_test");
    let worker = dir.path().join("voom-ffmpeg-worker");
    std::fs::write(&worker, b"").unwrap();

    let command = bundled_ffmpeg_worker_command_from(None, Ok(current_exe));

    assert_eq!(command.program, worker.as_os_str());
    assert_eq!(command.env, Vec::<(OsString, OsString)>::new());
}

#[test]
fn default_ffmpeg_worker_command_falls_back_to_path_when_sibling_is_missing() {
    let dir = tempfile::tempdir().unwrap();
    let current_exe = dir.path().join("voom");

    let command = bundled_ffmpeg_worker_command_from(None, Ok(current_exe));

    assert_eq!(command.program, OsStr::new("voom-ffmpeg-worker"));
}

// -- request_for: carries the resolved profile (Task 6.4) --

/// A non-default AV1 1080p profile so a regression that reintroduces a hardcoded
/// `default_hevc()` would change codec/container/dims and fail these assertions.
fn resolved_av1_1080p_mp4() -> ResolvedProfile {
    ResolvedProfile {
        profile: TranscodeVideoProfile {
            name: "av1-1080p".to_owned(),
            target_codec: "av1".to_owned(),
            encoder: "libsvtav1".to_owned(),
            crf: 32,
            preset: "8".to_owned(),
            tune: None,
            codec_profile: None,
            codec_level: None,
            pixel_format: Some("yuv420p".to_owned()),
            max_width: Some(1920),
            max_height: Some(1080),
            copy_compatible: true,
        },
        output_container: "mp4".to_owned(),
    }
}

fn selected_source() -> SelectedSource {
    SelectedSource {
        version: FileVersion {
            id: FileVersionId(1),
            file_asset_id: FileAssetId(1),
            content_hash: "blake3:source".to_owned(),
            size_bytes: 12,
            produced_by: ProducedBy::Ingest,
            produced_from_version_id: None,
            created_at: time::OffsetDateTime::UNIX_EPOCH,
            retired_at: None,
            epoch: 0,
        },
        location: FileLocation {
            id: FileLocationId(1),
            file_version_id: FileVersionId(1),
            kind: FileLocationKind::LocalPath,
            value: "/library/Movie.mkv".to_owned(),
            proof_kind: None,
            proof_value: None,
            observed_at: time::OffsetDateTime::UNIX_EPOCH,
            retired_at: None,
            epoch: 0,
        },
        canonical_path: PathBuf::from("/canonical/library/Movie.mkv"),
    }
}

#[test]
fn request_for_carries_resolved_profile_codec_and_container() {
    let resolved = resolved_av1_1080p_mp4();
    let request = request_for(
        &selected_source(),
        &resolved,
        true,
        Path::new("/tmp/stage"),
        Path::new("/tmp/stage/Movie.av1-1080p.av1.mp4"),
    )
    .unwrap();

    // The dispatched request must carry the RESOLVED profile verbatim, not a
    // hardcoded default.
    assert_eq!(request.profile, resolved.profile);
    assert_eq!(request.input.path, "/canonical/library/Movie.mkv");
    assert_eq!(request.output.container, "mp4");
    assert_eq!(request.output.video_codec, "av1");
    assert!(request.copy_video);
}

// -- validate_output_facts: per-branch coverage (Task 6.4) --

fn capped_request() -> TranscodeVideoRequest {
    let resolved = resolved_av1_1080p_mp4();
    TranscodeVideoRequest {
        input: TranscodeVideoInput {
            path: "/library/Movie.mkv".to_owned(),
            expected: TranscodeVideoExpectedFacts {
                size_bytes: 12,
                content_hash: "blake3:source".to_owned(),
                modified_at: None,
                local_file_key: None,
            },
        },
        output: TranscodeVideoOutput {
            staging_root: "/tmp/stage".to_owned(),
            path: "/tmp/stage/Movie.av1-1080p.av1.mp4".to_owned(),
            container: resolved.output_container.clone(),
            video_codec: resolved.profile.target_codec.clone(),
            overwrite: false,
        },
        profile: resolved.profile,
        copy_video: false,
    }
}

fn conforming_result() -> TranscodeVideoResult {
    let facts = TranscodeVideoObservedFacts {
        size_bytes: 12,
        content_hash: "blake3:source".to_owned(),
        modified_at: None,
        local_file_key: None,
    };
    TranscodeVideoResult {
        status: TranscodeVideoStatus::Transcoded,
        provider: "ffmpeg".to_owned(),
        provider_version: "test".to_owned(),
        input_pre: facts.clone(),
        input_post: facts,
        output: TranscodeVideoObservedFacts {
            size_bytes: 10,
            content_hash: "blake3:output".to_owned(),
            modified_at: None,
            local_file_key: None,
        },
        output_container: "mp4".to_owned(),
        output_video_codec: "av1".to_owned(),
        output_width: 1920,
        output_height: 1080,
        output_pixel_format: "yuv420p".to_owned(),
        copied_video: false,
    }
}

#[test]
fn validate_output_facts_accepts_conforming_result() {
    assert!(validate_output_facts(&capped_request(), &conforming_result()).is_ok());
}

#[test]
fn validate_output_facts_rejects_width_over_cap() {
    let mut result = conforming_result();
    result.output_width = 3840; // exceeds max_width 1920
    let err = validate_output_facts(&capped_request(), &result).unwrap_err();
    assert_eq!(
        err.error_code(),
        voom_core::ErrorCode::MalformedWorkerResult
    );
}

#[test]
fn validate_output_facts_rejects_height_over_cap() {
    let mut result = conforming_result();
    result.output_height = 2160; // exceeds max_height 1080
    let err = validate_output_facts(&capped_request(), &result).unwrap_err();
    assert_eq!(
        err.error_code(),
        voom_core::ErrorCode::MalformedWorkerResult
    );
}

#[test]
fn validate_output_facts_rejects_pixel_format_mismatch() {
    let mut result = conforming_result();
    result.output_pixel_format = "yuv420p10le".to_owned(); // profile constrains yuv420p
    let err = validate_output_facts(&capped_request(), &result).unwrap_err();
    assert_eq!(
        err.error_code(),
        voom_core::ErrorCode::MalformedWorkerResult
    );
}

#[test]
fn validate_output_facts_rejects_copied_video_disagreement() {
    let mut result = conforming_result();
    result.copied_video = true; // request copy_video=false
    let err = validate_output_facts(&capped_request(), &result).unwrap_err();
    assert_eq!(
        err.error_code(),
        voom_core::ErrorCode::MalformedWorkerResult
    );
}
