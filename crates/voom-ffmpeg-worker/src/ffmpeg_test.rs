use std::path::{Path, PathBuf};

use voom_worker_protocol::{
    AudioStreamRef, ExtractAudioOutput, ExtractAudioRequest, NvidiaVideoAcceleratorDescriptor,
    TranscodeAudioOutput, TranscodeAudioRequest, TranscodeAudioSelection, TranscodeAudioSettings,
    TranscodeVideoExpectedFacts, TranscodeVideoInput, TranscodeVideoOutput, TranscodeVideoProfile,
    TranscodeVideoRequest, VaapiVideoAcceleratorDescriptor, VideoHardwareAssignment,
    VideoToolboxDecodeCapability, VideoToolboxVideoAcceleratorDescriptor,
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
            video_codec: None,
            video_pixel_format: None,
        },
        output: TranscodeVideoOutput {
            staging_root: dir.to_string_lossy().into_owned(),
            path: dir.join("out.mkv").to_string_lossy().into_owned(),
            container: container.to_owned(),
            video_codec: codec.to_owned(),
            overwrite: false,
        },
        profile,
        hardware_assignment: None,
        copy_video: false,
    }
}

fn video_source(
    width: u32,
    height: u32,
    forced_subtitle_ordinals: &[usize],
) -> VideoTranscodeInput<'_> {
    VideoTranscodeInput {
        width,
        height,
        codec: "hevc",
        forced_subtitle_ordinals,
    }
}

fn profile_x265_main10() -> TranscodeVideoProfile {
    TranscodeVideoProfile {
        name: "hevc-archive".to_owned(),
        target_codec: "hevc".to_owned(),
        encoder: "libx265".to_owned(),
        crf: Some(18),
        cq: None,
        qp: None,
        bitrate_kbps: None,
        preset: Some("slow".to_owned()),
        tune: None,
        codec_profile: Some("main10".to_owned()),
        codec_level: None,
        pixel_format: Some("yuv420p10le".to_owned()),
        max_width: None,
        max_height: None,
        copy_compatible: false,
        decode: voom_core::VideoDecodeMode::default(),
    }
}

fn profile_svtav1() -> TranscodeVideoProfile {
    TranscodeVideoProfile {
        name: "default-av1".to_owned(),
        target_codec: "av1".to_owned(),
        encoder: "libsvtav1".to_owned(),
        crf: Some(32),
        cq: None,
        qp: None,
        bitrate_kbps: None,
        preset: Some("8".to_owned()),
        tune: None,
        codec_profile: None,
        codec_level: None,
        pixel_format: None,
        max_width: None,
        max_height: None,
        copy_compatible: false,
        decode: voom_core::VideoDecodeMode::default(),
    }
}

fn profile_libaom() -> TranscodeVideoProfile {
    TranscodeVideoProfile {
        name: "av1-archive".to_owned(),
        target_codec: "av1".to_owned(),
        encoder: "libaom-av1".to_owned(),
        crf: Some(20),
        cq: None,
        qp: None,
        bitrate_kbps: None,
        preset: Some("4".to_owned()),
        tune: None,
        codec_profile: None,
        codec_level: None,
        pixel_format: None,
        max_width: None,
        max_height: None,
        copy_compatible: false,
        decode: voom_core::VideoDecodeMode::default(),
    }
}

fn profile_1080p() -> TranscodeVideoProfile {
    TranscodeVideoProfile {
        name: "hevc-1080p".to_owned(),
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
        max_width: Some(1920),
        max_height: Some(1080),
        copy_compatible: true,
        decode: voom_core::VideoDecodeMode::default(),
    }
}

fn profile_x265() -> TranscodeVideoProfile {
    TranscodeVideoProfile::default_hevc()
}

fn profile_nvenc(decode: voom_core::VideoDecodeMode) -> TranscodeVideoProfile {
    TranscodeVideoProfile {
        name: "hevc-nvenc".to_owned(),
        target_codec: "hevc".to_owned(),
        encoder: "hevc_nvenc".to_owned(),
        crf: None,
        cq: Some(22),
        qp: None,
        bitrate_kbps: None,
        preset: Some("p5".to_owned()),
        tune: Some("hq".to_owned()),
        codec_profile: Some("main".to_owned()),
        codec_level: Some("5.1".to_owned()),
        pixel_format: Some("yuv420p".to_owned()),
        max_width: None,
        max_height: None,
        decode,
        copy_compatible: false,
    }
}

fn nvidia_descriptor() -> NvidiaVideoAcceleratorDescriptor {
    NvidiaVideoAcceleratorDescriptor {
        hardware_token: "nvidia:GPU-aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee".to_owned(),
        device_uuid: "GPU-aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee".to_owned(),
        device_name: "Test NVIDIA GPU".to_owned(),
        driver_version: "595.80".to_owned(),
        encoders: vec!["hevc_nvenc".to_owned()],
        decoders: vec![
            "h264_cuvid".to_owned(),
            "hevc_cuvid".to_owned(),
            "av1_cuvid".to_owned(),
        ],
        max_sessions: 2,
    }
}

fn nvidia_request(dir: &Path, decode: voom_core::VideoDecodeMode) -> TranscodeVideoRequest {
    let descriptor = nvidia_descriptor();
    let mut request = basic_request(dir, "mkv", "hevc", profile_nvenc(decode));
    request.hardware_assignment = Some(VideoHardwareAssignment::nvidia(
        descriptor.hardware_token,
        descriptor.device_uuid,
    ));
    request
}

