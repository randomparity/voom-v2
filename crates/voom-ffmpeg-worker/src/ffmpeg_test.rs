use std::path::{Path, PathBuf};

use voom_worker_protocol::{
    AudioStreamRef, ExtractAudioOutput, ExtractAudioRequest, TranscodeAudioOutput,
    TranscodeAudioRequest, TranscodeAudioSelection, TranscodeAudioSettings,
    TranscodeVideoExpectedFacts, TranscodeVideoInput, TranscodeVideoOutput, TranscodeVideoProfile,
    TranscodeVideoRequest,
};

use super::*;

// ---------------------------------------------------------------------------
// Helpers for the arg-capture seam
// ---------------------------------------------------------------------------

/// Writes a stub ffmpeg that records all its args one-per-line to args.txt in
/// the same directory, then writes "output" to the last arg (the output path).
fn arg_capture_ffmpeg(dir: &Path) -> (PathBuf, PathBuf) {
    let args_path = dir.join("args.txt");
    let ffmpeg = stub_bin(
        dir,
        "ffmpeg",
        &format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\nlast=\"\"\nfor arg in \"$@\"; do last=\"$arg\"; done\nprintf output > \"$last\"\n",
            args_path.display()
        ),
    );
    (ffmpeg, args_path)
}

/// Builds a hevc mkv probe stub returning yuv420p pixel format.
fn hevc_mkv_ffprobe(dir: &Path) -> PathBuf {
    stub_bin(
        dir,
        "ffprobe",
        "#!/bin/sh\ncat <<'JSON'\n{\"format\":{\"format_name\":\"matroska,webm\"},\"streams\":[{\"codec_type\":\"video\",\"codec_name\":\"hevc\",\"width\":1920,\"height\":1080,\"pix_fmt\":\"yuv420p\"}]}\nJSON\n",
    )
}

/// Builds a hevc mkv probe stub returning yuv420p10le pixel format.
fn hevc_mkv_ffprobe_10bit(dir: &Path) -> PathBuf {
    stub_bin(
        dir,
        "ffprobe",
        "#!/bin/sh\ncat <<'JSON'\n{\"format\":{\"format_name\":\"matroska,webm\"},\"streams\":[{\"codec_type\":\"video\",\"codec_name\":\"hevc\",\"width\":1920,\"height\":1080,\"pix_fmt\":\"yuv420p10le\"}]}\nJSON\n",
    )
}

/// Builds an av1 mp4 probe stub at `dir/ffprobe`.
fn av1_mp4_ffprobe(dir: &Path) -> PathBuf {
    stub_bin(
        dir,
        "ffprobe",
        "#!/bin/sh\nlast=\"\"\nfor arg in \"$@\"; do last=\"$arg\"; done\ncat <<'JSON'\n{\"format\":{\"format_name\":\"mp4\"},\"streams\":[{\"codec_type\":\"video\",\"codec_name\":\"av1\",\"width\":1920,\"height\":1080,\"pix_fmt\":\"yuv420p\"}]}\nJSON\n",
    )
}

fn basic_request(
    dir: &Path,
    container: &str,
    codec: &str,
    profile: TranscodeVideoProfile,
) -> TranscodeVideoRequest {
    let input = dir.join("input.mkv");
    TranscodeVideoRequest {
        input: TranscodeVideoInput {
            path: input.to_string_lossy().into_owned(),
            expected: TranscodeVideoExpectedFacts {
                size_bytes: 5,
                content_hash: "blake3:input".to_owned(),
                modified_at: None,
                local_file_key: None,
            },
        },
        output: TranscodeVideoOutput {
            staging_root: dir.to_string_lossy().into_owned(),
            path: dir.join("out.mkv").to_string_lossy().into_owned(),
            container: container.to_owned(),
            video_codec: codec.to_owned(),
            overwrite: false,
        },
        profile,
        copy_video: false,
    }
}

fn profile_x265_main10() -> TranscodeVideoProfile {
    TranscodeVideoProfile {
        name: "hevc-archive".to_owned(),
        target_codec: "hevc".to_owned(),
        encoder: "libx265".to_owned(),
        crf: 18,
        preset: "slow".to_owned(),
        tune: None,
        codec_profile: Some("main10".to_owned()),
        codec_level: None,
        pixel_format: Some("yuv420p10le".to_owned()),
        max_width: None,
        max_height: None,
        copy_compatible: false,
    }
}

fn profile_svtav1() -> TranscodeVideoProfile {
    TranscodeVideoProfile {
        name: "default-av1".to_owned(),
        target_codec: "av1".to_owned(),
        encoder: "libsvtav1".to_owned(),
        crf: 32,
        preset: "8".to_owned(),
        tune: None,
        codec_profile: None,
        codec_level: None,
        pixel_format: None,
        max_width: None,
        max_height: None,
        copy_compatible: false,
    }
}

