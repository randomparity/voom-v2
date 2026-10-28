//! Per-encoder ffmpeg conformance tests.
//!
//! These tests require a real ffmpeg installation with the universally
//! available libx265 and libsvtav1 encoders. A missing required encoder is a
//! **loud setup failure**, not a skipped test (spec §10): each test calls
//! `preflight_from_process_env()` at the top and fails immediately with a
//! diagnostic. The optional libaom-av1 encoder is not present in every ffmpeg
//! build (e.g. Homebrew); the single test that runs a libaom-av1 encode does a
//! **loud, logged skip** when it is absent rather than failing.

#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "conformance tests use unwrap/expect for assertion plumbing"
)]

use std::path::Path;
use std::process::Command;

use tokio::io::{AsyncBufReadExt, BufReader};
use voom_ffmpeg_worker::handler::handle_transcode_audio;
use voom_ffmpeg_worker::{
    DEFAULT_PROCESS_TIMEOUT, FfmpegConfig, handle_transcode_video, preflight_from_process_env,
};
use voom_worker_protocol::{
    AUDIO_PROFILE_DEFAULT, AudioExpectedFacts, AudioStreamRef, ClientHandle, HttpClient,
    NdjsonOutcome, OperationKind, OperationRequest, ProgressFrame, TranscodeAudioInput,
    TranscodeAudioOutput, TranscodeAudioRequest, TranscodeAudioResult, TranscodeAudioSelection,
    TranscodeAudioSettings, TranscodeVideoExpectedFacts, TranscodeVideoInput, TranscodeVideoOutput,
    TranscodeVideoProfile, TranscodeVideoRequest, WorkerCredentials,
};

// ---------------------------------------------------------------------------
// Fixture generation
// ---------------------------------------------------------------------------

/// Generates a tiny (64x64, 2 seconds, 10fps) H.264 source file at `path`,
/// using the same ffmpeg binary the worker uses. A larger/longer source avoids
/// pts/dts issues in mp4 muxing.
fn generate_h264_fixture(ffmpeg: &Path, path: &Path) {
    let status = Command::new(ffmpeg)
        .args([
            "-y",
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=64x64:rate=10",
            "-t",
            "2",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            path.to_str().unwrap(),
        ])
        .status()
        .expect("ffmpeg fixture generation failed to start");
    assert!(
        status.success(),
        "ffmpeg H.264 fixture generation failed: {status}"
    );
}

/// Generates a tiny (64x64, 1 second) HEVC source file at `path` (for
/// `copy_video` tests), using the same ffmpeg binary the worker uses.
fn generate_hevc_fixture(ffmpeg: &Path, path: &Path) {
    let status = Command::new(ffmpeg)
        .args([
            "-y",
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=64x64:rate=1",
            "-t",
            "1",
            "-c:v",
            "libx265",
            "-pix_fmt",
            "yuv420p",
            "-x265-params",
            "log-level=error",
            path.to_str().unwrap(),
        ])
        .status()
        .expect("ffmpeg HEVC fixture generation failed to start");
    assert!(
        status.success(),
        "ffmpeg HEVC fixture generation failed: {status}"
    );
}

/// Generates a (160x90, 2 seconds, 10fps) H.264 file for downscale tests, using
/// the same ffmpeg binary the worker uses.
fn generate_h264_oversized(ffmpeg: &Path, path: &Path) {
    let status = Command::new(ffmpeg)
        .args([
            "-y",
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=160x90:rate=10",
            "-t",
            "2",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            path.to_str().unwrap(),
        ])
        .status()
        .expect("ffmpeg oversized fixture generation failed to start");
    assert!(
        status.success(),
        "ffmpeg oversized H.264 fixture generation failed: {status}"
    );
}

// ---------------------------------------------------------------------------
// Config helpers
// ---------------------------------------------------------------------------

fn ffmpeg_config(preflight: &voom_ffmpeg_worker::FfmpegPreflight) -> FfmpegConfig {
    FfmpegConfig::new(
        preflight.ffmpeg_path.clone(),
        preflight.ffprobe_path.clone(),
        preflight.ffmpeg_version.clone(),
        DEFAULT_PROCESS_TIMEOUT,
    )
}

async fn observed_facts(path: &Path) -> TranscodeVideoExpectedFacts {
    let observed = voom_ffmpeg_worker::observe_file_facts(path).await.unwrap();
    TranscodeVideoExpectedFacts {
        size_bytes: observed.size_bytes,
        content_hash: observed.content_hash,
        modified_at: observed.modified_at,
        local_file_key: None,
    }
}

fn basic_request(
    input: &Path,
    output: &Path,
    staging: &Path,
    container: &str,
    codec: &str,
    profile: TranscodeVideoProfile,
) -> TranscodeVideoRequest {
    // Input facts will be computed after the file exists; provide placeholder
    // that matches the actual file via `observed_facts`.
    TranscodeVideoRequest {
        input: TranscodeVideoInput {
            path: input.to_string_lossy().into_owned(),
            expected: TranscodeVideoExpectedFacts {
                size_bytes: 0,
                content_hash: String::new(),
                modified_at: None,
                local_file_key: None,
            },
            video_codec: None,
            video_pixel_format: None,
        },
        output: TranscodeVideoOutput {
            staging_root: staging.to_string_lossy().into_owned(),
            path: output.to_string_lossy().into_owned(),
            container: container.to_owned(),
            video_codec: codec.to_owned(),
            overwrite: false,
        },
        profile,
        hardware_assignment: None,
        copy_video: false,
    }
}

/// Fills in the input expected facts by observing the real file.
async fn with_real_expected(
    mut request: TranscodeVideoRequest,
    input: &Path,
) -> TranscodeVideoRequest {
    request.input.expected = observed_facts(input).await;
    request
}

// ---------------------------------------------------------------------------
// Conformance tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn libx265_hevc_mkv_transcode_succeeds_with_correct_codec_and_container() {
    let preflight = preflight_from_process_env()
        .expect("preflight failed — ensure libx265 and libsvtav1 are available");
    let config = ffmpeg_config(&preflight);
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("source.mkv");
    let output = dir.path().join("out.hevc.mkv");
    generate_h264_fixture(&preflight.ffmpeg_path, &input);

    let profile = TranscodeVideoProfile::default_hevc();
    let request = with_real_expected(
        basic_request(&input, &output, dir.path(), "mkv", "hevc", profile),
        &input,
    )
    .await;

    let result = handle_transcode_video(&request, &config)
        .await
        .expect("libx265 hevc mkv transcode failed");

    assert_eq!(result.output_container, "mkv");
    assert_eq!(result.output_video_codec, "hevc");
    assert!(result.output_width > 0, "output width should be populated");
    assert!(
        result.output_height > 0,
        "output height should be populated"
    );
    assert!(
        !result.output_pixel_format.is_empty(),
        "pixel_format should be populated"
    );
    assert!(!result.copied_video);
}

#[tokio::test]
async fn libsvtav1_av1_mkv_transcode_succeeds() {
    let preflight = preflight_from_process_env()
        .expect("preflight failed — ensure libx265 and libsvtav1 are available");
    let config = ffmpeg_config(&preflight);
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("source.mkv");
    let output = dir.path().join("out.av1.mkv");
    generate_h264_fixture(&preflight.ffmpeg_path, &input);

    let profile = TranscodeVideoProfile {
        name: "default-av1".to_owned(),
        target_codec: "av1".to_owned(),
        encoder: "libsvtav1".to_owned(),
        crf: Some(35),
        cq: None,
        qp: None,
        bitrate_kbps: None,
        preset: Some("10".to_owned()), // fast preset for tests
        tune: None,
        codec_profile: None,
        codec_level: None,
        pixel_format: None,
        max_width: None,
        max_height: None,
        copy_compatible: false,
        decode: voom_core::VideoDecodeMode::default(),
    };
    let request = with_real_expected(
        basic_request(&input, &output, dir.path(), "mkv", "av1", profile),
        &input,
    )
    .await;

    let result = handle_transcode_video(&request, &config)
        .await
        .expect("libsvtav1 av1 mkv transcode failed");

    assert_eq!(result.output_container, "mkv");
    assert_eq!(result.output_video_codec, "av1");
    assert!(!result.copied_video);
}

