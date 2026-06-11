use std::path::{Path, PathBuf};

use voom_core::{ErrorCode, FailureClass, LeaseId};
use voom_worker_protocol::{
    AudioExpectedFacts, AudioStreamRef, ExtractAudioInput, ExtractAudioOutput, ExtractAudioRequest,
    OperationDispatch, OperationFuture, OperationKind, OperationRequest, ProgressFrame,
    ProtocolError, TranscodeAudioInput, TranscodeAudioOutput, TranscodeAudioRequest,
    TranscodeAudioSelection, TranscodeAudioSettings, TranscodeVideoExpectedFacts,
    TranscodeVideoInput, TranscodeVideoOutput, TranscodeVideoProfile, TranscodeVideoRequest,
};

use crate::DEFAULT_PROCESS_TIMEOUT;

use super::*;

#[tokio::test]
async fn missing_input_is_artifact_unavailable() {
    let dir = tempfile::tempdir().unwrap();
    let request = request(dir.path(), &dir.path().join("missing.mkv")).await;

    let err = handle_transcode_video(&request, &config(dir.path()))
        .await
        .unwrap_err();

    assert_eq!(err.failure_class(), FailureClass::ArtifactUnavailable);
}

#[tokio::test]
async fn output_path_escape_is_config_invalid() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.mkv");
    tokio::fs::write(&input, b"input").await.unwrap();
    let mut request = request(dir.path(), &input).await;
    request.output.path = dir
        .path()
        .join("../escape.mkv")
        .to_string_lossy()
        .into_owned();

    let err = handle_transcode_video(&request, &config(dir.path()))
        .await
        .unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::ConfigInvalid);
}

#[tokio::test]
async fn dash_leading_input_path_is_config_invalid_not_unavailable() {
    // M14: a path beginning with '-' is parsed by ffmpeg as an option, not a
    // filename. The input path is not staging-validated (only existence-checked),
    // so without an explicit guard a leading-'-' input would slip through as a
    // missing file (ArtifactUnavailable) rather than a rejected contract.
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.mkv");
    tokio::fs::write(&input, b"input").await.unwrap();
    let mut request = request(dir.path(), &input).await;
    request.input.path = "-injected.mkv".to_owned();

    let err = handle_transcode_video(&request, &config(dir.path()))
        .await
        .unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::ConfigInvalid);
}

#[test]
fn reject_option_like_path_flags_only_leading_dash() {
    assert!(reject_option_like_path("p", Path::new("-foo.mkv")).is_err());
    assert!(reject_option_like_path("p", Path::new("--")).is_err());
    // An absolute staging path (the normal case) begins with '/', and a
    // leading-'-' component *inside* an absolute path is harmless because the
    // whole arg no longer begins with '-'.
    assert!(reject_option_like_path("p", Path::new("/stage/-foo.mkv")).is_ok());
    assert!(reject_option_like_path("p", Path::new("/stage/out.mkv")).is_ok());
    assert!(reject_option_like_path("p", Path::new("out.mkv")).is_ok());
}

#[cfg(unix)]
#[tokio::test]
async fn existing_video_output_symlink_is_config_invalid() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.mkv");
    tokio::fs::write(&input, b"input").await.unwrap();
    let request = request(dir.path(), &input).await;
    std::os::unix::fs::symlink(dir.path().join("missing-target.mkv"), &request.output.path)
        .unwrap();

    let err = handle_transcode_video(&request, &config(dir.path()))
        .await
        .unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::ConfigInvalid);
    assert!(err.to_string().contains("already exists"));
}

#[tokio::test]
async fn unsupported_output_contract_is_rejected_before_ffmpeg() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.mkv");
    tokio::fs::write(&input, b"input").await.unwrap();
    let mut request = request(dir.path(), &input).await;
    // mp4 is now supported; use avi which is not supported
    request.output.container = "avi".to_owned();

    let err = handle_transcode_video(&request, &config(dir.path()))
        .await
        .unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::ConfigInvalid);
    assert!(!tokio::fs::try_exists(&request.output.path).await.unwrap());
}

