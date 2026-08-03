use std::ffi::OsStr;

use super::super::process::resolve_binary;
use super::*;
use voom_worker_protocol::{VIDEOTOOLBOX_PREFLIGHT_BUDGET, VIDEOTOOLBOX_PREFLIGHT_MAX_STAGES};

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

#[test]
fn videotoolbox_capacity_command_forbids_software_fallback() {
    let input = Path::new("input.mov");
    let command = videotoolbox_capacity_command(
        Path::new("ffmpeg"),
        CapacityInput::Hardware(input),
        "hevc_videotoolbox",
        "yuv420p10le",
        Path::new("progress.txt"),
    );
    let args = command
        .get_args()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect::<Vec<_>>();

    assert!(args.windows(2).any(|pair| pair == ["-allow_sw", "0"]));
    assert!(
        args.windows(2)
            .any(|pair| pair == ["-hwaccel_output_format", "videotoolbox_vld"])
    );
    assert!(
        args.windows(2)
            .any(|pair| pair == ["-c:v", "hevc_videotoolbox"])
    );
    assert!(args.windows(2).any(|pair| pair == ["-profile:v", "main10"]));
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