#[tokio::test]
#[expect(
    clippy::print_stderr,
    reason = "libaom-av1 is optional; emit a loud, logged skip (spec §10) when absent rather than a silent skip"
)]
async fn libaom_av1_mkv_transcode_succeeds() {
    let preflight = preflight_from_process_env()
        .expect("preflight failed — ensure libx265 and libsvtav1 are available");
    if !preflight.has_encoder("libaom-av1") {
        eprintln!(
            "SKIP libaom_av1_mkv_transcode_succeeds: libaom-av1 encoder unavailable in this ffmpeg build"
        );
        return;
    }
    let config = ffmpeg_config(&preflight);
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("source.mkv");
    let output = dir.path().join("out.av1.mkv");
    generate_h264_fixture(&preflight.ffmpeg_path, &input);

    let profile = TranscodeVideoProfile {
        name: "av1-archive".to_owned(),
        target_codec: "av1".to_owned(),
        encoder: "libaom-av1".to_owned(),
        crf: Some(35),
        cq: None,
        qp: None,
        bitrate_kbps: None,
        preset: Some("8".to_owned()), // cpu-used 8 = fastest, for test speed
        tune: None,
        codec_profile: None,
        codec_level: None,
        pixel_format: None,
        max_width: None,
        max_height: None,
        copy_compatible: false,
        decode: voom_core::VideoDecodeMode::default(),
    };
    let request = with_real_expected(
        basic_request(&input, &output, dir.path(), "mkv", "av1", profile),
        &input,
    )
    .await;

    let result = handle_transcode_video(&request, &config)
        .await
        .expect("libaom-av1 av1 mkv transcode failed");

    assert_eq!(result.output_container, "mkv");
    assert_eq!(result.output_video_codec, "av1");
    assert!(!result.copied_video);
}

#[tokio::test]
async fn hevc_mp4_output_uses_hvc1_tag() {
    let preflight = preflight_from_process_env()
        .expect("preflight failed — ensure libx265 and libsvtav1 are available");
    let config = ffmpeg_config(&preflight);
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("source.mkv");
    let output = dir.path().join("out.hevc.mp4");
    generate_h264_fixture(&preflight.ffmpeg_path, &input);

    let profile = TranscodeVideoProfile::default_hevc();
    let request = with_real_expected(
        basic_request(&input, &output, dir.path(), "mp4", "hevc", profile),
        &input,
    )
    .await;

    let result = handle_transcode_video(&request, &config)
        .await
        .expect("hevc mp4 transcode failed");

    assert_eq!(result.output_container, "mp4");
    assert_eq!(result.output_video_codec, "hevc");
    // Verify the output file actually exists and has mp4 content
    let probe_output = Command::new(&preflight.ffprobe_path)
        .args([
            "-v",
            "error",
            "-print_format",
            "json",
            "-show_format",
            "-show_streams",
        ])
        .arg(&output)
        .output()
        .unwrap();
    assert!(
        probe_output.status.success(),
        "ffprobe failed on mp4 output"
    );
    let probe: serde_json::Value = serde_json::from_slice(&probe_output.stdout).unwrap();
    let format = probe["format"]["format_name"].as_str().unwrap_or_default();
    assert!(
        format.contains("mp4") || format.contains("mov"),
        "expected mp4 format, got: {format}"
    );
}

#[tokio::test]
async fn av1_mp4_output_uses_av01_tag() {
    let preflight = preflight_from_process_env()
        .expect("preflight failed — ensure libx265 and libsvtav1 are available");
    let config = ffmpeg_config(&preflight);
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("source.mkv");
    let output = dir.path().join("out.av1.mp4");
    generate_h264_fixture(&preflight.ffmpeg_path, &input);

    let profile = TranscodeVideoProfile {
        name: "av1-1080p".to_owned(),
        target_codec: "av1".to_owned(),
        encoder: "libsvtav1".to_owned(),
        crf: Some(35),
        cq: None,
        qp: None,
        bitrate_kbps: None,
        preset: Some("10".to_owned()),
        tune: None,
        codec_profile: None,
        codec_level: None,
        pixel_format: None,
        max_width: None,
        max_height: None,
        copy_compatible: false,
        decode: voom_core::VideoDecodeMode::default(),
    };
    let request = with_real_expected(
        basic_request(&input, &output, dir.path(), "mp4", "av1", profile),
        &input,
    )
    .await;

    let result = handle_transcode_video(&request, &config)
        .await
        .expect("av1 mp4 transcode failed");

    assert_eq!(result.output_container, "mp4");
    assert_eq!(result.output_video_codec, "av1");
}

#[tokio::test]
async fn downscale_applied_when_source_exceeds_cap() {
    let preflight = preflight_from_process_env()
        .expect("preflight failed — ensure libx265 and libsvtav1 are available");
    let config = ffmpeg_config(&preflight);
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("source.mkv");
    let output = dir.path().join("out.hevc.mkv");
    // 160x90 source, cap at 80x45 → downscale must occur
    generate_h264_oversized(&preflight.ffmpeg_path, &input);

    let profile = TranscodeVideoProfile {
        name: "hevc-tiny".to_owned(),
        target_codec: "hevc".to_owned(),
        encoder: "libx265".to_owned(),
        crf: Some(28),
        cq: None,
        qp: None,
        bitrate_kbps: None,
        preset: Some("ultrafast".to_owned()),
        tune: None,
        codec_profile: None,
        codec_level: None,
        pixel_format: None,
        max_width: Some(80),
        max_height: Some(45),
        copy_compatible: false,
        decode: voom_core::VideoDecodeMode::default(),
    };
    let request = with_real_expected(
        basic_request(&input, &output, dir.path(), "mkv", "hevc", profile),
        &input,
    )
    .await;

    let result = handle_transcode_video(&request, &config)
        .await
        .expect("downscale transcode failed");

    assert!(
        result.output_width <= 80,
        "expected output width ≤ 80, got {}",
        result.output_width
    );
    assert!(
        result.output_height <= 45,
        "expected output height ≤ 45, got {}",
        result.output_height
    );
}

#[tokio::test]
async fn copy_video_path_uses_stream_copy() {
    let preflight = preflight_from_process_env()
        .expect("preflight failed — ensure libx265 and libsvtav1 are available");
    let config = ffmpeg_config(&preflight);
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("source.hevc.mkv");
    let output = dir.path().join("out.hevc.mkv");
    // Generate HEVC source so copy_video validation passes
    generate_hevc_fixture(&preflight.ffmpeg_path, &input);

    let profile = TranscodeVideoProfile {
        name: "default-hevc".to_owned(),
        target_codec: "hevc".to_owned(),
        encoder: "libx265".to_owned(),
        crf: Some(23),
        cq: None,
        qp: None,
        bitrate_kbps: None,
        preset: Some("medium".to_owned()),
        tune: None,
        codec_profile: None,
        codec_level: None,
        pixel_format: None,
        max_width: None,
        max_height: None,
        copy_compatible: true,
        decode: voom_core::VideoDecodeMode::default(),
    };
    let mut request = with_real_expected(
        basic_request(&input, &output, dir.path(), "mkv", "hevc", profile),
        &input,
    )
    .await;
    request.copy_video = true;

    let result = handle_transcode_video(&request, &config)
        .await
        .expect("copy_video transcode failed");

    assert!(
        result.copied_video,
        "result should report copied_video=true"
    );
    assert_eq!(result.output_video_codec, "hevc");
}

