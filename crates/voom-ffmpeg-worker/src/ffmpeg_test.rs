use std::path::{Path, PathBuf};

use voom_worker_protocol::TranscodeVideoProfile;

use super::*;

#[tokio::test]
async fn ffmpeg_non_zero_exit_is_error() {
    let dir = tempfile::tempdir().unwrap();
    let ffmpeg = stub_bin(dir.path(), "ffmpeg", "#!/bin/sh\necho fail >&2\nexit 7\n");
    let ffprobe = stub_bin(dir.path(), "ffprobe", "#!/bin/sh\nexit 0\n");
    let input = dir.path().join("input.mkv");
    let output = dir.path().join("out.mkv");
    tokio::fs::write(&input, b"input").await.unwrap();

    let err = run_ffmpeg_transcode(
        &FfmpegConfig::new(ffmpeg, ffprobe, "ffmpeg version test".to_owned()),
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
        &FfmpegConfig::new(ffmpeg, ffprobe, "ffmpeg version test".to_owned()),
        &input,
        &output,
        &TranscodeVideoProfile::default_hevc(),
    )
    .await
    .unwrap();

    assert_eq!(probe.container, "mkv");
    assert_eq!(probe.video_codec, "hevc");
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