fn profile_libaom() -> TranscodeVideoProfile {
    TranscodeVideoProfile {
        name: "av1-archive".to_owned(),
        target_codec: "av1".to_owned(),
        encoder: "libaom-av1".to_owned(),
        crf: 20,
        preset: "4".to_owned(),
        tune: None,
        codec_profile: None,
        codec_level: None,
        pixel_format: None,
        max_width: None,
        max_height: None,
        copy_compatible: false,
    }
}

fn profile_1080p() -> TranscodeVideoProfile {
    TranscodeVideoProfile {
        name: "hevc-1080p".to_owned(),
        target_codec: "hevc".to_owned(),
        encoder: "libx265".to_owned(),
        crf: 23,
        preset: "medium".to_owned(),
        tune: None,
        codec_profile: None,
        codec_level: None,
        pixel_format: None,
        max_width: Some(1920),
        max_height: Some(1080),
        copy_compatible: true,
    }
}

fn profile_x265() -> TranscodeVideoProfile {
    TranscodeVideoProfile::default_hevc()
}

fn output_mkv() -> (&'static str, &'static str) {
    ("mkv", "hevc")
}

fn output_mp4() -> (&'static str, &'static str) {
    ("mp4", "hevc")
}

fn output_mp4_av1() -> (&'static str, &'static str) {
    ("mp4", "av1")
}

// ---------------------------------------------------------------------------
// Golden arg-capture tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn libx265_command_uses_named_preset_and_optional_flags() {
    let dir = tempfile::tempdir().unwrap();
    let (ffmpeg, args_path) = arg_capture_ffmpeg(dir.path());
    // profile_x265_main10 has pixel_format = yuv420p10le, so ffprobe must match
    let ffprobe = hevc_mkv_ffprobe_10bit(dir.path());
    let input = dir.path().join("input.mkv");
    tokio::fs::write(&input, b"input").await.unwrap();
    let (container, codec) = output_mkv();
    let request = basic_request(dir.path(), container, codec, profile_x265_main10());
    let config = FfmpegConfig::new(ffmpeg, ffprobe, "test".to_owned(), DEFAULT_PROCESS_TIMEOUT);

    run_ffmpeg_transcode(&config, &request, 1920, 1080)
        .await
        .unwrap();

    let args = std::fs::read_to_string(args_path).unwrap();
    assert!(args.contains("-c:v\nlibx265\n"), "missing -c:v libx265");
    assert!(args.contains("-crf\n18\n"), "missing -crf 18");
    assert!(args.contains("-preset\nslow\n"), "missing -preset slow");
    assert!(
        args.contains("-profile:v\nmain10\n"),
        "missing -profile:v main10"
    );
    assert!(
        args.contains("-pix_fmt\nyuv420p10le\n"),
        "missing -pix_fmt yuv420p10le"
    );
    assert!(args.contains("-f\nmatroska\n"), "missing -f matroska");
}

#[test]
fn text_file_busy_is_detected_for_etxtbsy_only() {
    // ETXTBSY (os error 26) is the transient exec race we retry: another
    // thread's fork briefly inherited a writable fd to a freshly written
    // executable. ENOENT and other errors are real failures we must not retry.
    assert!(is_text_file_busy(&std::io::Error::from_raw_os_error(26)));
    assert!(!is_text_file_busy(&std::io::Error::from_raw_os_error(2)));
    assert!(!is_text_file_busy(&std::io::Error::other(
        "not an os error"
    )));
}

#[tokio::test]
async fn libsvtav1_command_uses_numeric_preset() {
    let dir = tempfile::tempdir().unwrap();
    let (ffmpeg, args_path) = arg_capture_ffmpeg(dir.path());
    let ffprobe = av1_mp4_ffprobe(dir.path());
    let input = dir.path().join("input.mkv");
    tokio::fs::write(&input, b"input").await.unwrap();
    let (container, codec) = output_mp4_av1();
    let request = basic_request(dir.path(), container, codec, profile_svtav1());
    let config = FfmpegConfig::new(ffmpeg, ffprobe, "test".to_owned(), DEFAULT_PROCESS_TIMEOUT);

    run_ffmpeg_transcode(&config, &request, 1920, 1080)
        .await
        .unwrap();

    let args = std::fs::read_to_string(args_path).unwrap();
    assert!(args.contains("-c:v\nlibsvtav1\n"), "missing -c:v libsvtav1");
    assert!(args.contains("-crf\n32\n"), "missing -crf 32");
    assert!(args.contains("-preset\n8\n"), "missing -preset 8");
    assert!(args.contains("-f\nmp4\n"), "missing -f mp4");
    assert!(args.contains("-tag:v\nav01\n"), "missing -tag:v av01");
}