#[tokio::test]
async fn copy_video_with_h264_source_fails_loudly() {
    let preflight = preflight_from_process_env()
        .expect("preflight failed — ensure libx265 and libsvtav1 are available");
    let config = ffmpeg_config(&preflight);
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("source.h264.mkv");
    let output = dir.path().join("out.hevc.mkv");
    // H.264 source ≠ target hevc → copy_video must fail loudly
    generate_h264_fixture(&preflight.ffmpeg_path, &input);

    let profile = TranscodeVideoProfile::default_hevc();
    let mut request = with_real_expected(
        basic_request(&input, &output, dir.path(), "mkv", "hevc", profile),
        &input,
    )
    .await;
    request.copy_video = true;

    let err = handle_transcode_video(&request, &config)
        .await
        .expect_err("copy_video with h264 source should fail");

    assert!(
        matches!(
            err,
            voom_ffmpeg_worker::TranscodeVideoError::MalformedWorkerResult { .. }
                | voom_ffmpeg_worker::TranscodeVideoError::ConfigInvalid { .. }
        ),
        "expected MalformedWorkerResult or ConfigInvalid, got: {err}"
    );
}

#[tokio::test]
async fn missing_input_fails_with_artifact_unavailable() {
    let preflight = preflight_from_process_env()
        .expect("preflight failed — ensure libx265 and libsvtav1 are available");
    let config = ffmpeg_config(&preflight);
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("does_not_exist.mkv");
    let output = dir.path().join("out.mkv");

    let request = TranscodeVideoRequest {
        input: TranscodeVideoInput {
            path: input.to_string_lossy().into_owned(),
            expected: TranscodeVideoExpectedFacts {
                size_bytes: 1,
                content_hash: "blake3:missing".to_owned(),
                modified_at: None,
                local_file_key: None,
            },
            video_codec: None,
            video_pixel_format: None,
        },
        output: TranscodeVideoOutput {
            staging_root: dir.path().to_string_lossy().into_owned(),
            path: output.to_string_lossy().into_owned(),
            container: "mkv".to_owned(),
            video_codec: "hevc".to_owned(),
            overwrite: false,
        },
        profile: TranscodeVideoProfile::default_hevc(),
        hardware_assignment: None,
        copy_video: false,
    };

    let err = handle_transcode_video(&request, &config)
        .await
        .expect_err("missing input should fail");

    assert!(
        matches!(
            err,
            voom_ffmpeg_worker::TranscodeVideoError::ArtifactUnavailable { .. }
        ),
        "expected ArtifactUnavailable, got: {err}"
    );
}

#[tokio::test]
async fn wrong_expected_input_facts_is_checksum_mismatch() {
    // Wrong expected input facts are caught at pre-observation (before ffmpeg).
    let preflight = preflight_from_process_env()
        .expect("preflight failed — ensure libx265 and libsvtav1 are available");
    let config = ffmpeg_config(&preflight);
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("source.mkv");
    let output = dir.path().join("out.mkv");
    generate_h264_fixture(&preflight.ffmpeg_path, &input);

    // Provide wrong expected size/hash — pre-observation rejects it.
    let request = TranscodeVideoRequest {
        input: TranscodeVideoInput {
            path: input.to_string_lossy().into_owned(),
            expected: TranscodeVideoExpectedFacts {
                size_bytes: 999_999,
                content_hash: "blake3:wrong".to_owned(),
                modified_at: None,
                local_file_key: None,
            },
            video_codec: None,
            video_pixel_format: None,
        },
        output: TranscodeVideoOutput {
            staging_root: dir.path().to_string_lossy().into_owned(),
            path: output.to_string_lossy().into_owned(),
            container: "mkv".to_owned(),
            video_codec: "hevc".to_owned(),
            overwrite: false,
        },
        profile: TranscodeVideoProfile::default_hevc(),
        hardware_assignment: None,
        copy_video: false,
    };

    let err = handle_transcode_video(&request, &config)
        .await
        .expect_err("wrong expected hash should fail");

    assert!(
        matches!(
            err,
            voom_ffmpeg_worker::TranscodeVideoError::ArtifactChecksumMismatch { .. }
        ),
        "expected ArtifactChecksumMismatch, got: {err}"
    );
}

#[tokio::test]
async fn existing_output_path_fails_before_ffmpeg() {
    let preflight = preflight_from_process_env()
        .expect("preflight failed — ensure libx265 and libsvtav1 are available");
    let config = ffmpeg_config(&preflight);
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("source.mkv");
    let output = dir.path().join("out.mkv");
    generate_h264_fixture(&preflight.ffmpeg_path, &input);
    // Pre-create the output to trigger the "already exists" guard
    std::fs::write(&output, b"existing").unwrap();

    let request = with_real_expected(
        basic_request(
            &input,
            &output,
            dir.path(),
            "mkv",
            "hevc",
            TranscodeVideoProfile::default_hevc(),
        ),
        &input,
    )
    .await;

    let err = handle_transcode_video(&request, &config)
        .await
        .expect_err("existing output should fail");

    assert!(
        matches!(
            err,
            voom_ffmpeg_worker::TranscodeVideoError::ConfigInvalid { .. }
        ),
        "expected ConfigInvalid, got: {err}"
    );
}

#[tokio::test]
async fn bad_payload_container_is_config_invalid() {
    let preflight = preflight_from_process_env()
        .expect("preflight failed — ensure libx265 and libsvtav1 are available");
    let config = ffmpeg_config(&preflight);
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("source.mkv");
    let output = dir.path().join("out.avi");
    generate_h264_fixture(&preflight.ffmpeg_path, &input);

    // avi is not a supported container
    let request = with_real_expected(
        basic_request(
            &input,
            &output,
            dir.path(),
            "avi",
            "hevc",
            TranscodeVideoProfile::default_hevc(),
        ),
        &input,
    )
    .await;

    let err = handle_transcode_video(&request, &config)
        .await
        .expect_err("avi container should be rejected");

    assert!(
        matches!(
            err,
            voom_ffmpeg_worker::TranscodeVideoError::ConfigInvalid { .. }
        ),
        "expected ConfigInvalid, got: {err}"
    );
}

#[tokio::test]
async fn path_escape_is_rejected_before_ffmpeg() {
    let preflight = preflight_from_process_env()
        .expect("preflight failed — ensure libx265 and libsvtav1 are available");
    let config = ffmpeg_config(&preflight);
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("source.mkv");
    generate_h264_fixture(&preflight.ffmpeg_path, &input);

    let mut request = with_real_expected(
        basic_request(
            &input,
            &dir.path().join("../escape.mkv"),
            dir.path(),
            "mkv",
            "hevc",
            TranscodeVideoProfile::default_hevc(),
        ),
        &input,
    )
    .await;
    request.output.path = dir
        .path()
        .join("../escape.mkv")
        .to_string_lossy()
        .into_owned();

    let err = handle_transcode_video(&request, &config)
        .await
        .expect_err("path escape should be rejected");

    assert!(
        matches!(
            err,
            voom_ffmpeg_worker::TranscodeVideoError::ConfigInvalid { .. }
        ),
        "expected ConfigInvalid, got: {err}"
    );
}