#[tokio::test]
async fn unsupported_profile_contract_is_rejected_before_ffmpeg() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.mkv");
    tokio::fs::write(&input, b"input").await.unwrap();
    let mut request = request(dir.path(), &input).await;
    // libx264 is not a recognized encoder — descriptor validation rejects it
    request.profile.encoder = "libx264".to_owned();

    let err = handle_transcode_video(&request, &config(dir.path()))
        .await
        .unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::ConfigInvalid);
    let message = err.to_string();
    assert!(
        message.contains("default-hevc") && message.contains("unknown encoder `libx264`"),
        "unexpected error: {err}"
    );
    assert!(!tokio::fs::try_exists(&request.output.path).await.unwrap());
}

#[tokio::test]
async fn unavailable_encoder_is_config_invalid_before_ffmpeg() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.mkv");
    tokio::fs::write(&input, b"input").await.unwrap();
    let mut request = request(dir.path(), &input).await;
    request.profile = TranscodeVideoProfile {
        name: "av1-archive".to_owned(),
        target_codec: "av1".to_owned(),
        encoder: "libaom-av1".to_owned(),
        crf: 35,
        preset: "8".to_owned(),
        tune: None,
        codec_profile: None,
        codec_level: None,
        pixel_format: None,
        max_width: None,
        max_height: None,
        copy_compatible: false,
    };
    request.output.video_codec = "av1".to_owned();
    let config = config(dir.path())
        .with_available_video_encoders(["libx265".to_owned(), "libsvtav1".to_owned()]);

    let err = handle_transcode_video(&request, &config).await.unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::ConfigInvalid);
    assert!(
        err.to_string().contains("libaom-av1") && err.to_string().contains("not available"),
        "unexpected error: {err}"
    );
    assert!(!tokio::fs::try_exists(&request.output.path).await.unwrap());
}

#[tokio::test]
async fn malformed_request_payload_is_accepted_then_terminal_error() {
    let request = OperationRequest {
        operation: OperationKind::TranscodeVideo,
        lease_id: LeaseId(42),
        payload: serde_json::json!({"input": 1}),
        heartbeat_deadline_ms: 1_000,
        progress_idle_deadline_ms: 1_000,
    };

    let (config, _config_dir) = config_path();
    let frames = dispatch_frames(
        handle_operation_with_test_config(request, config)
            .await
            .unwrap(),
    );

    assert_terminal_error(
        frames.last().unwrap(),
        FailureClass::MalformedWorkerResult,
        ErrorCode::MalformedWorkerResult,
    );
}

#[tokio::test]
async fn unsupported_operation_returns_unknown_operation_protocol_error() {
    let request = OperationRequest {
        operation: OperationKind::ProbeFile,
        lease_id: LeaseId(42),
        payload: serde_json::Value::Null,
        heartbeat_deadline_ms: 1_000,
        progress_idle_deadline_ms: 1_000,
    };

    let err = handle_operation(request).await.unwrap_err();

    assert!(matches!(err, ProtocolError::UnknownOperation { .. }));
}

#[tokio::test]
async fn transcode_audio_operation_decodes_typed_payload() {
    let request = OperationRequest {
        operation: OperationKind::TranscodeAudio,
        lease_id: LeaseId(42),
        payload: serde_json::json!({"input": 1}),
        heartbeat_deadline_ms: 1_000,
        progress_idle_deadline_ms: 1_000,
    };

    let (config, _config_dir) = config_path();
    let frames = dispatch_frames(
        handle_operation_with_test_config(request, config)
            .await
            .unwrap(),
    );

    let ProgressFrame::Error { message, .. } = frames.last().unwrap() else {
        panic!("expected terminal error");
    };
    assert!(message.contains("transcode_audio payload decode"));
}