#[tokio::test]
async fn libaom_command_sets_cpu_used_and_bitrate_zero() {
    let dir = tempfile::tempdir().unwrap();
    let (ffmpeg, args_path) = arg_capture_ffmpeg(dir.path());
    let ffprobe = stub_bin(
        dir.path(),
        "ffprobe",
        "#!/bin/sh\ncat <<'JSON'\n{\"format\":{\"format_name\":\"matroska,webm\"},\"streams\":[{\"codec_type\":\"video\",\"codec_name\":\"av1\",\"width\":1920,\"height\":1080,\"pix_fmt\":\"yuv420p\"}]}\nJSON\n",
    );
    let input = dir.path().join("input.mkv");
    tokio::fs::write(&input, b"input").await.unwrap();
    let request = basic_request(dir.path(), "mkv", "av1", profile_libaom());
    let config = FfmpegConfig::new(ffmpeg, ffprobe, "test".to_owned(), DEFAULT_PROCESS_TIMEOUT);

    run_ffmpeg_transcode(&config, &request, 1920, 1080)
        .await
        .unwrap();

    let args = std::fs::read_to_string(args_path).unwrap();
    assert!(
        args.contains("-c:v\nlibaom-av1\n"),
        "missing -c:v libaom-av1"
    );
    assert!(args.contains("-crf\n20\n"), "missing -crf 20");
    assert!(args.contains("-b:v\n0\n"), "missing -b:v 0");
    assert!(args.contains("-cpu-used\n4\n"), "missing -cpu-used 4");
}

#[tokio::test]
async fn mp4_hevc_tags_hvc1() {
    let dir = tempfile::tempdir().unwrap();
    let (ffmpeg, args_path) = arg_capture_ffmpeg(dir.path());
    let ffprobe = stub_bin(
        dir.path(),
        "ffprobe",
        "#!/bin/sh\ncat <<'JSON'\n{\"format\":{\"format_name\":\"mp4\"},\"streams\":[{\"codec_type\":\"video\",\"codec_name\":\"hevc\",\"width\":1920,\"height\":1080,\"pix_fmt\":\"yuv420p10le\"}]}\nJSON\n",
    );
    let input = dir.path().join("input.mkv");
    tokio::fs::write(&input, b"input").await.unwrap();
    let (container, codec) = output_mp4();
    let request = basic_request(dir.path(), container, codec, profile_x265_main10());
    let config = FfmpegConfig::new(ffmpeg, ffprobe, "test".to_owned(), DEFAULT_PROCESS_TIMEOUT);

    run_ffmpeg_transcode(&config, &request, 1920, 1080)
        .await
        .unwrap();

    let args = std::fs::read_to_string(args_path).unwrap();
    assert!(args.contains("-tag:v\nhvc1\n"), "missing -tag:v hvc1");
    assert!(args.contains("-f\nmp4\n"), "missing -f mp4");
}

#[tokio::test]
async fn downscale_applies_only_when_source_exceeds_cap() {
    // source 3840x2160, cap 1920x1080 -> scale filter present
    let dir = tempfile::tempdir().unwrap();
    let (ffmpeg, args_path) = arg_capture_ffmpeg(dir.path());
    let ffprobe = stub_bin(
        dir.path(),
        "ffprobe",
        "#!/bin/sh\ncat <<'JSON'\n{\"format\":{\"format_name\":\"mp4\"},\"streams\":[{\"codec_type\":\"video\",\"codec_name\":\"hevc\",\"width\":1920,\"height\":1080,\"pix_fmt\":\"yuv420p\"}]}\nJSON\n",
    );
    let input = dir.path().join("input.mkv");
    tokio::fs::write(&input, b"input").await.unwrap();
    let request = basic_request(dir.path(), "mp4", "hevc", profile_1080p());
    let config = FfmpegConfig::new(
        ffmpeg.clone(),
        ffprobe.clone(),
        "test".to_owned(),
        DEFAULT_PROCESS_TIMEOUT,
    );

    // 3840x2160 exceeds 1920x1080 cap → scale filter applied
    run_ffmpeg_transcode(&config, &request, 3840, 2160)
        .await
        .unwrap();
    let args = std::fs::read_to_string(&args_path).unwrap();
    assert!(
        args.contains("-vf\n"),
        "expected -vf when source exceeds cap"
    );
    assert!(
        args.lines()
            .any(|a| a.contains("scale=") && a.contains("min(")),
        "expected scale filter with min()"
    );

    // 1280x720 within cap → no scale filter
    let dir2 = tempfile::tempdir().unwrap();
    let (ffmpeg2, args_path2) = arg_capture_ffmpeg(dir2.path());
    let ffprobe2 = stub_bin(
        dir2.path(),
        "ffprobe",
        "#!/bin/sh\ncat <<'JSON'\n{\"format\":{\"format_name\":\"mp4\"},\"streams\":[{\"codec_type\":\"video\",\"codec_name\":\"hevc\",\"width\":1280,\"height\":720,\"pix_fmt\":\"yuv420p\"}]}\nJSON\n",
    );
    let input2 = dir2.path().join("input.mkv");
    tokio::fs::write(&input2, b"input").await.unwrap();
    let request2 = basic_request(dir2.path(), "mp4", "hevc", profile_1080p());
    let config2 = FfmpegConfig::new(
        ffmpeg2,
        ffprobe2,
        "test".to_owned(),
        DEFAULT_PROCESS_TIMEOUT,
    );

    run_ffmpeg_transcode(&config2, &request2, 1280, 720)
        .await
        .unwrap();
    let args2 = std::fs::read_to_string(args_path2).unwrap();
    assert!(
        !args2.contains("-vf\n"),
        "unexpected -vf when source within cap"
    );
}

