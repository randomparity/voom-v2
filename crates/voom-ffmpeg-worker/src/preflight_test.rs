use std::path::{Path, PathBuf};

use super::*;
use voom_worker_protocol::{VIDEOTOOLBOX_PREFLIGHT_BUDGET, VIDEOTOOLBOX_PREFLIGHT_MAX_STAGES};

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

#[test]
fn videotoolbox_stage_budget_matches_supervisor_deadline() {
    assert_eq!(VIDEOTOOLBOX_PREFLIGHT_MAX_STAGES, 29);
    assert_eq!(PROBE_TIMEOUT, Duration::from_secs(15));
    assert_eq!(VIDEOTOOLBOX_PREFLIGHT_BUDGET, Duration::from_secs(465));
}

#[test]
fn platform_identity_hash_is_normalized_and_errors_do_not_disclose_raw_uuid() {
    let raw_uuid = "e4ad1c3f-8b4a-4e4e-a9ad-9a0123456789";
    let ioreg = format!("\"IOPlatformUUID\" = \"{raw_uuid}\"");
    let normalized = parse_ioreg_platform_uuid(&ioreg).unwrap();
    let resource_id = platform_resource_id(&normalized).unwrap();

    assert_eq!(normalized, raw_uuid.to_ascii_uppercase());
    assert_eq!(resource_id.len(), 64);
    assert!(!resource_id.contains(raw_uuid));

    let malformed = "secret-platform-value";
    let error = parse_ioreg_platform_uuid(&format!("\"IOPlatformUUID\" = \"{malformed}\""))
        .unwrap_err()
        .to_string();
    assert!(!error.contains(malformed));
}

#[cfg(unix)]
#[test]
fn failed_identity_command_redacts_partial_platform_output() {
    let raw_uuid = "E4AD1C3F-8B4A-4E4E-A9AD-9A0123456789";
    let output = Command::new("/bin/sh")
        .args([
            "-c",
            &format!("printf '\"IOPlatformUUID\" = \"{raw_uuid}\"'; exit 1"),
        ])
        .output();

    let error = redacted_command_text("ioreg platform identity", output)
        .unwrap_err()
        .to_string();

    assert!(error.contains("ioreg platform identity exited with status 1"));
    assert!(!error.contains(raw_uuid));
}

#[test]
fn capacity_progress_requires_a_positive_frame() {
    assert!(!progress_reports_frame("frame=0\nprogress=continue\n"));
    assert!(progress_reports_frame("frame=1\nprogress=continue\n"));
}

#[cfg(unix)]
#[test]
fn capacity_failure_reaps_remaining_processes() {
    let sleeper = Command::new("/bin/sh")
        .args(["-c", "sleep 60"])
        .spawn()
        .unwrap();
    let sleeper_pid = sleeper.id();
    let failing = Command::new("/bin/sh")
        .args(["-c", "exit 2"])
        .spawn()
        .unwrap();
    let mut children = vec![sleeper, failing];

    let result = wait_videotoolbox_capacity_children(
        "test",
        &mut children,
        std::time::Instant::now() + Duration::from_secs(2),
    );

    assert!(result.is_err());
    assert!(children.is_empty());
    let status = Command::new("/bin/kill")
        .args(["-0", &sleeper_pid.to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(!status.success());
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
#[ignore = "requires the host FFmpeg VideoToolbox stack"]
fn real_videotoolbox_preflight_proves_host_pipelines() {
    let ffmpeg = resolve_binary(OsStr::new("ffmpeg"));
    let ffprobe = resolve_binary(OsStr::new("ffprobe"));
    let ioreg = command_text(
        "ioreg platform identity",
        command_output(Command::new("/usr/sbin/ioreg").args([
            "-rd1",
            "-c",
            "IOPlatformExpertDevice",
        ])),
    )
    .unwrap();
    let resource_id = platform_resource_id(&parse_ioreg_platform_uuid(&ioreg).unwrap()).unwrap();

    let report = preflight_with_videotoolbox(
        &ffmpeg,
        &ffprobe,
        &VideoToolboxPreflightConfig {
            resource_id,
            max_sessions: 1,
        },
    )
    .unwrap();

    let report = report.videotoolbox.unwrap();
    assert_eq!(report.encoders.len(), 2);
    assert!(!report.decoders.is_empty());
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