#[tokio::test]
async fn extract_audio_operation_decodes_typed_payload() {
    let request = OperationRequest {
        operation: OperationKind::ExtractAudio,
        lease_id: LeaseId(42),
        payload: serde_json::json!({"input": 1}),
        heartbeat_deadline_ms: 1_000,
        progress_idle_deadline_ms: 1_000,
    };

    let (config, _config_dir) = config_path();
    let frames = dispatch_frames(
        handle_operation_with_test_config(request, config)
            .await
            .unwrap(),
    );

    let ProgressFrame::Error { message, .. } = frames.last().unwrap() else {
        panic!("expected terminal error");
    };
    assert!(message.contains("extract_audio payload decode"));
}

#[tokio::test]
async fn transcode_audio_existing_output_path_is_config_invalid() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.mkv");
    tokio::fs::write(&input, b"input").await.unwrap();
    let request = transcode_audio_request(dir.path(), &input, audio_expected(&input).await, "opus");
    tokio::fs::write(&request.output.path, b"exists")
        .await
        .unwrap();

    let err = handle_transcode_audio(&request, &config(dir.path()))
        .await
        .unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::ConfigInvalid);
}

#[tokio::test]
async fn extract_audio_output_path_outside_staging_root_is_config_invalid() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.mkv");
    tokio::fs::write(&input, b"input").await.unwrap();
    let mut request = extract_audio_request(dir.path(), &input, audio_expected(&input).await);
    request.output.path = dir
        .path()
        .join("../escape.ogg")
        .to_string_lossy()
        .into_owned();

    let err = handle_extract_audio(&request, &config(dir.path()))
        .await
        .unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::ConfigInvalid);
}

#[tokio::test]
async fn transcode_audio_rejects_selected_stream_id_mismatch() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.mkv");
    tokio::fs::write(&input, b"input").await.unwrap();
    let request = transcode_audio_request(dir.path(), &input, audio_expected(&input).await, "opus");

    let err = handle_transcode_audio(
        &request,
        &audio_config(dir.path(), "matroska", "opus", "stream-9", "eng", "Main", 1),
    )
    .await
    .unwrap_err();

    assert_eq!(err.failure_class(), FailureClass::MalformedWorkerResult);
}

#[tokio::test]
async fn transcode_audio_rejects_selected_output_ordering_mismatch() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.mkv");
    tokio::fs::write(&input, b"input").await.unwrap();
    let mut request =
        transcode_audio_request(dir.path(), &input, audio_expected(&input).await, "opus");
    request.selection.selected_streams.push(AudioStreamRef {
        snapshot_stream_id: "stream-3".to_owned(),
        provider_stream_index: 3,
    });

    let err = handle_transcode_audio(&request, &audio_config_two_outputs_reversed(dir.path()))
        .await
        .unwrap_err();

    assert_eq!(err.failure_class(), FailureClass::MalformedWorkerResult);
}

#[tokio::test]
async fn transcode_audio_rejects_preservation_mismatch() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.mkv");
    tokio::fs::write(&input, b"input").await.unwrap();
    let request = transcode_audio_request(dir.path(), &input, audio_expected(&input).await, "opus");

    let err = handle_transcode_audio(
        &request,
        &audio_config(dir.path(), "matroska", "opus", "stream-1", "fra", "Main", 1),
    )
    .await
    .unwrap_err();

    assert_eq!(err.failure_class(), FailureClass::MalformedWorkerResult);
}

#[tokio::test]
async fn extract_audio_rejects_dropped_source_language_or_title() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.mkv");
    tokio::fs::write(&input, b"input").await.unwrap();
    let request = extract_audio_request(dir.path(), &input, audio_expected(&input).await);

    let err = handle_extract_audio(
        &request,
        &audio_extract_config(dir.path(), None, Some("Main")),
    )
    .await
    .unwrap_err();

    assert_eq!(err.failure_class(), FailureClass::MalformedWorkerResult);
}