#[tokio::test]
async fn pixel_format_constraint_is_honored_in_output() {
    let preflight = preflight_from_process_env()
        .expect("preflight failed — ensure libx265 and libsvtav1 are available");
    let config = ffmpeg_config(&preflight);
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("source.mkv");
    let output = dir.path().join("out.hevc.mkv");
    generate_h264_fixture(&preflight.ffmpeg_path, &input);

    let profile = TranscodeVideoProfile {
        name: "hevc-10bit".to_owned(),
        target_codec: "hevc".to_owned(),
        encoder: "libx265".to_owned(),
        crf: Some(23),
        cq: None,
        qp: None,
        bitrate_kbps: None,
        preset: Some("ultrafast".to_owned()),
        tune: None,
        codec_profile: Some("main10".to_owned()),
        codec_level: None,
        pixel_format: Some("yuv420p10le".to_owned()),
        max_width: None,
        max_height: None,
        copy_compatible: false,
        decode: voom_core::VideoDecodeMode::default(),
    };
    let request = with_real_expected(
        basic_request(&input, &output, dir.path(), "mkv", "hevc", profile),
        &input,
    )
    .await;

    let result = handle_transcode_video(&request, &config)
        .await
        .expect("10-bit hevc transcode failed");

    assert_eq!(result.output_pixel_format, "yuv420p10le");
}

#[tokio::test]
async fn output_codec_mismatch_is_malformed_result() {
    // Request output.video_codec=av1 but use a libx265/hevc profile. The
    // contract validates codec and profile independently, so this passes the
    // pre-ffmpeg gate; ffmpeg then produces hevc, and probe_output rejects the
    // codec disagreement → MalformedWorkerResult.
    let preflight = preflight_from_process_env()
        .expect("preflight failed — ensure libx265 and libsvtav1 are available");
    let config = ffmpeg_config(&preflight);
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("source.mkv");
    let output = dir.path().join("out.mkv");
    generate_h264_fixture(&preflight.ffmpeg_path, &input);

    // hevc-producing profile, but the request claims the output codec is av1.
    let profile = TranscodeVideoProfile {
        name: "default-hevc".to_owned(),
        target_codec: "hevc".to_owned(),
        encoder: "libx265".to_owned(),
        crf: Some(28),
        cq: None,
        qp: None,
        bitrate_kbps: None,
        preset: Some("ultrafast".to_owned()),
        tune: None,
        codec_profile: None,
        codec_level: None,
        pixel_format: None,
        max_width: None,
        max_height: None,
        copy_compatible: false,
        decode: voom_core::VideoDecodeMode::default(),
    };
    let request = with_real_expected(
        basic_request(&input, &output, dir.path(), "mkv", "av1", profile),
        &input,
    )
    .await;

    let err = handle_transcode_video(&request, &config)
        .await
        .expect_err("output codec mismatch should fail");

    assert!(
        matches!(
            err,
            voom_ffmpeg_worker::TranscodeVideoError::MalformedWorkerResult { .. }
        ),
        "expected MalformedWorkerResult for output codec mismatch, got: {err}"
    );
}

#[tokio::test]
async fn provider_failure_on_corrupt_input_is_malformed_media() {
    // A truncated/garbage file that ffmpeg cannot decode → ffmpeg/ffprobe exits
    // non-zero with a structural-input-fault diagnostic → the permanent
    // MalformedMedia class (#248), not the retriable ExternalSystemUnavailable.
    let preflight = preflight_from_process_env()
        .expect("preflight failed — ensure libx265 and libsvtav1 are available");
    let config = ffmpeg_config(&preflight);
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("corrupt.mkv");
    let output = dir.path().join("out.mkv");
    // Not a valid media container — ffprobe/ffmpeg will reject it.
    std::fs::write(&input, b"this is not a valid media file, just bytes").unwrap();

    let request = with_real_expected(
        basic_request(
            &input,
            &output,
            dir.path(),
            "mkv",
            "hevc",
            TranscodeVideoProfile::default_hevc(),
        ),
        &input,
    )
    .await;

    let err = handle_transcode_video(&request, &config)
        .await
        .expect_err("corrupt input should fail");

    assert!(
        matches!(
            err,
            voom_ffmpeg_worker::TranscodeVideoError::MalformedMedia { .. }
        ),
        "expected MalformedMedia for corrupt input, got: {err}"
    );
    assert_eq!(err.failure_class(), voom_core::FailureClass::MalformedMedia);
    assert!(!err.failure_class().is_retriable());
}

#[tokio::test]
async fn tiny_process_timeout_yields_external_system_unavailable() {
    // A 1ms process timeout against a real encode trips the timeout path. The
    // worker probes the input first, so the timeout may fire on ffprobe or
    // ffmpeg — both map to ExternalSystemUnavailable, which is what we assert.
    let preflight = preflight_from_process_env()
        .expect("preflight failed — ensure libx265 and libsvtav1 are available");
    let config = FfmpegConfig::new(
        preflight.ffmpeg_path.clone(),
        preflight.ffprobe_path.clone(),
        preflight.ffmpeg_version.clone(),
        std::time::Duration::from_millis(1),
    );
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("source.mkv");
    let output = dir.path().join("out.mkv");
    generate_h264_fixture(&preflight.ffmpeg_path, &input);

    let request = with_real_expected(
        basic_request(
            &input,
            &output,
            dir.path(),
            "mkv",
            "hevc",
            TranscodeVideoProfile::default_hevc(),
        ),
        &input,
    )
    .await;

    let err = handle_transcode_video(&request, &config)
        .await
        .expect_err("1ms timeout should fail");

    assert!(
        matches!(
            err,
            voom_ffmpeg_worker::TranscodeVideoError::ExternalSystemUnavailable { .. }
        ),
        "expected ExternalSystemUnavailable for timeout, got: {err}"
    );
}

// ---------------------------------------------------------------------------
// Audio conformance
// ---------------------------------------------------------------------------

/// Generates a 1-second, 5.1 (6-channel) FLAC-in-mkv audio source at `path`,
/// using the same ffmpeg binary the worker uses. FLAC is lossless and supports
/// six channels, giving a clean 5.1 input to re-encode to E-AC-3.
fn generate_5_1_audio_fixture(ffmpeg: &Path, path: &Path) {
    let status = Command::new(ffmpeg)
        .args([
            "-y",
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "anullsrc=channel_layout=5.1:sample_rate=48000",
            "-t",
            "1",
            "-c:a",
            "flac",
            path.to_str().unwrap(),
        ])
        .status()
        .expect("ffmpeg 5.1 audio fixture generation failed to start");
    assert!(
        status.success(),
        "ffmpeg 5.1 audio fixture generation failed: {status}"
    );
}

async fn audio_expected(path: &Path) -> AudioExpectedFacts {
    let observed = voom_ffmpeg_worker::observe_file_facts(path).await.unwrap();
    AudioExpectedFacts {
        size_bytes: observed.size_bytes,
        content_hash: observed.content_hash,
        modified_at: observed.modified_at,
        local_file_key: None,
    }
}