#[tokio::test]
async fn copy_video_emits_stream_copy() {
    let dir = tempfile::tempdir().unwrap();
    // Write the arg-capture ffmpeg stub
    let (_, args_path) = arg_capture_ffmpeg(dir.path());
    let input = dir.path().join("input.mkv");
    tokio::fs::write(&input, b"input").await.unwrap();
    let mut request = basic_request(dir.path(), "mp4", "hevc", profile_x265());
    request.copy_video = true;
    // Use an ffprobe that returns mp4/hevc to satisfy output validation
    let ffprobe = stub_bin(
        dir.path(),
        "ffprobe",
        "#!/bin/sh\ncat <<'JSON'\n{\"format\":{\"format_name\":\"mp4\"},\"streams\":[{\"codec_type\":\"video\",\"codec_name\":\"hevc\",\"width\":1920,\"height\":1080,\"pix_fmt\":\"yuv420p\"}]}\nJSON\n",
    );
    let config = FfmpegConfig::new(
        dir.path().join("ffmpeg"),
        ffprobe,
        "test".to_owned(),
        DEFAULT_PROCESS_TIMEOUT,
    );
    run_ffmpeg_transcode(&config, &request, 1920, 1080)
        .await
        .unwrap();

    let args = std::fs::read_to_string(args_path).unwrap();
    assert!(args.contains("-c:v\ncopy\n"), "expected -c:v copy");
    assert!(
        !args.contains("-c:v\nlibx265\n"),
        "unexpected -c:v libx265 when copy_video"
    );
}

// ---------------------------------------------------------------------------
// Unit tests for command builder helpers
// ---------------------------------------------------------------------------

#[test]
fn video_codec_args_copy_video_emits_copy() {
    let profile = TranscodeVideoProfile::default_hevc();
    let args = video_codec_args(&profile, true).unwrap();
    let strs: Vec<&str> = args.iter().map(|a| a.to_str().unwrap()).collect();
    assert_eq!(strs, &["-c:v", "copy"]);
}

#[test]
fn video_codec_args_x265_emits_required_flags() {
    let profile = TranscodeVideoProfile::default_hevc();
    let args = video_codec_args(&profile, false).unwrap();
    let strs: Vec<&str> = args.iter().map(|a| a.to_str().unwrap()).collect();
    assert!(strs.contains(&"-c:v"));
    assert!(strs.contains(&"libx265"));
    assert!(strs.contains(&"-crf"));
    assert!(strs.contains(&"23"));
    assert!(strs.contains(&"-preset"));
    assert!(strs.contains(&"medium"));
}

#[test]
fn video_codec_args_x265_optional_flags_emitted_when_set() {
    let profile = profile_x265_main10();
    let args = video_codec_args(&profile, false).unwrap();
    let strs: Vec<&str> = args.iter().map(|a| a.to_str().unwrap()).collect();
    assert!(strs.contains(&"-profile:v"));
    assert!(strs.contains(&"main10"));
    assert!(strs.contains(&"-pix_fmt"));
    assert!(strs.contains(&"yuv420p10le"));
}

#[test]
fn video_codec_args_svtav1_emits_preset_and_no_cpu_used() {
    let profile = profile_svtav1();
    let args = video_codec_args(&profile, false).unwrap();
    let strs: Vec<&str> = args.iter().map(|a| a.to_str().unwrap()).collect();
    assert!(strs.contains(&"libsvtav1"));
    assert!(strs.contains(&"-preset"));
    assert!(strs.contains(&"8"));
    assert!(!strs.contains(&"-cpu-used"));
    assert!(!strs.contains(&"-b:v"));
}

#[test]
fn video_codec_args_libaom_emits_cpu_used_and_bitrate_zero() {
    let profile = profile_libaom();
    let args = video_codec_args(&profile, false).unwrap();
    let strs: Vec<&str> = args.iter().map(|a| a.to_str().unwrap()).collect();
    assert!(strs.contains(&"libaom-av1"));
    assert!(strs.contains(&"-cpu-used"));
    assert!(strs.contains(&"4"));
    assert!(strs.contains(&"-b:v"));
    assert!(strs.contains(&"0"));
    assert!(!strs.contains(&"-preset"));
}