// ---- Task 7.2 tests ----

#[tokio::test]
async fn copy_video_with_nonconforming_codec_fails_loudly() {
    // copy_video=true but ffprobe reports h264 (not the target hevc)
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.mkv");
    tokio::fs::write(&input, b"input").await.unwrap();
    let mut req = request(dir.path(), &input).await;
    req.copy_video = true;
    // ffprobe reports h264 for the input
    let config = config_with_probe(
        dir.path(),
        "#!/bin/sh\ncat <<'JSON'\n{\"format\":{\"format_name\":\"matroska\"},\"streams\":[{\"codec_type\":\"video\",\"codec_name\":\"h264\",\"width\":1920,\"height\":1080,\"pix_fmt\":\"yuv420p\"}]}\nJSON\n",
    );

    let err = handle_transcode_video(&req, &config).await.unwrap_err();

    assert!(
        matches!(
            err,
            TranscodeVideoError::MalformedWorkerResult { .. }
                | TranscodeVideoError::ConfigInvalid { .. }
        ),
        "expected MalformedWorkerResult or ConfigInvalid, got: {err}"
    );
}

#[tokio::test]
async fn mp4_output_contract_now_accepted() {
    // mp4 was previously rejected; now it is a supported container
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.mkv");
    tokio::fs::write(&input, b"input").await.unwrap();
    let mut req = request(dir.path(), &input).await;
    req.output.container = "mp4".to_owned();
    req.output.path = dir
        .path()
        .join("stage")
        .join("input.hevc.mp4")
        .to_string_lossy()
        .into_owned();
    // ffprobe returns mp4/hevc for output validation
    let config = config_with_probe(
        dir.path(),
        "#!/bin/sh\ncat <<'JSON'\n{\"format\":{\"format_name\":\"mp4\"},\"streams\":[{\"codec_type\":\"video\",\"codec_name\":\"hevc\",\"width\":1920,\"height\":1080,\"pix_fmt\":\"yuv420p\"}]}\nJSON\n",
    );

    // Should succeed — mp4 is now accepted
    let result = handle_transcode_video(&req, &config).await;
    assert!(
        result.is_ok(),
        "mp4 output should now be accepted: {result:?}"
    );
    let result = result.unwrap();
    assert_eq!(result.output_container, "mp4");
}

#[tokio::test]
async fn output_dims_and_pixfmt_populated_from_probe() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.mkv");
    tokio::fs::write(&input, b"input").await.unwrap();
    let req = request(dir.path(), &input).await;
    let config = config(dir.path());

    let result = handle_transcode_video(&req, &config).await.unwrap();
    assert_eq!(result.output_width, 1920);
    assert_eq!(result.output_height, 1080);
    assert_eq!(result.output_pixel_format, "yuv420p");
    assert!(!result.copied_video);
}

#[tokio::test]
async fn copy_video_sets_copied_video_flag() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.mkv");
    tokio::fs::write(&input, b"input").await.unwrap();
    let mut req = request(dir.path(), &input).await;
    req.copy_video = true;
    // ffprobe returns hevc/mkv — matches the target codec
    let config = config(dir.path());

    let result = handle_transcode_video(&req, &config).await.unwrap();
    assert!(result.copied_video);
}