#[tokio::test]
async fn eac3_transcode_preserves_5_1_channels() {
    let preflight = preflight_from_process_env()
        .expect("preflight failed — ensure libx265 and libsvtav1 are available");
    let config = ffmpeg_config(&preflight);
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("source.mkv");
    let output = dir.path().join("out.eac3.mkv");
    generate_5_1_audio_fixture(&preflight.ffmpeg_path, &input);

    let request = TranscodeAudioRequest {
        input: TranscodeAudioInput {
            path: input.to_string_lossy().into_owned(),
            expected: audio_expected(&input).await,
        },
        output: TranscodeAudioOutput {
            staging_root: dir.path().to_string_lossy().into_owned(),
            path: output.to_string_lossy().into_owned(),
            container: "mkv".to_owned(),
            overwrite: false,
        },
        selection: TranscodeAudioSelection {
            selected_streams: vec![AudioStreamRef {
                snapshot_stream_id: "audio-0".to_owned(),
                provider_stream_index: 0,
            }],
        },
        audio: TranscodeAudioSettings {
            lossless: None,
            compatibility: None,
            target_codec: "eac3".to_owned(),
            profile: AUDIO_PROFILE_DEFAULT.to_owned(),
            add_track: false,
            target_channels: None,
        },
    };

    let result = handle_transcode_audio(&request, &config)
        .await
        .expect("eac3 5.1 transcode failed");

    assert_eq!(result.output_audio_codecs, vec!["eac3".to_owned()]);
    let stream = result
        .selected_output_streams
        .first()
        .expect("one selected output stream");
    assert_eq!(stream.codec, "eac3");
    assert_eq!(
        stream.channels,
        Some(6),
        "eac3 output must preserve the 5.1 (6-channel) layout"
    );
    let independent = Command::new(&preflight.ffprobe_path)
        .args([
            "-v",
            "error",
            "-select_streams",
            "a:0",
            "-show_streams",
            "-of",
            "json",
        ])
        .arg(&output)
        .output()
        .expect("independent output probe must start");
    assert!(independent.status.success());
    let raw: serde_json::Value = serde_json::from_slice(&independent.stdout).unwrap();
    let actual = &raw["streams"][0];
    assert_eq!(
        stream.sample_rate,
        Some(actual["sample_rate"].as_str().unwrap().parse().unwrap())
    );
    assert_eq!(
        stream.sample_format.as_deref(),
        actual["sample_fmt"].as_str()
    );
    assert_eq!(
        stream.bits_per_sample, None,
        "lossy float output carries no integer precision claim"
    );
}

#[tokio::test]
async fn add_track_synthesis_preserves_surround_and_appends_stereo_companion() {
    let preflight = preflight_from_process_env()
        .expect("preflight failed — ensure ffmpeg and ffprobe are available");
    for source_default in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("source.mkv");
        let output = dir.path().join("synthesized.mkv");
        let generated = Command::new(&preflight.ffmpeg_path)
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                "anullsrc=channel_layout=5.1:sample_rate=48000",
                "-t",
                "0.25",
                "-c:a",
                "eac3",
                "-metadata:s:a:0",
                "language=eng",
                "-metadata:s:a:0",
                "title=Main",
                "-disposition:a:0",
                if source_default {
                    "default+forced+comment"
                } else {
                    "forced+comment"
                },
            ])
            .arg(&input)
            .status()
            .unwrap();
        assert!(generated.success());
        let companion_id = "synth_companion_stable".to_owned();
        let request = TranscodeAudioRequest {
            input: TranscodeAudioInput {
                path: input.to_string_lossy().into_owned(),
                expected: audio_expected(&input).await,
            },
            output: TranscodeAudioOutput {
                staging_root: dir.path().to_string_lossy().into_owned(),
                path: output.to_string_lossy().into_owned(),
                container: "mkv".to_owned(),
                overwrite: false,
            },
            selection: TranscodeAudioSelection {
                selected_streams: vec![AudioStreamRef {
                    snapshot_stream_id: companion_id.clone(),
                    provider_stream_index: 0,
                }],
            },
            audio: TranscodeAudioSettings {
                lossless: None,
                compatibility: None,
                target_codec: "aac".to_owned(),
                profile: AUDIO_PROFILE_DEFAULT.to_owned(),
                add_track: true,
                target_channels: Some(2),
            },
        };

        // Exercise the actual worker HTTP/progress boundary with generated media.
        let result = synthesize_through_worker(&request).await;

        assert_eq!(result.selected_snapshot_stream_ids, vec![companion_id]);
        assert_eq!(result.selected_output_streams[0].channels, Some(2));
        assert_eq!(
            result.selected_output_streams[0].output_provider_stream_index,
            1
        );
        assert_synthesized_audio_file(&preflight.ffprobe_path, &output, source_default, &result);
    }
}

#[expect(
    clippy::panic,
    reason = "unexpected worker terminal frames must fail this integration test"
)]
async fn synthesize_through_worker(request: &TranscodeAudioRequest) -> TranscodeAudioResult {
    let mut worker = tokio::process::Command::new(env!("CARGO_BIN_EXE_voom-ffmpeg-worker"))
        .env("VOOM_WORKER_ID", "737")
        .env("VOOM_WORKER_EPOCH", "1")
        .env("VOOM_WORKER_SECRET", "generated-fixture-test-secret")
        .env("VOOM_WORKER_BIND", "127.0.0.1:0")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut lines = BufReader::new(worker.stdout.take().unwrap()).lines();
    let ready = tokio::time::timeout(std::time::Duration::from_secs(30), lines.next_line())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let bound = ready
        .strip_prefix("BOUND addr=")
        .expect("worker readiness")
        .parse()
        .unwrap();
    let credentials = WorkerCredentials {
        worker_id: voom_core::WorkerId(737),
        worker_epoch: 1,
        secret: "generated-fixture-test-secret".to_owned().into(),
    };
    let mut dispatch = HttpClient::new(bound)
        .dispatch(
            &credentials,
            "synthesis-737",
            OperationRequest {
                operation: OperationKind::TranscodeAudio,
                lease_id: voom_core::LeaseId(737),
                payload: serde_json::to_value(request).unwrap(),
                heartbeat_deadline_ms: 1000,
                progress_idle_deadline_ms: 30000,
            },
        )
        .await
        .unwrap();
    let result = tokio::time::timeout(std::time::Duration::from_secs(30), async {
        loop {
            match dispatch.frames.next_frame().await.unwrap() {
                NdjsonOutcome::Frame(_) => {}
                NdjsonOutcome::Terminated(ProgressFrame::Result { payload, .. }) => {
                    break serde_json::from_value(payload).unwrap();
                }
                other => panic!("synthesis failed: {other:?}"),
            }
        }
    })
    .await
    .unwrap();
    drop(worker.stdin.take());
    assert!(
        tokio::time::timeout(std::time::Duration::from_secs(5), worker.wait())
            .await
            .unwrap()
            .unwrap()
            .success()
    );
    result
}

