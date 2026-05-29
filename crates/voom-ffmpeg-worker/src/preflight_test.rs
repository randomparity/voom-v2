use std::path::{Path, PathBuf};

use super::*;

#[test]
fn preflight_rejects_missing_ffmpeg() {
    let temp = tempfile::tempdir().unwrap();
    let ffprobe = stub_bin(
        temp.path(),
        "ffprobe",
        "#!/bin/sh\necho 'ffprobe version 7.0'\n",
    );

    assert!(preflight_with_paths(&temp.path().join("missing-ffmpeg"), &ffprobe).is_err());
}

#[cfg(unix)]
#[test]
fn preflight_rejects_non_executable_ffmpeg() {
    let temp = tempfile::tempdir().unwrap();
    let ffmpeg = non_executable_file(temp.path(), "ffmpeg");
    let ffprobe = stub_bin(
        temp.path(),
        "ffprobe",
        "#!/bin/sh\necho 'ffprobe version 7.0'\n",
    );

    assert!(preflight_with_paths(&ffmpeg, &ffprobe).is_err());
}

const ALL_ENCODERS: &str = "Encoders:\n V..... libx265 H.265 / HEVC\n V..... libsvtav1 SVT-AV1\n V..... libaom-av1 libaom AV1\n A..... aac AAC\n A..... libopus Opus\n";
const ALL_MUXERS: &str = "Muxers:\n E matroska Matroska\n E mp4 MP4\n E ogg Ogg\n";

fn fake_ffmpeg_all_encoders(dir: &Path) -> PathBuf {
    ffmpeg_stub(
        dir,
        "ffmpeg",
        "ffmpeg version 7.0",
        ALL_ENCODERS,
        ALL_MUXERS,
    )
}

fn fake_ffmpeg_without(dir: &Path, missing_encoder: &str) -> PathBuf {
    let encoders = ALL_ENCODERS
        .lines()
        .filter(|line| !line.contains(missing_encoder))
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    ffmpeg_stub(dir, "ffmpeg", "ffmpeg version 7.0", &encoders, ALL_MUXERS)
}

fn fake_ffprobe(dir: &Path) -> PathBuf {
    stub_bin(dir, "ffprobe", "#!/bin/sh\necho 'ffprobe version 7.0'\n")
}

#[test]
fn preflight_detects_all_three_video_encoders() {
    let temp = tempfile::tempdir().unwrap();
    let ffmpeg = fake_ffmpeg_all_encoders(temp.path());
    let ffprobe = fake_ffprobe(temp.path());

    let report = preflight_with_paths(&ffmpeg, &ffprobe).unwrap();
    assert!(report.has_encoder("libx265"), "missing libx265");
    assert!(report.has_encoder("libsvtav1"), "missing libsvtav1");
    assert!(report.has_encoder("libaom-av1"), "missing libaom-av1");
    assert!(report.has_muxer("mp4"), "missing mp4 muxer");
}

#[test]
fn preflight_rejects_missing_libsvtav1() {
    let temp = tempfile::tempdir().unwrap();
    let ffmpeg = fake_ffmpeg_without(temp.path(), "libsvtav1");
    let ffprobe = fake_ffprobe(temp.path());

    let err = preflight_with_paths(&ffmpeg, &ffprobe);
    assert!(err.is_err(), "expected error when libsvtav1 is missing");
}

#[test]
fn preflight_rejects_missing_libaom_av1() {
    let temp = tempfile::tempdir().unwrap();
    let ffmpeg = fake_ffmpeg_without(temp.path(), "libaom-av1");
    let ffprobe = fake_ffprobe(temp.path());

    let err = preflight_with_paths(&ffmpeg, &ffprobe);
    assert!(err.is_err(), "expected error when libaom-av1 is missing");
}

#[test]
fn preflight_rejects_encoder_list_without_libx265() {
    let temp = tempfile::tempdir().unwrap();
    let ffmpeg = ffmpeg_stub(
        temp.path(),
        "ffmpeg",
        "ffmpeg version 7.0",
        "Encoders:\n V..... h264 encoder\n",
        ALL_MUXERS,
    );
    let ffprobe = stub_bin(
        temp.path(),
        "ffprobe",
        "#!/bin/sh\necho 'ffprobe version 7.0'\n",
    );

    assert!(preflight_with_paths(&ffmpeg, &ffprobe).is_err());
}

#[test]
fn preflight_accepts_encoder_list_containing_all_required() {
    let temp = tempfile::tempdir().unwrap();
    let ffmpeg = fake_ffmpeg_all_encoders(temp.path());
    let ffprobe = fake_ffprobe(temp.path());

    let preflight = preflight_with_paths(&ffmpeg, &ffprobe).unwrap();

    assert_eq!(preflight.ffmpeg_path, ffmpeg);
    assert_eq!(preflight.ffprobe_path, ffprobe);
    assert_eq!(preflight.ffmpeg_version, "ffmpeg version 7.0");
    assert_eq!(preflight.ffprobe_version, "ffprobe version 7.0");
    assert_eq!(preflight.hevc_encoder, "libx265");
    assert_eq!(preflight.svtav1_encoder, "libsvtav1");
    assert_eq!(preflight.libaom_encoder, "libaom-av1");
}

#[test]
fn preflight_checks_aac_and_opus_encoders() {
    let temp = tempfile::tempdir().unwrap();
    let ffmpeg = fake_ffmpeg_all_encoders(temp.path());
    let ffprobe = fake_ffprobe(temp.path());

    let preflight = preflight_with_paths(&ffmpeg, &ffprobe).unwrap();

    assert_eq!(preflight.aac_encoder, "aac");
    assert_eq!(preflight.opus_encoder, "libopus");
}

#[test]
fn preflight_checks_matroska_mp4_and_ogg_muxers() {
    let temp = tempfile::tempdir().unwrap();
    let ffmpeg = fake_ffmpeg_all_encoders(temp.path());
    let ffprobe = fake_ffprobe(temp.path());

    let preflight = preflight_with_paths(&ffmpeg, &ffprobe).unwrap();

    assert_eq!(preflight.matroska_muxer, "matroska");
    assert_eq!(preflight.mp4_muxer, "mp4");
    assert_eq!(preflight.ogg_muxer, "ogg");
}

fn ffmpeg_stub(dir: &Path, name: &str, version: &str, encoders: &str, muxers: &str) -> PathBuf {
    stub_bin(
        dir,
        name,
        &format!(
            "#!/bin/sh\ncase \"$*\" in\n  *-version*) echo '{version}' ;;\n  *-encoders*) cat <<'EOF'\n{encoders}EOF\n    ;;\n  *-muxers*) cat <<'EOF'\n{muxers}EOF\n    ;;\n  *) exit 2 ;;\nesac\n"
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
fn non_executable_file(dir: &Path, name: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, "not executable").unwrap();
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