#[test]
fn video_codec_args_unknown_encoder_is_error() {
    let mut profile = TranscodeVideoProfile::default_hevc();
    profile.encoder = "libx264".to_owned();
    let err = video_codec_args(&profile, false).unwrap_err();
    assert!(matches!(err, FfmpegError::OutputFactsMismatch(_)));
}

#[test]
fn container_args_mkv_emits_matroska() {
    let args = container_args("mkv", "hevc").unwrap();
    let strs: Vec<&str> = args.iter().map(|a| a.to_str().unwrap()).collect();
    assert_eq!(strs, &["-f", "matroska"]);
}

#[test]
fn container_args_mp4_hevc_tags_hvc1() {
    let args = container_args("mp4", "hevc").unwrap();
    let strs: Vec<&str> = args.iter().map(|a| a.to_str().unwrap()).collect();
    assert!(strs.contains(&"mp4"));
    assert!(strs.contains(&"-tag:v"));
    assert!(strs.contains(&"hvc1"));
}

#[test]
fn container_args_mp4_av1_tags_av01() {
    let args = container_args("mp4", "av1").unwrap();
    let strs: Vec<&str> = args.iter().map(|a| a.to_str().unwrap()).collect();
    assert!(strs.contains(&"mp4"));
    assert!(strs.contains(&"-tag:v"));
    assert!(strs.contains(&"av01"));
}

#[test]
fn container_args_mp4_unsupported_codec_is_error() {
    let err = container_args("mp4", "vp9").unwrap_err();
    assert!(matches!(err, FfmpegError::OutputFactsMismatch(_)));
}

#[test]
fn container_args_unsupported_container_is_error() {
    let err = container_args("webm", "hevc").unwrap_err();
    assert!(matches!(err, FfmpegError::OutputFactsMismatch(_)));
}

#[test]
fn audio_encoder_maps_supported_codecs_and_rejects_others() {
    assert_eq!(audio_encoder("aac").unwrap(), "aac");
    assert_eq!(audio_encoder("opus").unwrap(), "libopus");
    assert_eq!(audio_encoder("eac3").unwrap(), "eac3");
    assert!(matches!(
        audio_encoder("flac").unwrap_err(),
        FfmpegError::OutputFactsMismatch(_)
    ));
}

#[test]
fn scale_args_not_emitted_when_within_cap() {
    let profile = profile_1080p(); // max 1920x1080
    assert!(scale_args(&profile, 1280, 720).is_empty());
    assert!(scale_args(&profile, 1920, 1080).is_empty());
}

#[test]
fn scale_args_emitted_when_exceeds_cap() {
    let profile = profile_1080p(); // max 1920x1080
    let args = scale_args(&profile, 3840, 2160);
    let strs: Vec<&str> = args.iter().map(|a| a.to_str().unwrap()).collect();
    assert_eq!(strs[0], "-vf");
    assert!(strs[1].contains("scale="));
    assert!(strs[1].contains("min("));
    assert!(strs[1].contains("trunc("));
}

#[test]
fn scale_args_not_emitted_when_no_cap_set() {
    let profile = TranscodeVideoProfile::default_hevc(); // no max_width/max_height
    assert!(scale_args(&profile, 9999, 9999).is_empty());
}

#[test]
fn scale_args_emitted_for_width_only_cap_when_source_wider() {
    let mut profile = profile_1080p();
    profile.max_width = Some(1920);
    profile.max_height = None;
    let args = scale_args(&profile, 3840, 1080);
    let strs: Vec<&str> = args.iter().map(|a| a.to_str().unwrap()).collect();
    assert_eq!(strs[0], "-vf");
    assert!(strs[1].contains("min(1920,iw)"));
}

#[test]
fn scale_args_emitted_for_height_only_cap_when_source_taller() {
    let mut profile = profile_1080p();
    profile.max_width = None;
    profile.max_height = Some(1080);
    let args = scale_args(&profile, 1920, 2160);
    let strs: Vec<&str> = args.iter().map(|a| a.to_str().unwrap()).collect();
    assert_eq!(strs[0], "-vf");
    assert!(strs[1].contains("min(1080,ih)"));
}

#[test]
fn scale_args_not_emitted_for_single_cap_within_bound() {
    let mut profile = profile_1080p();
    profile.max_width = Some(1920);
    profile.max_height = None;
    assert!(scale_args(&profile, 1280, 9999).is_empty());
}

// ---------------------------------------------------------------------------
// Previously existing tests - updated for new run_ffmpeg_transcode signature
// ---------------------------------------------------------------------------

#[test]
fn ffmpeg_config_uses_explicit_process_timeout() {
    let dir = tempfile::tempdir().unwrap();
    let config = FfmpegConfig::new(
        dir.path().join("ffmpeg"),
        dir.path().join("ffprobe"),
        "ffmpeg version test".to_owned(),
        Duration::from_hours(1),
    );

    assert_eq!(config.process_timeout, Duration::from_hours(1));
}