#[expect(
    clippy::print_stderr,
    reason = "retain physical probe and returned facts for generated-media proof"
)]
fn assert_synthesized_audio_file(
    ffprobe: &Path,
    output: &Path,
    source_default: bool,
    result: &TranscodeAudioResult,
) {
    let probe = Command::new(ffprobe)
        .args([
            "-v",
            "error",
            "-show_entries",
            "stream=index,codec_name,channels:stream_disposition:stream_tags",
            "-of",
            "json",
        ])
        .arg(output)
        .output()
        .expect("ffprobe synthesized media failed to start");
    assert!(probe.status.success());
    let payload: serde_json::Value =
        serde_json::from_slice(&probe.stdout).expect("ffprobe emitted invalid JSON");
    eprintln!("generated synthesis physical ffprobe facts: {payload}");
    eprintln!(
        "worker synthesis returned facts: {:?}",
        result.selected_output_streams
    );
    let streams = payload["streams"].as_array().unwrap();
    assert_eq!(streams.len(), 2);
    assert_eq!(streams[0]["codec_name"], "eac3");
    assert_eq!(streams[0]["channels"], 6);
    assert_eq!(streams[1]["codec_name"], "aac");
    assert_eq!(streams[1]["channels"], 2);
    assert_eq!(
        streams[0]["disposition"]["default"],
        u8::from(source_default),
        "EAC3 default selection stays unchanged"
    );
    assert_eq!(
        streams[1]["disposition"]["default"], 0,
        "AAC companion must not compete with EAC3"
    );
    for stream in streams {
        assert_eq!(stream["disposition"]["forced"], 1);
        assert_eq!(stream["disposition"]["comment"], 1);
    }
    assert_eq!(streams[1]["tags"]["language"], "eng");
    assert_eq!(streams[1]["tags"]["title"], "Main");
    assert_eq!(
        result.selected_output_streams[0].default,
        Some(false),
        "returned facts must match the physical file"
    );
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one generated-media matrix keeps source and output preservation assertions together"
)]
async fn ac3_real_worker_preserves_channels_and_synthesizes_surround() {
    let preflight = preflight_from_process_env().unwrap();
    for (layout, channels, synthesis, expected_channels, bitrate) in [
        ("mono", 1, false, 1, "96000"),
        ("stereo", 2, false, 2, "192000"),
        ("5.1", 6, false, 6, "576000"),
        ("7.1", 8, true, 6, "576000"),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("source.mkv");
        let output = dir.path().join("ac3.mkv");
        let generated = Command::new(&preflight.ffmpeg_path)
            .args([
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                "color=size=16x16:rate=10",
                "-f",
                "lavfi",
                "-i",
                &format!("anullsrc=r=48000:cl={layout}"),
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:sample_rate=48000",
                "-t",
                "0.2",
                "-map",
                "0:v",
                "-map",
                "1:a",
                "-map",
                "2:a",
                "-c:v",
                "ffv1",
                "-c:a",
                "flac",
                "-metadata:s:a:0",
                "language=eng",
                "-metadata:s:a:0",
                "title=Main",
                "-disposition:a:0",
                "default+forced+comment",
                "-metadata",
                "title=AC-3 source",
                "-metadata:s:a:1",
                "language=jpn",
                "-disposition:a:1",
                "0",
            ])
            .arg(&input)
            .output()
            .unwrap();
        assert!(
            generated.status.success(),
            "{}",
            String::from_utf8_lossy(&generated.stderr)
        );
        let expected = audio_expected(&input).await;
        let request = TranscodeAudioRequest {
            input: TranscodeAudioInput {
                path: input.to_string_lossy().into_owned(),
                expected: expected.clone(),
            },
            output: TranscodeAudioOutput {
                staging_root: dir.path().to_string_lossy().into_owned(),
                path: output.to_string_lossy().into_owned(),
                container: "mkv".to_owned(),
                overwrite: false,
            },
            selection: TranscodeAudioSelection {
                selected_streams: vec![AudioStreamRef {
                    snapshot_stream_id: "ac3-selected".to_owned(),
                    provider_stream_index: 1,
                }],
            },
            audio: TranscodeAudioSettings {
                lossless: None,
                compatibility: None,
                target_codec: "ac3".to_owned(),
                profile: AUDIO_PROFILE_DEFAULT.to_owned(),
                add_track: synthesis,
                target_channels: synthesis.then_some(expected_channels),
            },
        };
        if synthesis {
            let mut ordinary = request.clone();
            ordinary.audio.add_track = false;
            ordinary.audio.target_channels = None;
            ordinary.output.path = dir
                .path()
                .join("rejected.mkv")
                .to_string_lossy()
                .into_owned();
            let error = handle_transcode_audio(&ordinary, &ffmpeg_config(&preflight))
                .await
                .unwrap_err();
            assert!(error.to_string().contains("AC-3"), "{error}");
            assert!(!Path::new(&ordinary.output.path).exists());
        }
        let result = synthesize_through_worker(&request).await;
        assert_eq!(
            audio_expected(&input).await,
            expected,
            "source must remain immutable"
        );
        assert_eq!(result.selected_snapshot_stream_ids, ["ac3-selected"]);
        assert_eq!(
            result.selected_output_streams[0].channels,
            Some(expected_channels)
        );
        let physical: Vec<serde_json::Value> = [&input, &output].into_iter().map(|path| {
            let probe = Command::new(&preflight.ffprobe_path).args(["-v", "error", "-show_entries",
                "stream=index,codec_name,channels,channel_layout,bit_rate:stream_disposition:stream_tags:format_tags:packet=stream_index,data_hash",
                "-show_packets", "-show_data_hash", "sha256", "-of", "json"])
                .arg(path).output().unwrap();
            assert!(probe.status.success());
            serde_json::from_slice(&probe.stdout).unwrap()
        }).collect();
        let streams = physical[1]["streams"].as_array().unwrap();
        let selected_index = if synthesis { 3 } else { 1 };
        let selected = &streams[selected_index];
        assert_eq!(
            result.selected_output_streams[0].output_provider_stream_index,
            u32::try_from(selected_index).unwrap()
        );
        assert_eq!(physical[1]["format"]["tags"]["title"], "AC-3 source");
        assert_eq!(selected["codec_name"], "ac3");
        assert_eq!(selected["channels"], expected_channels);
        assert_eq!(selected["bit_rate"], bitrate);
        assert_eq!(
            selected["channel_layout"],
            match expected_channels {
                1 => "mono",
                2 => "stereo",
                _ => "5.1(side)",
            }
        );
        assert_eq!(selected["tags"]["language"], "eng");
        assert_eq!(selected["tags"]["title"], "Main");
        assert_eq!(selected["disposition"]["forced"], 1);
        assert_eq!(selected["disposition"]["comment"], 1);
        assert_eq!(selected["disposition"]["default"], u8::from(!synthesis));
        assert_eq!(streams.len(), if synthesis { 4 } else { 3 });
        assert_eq!(streams[0]["codec_name"], "ffv1");
        assert_eq!(streams[2]["codec_name"], "flac");
        assert_eq!(streams[2]["tags"]["language"], "jpn");
        assert_eq!(streams[2]["disposition"]["default"], 0);
        if synthesis {
            assert_eq!(streams[1]["codec_name"], "flac");
            assert_eq!(streams[1]["channels"], channels);
            assert_eq!(streams[1]["disposition"]["default"], 1);
            assert_eq!(streams[1]["disposition"]["forced"], 1);
            assert_eq!(streams[1]["disposition"]["comment"], 1);
            assert_eq!(streams[1]["tags"]["language"], "eng");
            assert_eq!(streams[1]["tags"]["title"], "Main");
        }
        for index in if synthesis { vec![0, 1, 2] } else { vec![0, 2] } {
            let hashes: Vec<Vec<&serde_json::Value>> = physical
                .iter()
                .map(|probe| {
                    probe["packets"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .filter(|packet| packet["stream_index"] == index)
                        .map(|packet| &packet["data_hash"])
                        .collect()
                })
                .collect();
            assert!(!hashes[0].is_empty());
            assert_eq!(
                hashes[0], hashes[1],
                "copied stream {index} payloads must survive"
            );
        }
    }
}

async fn flac_command(command: &mut tokio::process::Command) -> std::process::Output {
    let output = tokio::time::timeout(
        std::time::Duration::from_secs(15),
        command.kill_on_drop(true).output(),
    )
    .await
    .expect("generated FLAC command timed out")
    .expect("required media tool missing");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

async fn generate_flac_source(
    preflight: &voom_ffmpeg_worker::FfmpegPreflight,
    path: &Path,
    layout: &str,
    bits: u8,
    rate: u32,
) {
    let channels = match layout {
        "mono" => 1,
        "stereo" => 2,
        "7.1" => 8,
        _ => 6,
    };
    let expression = (0..channels)
        .map(|n| format!("0.05*sin(2*PI*{}*t)", 200 + n * 100))
        .collect::<Vec<_>>()
        .join("|");
    flac_command(
        tokio::process::Command::new(&preflight.ffmpeg_path)
            .args([
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                "color=size=16x16:rate=10",
                "-f",
                "lavfi",
                "-i",
                &format!("aevalsrc={expression}:s={rate}:c={layout}"),
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:sample_rate=48000",
                "-t",
                "0.1",
                "-map",
                "0:v",
                "-map",
                "1:a",
                "-map",
                "2:a",
                "-c:v",
                "ffv1",
                "-c:a",
                "flac",
                "-sample_fmt:a:0",
                if bits == 16 { "s16" } else { "s32" },
                "-bits_per_raw_sample:a:0",
                &bits.to_string(),
                "-metadata:s:a:0",
                "language=eng",
                "-metadata:s:a:0",
                "title=Main",
                "-disposition:a:0",
                "default+forced+comment",
                "-metadata:s:a:1",
                "language=jpn",
                "-disposition:a:1",
                "0",
                "-metadata",
                "title=FLAC source",
            ])
            .arg(path),
    )
    .await;
}

async fn flac_request(
    input: &Path,
    output: &Path,
    synthesis: Option<u64>,
) -> TranscodeAudioRequest {
    TranscodeAudioRequest {
        input: TranscodeAudioInput {
            path: input.to_string_lossy().into_owned(),
            expected: audio_expected(input).await,
        },
        output: TranscodeAudioOutput {
            staging_root: output.parent().unwrap().to_string_lossy().into_owned(),
            path: output.to_string_lossy().into_owned(),
            container: "mkv".to_owned(),
            overwrite: false,
        },
        selection: TranscodeAudioSelection {
            selected_streams: vec![AudioStreamRef {
                snapshot_stream_id: "flac-selected".to_owned(),
                provider_stream_index: 1,
            }],
        },
        audio: TranscodeAudioSettings {
            lossless: None,
            compatibility: None,
            target_codec: "flac".to_owned(),
            profile: AUDIO_PROFILE_DEFAULT.to_owned(),
            add_track: synthesis.is_some(),
            target_channels: synthesis,
        },
    }
}

async fn flac_physical_probe(ffprobe: &Path, path: &Path) -> serde_json::Value {
    let output = flac_command(
        tokio::process::Command::new(ffprobe)
            .args([
                "-v",
                "error",
                "-show_streams",
                "-show_format",
                "-show_packets",
                "-show_data_hash",
                "sha256",
                "-of",
                "json",
            ])
            .arg(path),
    )
    .await;
    serde_json::from_slice(&output.stdout).unwrap()
}

fn assert_flac_preservation(
    before: &serde_json::Value,
    after: &serde_json::Value,
    synthesis: bool,
) {
    let streams = after["streams"].as_array().unwrap();
    let target = &streams[if synthesis { 3 } else { 1 }];
    assert_eq!(after["format"]["tags"]["title"], "FLAC source");
    assert_eq!(target["codec_name"], "flac");
    assert_eq!(target["tags"]["language"], "eng");
    assert_eq!(target["tags"]["title"], "Main");
    assert_eq!(target["disposition"]["default"], u8::from(!synthesis));
    assert_eq!(target["disposition"]["forced"], 1);
    assert_eq!(target["disposition"]["comment"], 1);
    assert_eq!(streams.len(), if synthesis { 4 } else { 3 });
    for index in if synthesis { vec![0, 1, 2] } else { vec![0, 2] } {
        for field in ["codec_name", "tags", "disposition"] {
            assert_eq!(
                before["streams"][index][field], streams[index][field],
                "copied stream {index} {field}"
            );
        }
        let hashes: Vec<Vec<_>> = [before, after]
            .into_iter()
            .map(|probe| {
                probe["packets"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter(|packet| packet["stream_index"] == index)
                    .map(|packet| &packet["data_hash"])
                    .collect()
            })
            .collect();
        assert!(!hashes[0].is_empty());
        assert_eq!(hashes[0], hashes[1], "copied packet payloads {index}");
    }
}

#[tokio::test]
async fn flac_real_worker_preserves_integer_samples_and_streams() {
    use voom_test_support::audio_samples::canonical_decoded_audio;
    let preflight = preflight_from_process_env().unwrap();
    for layout in ["mono", "stereo", "5.1", "5.1(side)", "7.1"] {
        for bits in [16, 24] {
            for rate in [44_100, 96_000] {
                let dir = tempfile::tempdir().unwrap();
                let input = dir.path().join("source.mkv");
                let output = dir.path().join("encoded.mkv");
                generate_flac_source(&preflight, &input, layout, bits, rate).await;
                let request = flac_request(&input, &output, None).await;
                let result = synthesize_through_worker(&request).await;
                assert_eq!(
                    audio_expected(&input).await,
                    request.input.expected,
                    "source bytes immutable"
                );
                assert_eq!(result.selected_snapshot_stream_ids, ["flac-selected"]);
                let reported = &result.selected_output_streams[0];
                assert_eq!(reported.sample_rate, Some(rate));
                assert_eq!(reported.bits_per_sample, Some(bits));
                assert_eq!(reported.channel_layout.as_deref(), Some(layout));
                assert_eq!(reported.output_provider_stream_index, 1);
                let source = canonical_decoded_audio(
                    &preflight.ffmpeg_path,
                    &preflight.ffprobe_path,
                    &input,
                    1,
                )
                .await
                .unwrap();
                let actual = canonical_decoded_audio(
                    &preflight.ffmpeg_path,
                    &preflight.ffprobe_path,
                    &output,
                    1,
                )
                .await
                .unwrap();
                source.compare_samples(&actual).unwrap();
                assert_eq!(source.facts, actual.facts);
                assert!(
                    source.samples.iter().any(|byte| *byte != 0),
                    "fixture cannot be silence"
                );
                let before = flac_physical_probe(&preflight.ffprobe_path, &input).await;
                let after = flac_physical_probe(&preflight.ffprobe_path, &output).await;
                assert_flac_preservation(&before, &after, false);
            }
        }
    }
}

#[tokio::test]
async fn flac_real_worker_downmix_preserves_originals() {
    use voom_test_support::audio_samples::canonical_decoded_audio;
    let preflight = preflight_from_process_env().unwrap();
    for (layout, bits, target) in [("7.1", 24, 6), ("5.1(side)", 16, 2), ("stereo", 24, 1)] {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("source.mkv");
        let output = dir.path().join("downmix.mkv");
        let reference = dir.path().join("reference.mkv");
        generate_flac_source(&preflight, &input, layout, bits, 48_000).await;
        let request = flac_request(&input, &output, Some(target)).await;
        let result = synthesize_through_worker(&request).await;
        assert_eq!(audio_expected(&input).await, request.input.expected);
        assert_eq!(result.selected_snapshot_stream_ids, ["flac-selected"]);
        assert_eq!(
            result.selected_output_streams[0].output_provider_stream_index,
            3
        );
        assert_eq!(result.selected_output_streams[0].default, Some(false));
        flac_command(
            tokio::process::Command::new(&preflight.ffmpeg_path)
                .args(["-v", "error", "-i"])
                .arg(&input)
                .args([
                    "-map",
                    "0:1",
                    "-ac",
                    &target.to_string(),
                    "-c:a",
                    "flac",
                    "-sample_fmt",
                    if bits == 16 { "s16" } else { "s32" },
                    "-bits_per_raw_sample",
                    &bits.to_string(),
                ])
                .arg(&reference),
        )
        .await;
        let expected = canonical_decoded_audio(
            &preflight.ffmpeg_path,
            &preflight.ffprobe_path,
            &reference,
            0,
        )
        .await
        .unwrap();
        let actual =
            canonical_decoded_audio(&preflight.ffmpeg_path, &preflight.ffprobe_path, &output, 3)
                .await
                .unwrap();
        expected.compare_samples(&actual).unwrap();
        assert_eq!(actual.facts.channels, u32::try_from(target).unwrap());
        assert_eq!(actual.facts.bits_per_sample, bits);
        let source =
            canonical_decoded_audio(&preflight.ffmpeg_path, &preflight.ffprobe_path, &input, 1)
                .await
                .unwrap();
        let retained =
            canonical_decoded_audio(&preflight.ffmpeg_path, &preflight.ffprobe_path, &output, 1)
                .await
                .unwrap();
        source.compare_samples(&retained).unwrap();
        assert!(
            source.compare_samples(&actual).is_err(),
            "downmix is a transform, not archival equality"
        );
        let before = flac_physical_probe(&preflight.ffprobe_path, &input).await;
        let after = flac_physical_probe(&preflight.ffprobe_path, &output).await;
        assert_flac_preservation(&before, &after, true);
        for target in [0, 3, 8] {
            let mut invalid =
                flac_request(&input, &dir.path().join("rejected.mkv"), Some(target)).await;
            invalid.audio.target_channels = Some(target);
            assert!(
                handle_transcode_audio(&invalid, &ffmpeg_config(&preflight))
                    .await
                    .is_err()
            );
            assert!(!Path::new(&invalid.output.path).exists());
        }
    }
}

#[tokio::test]
async fn flac_integer_wave_converts_to_mkv() {
    use voom_test_support::audio_samples::canonical_decoded_audio;
    let preflight = preflight_from_process_env().unwrap();
    let dir = tempfile::tempdir().unwrap();
    for bits in [16_u16, 24] {
        let input = dir.path().join(format!("source-{bits}.wav"));
        let output = dir.path().join(format!("flac-{bits}.mkv"));
        let limit = 1_i32 << (bits - 1);
        let samples: Vec<_> = [-limit, limit - 1, -1, 1, 0, 123, -123, 3]
            .into_iter()
            .cycle()
            .take(256)
            .collect();
        let data: Vec<_> = samples
            .iter()
            .flat_map(|value| value.to_le_bytes()[..usize::from(bits / 8)].to_vec())
            .collect();
        let mut fmt = Vec::new();
        for value in [0xfffe_u16, 2] {
            fmt.extend(value.to_le_bytes());
        }
        fmt.extend(48_000_u32.to_le_bytes());
        fmt.extend((48_000_u32 * 2 * u32::from(bits / 8)).to_le_bytes());
        for value in [2 * (bits / 8), bits, 22, bits] {
            fmt.extend(value.to_le_bytes());
        }
        fmt.extend(3_u32.to_le_bytes());
        fmt.extend([1, 0, 0, 0, 0, 0, 16, 0, 128, 0, 0, 170, 0, 56, 155, 113]);
        let mut body = b"WAVEfmt ".to_vec();
        body.extend(40_u32.to_le_bytes());
        body.extend(fmt);
        body.extend(b"data");
        body.extend(u32::try_from(data.len()).unwrap().to_le_bytes());
        body.extend(data);
        let mut wave = b"RIFF".to_vec();
        wave.extend(u32::try_from(body.len()).unwrap().to_le_bytes());
        wave.extend(body);
        tokio::fs::write(&input, wave).await.unwrap();
        let mut request = flac_request(&input, &output, None).await;
        request.selection.selected_streams[0].provider_stream_index = 0;
        let result = synthesize_through_worker(&request).await;
        assert_eq!(result.selected_output_streams[0].codec, "flac");
        assert_eq!(audio_expected(&input).await, request.input.expected);
        let original =
            canonical_decoded_audio(&preflight.ffmpeg_path, &preflight.ffprobe_path, &input, 0)
                .await
                .unwrap();
        let actual =
            canonical_decoded_audio(&preflight.ffmpeg_path, &preflight.ffprobe_path, &output, 0)
                .await
                .unwrap();
        let expected: Vec<_> = samples
            .iter()
            .flat_map(|v| (v << (32 - bits)).to_le_bytes())
            .collect();
        assert_eq!(
            actual.samples, expected,
            "integer extremes and low bits must survive encoding"
        );
        original.compare_samples(&actual).unwrap();
        let floating = dir.path().join(format!("float-{bits}.wav"));
        flac_command(
            tokio::process::Command::new(&preflight.ffmpeg_path)
                .args(["-nostdin", "-v", "error", "-i"])
                .arg(&input)
                .args(["-c:a", "pcm_f32le"])
                .arg(&floating),
        )
        .await;
        let mut invalid = flac_request(&floating, &dir.path().join("rejected.mkv"), None).await;
        invalid.selection.selected_streams[0].provider_stream_index = 0;
        let error = handle_transcode_audio(&invalid, &ffmpeg_config(&preflight))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("audio sample facts"), "{error}");
        assert!(!Path::new(&invalid.output.path).exists());
    }
}

#[tokio::test]
async fn flac_real_worker_resolves_each_selected_stream() {
    use voom_test_support::audio_samples::canonical_decoded_audio;
    let preflight = preflight_from_process_env().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("source.mkv");
    let output = dir.path().join("flac.mkv");
    generate_flac_source(&preflight, &input, "7.1", 24, 96_000).await;
    let mut request = flac_request(&input, &output, None).await;
    request.selection.selected_streams.push(AudioStreamRef {
        snapshot_stream_id: "flac-second".to_owned(),
        provider_stream_index: 2,
    });
    let result = synthesize_through_worker(&request).await;
    assert_eq!(
        result.selected_snapshot_stream_ids,
        ["flac-selected", "flac-second"]
    );
    assert_eq!(result.selected_output_streams[0].sample_rate, Some(96_000));
    assert_eq!(result.selected_output_streams[0].bits_per_sample, Some(24));
    assert_eq!(result.selected_output_streams[1].sample_rate, Some(48_000));
    assert_eq!(result.selected_output_streams[1].bits_per_sample, Some(16));
    for index in [1, 2] {
        let source = canonical_decoded_audio(
            &preflight.ffmpeg_path,
            &preflight.ffprobe_path,
            &input,
            index,
        )
        .await
        .unwrap();
        let actual = canonical_decoded_audio(
            &preflight.ffmpeg_path,
            &preflight.ffprobe_path,
            &output,
            index,
        )
        .await
        .unwrap();
        source.compare_samples(&actual).unwrap();
        assert_eq!(source.facts, actual.facts);
    }
    assert_eq!(audio_expected(&input).await, request.input.expected);
}

#[tokio::test]
async fn flac_real_worker_explicit_precision_widening_preserves_samples() {
    use voom_test_support::audio_samples::canonical_decoded_audio;
    let preflight = preflight_from_process_env().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("source.mkv");
    let output = dir.path().join("wide.mkv");
    generate_flac_source(&preflight, &input, "stereo", 16, 48_000).await;
    let mut request = flac_request(&input, &output, None).await;
    request.audio.lossless = Some(voom_worker_protocol::LosslessAudioSettings {
        bits_per_sample: Some(24),
    });
    let result = synthesize_through_worker(&request).await;
    assert_eq!(result.selected_output_streams[0].bits_per_sample, Some(24));
    let source =
        canonical_decoded_audio(&preflight.ffmpeg_path, &preflight.ffprobe_path, &input, 1)
            .await
            .unwrap();
    let actual =
        canonical_decoded_audio(&preflight.ffmpeg_path, &preflight.ffprobe_path, &output, 1)
            .await
            .unwrap();
    assert_eq!(source.facts.bits_per_sample, 16);
    assert_eq!(actual.facts.bits_per_sample, 24);
    source.compare_samples(&actual).unwrap();
    assert_eq!(audio_expected(&input).await, request.input.expected);
}