#[tokio::test]
async fn copy_video_with_constrained_profile_but_unknown_source_profile_fails_loudly() {
    // Profile constrains codec_profile=main10, but the source probe reports no
    // profile field (None). We cannot prove conformance → must fail loudly.
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.mkv");
    tokio::fs::write(&input, b"input").await.unwrap();
    let mut req = request(dir.path(), &input).await;
    req.copy_video = true;
    req.profile.codec_profile = Some("main10".to_owned());
    req.profile.pixel_format = Some("yuv420p10le".to_owned());
    // ffprobe reports hevc (matches codec) but emits NO "profile" key → None.
    let config = config_with_probe(
        dir.path(),
        "#!/bin/sh\ncat <<'JSON'\n{\"format\":{\"format_name\":\"matroska\"},\"streams\":[{\"codec_type\":\"video\",\"codec_name\":\"hevc\",\"width\":1920,\"height\":1080,\"pix_fmt\":\"yuv420p10le\"}]}\nJSON\n",
    );

    let err = handle_transcode_video(&req, &config).await.unwrap_err();

    assert!(
        matches!(err, TranscodeVideoError::MalformedWorkerResult { .. }),
        "expected MalformedWorkerResult for unknown source codec_profile, got: {err}"
    );
    assert!(
        err.to_string().contains("codec_profile"),
        "error should mention codec_profile: {err}"
    );
}

#[tokio::test]
async fn multi_video_stream_source_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.mkv");
    tokio::fs::write(&input, b"input").await.unwrap();
    let req = request(dir.path(), &input).await;
    // ffprobe reports two video streams.
    let config = config_with_probe(
        dir.path(),
        "#!/bin/sh\ncat <<'JSON'\n{\"format\":{\"format_name\":\"matroska\"},\"streams\":[{\"codec_type\":\"video\",\"codec_name\":\"hevc\",\"width\":1920,\"height\":1080,\"pix_fmt\":\"yuv420p\"},{\"codec_type\":\"video\",\"codec_name\":\"hevc\",\"width\":640,\"height\":360,\"pix_fmt\":\"yuv420p\"}]}\nJSON\n",
    );

    let err = handle_transcode_video(&req, &config).await.unwrap_err();

    assert!(
        matches!(err, TranscodeVideoError::ConfigInvalid { .. }),
        "expected ConfigInvalid for multi-video-stream source, got: {err}"
    );
    assert!(
        err.to_string().contains('2'),
        "error should name the video stream count: {err}"
    );
}

fn config_with_probe(root: &Path, probe_script: &str) -> FfmpegConfig {
    let ffmpeg = stub_bin(
        root,
        "ffmpeg",
        "#!/bin/sh\nlast=\"\"\nfor arg in \"$@\"; do last=\"$arg\"; done\nprintf output > \"$last\"\n",
    );
    let ffprobe = stub_bin(root, "ffprobe", probe_script);
    FfmpegConfig::new(
        ffmpeg,
        ffprobe,
        "ffmpeg version test".to_owned(),
        DEFAULT_PROCESS_TIMEOUT,
    )
}

// ---- End Task 7.2 tests ----

async fn request(root: &Path, input: &Path) -> TranscodeVideoRequest {
    let stage = root.join("stage");
    tokio::fs::create_dir(&stage).await.unwrap();
    let expected = if tokio::fs::try_exists(input).await.unwrap() {
        let observed = crate::observe_file_facts(input).await.unwrap();
        TranscodeVideoExpectedFacts {
            size_bytes: observed.size_bytes,
            content_hash: observed.content_hash,
            modified_at: observed.modified_at,
            local_file_key: None,
        }
    } else {
        TranscodeVideoExpectedFacts {
            size_bytes: 1,
            content_hash: "blake3:missing".to_owned(),
            modified_at: None,
            local_file_key: None,
        }
    };
    TranscodeVideoRequest {
        input: TranscodeVideoInput {
            path: input.to_string_lossy().into_owned(),
            expected,
        },
        output: TranscodeVideoOutput {
            staging_root: stage.to_string_lossy().into_owned(),
            path: stage.join("input.hevc.mkv").to_string_lossy().into_owned(),
            container: "mkv".to_owned(),
            video_codec: "hevc".to_owned(),
            overwrite: false,
        },
        profile: TranscodeVideoProfile::default_hevc(),
        copy_video: false,
    }
}