#[tokio::test]
async fn ffmpeg_non_zero_exit_is_error() {
    let dir = tempfile::tempdir().unwrap();
    let ffmpeg = stub_bin(dir.path(), "ffmpeg", "#!/bin/sh\necho fail >&2\nexit 7\n");
    let ffprobe = stub_bin(dir.path(), "ffprobe", "#!/bin/sh\nexit 0\n");
    let input = dir.path().join("input.mkv");
    let output = dir.path().join("out.mkv");
    tokio::fs::write(&input, b"input").await.unwrap();
    let request = TranscodeVideoRequest {
        input: TranscodeVideoInput {
            path: input.to_string_lossy().into_owned(),
            expected: TranscodeVideoExpectedFacts {
                size_bytes: 5,
                content_hash: "blake3:input".to_owned(),
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

    let err = run_ffmpeg_transcode(
        &FfmpegConfig::new(
            ffmpeg,
            ffprobe,
            "ffmpeg version test".to_owned(),
            DEFAULT_PROCESS_TIMEOUT,
        ),
        &request,
        1920,
        1080,
    )
    .await
    .unwrap_err();

    assert!(matches!(err, FfmpegError::FfmpegFailed(_)));
}

#[tokio::test]
async fn ffmpeg_success_requires_hevc_matroska_probe() {
    let dir = tempfile::tempdir().unwrap();
    let ffmpeg = stub_bin(
        dir.path(),
        "ffmpeg",
        "#!/bin/sh\nlast=\"\"\nfor arg in \"$@\"; do last=\"$arg\"; done\nprintf output > \"$last\"\n",
    );
    let ffprobe = hevc_mkv_ffprobe(dir.path());
    let input = dir.path().join("input.mkv");
    let output = dir.path().join("out.mkv");
    tokio::fs::write(&input, b"input").await.unwrap();
    let request = TranscodeVideoRequest {
        input: TranscodeVideoInput {
            path: input.to_string_lossy().into_owned(),
            expected: TranscodeVideoExpectedFacts {
                size_bytes: 5,
                content_hash: "blake3:input".to_owned(),
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

    let probe = run_ffmpeg_transcode(
        &FfmpegConfig::new(
            ffmpeg,
            ffprobe,
            "ffmpeg version test".to_owned(),
            DEFAULT_PROCESS_TIMEOUT,
        ),
        &request,
        1920,
        1080,
    )
    .await
    .unwrap();

    assert_eq!(probe.container, "mkv");
    assert_eq!(probe.video_codec, "hevc");
}

// ---------------------------------------------------------------------------
// Audio tests (unchanged)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn audio_transcode_maps_all_streams_and_encodes_only_selected_audio_indexes() {
    let dir = tempfile::tempdir().unwrap();
    let args_path = dir.path().join("args.txt");
    let ffmpeg = stub_bin(
        dir.path(),
        "ffmpeg",
        &format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\nlast=\"\"\nfor arg in \"$@\"; do last=\"$arg\"; done\nprintf output > \"$last\"\n",
            args_path.display()
        ),
    );
    let ffprobe = ffprobe_audio_stub(dir.path(), "matroska", "opus", "opus");
    let input = dir.path().join("input.mkv");
    let output = dir.path().join("out.mkv");
    tokio::fs::write(&input, b"input").await.unwrap();

    run_ffmpeg_transcode_audio(
        &FfmpegConfig::new(
            ffmpeg,
            ffprobe,
            "ffmpeg version test".to_owned(),
            DEFAULT_PROCESS_TIMEOUT,
        ),
        &input,
        &output,
        &transcode_audio_request(dir.path(), &[1, 3], "opus"),
    )
    .await
    .unwrap();

    let args = std::fs::read_to_string(args_path).unwrap();
    assert!(args.contains("-map\n0\n"));
    assert!(args.contains("-c\ncopy\n"));
    assert!(args.contains("-c:a:0\nlibopus\n"));
    assert!(args.contains("-c:a:2\nlibopus\n"));
    assert!(!args.contains("-c:a:1\nlibopus\n"));
    // opus default profile is 48 kbps/channel: stream 1 is 6-channel (288k);
    // stream 3 reports no channel count and falls back to stereo (96k).
    assert!(args.contains("-b:a:0\n288k\n"));
    assert!(args.contains("-b:a:2\n96k\n"));
}

#[tokio::test]
async fn eac3_transcode_5_1_emits_eac3_encoder_and_channel_scaled_bitrate() {
    let dir = tempfile::tempdir().unwrap();
    let args_path = dir.path().join("args.txt");
    let ffmpeg = stub_bin(
        dir.path(),
        "ffmpeg",
        &format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\nlast=\"\"\nfor arg in \"$@\"; do last=\"$arg\"; done\nprintf output > \"$last\"\n",
            args_path.display()
        ),
    );
    let ffprobe = ffprobe_audio_stub(dir.path(), "matroska", "eac3", "eac3");
    let input = dir.path().join("input.mkv");
    let output = dir.path().join("out.mkv");
    tokio::fs::write(&input, b"input").await.unwrap();

    let probe = run_ffmpeg_transcode_audio(
        &FfmpegConfig::new(
            ffmpeg,
            ffprobe,
            "ffmpeg version test".to_owned(),
            DEFAULT_PROCESS_TIMEOUT,
        ),
        &input,
        &output,
        &transcode_audio_request(dir.path(), &[1], "eac3"),
    )
    .await
    .unwrap();

    let args = std::fs::read_to_string(args_path).unwrap();
    assert!(args.contains("-c:a:0\neac3\n"));
    // eac3 default profile is 96 kbps/channel; a 5.1 (6-channel) source → 576k.
    assert!(args.contains("-b:a:0\n576k\n"));
    // The 6-channel (5.1) layout is preserved and verified in the output probe.
    assert_eq!(
        probe.selected_output_streams[0].channels,
        Some(6),
        "eac3 5.1 output must preserve six channels"
    );
}

#[tokio::test]
async fn audio_extraction_maps_exactly_one_selected_audio_stream() {
    let dir = tempfile::tempdir().unwrap();
    let args_path = dir.path().join("args.txt");
    let ffmpeg = stub_bin(
        dir.path(),
        "ffmpeg",
        &format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\nlast=\"\"\nfor arg in \"$@\"; do last=\"$arg\"; done\nprintf output > \"$last\"\n",
            args_path.display()
        ),
    );
    let ffprobe = ffprobe_audio_stub(dir.path(), "ogg", "opus", "opus");
    let input = dir.path().join("input.mkv");
    let output = dir.path().join("out.ogg");
    tokio::fs::write(&input, b"input").await.unwrap();

    run_ffmpeg_extract_audio(
        &FfmpegConfig::new(
            ffmpeg,
            ffprobe,
            "ffmpeg version test".to_owned(),
            DEFAULT_PROCESS_TIMEOUT,
        ),
        &input,
        &output,
        &extract_audio_request(dir.path(), 3),
    )
    .await
    .unwrap();

    let args = std::fs::read_to_string(args_path).unwrap();
    assert!(args.contains("-map\n0:3\n"));
    assert!(!args.contains("-map\n0\n"));
    assert!(args.contains("-metadata:s:a:0\nsnapshot_stream_id=stream-3\n"));
}

#[tokio::test]
async fn opus_extraction_requests_ogg_output() {
    let dir = tempfile::tempdir().unwrap();
    let args_path = dir.path().join("args.txt");
    let ffmpeg = stub_bin(
        dir.path(),
        "ffmpeg",
        &format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\nlast=\"\"\nfor arg in \"$@\"; do last=\"$arg\"; done\nprintf output > \"$last\"\n",
            args_path.display()
        ),
    );
    let ffprobe = ffprobe_audio_stub(dir.path(), "ogg", "opus", "opus");
    let input = dir.path().join("input.mkv");
    let output = dir.path().join("out.ogg");
    tokio::fs::write(&input, b"input").await.unwrap();

    run_ffmpeg_extract_audio(
        &FfmpegConfig::new(
            ffmpeg,
            ffprobe,
            "ffmpeg version test".to_owned(),
            DEFAULT_PROCESS_TIMEOUT,
        ),
        &input,
        &output,
        &extract_audio_request(dir.path(), 1),
    )
    .await
    .unwrap();

    let args = std::fs::read_to_string(args_path).unwrap();
    assert!(args.contains("-f\nogg\n"));
    assert!(args.contains("-c:a\nlibopus\n"));
}

#[tokio::test]
async fn audio_transcode_writes_metadata_and_disposition_for_selected_streams() {
    let dir = tempfile::tempdir().unwrap();
    let args_path = dir.path().join("args.txt");
    let ffmpeg = stub_bin(
        dir.path(),
        "ffmpeg",
        &format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\nlast=\"\"\nfor arg in \"$@\"; do last=\"$arg\"; done\nprintf output > \"$last\"\n",
            args_path.display()
        ),
    );
    let ffprobe = ffprobe_audio_stub(dir.path(), "matroska", "opus", "opus");
    let input = dir.path().join("input.mkv");
    let output = dir.path().join("out.mkv");
    tokio::fs::write(&input, b"input").await.unwrap();

    run_ffmpeg_transcode_audio(
        &FfmpegConfig::new(
            ffmpeg,
            ffprobe,
            "ffmpeg version test".to_owned(),
            DEFAULT_PROCESS_TIMEOUT,
        ),
        &input,
        &output,
        &transcode_audio_request(dir.path(), &[1], "opus"),
    )
    .await
    .unwrap();

    let args = std::fs::read_to_string(args_path).unwrap();
    assert!(args.contains("-metadata:s:a:0\nlanguage=eng\n"));
    assert!(args.contains("-metadata:s:a:0\ntitle=Main\n"));
    assert!(args.contains("-disposition:a:0\ndefault\n"));
}

