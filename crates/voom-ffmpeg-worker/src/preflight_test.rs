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

#[test]
fn nvidia_uuid_validation_rejects_ordinals_and_partial_tokens() {
    assert!(validate_nvidia_uuid("GPU-aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee").is_ok());
    for invalid in ["0", "GPU-0", "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee"] {
        assert!(validate_nvidia_uuid(invalid).is_err(), "{invalid}");
    }
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
fn preflight_succeeds_without_libaom_av1() {
    let temp = tempfile::tempdir().unwrap();
    let ffmpeg = fake_ffmpeg_without(temp.path(), "libaom-av1");
    let ffprobe = fake_ffprobe(temp.path());

    let preflight = preflight_with_paths(&ffmpeg, &ffprobe).unwrap();

    assert!(
        preflight.libaom_encoder.is_empty(),
        "absent libaom-av1 must leave the encoder field empty"
    );
    assert!(
        !preflight.has_encoder("libaom-av1"),
        "has_encoder must report libaom-av1 unavailable when absent"
    );
    assert!(
        preflight.has_encoder("libsvtav1"),
        "libsvtav1 stays required"
    );
    assert!(preflight.has_encoder("libx265"), "libx265 stays required");
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

/// `command_output` pipes stdout and stderr, so it must drain them while it waits.
/// A child writing more than the OS pipe capacity blocks in `write()` until someone
/// reads, so a waiter that only polls `try_wait()` can never observe it exit — the
/// probe burns its full timeout and a healthy `ffmpeg -encoders` is reported as a
/// preflight failure. Pipe capacity is host-dependent (64 KiB by default, but as
/// little as 8 KiB under pipe-page pressure), so overshoot any plausible value.
#[cfg(unix)]
#[test]
fn command_output_drains_a_child_that_outwrites_the_pipe_capacity() {
    let temp = tempfile::tempdir().unwrap();
    let chatty = stub_bin(
        temp.path(),
        "chatty",
        "#!/bin/sh\nawk 'BEGIN { while (n++ < 8192) print \"0123456789abcdef0123456789abcde\" }'\n",
    );

    let output = command_output(&mut Command::new(&chatty)).unwrap();

    assert!(output.status.success());
    assert_eq!(output.stdout.len(), 8192 * 32);
}

/// Draining must not defeat the deadline. The stub leaves a `sleep` grandchild
/// holding the same pipe, so killing the direct child does not close it — waiting on
/// the drain would block for the grandchild's whole lifetime, which is precisely the
/// startup hang the deadline exists to prevent. The elapsed assertion is the point of
/// this test: returning `TimedOut` eventually is not the same as returning promptly.
#[cfg(unix)]
#[test]
fn wait_child_output_times_out_promptly_when_a_grandchild_holds_the_pipe() {
    let temp = tempfile::tempdir().unwrap();
    let sleeper = stub_bin(temp.path(), "sleeper", "#!/bin/sh\nsleep 60\n");
    let child = Command::new(&sleeper)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    let started = std::time::Instant::now();
    let error = wait_child_output_io(child, Duration::from_millis(200), "stuck probe").unwrap_err();
    let elapsed = started.elapsed();

    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    assert!(error.to_string().contains("stuck probe exceeded"));
    assert!(elapsed < Duration::from_secs(5), "took {elapsed:?}");
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