fn config(root: &Path) -> FfmpegConfig {
    let ffmpeg = stub_bin(
        root,
        "ffmpeg",
        "#!/bin/sh\nlast=\"\"\nfor arg in \"$@\"; do last=\"$arg\"; done\nprintf output > \"$last\"\n",
    );
    // ffprobe returns the same JSON for both probe_input and probe_output calls.
    // Includes width/height/pix_fmt so both probes succeed.
    let ffprobe = stub_bin(
        root,
        "ffprobe",
        "#!/bin/sh\ncat <<'JSON'\n{\"format\":{\"format_name\":\"matroska\"},\"streams\":[{\"codec_type\":\"video\",\"codec_name\":\"hevc\",\"width\":1920,\"height\":1080,\"pix_fmt\":\"yuv420p\"}]}\nJSON\n",
    );
    FfmpegConfig::new(
        ffmpeg,
        ffprobe,
        "ffmpeg version test".to_owned(),
        DEFAULT_PROCESS_TIMEOUT,
    )
}

fn audio_config(
    root: &Path,
    container: &str,
    codec: &str,
    snapshot_id: &str,
    language: &str,
    title: &str,
    default: u8,
) -> FfmpegConfig {
    let ffmpeg = stub_bin(
        root,
        "ffmpeg",
        "#!/bin/sh\nlast=\"\"\nfor arg in \"$@\"; do last=\"$arg\"; done\nprintf output > \"$last\"\n",
    );
    let ffprobe = stub_bin(
        root,
        "ffprobe",
        &format!(
            "#!/bin/sh\nlast=\"\"\nfor arg in \"$@\"; do last=\"$arg\"; done\ncase \"$last\" in\n  *audio-stage*) cat <<'JSON'\n{{\"format\":{{\"format_name\":\"{container}\"}},\"streams\":[{{\"index\":1,\"codec_type\":\"audio\",\"codec_name\":\"{codec}\",\"channels\":6,\"tags\":{{\"snapshot_stream_id\":\"{snapshot_id}\",\"language\":\"{language}\",\"title\":\"{title}\"}},\"disposition\":{{\"default\":{default},\"forced\":0,\"comment\":0}}}}]}}\nJSON\n    ;;\n  *) cat <<'JSON'\n{{\"format\":{{\"format_name\":\"matroska\"}},\"streams\":[{{\"index\":1,\"codec_type\":\"audio\",\"codec_name\":\"aac\",\"channels\":6,\"tags\":{{\"language\":\"eng\",\"title\":\"Main\"}},\"disposition\":{{\"default\":1,\"forced\":0,\"comment\":0}}}}]}}\nJSON\n    ;;\nesac\n"
        ),
    );
    FfmpegConfig::new(
        ffmpeg,
        ffprobe,
        "ffmpeg version test".to_owned(),
        DEFAULT_PROCESS_TIMEOUT,
    )
}

fn audio_config_two_outputs_reversed(root: &Path) -> FfmpegConfig {
    let ffmpeg = stub_bin(
        root,
        "ffmpeg",
        "#!/bin/sh\nlast=\"\"\nfor arg in \"$@\"; do last=\"$arg\"; done\nprintf output > \"$last\"\n",
    );
    let ffprobe = stub_bin(
        root,
        "ffprobe",
        "#!/bin/sh\nlast=\"\"\nfor arg in \"$@\"; do last=\"$arg\"; done\ncase \"$last\" in\n  *audio-stage*) cat <<'JSON'\n{\"format\":{\"format_name\":\"matroska\"},\"streams\":[{\"index\":3,\"codec_type\":\"audio\",\"codec_name\":\"opus\",\"channels\":2,\"tags\":{\"snapshot_stream_id\":\"stream-3\",\"language\":\"jpn\",\"title\":\"Commentary\"},\"disposition\":{\"default\":0,\"forced\":0,\"comment\":1}},{\"index\":1,\"codec_type\":\"audio\",\"codec_name\":\"opus\",\"channels\":6,\"tags\":{\"snapshot_stream_id\":\"stream-1\",\"language\":\"eng\",\"title\":\"Main\"},\"disposition\":{\"default\":1,\"forced\":0,\"comment\":0}}]}\nJSON\n    ;;\n  *) cat <<'JSON'\n{\"format\":{\"format_name\":\"matroska\"},\"streams\":[{\"index\":1,\"codec_type\":\"audio\",\"codec_name\":\"aac\",\"channels\":6,\"tags\":{\"language\":\"eng\",\"title\":\"Main\"},\"disposition\":{\"default\":1,\"forced\":0,\"comment\":0}},{\"index\":3,\"codec_type\":\"audio\",\"codec_name\":\"aac\",\"channels\":2,\"tags\":{\"language\":\"jpn\",\"title\":\"Commentary\"},\"disposition\":{\"default\":0,\"forced\":0,\"comment\":1}}]}\nJSON\n    ;;\nesac\n",
    );
    FfmpegConfig::new(
        ffmpeg,
        ffprobe,
        "ffmpeg version test".to_owned(),
        DEFAULT_PROCESS_TIMEOUT,
    )
}