#[tokio::test]
async fn audio_extraction_writes_source_language_and_title_metadata_when_present() {
    let dir = tempfile::tempdir().unwrap();
    let args_path = dir.path().join("args.txt");
    let ffmpeg = stub_bin(
        dir.path(),
        "ffmpeg",
        &format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\nlast=\"\"\nfor arg in \"$@\"; do last=\"$arg\"; done\nprintf output > \"$last\"\n",
            args_path.display()
        ),
    );
    let ffprobe = ffprobe_audio_stub(dir.path(), "ogg", "opus", "opus");
    let input = dir.path().join("input.mkv");
    let output = dir.path().join("out.ogg");
    tokio::fs::write(&input, b"input").await.unwrap();

    run_ffmpeg_extract_audio(
        &FfmpegConfig::new(
            ffmpeg,
            ffprobe,
            "ffmpeg version test".to_owned(),
            DEFAULT_PROCESS_TIMEOUT,
        ),
        &input,
        &output,
        &extract_audio_request(dir.path(), 1),
    )
    .await
    .unwrap();

    let args = std::fs::read_to_string(args_path).unwrap();
    assert!(args.contains("-metadata:s:a:0\nlanguage=eng\n"));
    assert!(args.contains("-metadata:s:a:0\ntitle=Main\n"));
}

