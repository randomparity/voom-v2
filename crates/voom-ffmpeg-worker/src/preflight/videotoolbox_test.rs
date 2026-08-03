#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
use std::ffi::OsStr;

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
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

#[test]
fn videotoolbox_decoder_smoke_commands_are_hardware_only() {
    for spec in VIDEOTOOLBOX_FIXTURE_SPECS {
        let fixture = VideoToolboxFixture {
            spec,
            path: PathBuf::from(format!("{}.mkv", spec.name)),
        };
        let args = videotoolbox_decoder_smoke_command(Path::new("ffmpeg"), &fixture)
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        let profile = if spec.pixel_format == "yuv420p10le" {
            "main10"
        } else {
            "main"
        };

        assert_eq!(
            args,
            [
                "-hide_banner",
                "-nostdin",
                "-hwaccel",
                "videotoolbox",
                "-hwaccel_output_format",
                "videotoolbox_vld",
                "-i",
                &format!("{}.mkv", spec.name),
                "-frames:v",
                "1",
                "-an",
                "-c:v",
                "hevc_videotoolbox",
                "-allow_sw",
                "0",
                "-b:v",
                "4M",
                "-profile:v",
                profile,
                "-f",
                "null",
                "-",
            ],
            "decoder smoke command drifted for {}",
            spec.name
        );
    }
}

#[cfg(unix)]
#[test]
fn videotoolbox_capacity_runs_every_encoder_and_proven_decoder_group() {
    let temp = tempfile::tempdir().unwrap();
    let log = temp.path().join("capacity.log");
    let ffmpeg = capacity_ffmpeg_stub(temp.path(), &log);
    let probe_dir = ProbeDir::new("videotoolbox-capacity-test").unwrap();
    let fixtures = VIDEOTOOLBOX_FIXTURE_SPECS
        .into_iter()
        .map(|spec| VideoToolboxFixture {
            spec,
            path: temp.path().join(format!("{}.mkv", spec.name)),
        })
        .collect::<Vec<_>>();
    let decoders = all_videotoolbox_decoder_capabilities();

    prove_videotoolbox_capacity(
        &ffmpeg,
        &VideoToolboxPreflightConfig {
            resource_id: String::new(),
            max_sessions: 1,
        },
        &probe_dir,
        &fixtures,
        &decoders,
    )
    .unwrap();

    let lines = std::fs::read_to_string(log)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    assert_capacity_groups(&lines, temp.path());
}

#[cfg(unix)]
fn all_videotoolbox_decoder_capabilities() -> Vec<VideoToolboxDecodeCapability> {
    vec![
        VideoToolboxDecodeCapability {
            codec: "h264".to_owned(),
            pixel_formats: vec!["yuv420p".to_owned()],
        },
        VideoToolboxDecodeCapability {
            codec: "hevc".to_owned(),
            pixel_formats: vec!["yuv420p".to_owned(), "yuv420p10le".to_owned()],
        },
        VideoToolboxDecodeCapability {
            codec: "av1".to_owned(),
            pixel_formats: vec!["yuv420p".to_owned(), "yuv420p10le".to_owned()],
        },
    ]
}

#[cfg(unix)]
fn assert_capacity_groups(lines: &[String], fixture_dir: &Path) {
    let expected = [
        (
            "testsrc2=size=256x256:rate=30",
            "h264_videotoolbox",
            "high",
            false,
        ),
        (
            "testsrc2=size=256x256:rate=30",
            "hevc_videotoolbox",
            "main",
            false,
        ),
        (
            "testsrc2=size=256x256:rate=30",
            "hevc_videotoolbox",
            "main10",
            false,
        ),
        ("h264-8.mkv", "h264_videotoolbox", "high", true),
        ("hevc-8.mkv", "hevc_videotoolbox", "main", true),
        ("hevc-10.mkv", "hevc_videotoolbox", "main10", true),
        ("av1-8.mkv", "hevc_videotoolbox", "main", true),
        ("av1-10.mkv", "hevc_videotoolbox", "main10", true),
    ];
    assert_eq!(lines.len(), expected.len(), "capacity groups: {lines:#?}");
    for (line, (input, encoder, profile, hardware)) in lines.iter().zip(expected) {
        let input = if hardware {
            fixture_dir.join(input).display().to_string()
        } else {
            input.to_owned()
        };
        assert!(line.contains(&input), "missing input `{input}` in `{line}`");
        assert!(line.contains(&format!("-c:v {encoder}")), "{line}");
        assert!(line.contains(&format!("-profile:v {profile}")), "{line}");
        assert!(
            line.contains("-allow_sw 0"),
            "software fallback allowed: {line}"
        );
        assert_eq!(line.contains("-hwaccel videotoolbox"), hardware, "{line}");
        assert_eq!(
            line.contains("-hwaccel_output_format videotoolbox_vld"),
            hardware,
            "{line}"
        );
    }
}

#[cfg(unix)]
fn capacity_ffmpeg_stub(dir: &Path, log: &Path) -> PathBuf {
    let body = format!(
        "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{log}'\n\
         previous=''\n\
         for argument in \"$@\"; do\n\
           if test \"$previous\" = '-progress'; then\n\
             printf 'frame=1\\nprogress=continue\\n' > \"$argument\"\n\
           fi\n\
           previous=\"$argument\"\n\
         done\n\
         sleep 1\n",
        log = log.display(),
    );
    let path = dir.join("ffmpeg");
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