fn audio_extract_config(root: &Path, language: Option<&str>, title: Option<&str>) -> FfmpegConfig {
    let ffmpeg = stub_bin(
        root,
        "ffmpeg",
        "#!/bin/sh\nlast=\"\"\nfor arg in \"$@\"; do last=\"$arg\"; done\nprintf output > \"$last\"\n",
    );
    let tags = match (language, title) {
        (Some(language), Some(title)) => {
            format!("\"tags\":{{\"language\":\"{language}\",\"title\":\"{title}\"}},")
        }
        (Some(language), None) => format!("\"tags\":{{\"language\":\"{language}\"}},"),
        (None, Some(title)) => format!("\"tags\":{{\"title\":\"{title}\"}},"),
        (None, None) => String::new(),
    };
    let ffprobe = stub_bin(
        root,
        "ffprobe",
        &format!(
            "#!/bin/sh\nlast=\"\"\nfor arg in \"$@\"; do last=\"$arg\"; done\ncase \"$last\" in\n  *extract-stage*) cat <<'JSON'\n{{\"format\":{{\"format_name\":\"ogg\"}},\"streams\":[{{\"index\":1,\"codec_type\":\"audio\",\"codec_name\":\"opus\",{tags}\"disposition\":{{\"default\":1,\"forced\":0,\"comment\":0}}}}]}}\nJSON\n    ;;\n  *) cat <<'JSON'\n{{\"format\":{{\"format_name\":\"matroska\"}},\"streams\":[{{\"index\":1,\"codec_type\":\"audio\",\"codec_name\":\"aac\",\"tags\":{{\"language\":\"eng\",\"title\":\"Main\"}},\"disposition\":{{\"default\":1,\"forced\":0,\"comment\":0}}}}]}}\nJSON\n    ;;\nesac\n"
        ),
    );
    FfmpegConfig::new(
        ffmpeg,
        ffprobe,
        "ffmpeg version test".to_owned(),
        DEFAULT_PROCESS_TIMEOUT,
    )
}

async fn audio_expected(input: &Path) -> AudioExpectedFacts {
    let observed = crate::observe_file_facts(input).await.unwrap();
    AudioExpectedFacts {
        size_bytes: observed.size_bytes,
        content_hash: observed.content_hash,
        modified_at: observed.modified_at,
        local_file_key: None,
    }
}

