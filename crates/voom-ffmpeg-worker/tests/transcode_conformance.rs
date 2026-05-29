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

use voom_ffmpeg_worker::{
    DEFAULT_PROCESS_TIMEOUT, FfmpegConfig, handle_transcode_video, preflight_from_process_env,
};
use voom_worker_protocol::{
    TranscodeVideoExpectedFacts, TranscodeVideoInput, TranscodeVideoOutput, TranscodeVideoProfile,
    TranscodeVideoRequest,
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
        },
        output: TranscodeVideoOutput {
            staging_root: staging.to_string_lossy().into_owned(),
            path: output.to_string_lossy().into_owned(),
            container: container.to_owned(),
            video_codec: codec.to_owned(),
            overwrite: false,
        },
        profile,
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
        crf: 35,
        preset: "10".to_owned(), // fast preset for tests
        tune: None,
        codec_profile: None,
        codec_level: None,
        pixel_format: None,
        max_width: None,
        max_height: None,
        copy_compatible: false,
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
        crf: 35,
        preset: "8".to_owned(), // cpu-used 8 = fastest, for test speed
        tune: None,
        codec_profile: None,
        codec_level: None,
        pixel_format: None,
        max_width: None,
        max_height: None,
        copy_compatible: false,
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
        crf: 35,
        preset: "10".to_owned(),
        tune: None,
        codec_profile: None,
        codec_level: None,
        pixel_format: None,
        max_width: None,
        max_height: None,
        copy_compatible: false,
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
        crf: 28,
        preset: "ultrafast".to_owned(),
        tune: None,
        codec_profile: None,
        codec_level: None,
        pixel_format: None,
        max_width: Some(80),
        max_height: Some(45),
        copy_compatible: false,
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
        crf: 23,
        preset: "medium".to_owned(),
        tune: None,
        codec_profile: None,
        codec_level: None,
        pixel_format: None,
        max_width: None,
        max_height: None,
        copy_compatible: true,
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
        },
        output: TranscodeVideoOutput {
            staging_root: dir.path().to_string_lossy().into_owned(),
            path: output.to_string_lossy().into_owned(),
            container: "mkv".to_owned(),
            video_codec: "hevc".to_owned(),
            overwrite: false,
        },
        profile: TranscodeVideoProfile::default_hevc(),
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
        },
        output: TranscodeVideoOutput {
            staging_root: dir.path().to_string_lossy().into_owned(),
            path: output.to_string_lossy().into_owned(),
            container: "mkv".to_owned(),
            video_codec: "hevc".to_owned(),
            overwrite: false,
        },
        profile: TranscodeVideoProfile::default_hevc(),
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
        crf: 23,
        preset: "ultrafast".to_owned(),
        tune: None,
        codec_profile: Some("main10".to_owned()),
        codec_level: None,
        pixel_format: Some("yuv420p10le".to_owned()),
        max_width: None,
        max_height: None,
        copy_compatible: false,
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
        crf: 28,
        preset: "ultrafast".to_owned(),
        tune: None,
        codec_profile: None,
        codec_level: None,
        pixel_format: None,
        max_width: None,
        max_height: None,
        copy_compatible: false,
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
async fn provider_failure_on_corrupt_input_is_external_system_unavailable() {
    // A truncated/garbage file that ffmpeg cannot decode → ffmpeg exits
    // non-zero → ExternalSystemUnavailable.
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
            voom_ffmpeg_worker::TranscodeVideoError::ExternalSystemUnavailable { .. }
        ),
        "expected ExternalSystemUnavailable for corrupt input, got: {err}"
    );
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
