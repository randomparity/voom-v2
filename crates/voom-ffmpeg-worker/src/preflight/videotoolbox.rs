use sha2::{Digest, Sha256};
use std::{
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::Duration,
};

use voom_worker_protocol::VideoToolboxDecodeCapability;

use super::{
    FfmpegPreflight, FfmpegPreflightError, preflight_with_paths,
    process::{
        PROBE_TIMEOUT, ProbeDir, command_output, command_text, first_output_line,
        kill_and_reap_all, parse_token, wait_child_output,
    },
    validate_resource_id,
};

/// `VideoToolbox` platform binding and declared concurrent session capacity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoToolboxPreflightConfig {
    pub resource_id: String,
    pub max_sessions: u32,
}

/// `VideoToolbox` platform identity and codec paths proven by startup probes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoToolboxPreflight {
    pub resource_id: String,
    pub model_identifier: String,
    pub chip_name: String,
    pub macos_version: String,
    pub macos_build: String,
    pub max_sessions: u32,
    pub encoders: Vec<String>,
    pub decoders: Vec<VideoToolboxDecodeCapability>,
    pub decoder_diagnostics: Vec<String>,
}

/// Proves `VideoToolbox` platform identity, hardware codec paths, and capacity.
///
/// # Errors
///
/// Returns an error when the platform, configuration, `FFmpeg` features, codec probes, or declared
/// concurrency cannot be proven.
pub fn preflight_with_videotoolbox(
    ffmpeg_path: &Path,
    ffprobe_path: &Path,
    config: &VideoToolboxPreflightConfig,
) -> Result<FfmpegPreflight, FfmpegPreflightError> {
    if !cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        return Err(FfmpegPreflightError::Failed(
            "VideoToolbox requires Apple silicon macOS".to_owned(),
        ));
    }
    validate_resource_id(&config.resource_id)?;
    if !(1..=16).contains(&config.max_sessions) {
        return Err(FfmpegPreflightError::Failed(
            "VideoToolbox max sessions must be in 1..=16".to_owned(),
        ));
    }
    let mut preflight = preflight_with_paths(ffmpeg_path, ffprobe_path)?;
    let platform = probe_videotoolbox_platform(config)?;
    require_videotoolbox_build_features(ffmpeg_path)?;
    let probe_dir = ProbeDir::new("videotoolbox-probe")?;
    let fixtures = create_videotoolbox_fixtures(ffmpeg_path, &probe_dir)?;
    let (decoders, decoder_diagnostics) = probe_videotoolbox_decoders(ffmpeg_path, &fixtures);
    if decoders.is_empty() {
        return Err(FfmpegPreflightError::Failed(
            "VideoToolbox did not prove any decoder codec/pixel-format path".to_owned(),
        ));
    }
    prove_videotoolbox_capacity(ffmpeg_path, config, &probe_dir, &fixtures, &decoders)?;
    preflight.videotoolbox = Some(VideoToolboxPreflight {
        resource_id: config.resource_id.clone(),
        model_identifier: platform.model_identifier,
        chip_name: platform.chip_name,
        macos_version: platform.macos_version,
        macos_build: platform.macos_build,
        max_sessions: config.max_sessions,
        encoders: vec![
            "h264_videotoolbox".to_owned(),
            "hevc_videotoolbox".to_owned(),
        ],
        decoders,
        decoder_diagnostics,
    });
    Ok(preflight)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct VideoToolboxPlatform {
    model_identifier: String,
    chip_name: String,
    macos_version: String,
    macos_build: String,
}