fn transcode_audio_request(
    root: &Path,
    input: &Path,
    expected: AudioExpectedFacts,
    target_codec: &str,
) -> TranscodeAudioRequest {
    let stage = root.join("audio-stage");
    std::fs::create_dir_all(&stage).unwrap();
    TranscodeAudioRequest {
        input: TranscodeAudioInput {
            path: input.to_string_lossy().into_owned(),
            expected,
        },
        output: TranscodeAudioOutput {
            staging_root: stage.to_string_lossy().into_owned(),
            path: stage.join("input.audio.mkv").to_string_lossy().into_owned(),
            container: "mkv".to_owned(),
            overwrite: false,
        },
        selection: TranscodeAudioSelection {
            selected_streams: vec![AudioStreamRef {
                snapshot_stream_id: "stream-1".to_owned(),
                provider_stream_index: 1,
            }],
        },
        audio: TranscodeAudioSettings {
            target_codec: target_codec.to_owned(),
            profile: format!("default-{target_codec}"),
        },
    }
}

fn extract_audio_request(
    root: &Path,
    input: &Path,
    expected: AudioExpectedFacts,
) -> ExtractAudioRequest {
    let stage = root.join("extract-stage");
    std::fs::create_dir_all(&stage).unwrap();
    ExtractAudioRequest {
        input: ExtractAudioInput {
            path: input.to_string_lossy().into_owned(),
            expected,
        },
        output: ExtractAudioOutput {
            staging_root: stage.to_string_lossy().into_owned(),
            path: stage.join("input.audio.ogg").to_string_lossy().into_owned(),
            container: "ogg".to_owned(),
            audio_codec: "opus".to_owned(),
            overwrite: false,
        },
        selection: AudioStreamRef {
            snapshot_stream_id: "stream-1".to_owned(),
            provider_stream_index: 1,
        },
    }
}

/// Returns a config backed by stub binaries plus the `TempDir` guard. Hold the
/// guard for the test's duration so the tempdir is cleaned up afterward.
fn config_path() -> (FfmpegConfig, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let config = config(dir.path());
    (config, dir)
}

fn stub_bin(dir: &Path, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, body).unwrap();
    make_executable(&path);
    path
}

fn handle_operation_with_test_config(
    req: OperationRequest,
    config: FfmpegConfig,
) -> OperationFuture {
    operation_handler(config)(req)
}

fn dispatch_frames(dispatch: OperationDispatch) -> Vec<ProgressFrame> {
    let voom_worker_protocol::http::OperationBody::Buffered(body) = dispatch.body else {
        panic!("ffmpeg worker should buffer test responses");
    };
    body.split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_slice(line).unwrap())
        .collect()
}

fn assert_terminal_error(frame: &ProgressFrame, class: FailureClass, code: ErrorCode) {
    let ProgressFrame::Error {
        class: actual_class,
        code: actual_code,
        message,
        payload,
        ..
    } = frame
    else {
        panic!("expected terminal error frame, got {frame:?}");
    };
    assert_eq!(*actual_class, class);
    assert_eq!(*actual_code, code);
    assert!(!message.trim().is_empty());
    assert!(payload.is_some());
}

fn input_probe_with_codec(codec: &str) -> InputProbe {
    InputProbe {
        width: 1920,
        height: 1080,
        codec: codec.to_owned(),
        pixel_format: "yuv420p".to_owned(),
        codec_profile: None,
        codec_level: None,
        video_stream_count: 1,
    }
}

#[test]
fn validate_copy_codec_accepts_h265_alias_against_hevc_target() {
    let probe = input_probe_with_codec("h265");
    assert!(validate_copy_codec("hevc", &probe).is_ok());
}

#[test]
fn validate_copy_codec_accepts_hevc_against_h265_target() {
    let probe = input_probe_with_codec("hevc");
    assert!(validate_copy_codec("h265", &probe).is_ok());
}

#[test]
fn validate_copy_codec_rejects_mismatched_codec() {
    let probe = input_probe_with_codec("h264");
    let err = validate_copy_codec("hevc", &probe).unwrap_err();
    assert_eq!(err.error_code(), ErrorCode::MalformedWorkerResult);
}

#[cfg(unix)]
fn make_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;

    let mut permissions = std::fs::metadata(path).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(path, permissions).unwrap();
}

#[cfg(not(unix))]
fn make_executable(_path: &Path) {}
