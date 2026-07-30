use sha2::{Digest, Sha256};
use std::{
    ffi::{OsStr, OsString},
    io,
    path::{Path, PathBuf},
    process::{Child, Command, Output, Stdio},
    thread,
    time::Duration,
};
use voom_core::NVIDIA_VIDEO_DECODERS;
use voom_worker_protocol::{VIDEOTOOLBOX_PROBE_TIMEOUT, VideoToolboxDecodeCapability};

const PROBE_TIMEOUT: Duration = VIDEOTOOLBOX_PROBE_TIMEOUT;
const IDENTITY_POLL_WINDOW: Duration = Duration::from_secs(2);
const IDENTITY_POLL_INTERVAL: Duration = Duration::from_millis(100);
const NVIDIA_DEVICE_ENV: &str = "VOOM_NVIDIA_DEVICE";
const NVIDIA_MAX_SESSIONS_ENV: &str = "VOOM_NVIDIA_MAX_SESSIONS";
const NVIDIA_SMI_BIN_ENV: &str = "VOOM_NVIDIA_SMI_BIN";
const DEFAULT_NVIDIA_SMI_BIN: &str = "nvidia-smi";
const VIDEOTOOLBOX_RESOURCE_ID_ENV: &str = "VOOM_VIDEOTOOLBOX_RESOURCE_ID";
const VIDEOTOOLBOX_MAX_SESSIONS_ENV: &str = "VOOM_VIDEOTOOLBOX_MAX_SESSIONS";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NvidiaPreflightConfig {
    pub device_uuid: String,
    pub max_sessions: u32,
    pub nvidia_smi_path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NvidiaPreflight {
    pub device_uuid: String,
    pub device_name: String,
    pub driver_version: String,
    pub max_sessions: u32,
    pub decoders: Vec<String>,
    pub decoder_diagnostics: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoToolboxPreflightConfig {
    pub resource_id: String,
    pub max_sessions: u32,
}

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FfmpegPreflight {
    pub ffmpeg_path: PathBuf,
    pub ffprobe_path: PathBuf,
    pub ffmpeg_version: String,
    pub ffprobe_version: String,
    pub hevc_encoder: String,
    pub svtav1_encoder: String,
    pub libaom_encoder: String,
    pub aac_encoder: String,
    pub opus_encoder: String,
    pub matroska_muxer: String,
    pub mp4_muxer: String,
    pub ogg_muxer: String,
    pub nvidia: Option<NvidiaPreflight>,
    pub videotoolbox: Option<VideoToolboxPreflight>,
}

impl FfmpegPreflight {
    /// Returns true when the named video encoder was detected during preflight.
    #[must_use]
    pub fn has_encoder(&self, encoder: &str) -> bool {
        match encoder {
            "libx265" => !self.hevc_encoder.is_empty(),
            "libsvtav1" => !self.svtav1_encoder.is_empty(),
            "libaom-av1" => !self.libaom_encoder.is_empty(),
            "hevc_nvenc" => self.nvidia.is_some(),
            "h264_videotoolbox" | "hevc_videotoolbox" => self
                .videotoolbox
                .as_ref()
                .is_some_and(|preflight| preflight.encoders.iter().any(|item| item == encoder)),
            "aac" => !self.aac_encoder.is_empty(),
            "libopus" => !self.opus_encoder.is_empty(),
            _ => false,
        }
    }

    /// Returns true when the named muxer was detected during preflight.
    #[must_use]
    pub fn has_muxer(&self, muxer: &str) -> bool {
        match muxer {
            "matroska" | "mkv" => !self.matroska_muxer.is_empty(),
            "mp4" => !self.mp4_muxer.is_empty(),
            "ogg" => !self.ogg_muxer.is_empty(),
            _ => false,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum FFmpegPreflightError {
    #[error("ffmpeg preflight failed: {0}")]
    Failed(String),
}

pub const FFMPEG_BIN_ENV: &str = "VOOM_FFMPEG_BIN";
pub const FFPROBE_BIN_ENV: &str = "VOOM_FFPROBE_BIN";
const DEFAULT_FFMPEG_BIN: &str = "ffmpeg";
const DEFAULT_FFPROBE_BIN: &str = "ffprobe";

pub fn preflight_from_process_env() -> Result<FfmpegPreflight, FFmpegPreflightError> {
    let ffmpeg =
        std::env::var_os(FFMPEG_BIN_ENV).unwrap_or_else(|| OsString::from(DEFAULT_FFMPEG_BIN));
    let ffprobe =
        std::env::var_os(FFPROBE_BIN_ENV).unwrap_or_else(|| OsString::from(DEFAULT_FFPROBE_BIN));
    let ffmpeg_path = resolve_binary(&ffmpeg);
    let ffprobe_path = resolve_binary(&ffprobe);
    let nvidia = nvidia_config_from_process_env()?;
    let videotoolbox = videotoolbox_config_from_process_env()?;
    match (nvidia, videotoolbox) {
        (Some(_), Some(_)) => Err(FFmpegPreflightError::Failed(
            "NVIDIA and VideoToolbox configurations are mutually exclusive".to_owned(),
        )),
        (Some(config), None) => preflight_with_nvidia(&ffmpeg_path, &ffprobe_path, &config),
        (None, Some(config)) => preflight_with_videotoolbox(&ffmpeg_path, &ffprobe_path, &config),
        (None, None) => preflight_with_paths(&ffmpeg_path, &ffprobe_path),
    }
}

fn nvidia_config_from_process_env() -> Result<Option<NvidiaPreflightConfig>, FFmpegPreflightError> {
    let device = std::env::var(NVIDIA_DEVICE_ENV).ok();
    let sessions = std::env::var(NVIDIA_MAX_SESSIONS_ENV).ok();
    let Some(device_uuid) = device else {
        if sessions.is_some() {
            return Err(FFmpegPreflightError::Failed(format!(
                "{NVIDIA_MAX_SESSIONS_ENV} requires {NVIDIA_DEVICE_ENV}"
            )));
        }
        return Ok(None);
    };
    validate_nvidia_uuid(&device_uuid)?;
    let max_sessions = sessions
        .as_deref()
        .unwrap_or("1")
        .parse::<u32>()
        .map_err(|error| {
            FFmpegPreflightError::Failed(format!(
                "{NVIDIA_MAX_SESSIONS_ENV} must be an integer in 1..=16: {error}"
            ))
        })?;
    if !(1..=16).contains(&max_sessions) {
        return Err(FFmpegPreflightError::Failed(format!(
            "{NVIDIA_MAX_SESSIONS_ENV} must be in 1..=16"
        )));
    }
    let nvidia_smi = std::env::var_os(NVIDIA_SMI_BIN_ENV)
        .unwrap_or_else(|| OsString::from(DEFAULT_NVIDIA_SMI_BIN));
    Ok(Some(NvidiaPreflightConfig {
        device_uuid,
        max_sessions,
        nvidia_smi_path: resolve_binary(&nvidia_smi),
    }))
}

fn videotoolbox_config_from_process_env()
-> Result<Option<VideoToolboxPreflightConfig>, FFmpegPreflightError> {
    let resource_id = std::env::var(VIDEOTOOLBOX_RESOURCE_ID_ENV).ok();
    let sessions = std::env::var(VIDEOTOOLBOX_MAX_SESSIONS_ENV).ok();
    let Some(resource_id) = resource_id else {
        if sessions.is_some() {
            return Err(FFmpegPreflightError::Failed(format!(
                "{VIDEOTOOLBOX_MAX_SESSIONS_ENV} requires {VIDEOTOOLBOX_RESOURCE_ID_ENV}"
            )));
        }
        return Ok(None);
    };
    validate_resource_id(&resource_id)?;
    let max_sessions = sessions
        .as_deref()
        .unwrap_or("1")
        .parse::<u32>()
        .map_err(|error| {
            FFmpegPreflightError::Failed(format!(
                "{VIDEOTOOLBOX_MAX_SESSIONS_ENV} must be an integer in 1..=16: {error}"
            ))
        })?;
    if !(1..=16).contains(&max_sessions) {
        return Err(FFmpegPreflightError::Failed(format!(
            "{VIDEOTOOLBOX_MAX_SESSIONS_ENV} must be in 1..=16"
        )));
    }
    Ok(Some(VideoToolboxPreflightConfig {
        resource_id,
        max_sessions,
    }))
}

fn validate_resource_id(resource_id: &str) -> Result<(), FFmpegPreflightError> {
    if resource_id.len() == 64
        && resource_id
            .chars()
            .all(|character| character.is_ascii_hexdigit() && !character.is_ascii_uppercase())
    {
        return Ok(());
    }
    Err(FFmpegPreflightError::Failed(
        "VideoToolbox resource ID must be a lowercase SHA-256 digest".to_owned(),
    ))
}

pub fn preflight_with_paths(
    ffmpeg_path: &Path,
    ffprobe_path: &Path,
) -> Result<FfmpegPreflight, FFmpegPreflightError> {
    require_executable_file("ffmpeg", ffmpeg_path)?;
    require_executable_file("ffprobe", ffprobe_path)?;

    let ffmpeg_version = first_output_line(
        "ffmpeg -hide_banner -version",
        command_output(
            Command::new(ffmpeg_path)
                .arg("-hide_banner")
                .arg("-version"),
        ),
    )?;
    let ffprobe_version = first_output_line(
        "ffprobe -hide_banner -version",
        command_output(
            Command::new(ffprobe_path)
                .arg("-hide_banner")
                .arg("-version"),
        ),
    )?;
    let encoders = command_text(
        "ffmpeg -hide_banner -encoders",
        command_output(
            Command::new(ffmpeg_path)
                .arg("-hide_banner")
                .arg("-encoders"),
        ),
    )?;
    let hevc_encoder = parse_token(&encoders, "libx265").ok_or_else(|| {
        FFmpegPreflightError::Failed(
            "ffmpeg does not advertise required libx265 encoder".to_owned(),
        )
    })?;
    let svtav1_encoder = parse_token(&encoders, "libsvtav1").ok_or_else(|| {
        FFmpegPreflightError::Failed(
            "ffmpeg does not advertise required libsvtav1 encoder".to_owned(),
        )
    })?;
    let libaom_encoder = parse_token(&encoders, "libaom-av1").unwrap_or_default();
    let aac_encoder = parse_token(&encoders, "aac").ok_or_else(|| {
        FFmpegPreflightError::Failed("ffmpeg does not advertise required aac encoder".to_owned())
    })?;
    let opus_encoder = parse_token(&encoders, "libopus").ok_or_else(|| {
        FFmpegPreflightError::Failed(
            "ffmpeg does not advertise required libopus encoder".to_owned(),
        )
    })?;
    let muxers = command_text(
        "ffmpeg -hide_banner -muxers",
        command_output(Command::new(ffmpeg_path).arg("-hide_banner").arg("-muxers")),
    )?;
    let matroska_muxer = parse_token(&muxers, "matroska").ok_or_else(|| {
        FFmpegPreflightError::Failed("ffmpeg does not advertise required matroska muxer".to_owned())
    })?;
    let mp4_muxer = parse_token(&muxers, "mp4").ok_or_else(|| {
        FFmpegPreflightError::Failed("ffmpeg does not advertise required mp4 muxer".to_owned())
    })?;
    let ogg_muxer = parse_token(&muxers, "ogg").ok_or_else(|| {
        FFmpegPreflightError::Failed("ffmpeg does not advertise required ogg muxer".to_owned())
    })?;

    Ok(FfmpegPreflight {
        ffmpeg_path: ffmpeg_path.to_owned(),
        ffprobe_path: ffprobe_path.to_owned(),
        ffmpeg_version,
        ffprobe_version,
        hevc_encoder,
        svtav1_encoder,
        libaom_encoder,
        aac_encoder,
        opus_encoder,
        matroska_muxer,
        mp4_muxer,
        ogg_muxer,
        nvidia: None,
        videotoolbox: None,
    })
}

pub fn preflight_with_nvidia(
    ffmpeg_path: &Path,
    ffprobe_path: &Path,
    config: &NvidiaPreflightConfig,
) -> Result<FfmpegPreflight, FFmpegPreflightError> {
    validate_nvidia_uuid(&config.device_uuid)?;
    if !(1..=16).contains(&config.max_sessions) {
        return Err(FFmpegPreflightError::Failed(
            "NVIDIA max sessions must be in 1..=16".to_owned(),
        ));
    }
    require_executable_file("nvidia-smi", &config.nvidia_smi_path)?;
    let mut preflight = preflight_with_paths(ffmpeg_path, ffprobe_path)?;
    let (device_name, driver_version) = probe_nvidia_identity(config)?;
    require_nvidia_build_features(ffmpeg_path)?;
    prove_nvidia_process_identity(ffmpeg_path, config)?;
    run_hevc_nvenc_smoke(ffmpeg_path, config)?;
    let (decoders, decoder_diagnostics) = probe_nvidia_decoders(ffmpeg_path, config)?;
    prove_nvidia_capacity(ffmpeg_path, config)?;
    preflight.nvidia = Some(NvidiaPreflight {
        device_uuid: config.device_uuid.clone(),
        device_name,
        driver_version,
        max_sessions: config.max_sessions,
        decoders,
        decoder_diagnostics,
    });
    Ok(preflight)
}

pub fn preflight_with_videotoolbox(
    ffmpeg_path: &Path,
    ffprobe_path: &Path,
    config: &VideoToolboxPreflightConfig,
) -> Result<FfmpegPreflight, FFmpegPreflightError> {
    if !cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        return Err(FFmpegPreflightError::Failed(
            "VideoToolbox requires Apple silicon macOS".to_owned(),
        ));
    }
    validate_resource_id(&config.resource_id)?;
    if !(1..=16).contains(&config.max_sessions) {
        return Err(FFmpegPreflightError::Failed(
            "VideoToolbox max sessions must be in 1..=16".to_owned(),
        ));
    }
    let mut preflight = preflight_with_paths(ffmpeg_path, ffprobe_path)?;
    let platform = probe_videotoolbox_platform(config)?;
    require_videotoolbox_build_features(ffmpeg_path)?;
    let probe_dir = VideoToolboxProbeDir::new()?;
    let fixtures = create_videotoolbox_fixtures(ffmpeg_path, &probe_dir)?;
    let (decoders, decoder_diagnostics) = probe_videotoolbox_decoders(ffmpeg_path, &fixtures);
    if decoders.is_empty() {
        return Err(FFmpegPreflightError::Failed(
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
) -> Result<VideoToolboxPlatform, FFmpegPreflightError> {
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
        return Err(FFmpegPreflightError::Failed(
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

fn parse_ioreg_platform_uuid(output: &str) -> Result<String, FFmpegPreflightError> {
    let line = output
        .lines()
        .find(|line| line.contains("\"IOPlatformUUID\""))
        .ok_or_else(|| FFmpegPreflightError::Failed("ioreg omitted IOPlatformUUID".to_owned()))?;
    let value = line
        .split('=')
        .nth(1)
        .map(str::trim)
        .and_then(|value| value.strip_prefix('"'))
        .and_then(|value| value.strip_suffix('"'))
        .ok_or_else(|| {
            FFmpegPreflightError::Failed("ioreg returned malformed IOPlatformUUID".to_owned())
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
        return Err(FFmpegPreflightError::Failed(
            "platform identity was not a canonical UUID".to_owned(),
        ));
    }
    Ok(normalized)
}

fn platform_resource_id(normalized_uuid: &str) -> Result<String, FFmpegPreflightError> {
    parse_ioreg_platform_uuid(&format!("\"IOPlatformUUID\" = \"{normalized_uuid}\""))?;
    Ok(hex::encode(Sha256::digest(normalized_uuid.as_bytes())))
}

fn parse_labeled_value(text: &str, label: &str) -> Result<String, FFmpegPreflightError> {
    text.lines()
        .find_map(|line| line.trim().strip_prefix(label))
        .and_then(|value| value.strip_prefix(':'))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| {
            FFmpegPreflightError::Failed(format!(
                "system_profiler omitted required `{label}` value"
            ))
        })
}

fn require_videotoolbox_build_features(ffmpeg_path: &Path) -> Result<(), FFmpegPreflightError> {
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
                return Err(FFmpegPreflightError::Failed(format!(
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
    probe_dir: &VideoToolboxProbeDir,
) -> Result<Vec<VideoToolboxFixture>, FFmpegPreflightError> {
    let mut fixtures = Vec::with_capacity(VIDEOTOOLBOX_FIXTURE_SPECS.len());
    for spec in VIDEOTOOLBOX_FIXTURE_SPECS {
        let path = probe_dir.path.join(format!("{}.mkv", spec.name));
        create_videotoolbox_fixture(ffmpeg_path, spec, &path)?;
        fixtures.push(VideoToolboxFixture { spec, path });
    }
    Ok(fixtures)
}

fn create_videotoolbox_fixture(
    ffmpeg_path: &Path,
    spec: VideoToolboxFixtureSpec,
    output: &Path,
) -> Result<(), FFmpegPreflightError> {
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
) -> Result<(), FFmpegPreflightError> {
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
    probe_dir: &VideoToolboxProbeDir,
    fixtures: &[VideoToolboxFixture],
    decoders: &[VideoToolboxDecodeCapability],
) -> Result<(), FFmpegPreflightError> {
    for (name, encoder, pixel_format) in [
        ("h264-encode", "h264_videotoolbox", "yuv420p"),
        ("hevc-main-encode", "hevc_videotoolbox", "yuv420p"),
        ("hevc-main10-encode", "hevc_videotoolbox", "yuv420p10le"),
    ] {
        prove_videotoolbox_capacity_group(
            ffmpeg_path,
            config.max_sessions,
            probe_dir,
            CapacityInput::Software,
            name,
            encoder,
            pixel_format,
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
        prove_videotoolbox_capacity_group(
            ffmpeg_path,
            config.max_sessions,
            probe_dir,
            CapacityInput::Hardware(&fixture.path),
            fixture.spec.name,
            encoder,
            fixture.spec.pixel_format,
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
    probe_dir: &VideoToolboxProbeDir,
    input: CapacityInput<'_>,
    name: &str,
    encoder: &str,
    pixel_format: &str,
) -> Result<(), FFmpegPreflightError> {
    let deadline = std::time::Instant::now() + PROBE_TIMEOUT;
    let mut children = Vec::new();
    let mut progress_paths = Vec::new();
    for session in 0..max_sessions {
        let progress_path = probe_dir.path.join(format!("{name}-{session}.progress"));
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
                return Err(FFmpegPreflightError::Failed(format!(
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
) -> Result<(), FFmpegPreflightError> {
    while !children.is_empty() {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            kill_and_reap_all(children);
            return Err(FFmpegPreflightError::Failed(format!(
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
) -> Result<(), FFmpegPreflightError> {
    while std::time::Instant::now() < deadline {
        for index in 0..children.len() {
            let status = match children[index].try_wait() {
                Ok(status) => status,
                Err(error) => {
                    kill_and_reap_all(children);
                    return Err(FFmpegPreflightError::Failed(format!(
                        "polling VideoToolbox {name} capacity process: {error}"
                    )));
                }
            };
            if let Some(status) = status {
                kill_and_reap_all(children);
                return Err(FFmpegPreflightError::Failed(format!(
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
    Err(FFmpegPreflightError::Failed(format!(
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

struct VideoToolboxProbeDir {
    path: PathBuf,
}

impl VideoToolboxProbeDir {
    fn new() -> Result<Self, FFmpegPreflightError> {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|error| {
                FFmpegPreflightError::Failed(format!(
                    "system clock before Unix epoch during VideoToolbox preflight: {error}"
                ))
            })?
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "voom-videotoolbox-probe-{}-{nonce}",
            std::process::id(),
        ));
        std::fs::create_dir(&path).map_err(|error| {
            FFmpegPreflightError::Failed(format!(
                "create VideoToolbox probe directory {}: {error}",
                path.display()
            ))
        })?;
        set_private_directory_permissions(&path)?;
        Ok(Self { path })
    }
}

#[cfg(unix)]
fn set_private_directory_permissions(path: &Path) -> Result<(), FFmpegPreflightError> {
    use std::os::unix::fs::PermissionsExt;

    let permissions = std::fs::Permissions::from_mode(0o700);
    std::fs::set_permissions(path, permissions).map_err(|error| {
        FFmpegPreflightError::Failed(format!(
            "secure VideoToolbox probe directory {}: {error}",
            path.display()
        ))
    })
}

#[cfg(not(unix))]
fn set_private_directory_permissions(_path: &Path) -> Result<(), FFmpegPreflightError> {
    Ok(())
}

impl Drop for VideoToolboxProbeDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn validate_nvidia_uuid(device_uuid: &str) -> Result<(), FFmpegPreflightError> {
    let Some(uuid) = device_uuid.strip_prefix("GPU-") else {
        return Err(FFmpegPreflightError::Failed(
            "NVIDIA device must be a full GPU- UUID".to_owned(),
        ));
    };
    let valid = uuid.len() == 36
        && uuid.char_indices().all(|(index, ch)| match index {
            8 | 13 | 18 | 23 => ch == '-',
            _ => ch.is_ascii_hexdigit(),
        });
    if !valid {
        return Err(FFmpegPreflightError::Failed(
            "NVIDIA device must be a full GPU- UUID".to_owned(),
        ));
    }
    Ok(())
}

fn probe_nvidia_identity(
    config: &NvidiaPreflightConfig,
) -> Result<(String, String), FFmpegPreflightError> {
    let output = command_text(
        "nvidia-smi device identity",
        command_output(
            Command::new(&config.nvidia_smi_path)
                .arg("-i")
                .arg(&config.device_uuid)
                .arg("--query-gpu=uuid,name,driver_version")
                .arg("--format=csv,noheader,nounits"),
        ),
    )?;
    let line = output.lines().next().unwrap_or_default();
    let fields: Vec<&str> = line.split(',').map(str::trim).collect();
    if fields.len() != 3 || fields[0] != config.device_uuid {
        return Err(FFmpegPreflightError::Failed(format!(
            "nvidia-smi returned unexpected identity `{line}` for {}",
            config.device_uuid
        )));
    }
    Ok((fields[1].to_owned(), fields[2].to_owned()))
}

fn require_nvidia_build_features(ffmpeg_path: &Path) -> Result<(), FFmpegPreflightError> {
    for (flag, required) in [
        ("-encoders", &["hevc_nvenc"][..]),
        ("-filters", &["hwupload_cuda", "scale_cuda"][..]),
    ] {
        let text = command_text(
            &format!("ffmpeg {flag}"),
            command_output(Command::new(ffmpeg_path).arg("-hide_banner").arg(flag)),
        )?;
        for token in required {
            if parse_token(&text, token).is_none() {
                return Err(FFmpegPreflightError::Failed(format!(
                    "ffmpeg does not advertise required NVIDIA feature `{token}`"
                )));
            }
        }
    }
    Ok(())
}

fn prove_nvidia_process_identity(
    ffmpeg_path: &Path,
    config: &NvidiaPreflightConfig,
) -> Result<(), FFmpegPreflightError> {
    for _attempt in 0..3 {
        if run_identity_attempt(ffmpeg_path, config)? {
            return Ok(());
        }
    }
    Err(FFmpegPreflightError::Failed(
        "NVENC identity probe exited before its PID could be observed after three attempts"
            .to_owned(),
    ))
}

fn run_identity_attempt(
    ffmpeg_path: &Path,
    config: &NvidiaPreflightConfig,
) -> Result<bool, FFmpegPreflightError> {
    let mut command = nvidia_ffmpeg_command(ffmpeg_path, config);
    command.args([
        "-hide_banner",
        "-nostdin",
        "-f",
        "lavfi",
        "-re",
        "-i",
        "testsrc2=size=256x256:rate=30",
        "-t",
        "3",
        "-an",
        "-c:v",
        "hevc_nvenc",
        "-rc",
        "vbr",
        "-cq",
        "23",
        "-b:v",
        "0",
        "-preset",
        "p4",
        "-f",
        "null",
        "-",
    ]);
    command.stdout(Stdio::null()).stderr(Stdio::piped());
    let mut child = command.spawn().map_err(|error| {
        FFmpegPreflightError::Failed(format!("NVENC identity encode failed to start: {error}"))
    })?;
    let pid = child.id();
    let started = std::time::Instant::now();
    while started.elapsed() < IDENTITY_POLL_WINDOW {
        if let Some(status) = child.try_wait().map_err(|error| {
            FFmpegPreflightError::Failed(format!("polling NVENC identity encode: {error}"))
        })? {
            let output = child.wait_with_output().map_err(|error| {
                FFmpegPreflightError::Failed(format!("reaping NVENC identity encode: {error}"))
            })?;
            if status.success() {
                return Ok(false);
            }
            return Err(FFmpegPreflightError::Failed(format!(
                "NVENC identity encode exited {status}: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        let uuid = match query_compute_uuid(config, pid) {
            Ok(uuid) => uuid,
            Err(error) => {
                kill_and_reap(&mut child);
                return Err(error);
            }
        };
        if let Some(uuid) = uuid {
            if uuid == config.device_uuid {
                let output = wait_child_output(child, PROBE_TIMEOUT, "NVENC identity encode")?;
                command_text("NVENC identity encode", Ok(output))?;
                return Ok(true);
            }
            kill_and_reap(&mut child);
            return Err(FFmpegPreflightError::Failed(format!(
                "NVENC PID {pid} ran on `{uuid}`, expected `{}`",
                config.device_uuid
            )));
        }
        thread::sleep(IDENTITY_POLL_INTERVAL);
    }
    kill_and_reap(&mut child);
    Ok(false)
}

fn query_compute_uuid(
    config: &NvidiaPreflightConfig,
    pid: u32,
) -> Result<Option<String>, FFmpegPreflightError> {
    let output = command_text(
        "nvidia-smi compute-app identity",
        command_output(
            Command::new(&config.nvidia_smi_path)
                .arg("--query-compute-apps=pid,gpu_uuid")
                .arg("--format=csv,noheader,nounits"),
        ),
    )?;
    for line in output.lines() {
        let fields: Vec<&str> = line.split(',').map(str::trim).collect();
        if fields.len() == 2 && fields[0] == pid.to_string() {
            return Ok(Some(fields[1].to_owned()));
        }
    }
    Ok(None)
}

fn run_hevc_nvenc_smoke(
    ffmpeg_path: &Path,
    config: &NvidiaPreflightConfig,
) -> Result<(), FFmpegPreflightError> {
    let mut command = nvidia_ffmpeg_command(ffmpeg_path, config);
    command.args([
        "-hide_banner",
        "-nostdin",
        "-f",
        "lavfi",
        "-i",
        "testsrc2=size=256x256:rate=1",
        "-frames:v",
        "1",
        "-an",
        "-c:v",
        "hevc_nvenc",
        "-rc",
        "vbr",
        "-cq",
        "23",
        "-b:v",
        "0",
        "-preset",
        "p4",
        "-f",
        "null",
        "-",
    ]);
    command_text("HEVC NVENC smoke encode", command_output(&mut command)).map(|_| ())
}

fn probe_nvidia_decoders(
    ffmpeg_path: &Path,
    config: &NvidiaPreflightConfig,
) -> Result<(Vec<String>, Vec<String>), FFmpegPreflightError> {
    let probe_dir = DecoderProbeDir::new()?;
    let mut decoders = Vec::new();
    let mut diagnostics = Vec::new();
    for (codec, decoder) in NVIDIA_VIDEO_DECODERS {
        let encoder = if *codec == "h264" {
            "libx264"
        } else if *codec == "hevc" {
            "libx265"
        } else if *codec == "av1" {
            "libsvtav1"
        } else {
            return Err(FFmpegPreflightError::Failed(format!(
                "NVIDIA decoder probe has no fixture encoder for `{codec}`"
            )));
        };
        let fixture = probe_dir.path.join(format!("{codec}.mkv"));
        create_decoder_fixture(ffmpeg_path, encoder, &fixture)?;
        match run_decoder_smoke(ffmpeg_path, config, decoder, &fixture) {
            Ok(()) => decoders.push((*decoder).to_owned()),
            Err(error) => diagnostics.push(format!("{decoder}: {error}")),
        }
    }
    Ok((decoders, diagnostics))
}

fn create_decoder_fixture(
    ffmpeg_path: &Path,
    encoder: &str,
    output: &Path,
) -> Result<(), FFmpegPreflightError> {
    let mut command = Command::new(ffmpeg_path);
    command.args([
        "-hide_banner",
        "-nostdin",
        "-f",
        "lavfi",
        "-i",
        "testsrc2=size=256x256:rate=1",
        "-frames:v",
        "1",
        "-an",
        "-c:v",
        encoder,
        "-y",
    ]);
    command.arg(output);
    command_text(
        &format!("create {encoder} decoder fixture"),
        command_output(&mut command),
    )
    .map(|_| ())
}

fn run_decoder_smoke(
    ffmpeg_path: &Path,
    config: &NvidiaPreflightConfig,
    decoder: &str,
    fixture: &Path,
) -> Result<(), FFmpegPreflightError> {
    let mut command = nvidia_ffmpeg_command(ffmpeg_path, config);
    command.args([
        "-hide_banner",
        "-nostdin",
        "-hwaccel",
        "cuda",
        "-hwaccel_device",
        "0",
        "-hwaccel_output_format",
        "cuda",
        "-c:v",
        decoder,
        "-i",
    ]);
    command.arg(fixture).args([
        "-vf",
        "scale_cuda=w=256:h=256:format=nv12",
        "-frames:v",
        "1",
        "-f",
        "null",
        "-",
    ]);
    command_text(
        &format!("{decoder} exact-device smoke decode"),
        command_output(&mut command),
    )
    .map(|_| ())
}

fn prove_nvidia_capacity(
    ffmpeg_path: &Path,
    config: &NvidiaPreflightConfig,
) -> Result<(), FFmpegPreflightError> {
    let mut children = Vec::new();
    for _session in 0..config.max_sessions {
        let mut command = nvidia_ffmpeg_command(ffmpeg_path, config);
        command.args([
            "-hide_banner",
            "-nostdin",
            "-f",
            "lavfi",
            "-re",
            "-i",
            "testsrc2=size=256x256:rate=30",
            "-t",
            "1",
            "-an",
            "-c:v",
            "hevc_nvenc",
            "-rc",
            "vbr",
            "-cq",
            "23",
            "-b:v",
            "0",
            "-preset",
            "p4",
            "-f",
            "null",
            "-",
        ]);
        command.stdout(Stdio::piped()).stderr(Stdio::piped());
        match command.spawn() {
            Ok(child) => children.push(child),
            Err(error) => {
                kill_and_reap_all(&mut children);
                return Err(FFmpegPreflightError::Failed(format!(
                    "NVENC concurrency probe failed to start: {error}"
                )));
            }
        }
    }
    while let Some(child) = children.pop() {
        let result = wait_child_output(child, PROBE_TIMEOUT, "NVENC concurrency probe")
            .and_then(|output| command_text("NVENC concurrency probe", Ok(output)));
        if let Err(error) = result {
            kill_and_reap_all(&mut children);
            return Err(error);
        }
    }
    Ok(())
}

fn nvidia_ffmpeg_command(ffmpeg_path: &Path, config: &NvidiaPreflightConfig) -> Command {
    let mut command = Command::new(ffmpeg_path);
    command.env("CUDA_VISIBLE_DEVICES", &config.device_uuid);
    command
}

struct DecoderProbeDir {
    path: PathBuf,
}

impl DecoderProbeDir {
    fn new() -> Result<Self, FFmpegPreflightError> {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|error| {
                FFmpegPreflightError::Failed(format!(
                    "system clock before Unix epoch during NVIDIA preflight: {error}"
                ))
            })?
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "voom-nvidia-decoder-probe-{}-{nonce}",
            std::process::id(),
        ));
        std::fs::create_dir(&path).map_err(|error| {
            FFmpegPreflightError::Failed(format!(
                "create NVIDIA decoder probe directory {}: {error}",
                path.display()
            ))
        })?;
        Ok(Self { path })
    }
}

impl Drop for DecoderProbeDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn resolve_binary(binary: &OsStr) -> PathBuf {
    let path = PathBuf::from(binary);
    if path.components().count() > 1 {
        return path;
    }
    let Some(paths) = std::env::var_os("PATH") else {
        return path;
    };
    for dir in std::env::split_paths(&paths) {
        let candidate = dir.join(&path);
        if is_executable_file(&candidate) {
            return candidate;
        }
    }
    path
}

fn require_executable_file(label: &str, path: &Path) -> Result<(), FFmpegPreflightError> {
    if !is_executable_file(path) {
        return Err(FFmpegPreflightError::Failed(format!(
            "{label} binary is missing or not executable: {}",
            path.display()
        )));
    }
    Ok(())
}

fn is_executable_file(path: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    is_executable_metadata(&metadata)
}

#[cfg(unix)]
fn is_executable_metadata(metadata: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;

    metadata.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn is_executable_metadata(_metadata: &std::fs::Metadata) -> bool {
    true
}

fn first_output_line(
    command_name: &str,
    output: std::io::Result<std::process::Output>,
) -> Result<String, FFmpegPreflightError> {
    command_text(command_name, output)?
        .lines()
        .next()
        .filter(|line| !line.trim().is_empty())
        .map(str::to_owned)
        .ok_or_else(|| FFmpegPreflightError::Failed(format!("{command_name} produced no output")))
}

fn command_text(
    command_name: &str,
    output: std::io::Result<std::process::Output>,
) -> Result<String, FFmpegPreflightError> {
    let output = output.map_err(|err| {
        FFmpegPreflightError::Failed(format!("{command_name} failed to start: {err}"))
    })?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let text = format!("{stdout}{stderr}");
    if output.status.success() {
        Ok(text)
    } else {
        Err(FFmpegPreflightError::Failed(format!(
            "{command_name} exited with status {}: {}",
            output
                .status
                .code()
                .map_or_else(|| "signal".to_owned(), |code| code.to_string()),
            text.trim()
        )))
    }
}

fn redacted_command_text(
    command_name: &str,
    output: std::io::Result<std::process::Output>,
) -> Result<String, FFmpegPreflightError> {
    let output = output.map_err(|err| {
        FFmpegPreflightError::Failed(format!("{command_name} failed to start: {err}"))
    })?;
    if output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Ok(format!("{stdout}{stderr}"));
    }
    Err(FFmpegPreflightError::Failed(format!(
        "{command_name} exited with status {}",
        output
            .status
            .code()
            .map_or_else(|| "signal".to_owned(), |code| code.to_string())
    )))
}

fn command_output(command: &mut Command) -> io::Result<Output> {
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut attempts_remaining = 3;
    loop {
        attempts_remaining -= 1;
        match command
            .spawn()
            .and_then(|child| wait_child_output_io(child, PROBE_TIMEOUT, "dependency probe"))
        {
            Err(err) if is_text_file_busy(&err) && attempts_remaining > 0 => {
                thread::sleep(Duration::from_millis(10));
            }
            result => return result,
        }
    }
}

fn wait_child_output(
    child: Child,
    deadline: Duration,
    label: &str,
) -> Result<Output, FFmpegPreflightError> {
    wait_child_output_io(child, deadline, label).map_err(|error| {
        FFmpegPreflightError::Failed(format!("{label} failed while waiting: {error}"))
    })
}

fn wait_child_output_io(mut child: Child, deadline: Duration, label: &str) -> io::Result<Output> {
    let started = std::time::Instant::now();
    loop {
        if child.try_wait()?.is_some() {
            return child.wait_with_output();
        }
        if started.elapsed() >= deadline {
            kill_and_reap(&mut child);
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!("{label} exceeded {} seconds", deadline.as_secs()),
            ));
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn kill_and_reap(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

fn kill_and_reap_all(children: &mut Vec<Child>) {
    for child in &mut *children {
        kill_and_reap(child);
    }
    children.clear();
}

fn is_text_file_busy(err: &io::Error) -> bool {
    err.raw_os_error() == Some(26)
}

fn parse_token(text: &str, token: &str) -> Option<String> {
    text.lines()
        .find(|line| line.split_whitespace().any(|candidate| candidate == token))
        .map(|_| token.to_owned())
}

#[cfg(test)]
#[path = "preflight_test.rs"]
mod tests;