fn probe_videotoolbox_platform(
    config: &VideoToolboxPreflightConfig,
) -> Result<VideoToolboxPlatform, FfmpegPreflightError> {
    let ioreg = redacted_command_text(
        "ioreg platform identity",
        command_output(Command::new("/usr/sbin/ioreg").args([
            "-rd1",
            "-c",
            "IOPlatformExpertDevice",
        ])),
    )?;
    let observed_resource_id = platform_resource_id(&parse_ioreg_platform_uuid(&ioreg)?)?;
    if observed_resource_id != config.resource_id {
        return Err(FfmpegPreflightError::Failed(
            "VideoToolbox platform resource does not match supervisor configuration".to_owned(),
        ));
    }
    let hardware = redacted_command_text(
        "system_profiler hardware identity",
        command_output(Command::new("/usr/sbin/system_profiler").arg("SPHardwareDataType")),
    )?;
    let model_identifier = parse_labeled_value(&hardware, "Model Identifier")?;
    let chip_name = parse_labeled_value(&hardware, "Chip")?;
    let macos_version = first_output_line(
        "sw_vers product version",
        command_output(Command::new("/usr/bin/sw_vers").arg("-productVersion")),
    )?;
    let macos_build = first_output_line(
        "sw_vers build version",
        command_output(Command::new("/usr/bin/sw_vers").arg("-buildVersion")),
    )?;
    Ok(VideoToolboxPlatform {
        model_identifier,
        chip_name,
        macos_version,
        macos_build,
    })
}

fn parse_ioreg_platform_uuid(output: &str) -> Result<String, FfmpegPreflightError> {
    let line = output
        .lines()
        .find(|line| line.contains("\"IOPlatformUUID\""))
        .ok_or_else(|| FfmpegPreflightError::Failed("ioreg omitted IOPlatformUUID".to_owned()))?;
    let value = line
        .split('=')
        .nth(1)
        .map(str::trim)
        .and_then(|value| value.strip_prefix('"'))
        .and_then(|value| value.strip_suffix('"'))
        .ok_or_else(|| {
            FfmpegPreflightError::Failed("ioreg returned malformed IOPlatformUUID".to_owned())
        })?;
    let normalized = value.to_ascii_uppercase();
    let valid = normalized.len() == 36
        && normalized
            .char_indices()
            .all(|(index, character)| match index {
                8 | 13 | 18 | 23 => character == '-',
                _ => character.is_ascii_hexdigit(),
            });
    if !valid {
        return Err(FfmpegPreflightError::Failed(
            "platform identity was not a canonical UUID".to_owned(),
        ));
    }
    Ok(normalized)
}

fn platform_resource_id(normalized_uuid: &str) -> Result<String, FfmpegPreflightError> {
    parse_ioreg_platform_uuid(&format!("\"IOPlatformUUID\" = \"{normalized_uuid}\""))?;
    Ok(hex::encode(Sha256::digest(normalized_uuid.as_bytes())))
}

fn parse_labeled_value(text: &str, label: &str) -> Result<String, FfmpegPreflightError> {
    text.lines()
        .find_map(|line| line.trim().strip_prefix(label))
        .and_then(|value| value.strip_prefix(':'))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| {
            FfmpegPreflightError::Failed(format!(
                "system_profiler omitted required `{label}` value"
            ))
        })
}

