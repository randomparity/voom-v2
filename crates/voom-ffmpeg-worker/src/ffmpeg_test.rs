use std::path::{Path, PathBuf};

use voom_worker_protocol::{
    AudioStreamRef, ExtractAudioOutput, ExtractAudioRequest, TranscodeAudioOutput,
    TranscodeAudioRequest, TranscodeAudioSelection, TranscodeAudioSettings, TranscodeVideoProfile,
};

use super::*;

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

    let err = run_ffmpeg_transcode(
        &FfmpegConfig::new(
            ffmpeg,
            ffprobe,
            "ffmpeg version test".to_owned(),
            DEFAULT_PROCESS_TIMEOUT,
        ),
        &input,
        &output,
        &TranscodeVideoProfile::default_hevc(),
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
    let ffprobe = stub_bin(
        dir.path(),
        "ffprobe",
        "#!/bin/sh\ncat <<'JSON'\n{\"format\":{\"format_name\":\"matroska,webm\"},\"streams\":[{\"codec_type\":\"video\",\"codec_name\":\"hevc\"}]}\nJSON\n",
    );
    let input = dir.path().join("input.mkv");
    let output = dir.path().join("out.mkv");
    tokio::fs::write(&input, b"input").await.unwrap();

    let probe = run_ffmpeg_transcode(
        &FfmpegConfig::new(
            ffmpeg,
            ffprobe,
            "ffmpeg version test".to_owned(),
            DEFAULT_PROCESS_TIMEOUT,
        ),
        &input,
        &output,
        &TranscodeVideoProfile::default_hevc(),
    )
    .await
    .unwrap();

    assert_eq!(probe.container, "mkv");
    assert_eq!(probe.video_codec, "hevc");
}

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
            profile: format!("default-{target_codec}"),
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