fn videotoolbox_descriptor() -> VideoToolboxVideoAcceleratorDescriptor {
    VideoToolboxVideoAcceleratorDescriptor {
        hardware_token: "videotoolbox:0123456789abcdef".to_owned(),
        resource_id: "0123456789abcdef".to_owned(),
        model_identifier: "Mac17,6".to_owned(),
        chip_name: "Apple M5 Max".to_owned(),
        macos_version: "26.5.2".to_owned(),
        macos_build: "25F90".to_owned(),
        encoders: vec![
            "h264_videotoolbox".to_owned(),
            "hevc_videotoolbox".to_owned(),
        ],
        decoders: vec![
            VideoToolboxDecodeCapability {
                codec: "h264".to_owned(),
                pixel_formats: vec!["yuv420p".to_owned()],
            },
            VideoToolboxDecodeCapability {
                codec: "hevc".to_owned(),
                pixel_formats: vec!["yuv420p".to_owned(), "yuv420p10le".to_owned()],
            },
        ],
        max_sessions: 4,
    }
}

fn profile_videotoolbox(
    encoder: &str,
    target_codec: &str,
    codec_profile: &str,
    codec_level: Option<&str>,
    pixel_format: &str,
    decode: voom_core::VideoDecodeMode,
) -> TranscodeVideoProfile {
    TranscodeVideoProfile {
        name: format!("{target_codec}-videotoolbox"),
        target_codec: target_codec.to_owned(),
        encoder: encoder.to_owned(),
        crf: None,
        cq: None,
        qp: None,
        bitrate_kbps: Some(8_000),
        preset: Some("default".to_owned()),
        tune: None,
        codec_profile: Some(codec_profile.to_owned()),
        codec_level: codec_level.map(str::to_owned),
        pixel_format: Some(pixel_format.to_owned()),
        max_width: None,
        max_height: None,
        copy_compatible: false,
        decode,
    }
}