fn require_videotoolbox_build_features(ffmpeg_path: &Path) -> Result<(), FfmpegPreflightError> {
    for (flag, required) in [
        ("-hwaccels", &["videotoolbox"][..]),
        ("-encoders", &["h264_videotoolbox", "hevc_videotoolbox"][..]),
        ("-filters", &["scale_vt"][..]),
    ] {
        let text = command_text(
            &format!("ffmpeg {flag}"),
            command_output(Command::new(ffmpeg_path).arg("-hide_banner").arg(flag)),
        )?;
        for token in required {
            if parse_token(&text, token).is_none() {
                return Err(FfmpegPreflightError::Failed(format!(
                    "ffmpeg does not advertise required VideoToolbox feature `{token}`"
                )));
            }
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Copy)]
struct VideoToolboxFixtureSpec {
    name: &'static str,
    codec: &'static str,
    pixel_format: &'static str,
    encoder: &'static str,
}

const VIDEOTOOLBOX_FIXTURE_SPECS: [VideoToolboxFixtureSpec; 5] = [
    VideoToolboxFixtureSpec {
        name: "h264-8",
        codec: "h264",
        pixel_format: "yuv420p",
        encoder: "h264_videotoolbox",
    },
    VideoToolboxFixtureSpec {
        name: "hevc-8",
        codec: "hevc",
        pixel_format: "yuv420p",
        encoder: "hevc_videotoolbox",
    },
    VideoToolboxFixtureSpec {
        name: "hevc-10",
        codec: "hevc",
        pixel_format: "yuv420p10le",
        encoder: "hevc_videotoolbox",
    },
    VideoToolboxFixtureSpec {
        name: "av1-8",
        codec: "av1",
        pixel_format: "yuv420p",
        encoder: "libsvtav1",
    },
    VideoToolboxFixtureSpec {
        name: "av1-10",
        codec: "av1",
        pixel_format: "yuv420p10le",
        encoder: "libsvtav1",
    },
];

#[derive(Debug, Clone)]
struct VideoToolboxFixture {
    spec: VideoToolboxFixtureSpec,
    path: PathBuf,
}

fn create_videotoolbox_fixtures(
    ffmpeg_path: &Path,
    probe_dir: &ProbeDir,
) -> Result<Vec<VideoToolboxFixture>, FfmpegPreflightError> {
    let mut fixtures = Vec::with_capacity(VIDEOTOOLBOX_FIXTURE_SPECS.len());
    for spec in VIDEOTOOLBOX_FIXTURE_SPECS {
        let path = probe_dir.path().join(format!("{}.mkv", spec.name));
        create_videotoolbox_fixture(ffmpeg_path, spec, &path)?;
        fixtures.push(VideoToolboxFixture { spec, path });
    }
    Ok(fixtures)
}

fn create_videotoolbox_fixture(
    ffmpeg_path: &Path,
    spec: VideoToolboxFixtureSpec,
    output: &Path,
) -> Result<(), FfmpegPreflightError> {
    let mut command = Command::new(ffmpeg_path);
    command.args([
        "-hide_banner",
        "-nostdin",
        "-f",
        "lavfi",
        "-i",
        "testsrc2=size=256x256:rate=30",
        "-t",
        "3",
        "-an",
        "-vf",
        &format!("format={}", spec.pixel_format),
        "-c:v",
        spec.encoder,
    ]);
    append_videotoolbox_probe_encoder_args(&mut command, spec.encoder, spec.pixel_format);
    command.args(["-f", "matroska", "-y"]).arg(output);
    command_text(
        &format!("create VideoToolbox {} fixture", spec.name),
        command_output(&mut command),
    )
    .map(|_| ())
}

fn append_videotoolbox_probe_encoder_args(
    command: &mut Command,
    encoder: &str,
    pixel_format: &str,
) {
    match encoder {
        "h264_videotoolbox" => {
            command.args([
                "-allow_sw",
                "0",
                "-b:v",
                "4M",
                "-profile:v",
                "high",
                "-level",
                "4.1",
            ]);
        }
        "hevc_videotoolbox" => {
            let profile = if pixel_format == "yuv420p10le" {
                "main10"
            } else {
                "main"
            };
            command.args(["-allow_sw", "0", "-b:v", "4M", "-profile:v", profile]);
        }
        "libsvtav1" => {
            command.args(["-crf", "35", "-preset", "8"]);
        }
        _ => {}
    }
}

fn probe_videotoolbox_decoders(
    ffmpeg_path: &Path,
    fixtures: &[VideoToolboxFixture],
) -> (Vec<VideoToolboxDecodeCapability>, Vec<String>) {
    let mut decoders = Vec::<VideoToolboxDecodeCapability>::new();
    let mut diagnostics = Vec::new();
    for fixture in fixtures {
        match run_videotoolbox_decoder_smoke(ffmpeg_path, fixture) {
            Ok(()) => record_videotoolbox_decoder(&mut decoders, fixture.spec),
            Err(error) => diagnostics.push(format!("{}: {error}", fixture.spec.name)),
        }
    }
    (decoders, diagnostics)
}

fn record_videotoolbox_decoder(
    decoders: &mut Vec<VideoToolboxDecodeCapability>,
    spec: VideoToolboxFixtureSpec,
) {
    if let Some(decoder) = decoders.iter_mut().find(|item| item.codec == spec.codec) {
        decoder.pixel_formats.push(spec.pixel_format.to_owned());
        return;
    }
    decoders.push(VideoToolboxDecodeCapability {
        codec: spec.codec.to_owned(),
        pixel_formats: vec![spec.pixel_format.to_owned()],
    });
}

fn run_videotoolbox_decoder_smoke(
    ffmpeg_path: &Path,
    fixture: &VideoToolboxFixture,
) -> Result<(), FfmpegPreflightError> {
    let mut command = Command::new(ffmpeg_path);
    command.args([
        "-hide_banner",
        "-nostdin",
        "-hwaccel",
        "videotoolbox",
        "-hwaccel_output_format",
        "videotoolbox_vld",
        "-i",
    ]);
    command.arg(&fixture.path).args([
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
        if fixture.spec.pixel_format == "yuv420p10le" {
            "main10"
        } else {
            "main"
        },
        "-f",
        "null",
        "-",
    ]);
    command_text(
        &format!("VideoToolbox {} decoder smoke", fixture.spec.name),
        command_output(&mut command),
    )
    .map(|_| ())
}

fn prove_videotoolbox_capacity(
    ffmpeg_path: &Path,
    config: &VideoToolboxPreflightConfig,
    probe_dir: &ProbeDir,
    fixtures: &[VideoToolboxFixture],
    decoders: &[VideoToolboxDecodeCapability],
) -> Result<(), FfmpegPreflightError> {
    for spec in [
        VideoToolboxFixtureSpec {
            name: "h264-encode",
            codec: "h264",
            pixel_format: "yuv420p",
            encoder: "h264_videotoolbox",
        },
        VideoToolboxFixtureSpec {
            name: "hevc-main-encode",
            codec: "hevc",
            pixel_format: "yuv420p",
            encoder: "hevc_videotoolbox",
        },
        VideoToolboxFixtureSpec {
            name: "hevc-main10-encode",
            codec: "hevc",
            pixel_format: "yuv420p10le",
            encoder: "hevc_videotoolbox",
        },
    ] {
        prove_videotoolbox_capacity_group(
            ffmpeg_path,
            config.max_sessions,
            probe_dir,
            CapacityInput::Software,
            spec,
        )?;
    }
    for fixture in fixtures {
        if !decoder_capability_contains(decoders, fixture.spec) {
            continue;
        }
        let encoder = if fixture.spec.codec == "h264" {
            "h264_videotoolbox"
        } else {
            "hevc_videotoolbox"
        };
        let spec = VideoToolboxFixtureSpec {
            encoder,
            ..fixture.spec
        };
        prove_videotoolbox_capacity_group(
            ffmpeg_path,
            config.max_sessions,
            probe_dir,
            CapacityInput::Hardware(&fixture.path),
            spec,
        )?;
    }
    Ok(())
}

fn decoder_capability_contains(
    decoders: &[VideoToolboxDecodeCapability],
    spec: VideoToolboxFixtureSpec,
) -> bool {
    decoders.iter().any(|decoder| {
        decoder.codec == spec.codec
            && decoder
                .pixel_formats
                .iter()
                .any(|format| format == spec.pixel_format)
    })
}

#[derive(Debug, Clone, Copy)]
enum CapacityInput<'a> {
    Software,
    Hardware(&'a Path),
}

fn prove_videotoolbox_capacity_group(
    ffmpeg_path: &Path,
    max_sessions: u32,
    probe_dir: &ProbeDir,
    input: CapacityInput<'_>,
    spec: VideoToolboxFixtureSpec,
) -> Result<(), FfmpegPreflightError> {
    let VideoToolboxFixtureSpec {
        name,
        encoder,
        pixel_format,
        ..
    } = spec;
    let deadline = std::time::Instant::now() + PROBE_TIMEOUT;
    let mut children = Vec::new();
    let mut progress_paths = Vec::new();
    for session in 0..max_sessions {
        let progress_path = probe_dir.path().join(format!("{name}-{session}.progress"));
        let mut command = videotoolbox_capacity_command(
            ffmpeg_path,
            input,
            encoder,
            pixel_format,
            &progress_path,
        );
        command.stdout(Stdio::null()).stderr(Stdio::piped());
        match command.spawn() {
            Ok(child) => {
                children.push(child);
                progress_paths.push(progress_path);
            }
            Err(error) => {
                kill_and_reap_all(&mut children);
                return Err(FfmpegPreflightError::Failed(format!(
                    "VideoToolbox {name} capacity process failed to start: {error}"
                )));
            }
        }
    }
    require_overlapping_first_frames(name, &mut children, &progress_paths, deadline)?;
    wait_videotoolbox_capacity_children(name, &mut children, deadline)
}

fn wait_videotoolbox_capacity_children(
    name: &str,
    children: &mut Vec<Child>,
    deadline: std::time::Instant,
) -> Result<(), FfmpegPreflightError> {
    while !children.is_empty() {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            kill_and_reap_all(children);
            return Err(FfmpegPreflightError::Failed(format!(
                "VideoToolbox {name} capacity group exceeded {} seconds",
                PROBE_TIMEOUT.as_secs()
            )));
        }
        let Some(child) = children.pop() else {
            break;
        };
        let result = wait_child_output(child, remaining, name)
            .and_then(|output| command_text(name, Ok(output)));
        if let Err(error) = result {
            kill_and_reap_all(children);
            return Err(error);
        }
    }
    Ok(())
}