// ---------------------------------------------------------------------------
// Audio helpers
// ---------------------------------------------------------------------------

fn transcode_audio_request(
    root: &Path,
    selected: &[u32],
    target_codec: &str,
) -> TranscodeAudioRequest {
    TranscodeAudioRequest {
        input: voom_worker_protocol::TranscodeAudioInput {
            path: root.join("input.mkv").to_string_lossy().into_owned(),
            expected: voom_worker_protocol::AudioExpectedFacts {
                size_bytes: 5,
                content_hash: "blake3:input".to_owned(),
                modified_at: None,
                local_file_key: None,
            },
        },
        output: TranscodeAudioOutput {
            staging_root: root.to_string_lossy().into_owned(),
            path: root.join("out.mkv").to_string_lossy().into_owned(),
            container: "mkv".to_owned(),
            overwrite: false,
        },
        selection: TranscodeAudioSelection {
            selected_streams: selected
                .iter()
                .map(|index| AudioStreamRef {
                    snapshot_stream_id: format!("stream-{index}"),
                    provider_stream_index: *index,
                })
                .collect(),
        },
        audio: TranscodeAudioSettings {
            target_codec: target_codec.to_owned(),
            profile: voom_worker_protocol::AUDIO_PROFILE_DEFAULT.to_owned(),
        },
    }
}

fn extract_audio_request(root: &Path, selected: u32) -> ExtractAudioRequest {
    ExtractAudioRequest {
        input: voom_worker_protocol::ExtractAudioInput {
            path: root.join("input.mkv").to_string_lossy().into_owned(),
            expected: voom_worker_protocol::AudioExpectedFacts {
                size_bytes: 5,
                content_hash: "blake3:input".to_owned(),
                modified_at: None,
                local_file_key: None,
            },
        },
        output: ExtractAudioOutput {
            staging_root: root.to_string_lossy().into_owned(),
            path: root.join("out.ogg").to_string_lossy().into_owned(),
            container: "ogg".to_owned(),
            audio_codec: "opus".to_owned(),
            overwrite: false,
        },
        selection: AudioStreamRef {
            snapshot_stream_id: format!("stream-{selected}"),
            provider_stream_index: selected,
        },
    }
}

fn ffprobe_audio_stub(
    dir: &Path,
    container: &str,
    first_codec: &str,
    third_codec: &str,
) -> PathBuf {
    stub_bin(
        dir,
        "ffprobe",
        &format!(
            "#!/bin/sh\ncat <<'JSON'\n{{\"format\":{{\"format_name\":\"{container}\"}},\"streams\":[{{\"index\":1,\"codec_type\":\"audio\",\"codec_name\":\"{first_codec}\",\"channels\":6,\"tags\":{{\"language\":\"eng\",\"title\":\"Main\"}},\"disposition\":{{\"default\":1,\"forced\":0,\"comment\":0}}}},{{\"index\":2,\"codec_type\":\"audio\",\"codec_name\":\"aac\",\"channels\":2}},{{\"index\":3,\"codec_type\":\"audio\",\"codec_name\":\"{third_codec}\",\"tags\":{{\"language\":\"jpn\",\"title\":\"Commentary\"}},\"disposition\":{{\"default\":0,\"forced\":0,\"comment\":1}}}}]}}\nJSON\n"
        ),
    )
}

fn stub_bin(dir: &Path, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, body).unwrap();
    make_executable(&path);
    path
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