fn videotoolbox_request(dir: &Path, profile: TranscodeVideoProfile) -> TranscodeVideoRequest {
    let target_codec = profile.target_codec.clone();
    let mut request = basic_request(dir, "mkv", &target_codec, profile);
    let descriptor = videotoolbox_descriptor();
    request.hardware_assignment = Some(VideoHardwareAssignment::video_toolbox(
        descriptor.hardware_token,
        descriptor.resource_id,
    ));
    request
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

    run_ffmpeg_transcode(&config, &request, video_source(1920, 1080, &[]))
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

#[tokio::test]
async fn nvenc_software_decode_command_uploads_to_bound_cuda_device() {
    let dir = tempfile::tempdir().unwrap();
    let (ffmpeg, args_path) = arg_capture_ffmpeg(dir.path());
    let ffprobe = hevc_mkv_ffprobe(dir.path());
    tokio::fs::write(dir.path().join("input.mkv"), b"input")
        .await
        .unwrap();
    let request = nvidia_request(dir.path(), voom_core::VideoDecodeMode::default());
    let config = FfmpegConfig::new(ffmpeg, ffprobe, "test".to_owned(), DEFAULT_PROCESS_TIMEOUT)
        .with_accelerator(nvidia_descriptor());

    run_ffmpeg_transcode(&config, &request, video_source(1920, 1080, &[]))
        .await
        .unwrap();

    let args = std::fs::read_to_string(args_path).unwrap();
    assert!(
        args.contains("-vf\nformat=nv12,hwupload_cuda=device=0\n"),
        "{args}"
    );
    assert!(args.contains("-c:v\nhevc_nvenc\n"), "{args}");
    assert!(args.contains("-rc\nvbr\n-cq\n22\n-b:v\n0\n"), "{args}");
    assert!(!args.lines().any(|arg| arg == "-gpu"), "{args}");
}

#[tokio::test]
async fn nvenc_cuda_decode_command_pins_decoder_and_zero_copy_filter() {
    let dir = tempfile::tempdir().unwrap();
    let (ffmpeg, args_path) = arg_capture_ffmpeg(dir.path());
    let ffprobe = hevc_mkv_ffprobe(dir.path());
    tokio::fs::write(dir.path().join("input.mkv"), b"input")
        .await
        .unwrap();
    let request = nvidia_request(dir.path(), voom_core::VideoDecodeMode::nvidia());
    let config = FfmpegConfig::new(ffmpeg, ffprobe, "test".to_owned(), DEFAULT_PROCESS_TIMEOUT)
        .with_accelerator(nvidia_descriptor());
    let source = VideoTranscodeInput {
        width: 1920,
        height: 1080,
        codec: "h264",
        forced_subtitle_ordinals: &[],
    };

    run_ffmpeg_transcode(&config, &request, source)
        .await
        .unwrap();

    let args = std::fs::read_to_string(args_path).unwrap();
    assert!(
        args.contains(
            "-hwaccel\ncuda\n-hwaccel_device\n0\n-hwaccel_output_format\ncuda\n\
             -c:v\nh264_cuvid\n-i\n"
        ),
        "{args}"
    );
    assert!(args.contains("-vf\nscale_cuda=format=nv12\n"), "{args}");
    assert!(args.contains("-c:v\nhevc_nvenc\n"), "{args}");
    assert!(!args.lines().any(|arg| arg == "-gpu"), "{args}");
}

#[tokio::test]
async fn videotoolbox_software_decode_uses_explicit_format_and_hardware_only_encode() {
    let dir = tempfile::tempdir().unwrap();
    let (ffmpeg, args_path) = arg_capture_ffmpeg(dir.path());
    let ffprobe = hevc_mkv_ffprobe(dir.path());
    tokio::fs::write(dir.path().join("input.mkv"), b"input")
        .await
        .unwrap();
    let profile = profile_videotoolbox(
        "hevc_videotoolbox",
        "hevc",
        "main",
        None,
        "yuv420p",
        voom_core::VideoDecodeMode::default(),
    );
    let request = videotoolbox_request(dir.path(), profile);
    let config = FfmpegConfig::new(ffmpeg, ffprobe, "test".to_owned(), DEFAULT_PROCESS_TIMEOUT)
        .with_videotoolbox_device(videotoolbox_descriptor());

    run_ffmpeg_transcode(&config, &request, video_source(1920, 1080, &[]))
        .await
        .unwrap();

    let args = std::fs::read_to_string(args_path).unwrap();
    assert!(args.contains("-vf\nformat=nv12\n"), "{args}");
    assert!(
        args.contains("-c:v\nhevc_videotoolbox\n-allow_sw\n0\n-b:v\n8000k\n"),
        "{args}"
    );
    assert!(!args.contains("-hwaccel\n"), "{args}");
    assert!(!args.contains("allow_sw\n1"), "{args}");
}

#[tokio::test]
async fn videotoolbox_decode_without_scaling_keeps_hardware_frames() {
    let dir = tempfile::tempdir().unwrap();
    let (ffmpeg, args_path) = arg_capture_ffmpeg(dir.path());
    let ffprobe = hevc_mkv_ffprobe_10bit(dir.path());
    tokio::fs::write(dir.path().join("input.mkv"), b"input")
        .await
        .unwrap();
    let profile = profile_videotoolbox(
        "hevc_videotoolbox",
        "hevc",
        "main10",
        None,
        "yuv420p10le",
        voom_core::VideoDecodeMode::video_toolbox(),
    );
    let request = videotoolbox_request(dir.path(), profile);
    let config = FfmpegConfig::new(ffmpeg, ffprobe, "test".to_owned(), DEFAULT_PROCESS_TIMEOUT)
        .with_videotoolbox_device(videotoolbox_descriptor());

    run_ffmpeg_transcode(&config, &request, video_source(1920, 1080, &[]))
        .await
        .unwrap();

    let args = std::fs::read_to_string(args_path).unwrap();
    assert!(
        args.contains("-hwaccel\nvideotoolbox\n-hwaccel_output_format\nvideotoolbox_vld\n-i\n"),
        "{args}"
    );
    assert!(!args.contains("-vf\n"), "{args}");
    for forbidden in ["hwdownload", "hwupload", "format=", "scale="] {
        assert!(!args.contains(forbidden), "{args}");
    }
}

#[tokio::test]
async fn videotoolbox_decode_downscale_uses_scale_vt_only() {
    let dir = tempfile::tempdir().unwrap();
    let (ffmpeg, args_path) = arg_capture_ffmpeg(dir.path());
    let ffprobe = hevc_mkv_ffprobe(dir.path());
    tokio::fs::write(dir.path().join("input.mkv"), b"input")
        .await
        .unwrap();
    let mut profile = profile_videotoolbox(
        "hevc_videotoolbox",
        "hevc",
        "main",
        None,
        "yuv420p",
        voom_core::VideoDecodeMode::video_toolbox(),
    );
    profile.max_width = Some(1920);
    profile.max_height = Some(1080);
    let request = videotoolbox_request(dir.path(), profile);
    let config = FfmpegConfig::new(ffmpeg, ffprobe, "test".to_owned(), DEFAULT_PROCESS_TIMEOUT)
        .with_videotoolbox_device(videotoolbox_descriptor());

    run_ffmpeg_transcode(&config, &request, video_source(3840, 2160, &[]))
        .await
        .unwrap();

    let args = std::fs::read_to_string(args_path).unwrap();
    assert!(args.contains("-vf\nscale_vt=w=1920:h=1080\n"), "{args}");
    for forbidden in ["hwdownload", "hwupload", "format=", "scale="] {
        assert!(!args.contains(forbidden), "{args}");
    }
}

#[test]
fn h264_mp4_uses_avc1_tag() {
    assert_eq!(
        container_args("mp4", "h264").unwrap(),
        ["-f", "mp4", "-tag:v", "avc1"].map(OsString::from)
    );
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

    run_ffmpeg_transcode(&config, &request, video_source(1920, 1080, &[]))
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

    run_ffmpeg_transcode(&config, &request, video_source(1920, 1080, &[]))
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

    run_ffmpeg_transcode(&config, &request, video_source(1920, 1080, &[]))
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
    run_ffmpeg_transcode(&config, &request, video_source(3840, 2160, &[]))
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

    run_ffmpeg_transcode(&config2, &request2, video_source(1280, 720, &[]))
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
    run_ffmpeg_transcode(&config, &request, video_source(1920, 1080, &[]))
        .await
        .unwrap();

    let args = std::fs::read_to_string(args_path).unwrap();
    assert!(args.contains("-c:v\ncopy\n"), "expected -c:v copy");
    assert!(
        !args.contains("-c:v\nlibx265\n"),
        "unexpected -c:v libx265 when copy_video"
    );
}

#[tokio::test]
async fn video_transcode_restores_forced_subtitle_dispositions() {
    let dir = tempfile::tempdir().unwrap();
    let (ffmpeg, args_path) = arg_capture_ffmpeg(dir.path());
    let ffprobe = hevc_mkv_ffprobe(dir.path());
    let input = dir.path().join("input.mkv");
    tokio::fs::write(&input, b"input").await.unwrap();
    let request = basic_request(dir.path(), "mkv", "hevc", profile_x265());
    let config = FfmpegConfig::new(ffmpeg, ffprobe, "test".to_owned(), DEFAULT_PROCESS_TIMEOUT);

    run_ffmpeg_transcode(&config, &request, video_source(1920, 1080, &[1]))
        .await
        .unwrap();

    let args = std::fs::read_to_string(args_path).unwrap();
    assert!(args.contains("-disposition:s:1\n+forced\n"), "{args}");
}

#[tokio::test]
async fn input_probe_collects_forced_subtitle_ordinals() {
    let dir = tempfile::tempdir().unwrap();
    let ffprobe = stub_bin(
        dir.path(),
        "ffprobe",
        "#!/bin/sh\ncat <<'JSON'\n{\"streams\":[{\"codec_type\":\"video\",\"codec_name\":\"h264\",\"width\":1920,\"height\":1080,\"pix_fmt\":\"yuv420p\"},{\"codec_type\":\"subtitle\",\"disposition\":{\"forced\":0}},{\"codec_type\":\"audio\",\"disposition\":{\"forced\":1}},{\"codec_type\":\"subtitle\",\"disposition\":{\"forced\":1}}]}\nJSON\n",
    );
    let config = FfmpegConfig::new(
        dir.path().join("ffmpeg"),
        ffprobe,
        "test".to_owned(),
        DEFAULT_PROCESS_TIMEOUT,
    );

    let probe = probe_input(&config, &dir.path().join("input.mkv"))
        .await
        .unwrap();

    assert_eq!(probe.forced_subtitle_ordinals, [1]);
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

    let err = run_ffmpeg_transcode(
        &FfmpegConfig::new(
            ffmpeg,
            ffprobe,
            "ffmpeg version test".to_owned(),
            DEFAULT_PROCESS_TIMEOUT,
        ),
        &request,
        video_source(1920, 1080, &[]),
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

    let probe = run_ffmpeg_transcode(
        &FfmpegConfig::new(
            ffmpeg,
            ffprobe,
            "ffmpeg version test".to_owned(),
            DEFAULT_PROCESS_TIMEOUT,
        ),
        &request,
        video_source(1920, 1080, &[]),
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
    assert!(args.contains("-mapping_family\n1\n"));
    assert!(args.contains("-channel_layout\n5.1\n"));
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

#[tokio::test]
async fn synthesize_audio_appends_downmixed_companion_stream() {
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
    let ffprobe = ffprobe_synthesize_stub(dir.path());
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
        &synthesize_audio_request(dir.path()),
    )
    .await
    .unwrap();

    let args = std::fs::read_to_string(args_path).unwrap();
    // Every source stream is copied, and the source is re-mapped once more to
    // encode the appended companion at audio ordinal 2 (after the two sources).
    assert!(args.contains("-map\n0\n"));
    assert!(args.contains("-map\n-0:t?\n"));
    assert!(args.contains("-c\ncopy\n"));
    assert!(args.contains("-map\n0:1\n"));
    assert!(args.contains("-c:a:2\naac\n"));
    assert!(
        args.find("-map\n0:1\n").unwrap() < args.rfind("-map\n0:t?\n").unwrap(),
        "attachments must be mapped after appended companion tracks"
    );
    // Downmix to stereo; aac default profile is 64 kbps/channel → 128k.
    assert!(args.contains("-ac:a:2\n2\n"));
    assert!(args.contains("-b:a:2\n128k\n"));
    assert!(args.contains("-metadata:s:a:2\nsnapshot_stream_id=synth-1\n"));
    // The companion carries the downmixed channel count, not the source's six.
    assert_eq!(probe.selected_output_streams[0].channels, Some(2));
    assert_eq!(probe.selected_output_streams[0].codec, "aac");
}

// ---------------------------------------------------------------------------
// Audio helpers
// ---------------------------------------------------------------------------

fn synthesize_audio_request(root: &Path) -> TranscodeAudioRequest {
    let mut request = transcode_audio_request(root, &[1], "aac");
    request.audio.add_track = true;
    request.audio.target_channels = Some(2);
    // The selection's snapshot id is the NEW derived companion track id, not
    // the source stream's id.
    request.selection.selected_streams[0].snapshot_stream_id = "synth-1".to_owned();
    request
}

/// ffprobe stub for synthesis: a 5.1 source (index 1) plus the stereo companion
/// (index 4) tagged with the new snapshot id, so the output probe resolves the
/// appended downmixed stream.
fn ffprobe_synthesize_stub(dir: &Path) -> PathBuf {
    stub_bin(
        dir,
        "ffprobe",
        "#!/bin/sh\ncat <<'JSON'\n{\"format\":{\"format_name\":\"matroska\"},\"streams\":[{\"index\":1,\"codec_type\":\"audio\",\"codec_name\":\"eac3\",\"channels\":6,\"tags\":{\"language\":\"eng\",\"title\":\"Main\"},\"disposition\":{\"default\":1,\"forced\":0,\"comment\":0}},{\"index\":4,\"codec_type\":\"audio\",\"codec_name\":\"aac\",\"channels\":2,\"tags\":{\"language\":\"eng\",\"snapshot_stream_id\":\"synth-1\"}}]}\nJSON\n",
    )
}

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
            add_track: false,
            target_channels: None,
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
        outputs: None,
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

/// A `hevc_vaapi` profile: `qp` quality domain, no preset (spec §3), and a
/// hardware *surface* pixel format rather than a software one.
fn profile_vaapi(
    decode: voom_core::VideoDecodeMode,
    pixel_format: Option<&str>,
    codec_profile: Option<&str>,
) -> TranscodeVideoProfile {
    TranscodeVideoProfile {
        name: "hevc-vaapi".to_owned(),
        target_codec: "hevc".to_owned(),
        encoder: "hevc_vaapi".to_owned(),
        crf: None,
        cq: None,
        qp: Some(24),
        bitrate_kbps: None,
        preset: None,
        tune: None,
        codec_profile: codec_profile.map(str::to_owned),
        codec_level: None,
        pixel_format: pixel_format.map(str::to_owned),
        max_width: None,
        max_height: None,
        decode,
        copy_compatible: false,
    }
}

/// A bound VAAPI device whose render node is deliberately **not**
/// `/dev/dri/renderD128`: the argv must name the node preflight resolved for the
/// configured PCI address, so an implementation that hardcoded the common default
/// fails these tests instead of passing by coincidence.
fn vaapi_binding(dir: &Path) -> VaapiDeviceBinding {
    VaapiDeviceBinding {
        render_node: dir.join("renderD129"),
        descriptor: VaapiVideoAcceleratorDescriptor {
            pci_address: "0000:f4:00.0".to_owned(),
            device_name: "radeonsi".to_owned(),
            driver_version: "Mesa Gallium 26.1.5 (radeonsi, strix_halo)".to_owned(),
            encoders: vec!["hevc_vaapi".to_owned()],
            decoders: vec!["h264".to_owned(), "hevc".to_owned(), "av1".to_owned()],
            max_sessions: 1,
        },
    }
}

fn vaapi_request(dir: &Path, profile: TranscodeVideoProfile) -> TranscodeVideoRequest {
    let mut request = basic_request(dir, "mkv", "hevc", profile);
    request.hardware_assignment = Some(VideoHardwareAssignment::vaapi(
        "vaapi:pci-0000:f4:00.0",
        "0000:f4:00.0",
    ));
    request
}

/// The captured argv with the tempdir prefix elided, so a snapshot can pin the
/// **whole** command line. Pinning all of it is what makes the spec's negative
/// rules — no `-level`, no `-vf` on the hardware-decode path, no software encoder
/// anywhere — falsifiable, which a handful of substring assertions cannot do.
fn captured_argv(args_path: &Path, dir: &Path) -> String {
    std::fs::read_to_string(args_path)
        .unwrap()
        .replace(&format!("{}/", dir.display()), "<DIR>/")
}

/// Spec §7 row 1, 8-bit. `-vaapi_device` names the bound node at open time, which
/// is where VAAPI binding strength comes from (ADR 0052 §1), and the software
/// frames are uploaded explicitly — no implicit transfer, and `-rc_mode CQP` is
/// stated rather than left to the encoder's `auto` default.
#[tokio::test]
async fn vaapi_software_decode_uploads_nv12_to_the_bound_render_node() {
    let dir = tempfile::tempdir().unwrap();
    let (ffmpeg, args_path) = arg_capture_ffmpeg(dir.path());
    let ffprobe = hevc_mkv_ffprobe(dir.path());
    tokio::fs::write(dir.path().join("input.mkv"), b"input")
        .await
        .unwrap();
    let profile = profile_vaapi(voom_core::VideoDecodeMode::default(), Some("nv12"), None);
    let request = vaapi_request(dir.path(), profile);
    let config = FfmpegConfig::new(ffmpeg, ffprobe, "test".to_owned(), DEFAULT_PROCESS_TIMEOUT)
        .with_vaapi_device(vaapi_binding(dir.path()));

    run_ffmpeg_transcode(&config, &request, video_source(1920, 1080, &[]))
        .await
        .unwrap();

    insta::assert_snapshot!(captured_argv(&args_path, dir.path()), @r"
    -hide_banner
    -nostdin
    -n
    -vaapi_device
    <DIR>/renderD129
    -i
    <DIR>/input.mkv
    -map
    0:v:0
    -map
    0:a?
    -map
    0:s?
    -map
    0:t?
    -c:v
    hevc_vaapi
    -rc_mode
    CQP
    -qp
    24
    -vf
    format=nv12,hwupload
    -c:a
    copy
    -c:s
    copy
    -c:t
    copy
    -map_metadata
    0
    -f
    matroska
    <DIR>/out.mkv
    ");
}

/// Spec §7 row 3 over row 1: Main10 differs from Main only in the uploaded surface
/// format and the named profile. `-profile:v main10` is passed **by name** because
/// `hevc_vaapi`'s `-profile` carries named constants (spec §2.2), so there is no
/// name-to-integer mapping layer to get wrong.
#[tokio::test]
async fn vaapi_software_decode_uploads_p010_and_names_main10_profile() {
    let dir = tempfile::tempdir().unwrap();
    let (ffmpeg, args_path) = arg_capture_ffmpeg(dir.path());
    let ffprobe = hevc_mkv_ffprobe_10bit(dir.path());
    tokio::fs::write(dir.path().join("input.mkv"), b"input")
        .await
        .unwrap();
    let profile = profile_vaapi(
        voom_core::VideoDecodeMode::default(),
        Some("p010"),
        Some("main10"),
    );
    let request = vaapi_request(dir.path(), profile);
    let config = FfmpegConfig::new(ffmpeg, ffprobe, "test".to_owned(), DEFAULT_PROCESS_TIMEOUT)
        .with_vaapi_device(vaapi_binding(dir.path()));

    run_ffmpeg_transcode(&config, &request, video_source(1920, 1080, &[]))
        .await
        .unwrap();

    insta::assert_snapshot!(captured_argv(&args_path, dir.path()), @r"
    -hide_banner
    -nostdin
    -n
    -vaapi_device
    <DIR>/renderD129
    -i
    <DIR>/input.mkv
    -map
    0:v:0
    -map
    0:a?
    -map
    0:s?
    -map
    0:t?
    -c:v
    hevc_vaapi
    -rc_mode
    CQP
    -qp
    24
    -profile:v
    main10
    -vf
    format=p010,hwupload
    -c:a
    copy
    -c:s
    copy
    -c:t
    copy
    -map_metadata
    0
    -f
    matroska
    <DIR>/out.mkv
    ");
}

/// Spec §7 row 2, 8-bit. A VAAPI-decoded source is already in hardware frames, so
/// **no** filter is inserted: a `format=...,hwupload` here would download and
/// re-upload every frame. `-hwaccel_output_format vaapi` is what makes a silent
/// software-decode fallback impossible — `FFmpeg` errors instead (spec §2.2).
#[tokio::test]
async fn vaapi_hardware_decode_stays_in_hardware_frames_with_no_filter() {
    let dir = tempfile::tempdir().unwrap();
    let (ffmpeg, args_path) = arg_capture_ffmpeg(dir.path());
    let ffprobe = hevc_mkv_ffprobe(dir.path());
    tokio::fs::write(dir.path().join("input.mkv"), b"input")
        .await
        .unwrap();
    let profile = profile_vaapi(voom_core::VideoDecodeMode::vaapi(), Some("nv12"), None);
    let request = vaapi_request(dir.path(), profile);
    let config = FfmpegConfig::new(ffmpeg, ffprobe, "test".to_owned(), DEFAULT_PROCESS_TIMEOUT)
        .with_vaapi_device(vaapi_binding(dir.path()));

    run_ffmpeg_transcode(&config, &request, video_source(1920, 1080, &[]))
        .await
        .unwrap();

    insta::assert_snapshot!(captured_argv(&args_path, dir.path()), @r"
    -hide_banner
    -nostdin
    -n
    -hwaccel
    vaapi
    -hwaccel_device
    <DIR>/renderD129
    -hwaccel_output_format
    vaapi
    -i
    <DIR>/input.mkv
    -map
    0:v:0
    -map
    0:a?
    -map
    0:s?
    -map
    0:t?
    -c:v
    hevc_vaapi
    -rc_mode
    CQP
    -qp
    24
    -c:a
    copy
    -c:s
    copy
    -c:t
    copy
    -map_metadata
    0
    -f
    matroska
    <DIR>/out.mkv
    ");
}

/// Spec §7 row 3 over row 2. The decoder already produced 10-bit surfaces, so
/// Main10 hardware-decode adds only `-profile:v main10` and still inserts no
/// filter: the surface format is not ours to restate here.
#[tokio::test]
async fn vaapi_hardware_decode_main10_names_profile_without_a_filter() {
    let dir = tempfile::tempdir().unwrap();
    let (ffmpeg, args_path) = arg_capture_ffmpeg(dir.path());
    let ffprobe = hevc_mkv_ffprobe_10bit(dir.path());
    tokio::fs::write(dir.path().join("input.mkv"), b"input")
        .await
        .unwrap();
    let profile = profile_vaapi(
        voom_core::VideoDecodeMode::vaapi(),
        Some("p010"),
        Some("main10"),
    );
    let request = vaapi_request(dir.path(), profile);
    let config = FfmpegConfig::new(ffmpeg, ffprobe, "test".to_owned(), DEFAULT_PROCESS_TIMEOUT)
        .with_vaapi_device(vaapi_binding(dir.path()));

    run_ffmpeg_transcode(&config, &request, video_source(1920, 1080, &[]))
        .await
        .unwrap();

    insta::assert_snapshot!(captured_argv(&args_path, dir.path()), @r"
    -hide_banner
    -nostdin
    -n
    -hwaccel
    vaapi
    -hwaccel_device
    <DIR>/renderD129
    -hwaccel_output_format
    vaapi
    -i
    <DIR>/input.mkv
    -map
    0:v:0
    -map
    0:a?
    -map
    0:s?
    -map
    0:t?
    -c:v
    hevc_vaapi
    -rc_mode
    CQP
    -qp
    24
    -profile:v
    main10
    -c:a
    copy
    -c:s
    copy
    -c:t
    copy
    -map_metadata
    0
    -f
    matroska
    <DIR>/out.mkv
    ");
}

/// Rate control is stated on every VAAPI shape, never inherited. `auto` is
/// `-rc_mode`'s `FFmpeg` default, so relying on it would let rate-control behavior
/// move with an `FFmpeg` or driver upgrade (ADR 0052 §5).
#[test]
fn vaapi_always_states_rc_mode_cqp_and_never_relies_on_auto() {
    for decode in [
        voom_core::VideoDecodeMode::default(),
        voom_core::VideoDecodeMode::vaapi(),
    ] {
        for codec_profile in [None, Some("main"), Some("main10")] {
            let profile = profile_vaapi(decode, Some("nv12"), codec_profile);
            let args = video_codec_args(&profile, false).unwrap();
            let rc_mode = args.iter().position(|arg| arg == "-rc_mode").unwrap();
            assert_eq!(args[rc_mode + 1], "CQP");
            assert!(!args.iter().any(|arg| arg == "auto"), "{args:?}");
        }
    }
}

/// `codec_profile` reaches `-profile:v` verbatim. `hevc_vaapi`'s `-profile` is an
/// int-typed `AVOption` carrying named constants, so `FFmpeg` resolves `main10`
/// itself and rejects an unknown name — which is the behavior we want. A
/// name-to-integer table here would be a second place to keep correct.
#[test]
fn vaapi_passes_codec_profile_by_name_with_no_integer_mapping() {
    for (codec_profile, integer) in [("main", "1"), ("main10", "2")] {
        let profile = profile_vaapi(
            voom_core::VideoDecodeMode::default(),
            Some("nv12"),
            Some(codec_profile),
        );
        let args = video_codec_args(&profile, false).unwrap();
        let at = args.iter().position(|arg| arg == "-profile:v").unwrap();
        assert_eq!(args[at + 1], codec_profile);
        assert!(!args.iter().any(|arg| arg == integer), "{args:?}");
    }
}

/// The uploaded surface format already selects Main or Main10 (spec §2.2), so an
/// unconditional `-profile:v` would invent a value the operator never chose.
#[test]
fn vaapi_omits_profile_when_the_operator_set_none() {
    let profile = profile_vaapi(voom_core::VideoDecodeMode::default(), Some("p010"), None);

    let args = video_codec_args(&profile, false).unwrap();

    assert!(!args.iter().any(|arg| arg == "-profile:v"), "{args:?}");
}

/// `codec_level` is outside the VAAPI vocabulary: `HEVC_VAAPI`'s `codec_levels` is
/// empty, so descriptor validation rejects it first. Reaching the builder with one
/// set means validation was bypassed — drop it silently and the operator's stated
/// level would vanish from a command that claims to honor the profile.
#[test]
fn vaapi_never_emits_level_and_refuses_a_profile_that_sets_one() {
    let clean = profile_vaapi(voom_core::VideoDecodeMode::default(), Some("nv12"), None);
    let args = video_codec_args(&clean, false).unwrap();
    assert!(!args.iter().any(|arg| arg == "-level"), "{args:?}");

    let mut leveled = clean;
    leveled.codec_level = Some("5.1".to_owned());

    let err = video_codec_args(&leveled, false).unwrap_err();

    assert!(err.to_string().contains("codec_level"), "{err}");
    assert!(err.to_string().contains("5.1"), "{err}");
}

/// Issue #409's hard rule: a VAAPI failure is a failure. No error path may fall
/// back to a software encoder, so every rejected VAAPI request must name no
/// encoder at all rather than a substituted one.
#[test]
fn no_vaapi_failure_path_substitutes_a_software_encoder() {
    let software_encoders = ["libx265", "libsvtav1", "libaom-av1"];
    let mut no_quality = profile_vaapi(voom_core::VideoDecodeMode::default(), Some("nv12"), None);
    no_quality.qp = None;
    let mut leveled = profile_vaapi(voom_core::VideoDecodeMode::default(), Some("nv12"), None);
    leveled.codec_level = Some("5.1".to_owned());

    for profile in [no_quality, leveled] {
        let err = video_codec_args(&profile, false).unwrap_err();
        for encoder in software_encoders {
            assert!(!err.to_string().contains(encoder), "{err}");
        }
    }
    for pixel_format in [Some("nv12"), Some("p010"), None] {
        let profile = profile_vaapi(voom_core::VideoDecodeMode::default(), pixel_format, None);
        let args = video_codec_args(&profile, false).unwrap();
        for encoder in software_encoders {
            assert!(!args.iter().any(|arg| arg == encoder), "{args:?}");
        }
    }
}

/// A `hevc_vaapi` request that reached a worker with no bound device must fail
/// before ffmpeg runs. The alternative — opening whatever VAAPI device happens to
/// be default — would encode on a device the scheduler never leased.
#[tokio::test]
async fn vaapi_request_on_an_unbound_worker_fails_before_ffmpeg_runs() {
    let dir = tempfile::tempdir().unwrap();
    let (ffmpeg, args_path) = arg_capture_ffmpeg(dir.path());
    let ffprobe = hevc_mkv_ffprobe(dir.path());
    tokio::fs::write(dir.path().join("input.mkv"), b"input")
        .await
        .unwrap();
    let profile = profile_vaapi(voom_core::VideoDecodeMode::default(), Some("nv12"), None);
    let request = vaapi_request(dir.path(), profile);
    let config = FfmpegConfig::new(ffmpeg, ffprobe, "test".to_owned(), DEFAULT_PROCESS_TIMEOUT);

    let err = run_ffmpeg_transcode(&config, &request, video_source(1920, 1080, &[]))
        .await
        .unwrap_err();

    assert!(err.to_string().contains("hevc_vaapi"), "{err}");
    assert!(!args_path.exists(), "ffmpeg must not have been invoked");
}

/// VAAPI decode covers `h264`/`hevc`/`av1` only. An unsupported source codec is
/// reported against the codec, not silently decoded in software into the hardware
/// encoder — that would be the implicit fallback #409 forbids.
#[tokio::test]
async fn vaapi_hardware_decode_rejects_an_unsupported_source_codec() {
    let dir = tempfile::tempdir().unwrap();
    let (ffmpeg, args_path) = arg_capture_ffmpeg(dir.path());
    let ffprobe = hevc_mkv_ffprobe(dir.path());
    tokio::fs::write(dir.path().join("input.mkv"), b"input")
        .await
        .unwrap();
    let profile = profile_vaapi(voom_core::VideoDecodeMode::vaapi(), Some("nv12"), None);
    let request = vaapi_request(dir.path(), profile);
    let config = FfmpegConfig::new(ffmpeg, ffprobe, "test".to_owned(), DEFAULT_PROCESS_TIMEOUT)
        .with_vaapi_device(vaapi_binding(dir.path()));
    let source = VideoTranscodeInput {
        width: 1920,
        height: 1080,
        codec: "vp9",
        forced_subtitle_ordinals: &[],
    };

    let err = run_ffmpeg_transcode(&config, &request, source)
        .await
        .unwrap_err();

    assert!(err.to_string().contains("vp9"), "{err}");
    assert!(!args_path.exists(), "ffmpeg must not have been invoked");
}

/// An unknown surface format must not reach ffmpeg. `nv12` and `p010` are the two
/// the descriptor allows and the two spec §2.2 verified; a software format name
/// like `yuv420p` here would silently produce a filter VAAPI cannot upload.
#[test]
fn vaapi_rejects_a_pixel_format_outside_the_surface_vocabulary() {
    let profile = profile_vaapi(voom_core::VideoDecodeMode::default(), Some("yuv420p"), None);

    let err = video_filter_args(&profile, 1920, 1080, false).unwrap_err();

    assert!(err.to_string().contains("yuv420p"), "{err}");
}

/// This slice generates no VAAPI scale filter (spec §7 records none), so a profile
/// whose dimension cap the source exceeds is refused up front. Ignoring the cap
/// would emit an over-cap output, and picking a filter shape the spec never
/// verified on hardware would be inventing one.
#[test]
fn vaapi_refuses_a_downscale_it_has_no_verified_filter_for() {
    let mut profile = profile_vaapi(voom_core::VideoDecodeMode::default(), Some("nv12"), None);
    profile.max_width = Some(1280);
    profile.max_height = Some(720);

    let within = video_filter_args(&profile, 1280, 720, false).unwrap();
    let err = video_filter_args(&profile, 1920, 1080, false).unwrap_err();

    assert_eq!(
        within,
        vec![
            OsString::from("-vf"),
            OsString::from("format=nv12,hwupload")
        ]
    );
    assert!(err.to_string().contains("1920x1080"), "{err}");
    assert!(err.to_string().contains("hevc_vaapi"), "{err}");
}

/// A VAAPI profile names a hardware surface format, but the file ffmpeg writes
/// carries the software format that surface decodes to — spec §2.2 measured
/// `nv12` → `yuv420p` and `p010` → `yuv420p10le`. Comparing the surface name to
/// the file's format would reject every conforming VAAPI encode.
#[tokio::test]
async fn vaapi_output_verification_expects_the_file_format_not_the_surface_format() {
    let dir = tempfile::tempdir().unwrap();
    let (ffmpeg, _args_path) = arg_capture_ffmpeg(dir.path());
    tokio::fs::write(dir.path().join("input.mkv"), b"input")
        .await
        .unwrap();
    let eight_bit = profile_vaapi(voom_core::VideoDecodeMode::default(), Some("nv12"), None);
    let ten_bit = profile_vaapi(
        voom_core::VideoDecodeMode::default(),
        Some("p010"),
        Some("main10"),
    );
    let binding = vaapi_binding(dir.path());

    let eight = FfmpegConfig::new(
        ffmpeg.clone(),
        hevc_mkv_ffprobe(dir.path()),
        "test".to_owned(),
        DEFAULT_PROCESS_TIMEOUT,
    )
    .with_vaapi_device(binding.clone());
    let probe = run_ffmpeg_transcode(
        &eight,
        &vaapi_request(dir.path(), eight_bit.clone()),
        video_source(1920, 1080, &[]),
    )
    .await
    .unwrap();
    assert_eq!(probe.pixel_format, "yuv420p");

    tokio::fs::remove_file(dir.path().join("out.mkv"))
        .await
        .unwrap();
    let ten = FfmpegConfig::new(
        ffmpeg.clone(),
        hevc_mkv_ffprobe_10bit(dir.path()),
        "test".to_owned(),
        DEFAULT_PROCESS_TIMEOUT,
    )
    .with_vaapi_device(binding.clone());
    let probe = run_ffmpeg_transcode(
        &ten,
        &vaapi_request(dir.path(), ten_bit),
        video_source(1920, 1080, &[]),
    )
    .await
    .unwrap();
    assert_eq!(probe.pixel_format, "yuv420p10le");

    // A 10-bit file under an 8-bit surface format is still a mismatch to report.
    tokio::fs::remove_file(dir.path().join("out.mkv"))
        .await
        .unwrap();
    let mismatched = FfmpegConfig::new(
        ffmpeg,
        hevc_mkv_ffprobe_10bit(dir.path()),
        "test".to_owned(),
        DEFAULT_PROCESS_TIMEOUT,
    )
    .with_vaapi_device(binding);
    let err = run_ffmpeg_transcode(
        &mismatched,
        &vaapi_request(dir.path(), eight_bit),
        video_source(1920, 1080, &[]),
    )
    .await
    .unwrap_err();
    assert!(err.to_string().contains("yuv420p"), "{err}");
}