fn videotoolbox_capacity_command(
    ffmpeg_path: &Path,
    input: CapacityInput<'_>,
    encoder: &str,
    pixel_format: &str,
    progress_path: &Path,
) -> Command {
    let mut command = Command::new(ffmpeg_path);
    command.args(["-hide_banner", "-nostdin"]);
    match input {
        CapacityInput::Software => {
            command.args([
                "-f",
                "lavfi",
                "-re",
                "-i",
                "testsrc2=size=256x256:rate=30",
                "-t",
                "3",
                "-vf",
                &format!("format={pixel_format}"),
            ]);
        }
        CapacityInput::Hardware(path) => {
            command.args([
                "-re",
                "-hwaccel",
                "videotoolbox",
                "-hwaccel_output_format",
                "videotoolbox_vld",
                "-i",
            ]);
            command.arg(path);
        }
    }
    command.args(["-an", "-c:v", encoder]);
    append_videotoolbox_probe_encoder_args(&mut command, encoder, pixel_format);
    command
        .arg("-progress")
        .arg(progress_path)
        .args(["-nostats", "-f", "null", "-"]);
    command
}

fn require_overlapping_first_frames(
    name: &str,
    children: &mut Vec<Child>,
    progress_paths: &[PathBuf],
    deadline: std::time::Instant,
) -> Result<(), FfmpegPreflightError> {
    while std::time::Instant::now() < deadline {
        for index in 0..children.len() {
            let status = match children[index].try_wait() {
                Ok(status) => status,
                Err(error) => {
                    kill_and_reap_all(children);
                    return Err(FfmpegPreflightError::Failed(format!(
                        "polling VideoToolbox {name} capacity process: {error}"
                    )));
                }
            };
            if let Some(status) = status {
                kill_and_reap_all(children);
                return Err(FfmpegPreflightError::Failed(format!(
                    "VideoToolbox {name} capacity process exited {status} before overlap proof"
                )));
            }
        }
        let all_started = progress_paths.iter().all(|path| {
            std::fs::read_to_string(path).is_ok_and(|progress| progress_reports_frame(&progress))
        });
        if all_started {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(50));
    }
    kill_and_reap_all(children);
    Err(FfmpegPreflightError::Failed(format!(
        "VideoToolbox {name} capacity group did not report a first frame before deadline"
    )))
}

fn progress_reports_frame(progress: &str) -> bool {
    progress.lines().any(|line| {
        line.strip_prefix("frame=")
            .and_then(|value| value.trim().parse::<u64>().ok())
            .is_some_and(|frame| frame > 0)
    })
}

fn redacted_command_text(
    command_name: &str,
    output: std::io::Result<std::process::Output>,
) -> Result<String, FfmpegPreflightError> {
    let output = output.map_err(|err| {
        FfmpegPreflightError::Failed(format!("{command_name} failed to start: {err}"))
    })?;
    if output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Ok(format!("{stdout}{stderr}"));
    }
    Err(FfmpegPreflightError::Failed(format!(
        "{command_name} exited with status {}",
        output
            .status
            .code()
            .map_or_else(|| "signal".to_owned(), |code| code.to_string())
    )))
}

#[cfg(test)]
#[path = "videotoolbox_test.rs"]
mod tests;
