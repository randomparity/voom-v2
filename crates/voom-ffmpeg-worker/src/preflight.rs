use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    process::Command,
};

mod nvidia;
mod process;
mod vaapi;
mod videotoolbox;

pub use nvidia::{NvidiaPreflight, NvidiaPreflightConfig, preflight_with_nvidia};
pub use vaapi::{
    DRI_ROOT_ENV, DRM_SYSFS_ROOT_ENV, VAAPI_DEVICE_ENV, VAAPI_MAX_SESSIONS_ENV, VaapiPreflight,
    VaapiPreflightConfig, VaapiProbeClocks, preflight_with_vaapi,
};
pub use videotoolbox::{
    VideoToolboxPreflight, VideoToolboxPreflightConfig, preflight_with_videotoolbox,
};

use nvidia::validate_nvidia_uuid;
use vaapi::{VAAPI_HEVC_ENCODER, vaapi_config_from_env_values};

use process::{
    command_output, command_text, first_output_line, parse_token, require_executable_file,
    resolve_binary,
};

const NVIDIA_DEVICE_ENV: &str = "VOOM_NVIDIA_DEVICE";
const NVIDIA_MAX_SESSIONS_ENV: &str = "VOOM_NVIDIA_MAX_SESSIONS";
const NVIDIA_SMI_BIN_ENV: &str = "VOOM_NVIDIA_SMI_BIN";
const DEFAULT_NVIDIA_SMI_BIN: &str = "nvidia-smi";
const VIDEOTOOLBOX_RESOURCE_ID_ENV: &str = "VOOM_VIDEOTOOLBOX_RESOURCE_ID";
const VIDEOTOOLBOX_MAX_SESSIONS_ENV: &str = "VOOM_VIDEOTOOLBOX_MAX_SESSIONS";

/// Shared `FFmpeg` capabilities plus any accelerator-specific startup proof.
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
    pub vaapi: Option<VaapiPreflight>,
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
            "aac" => !self.aac_encoder.is_empty(),
            "libopus" => !self.opus_encoder.is_empty(),
            _ => self.has_hardware_encoder(encoder),
        }
    }

    fn has_hardware_encoder(&self, encoder: &str) -> bool {
        match encoder {
            "hevc_nvenc" => self.nvidia.is_some(),
            VAAPI_HEVC_ENCODER => self.vaapi.as_ref().is_some_and(|vaapi| {
                vaapi
                    .encoders
                    .iter()
                    .any(|proven| proven == VAAPI_HEVC_ENCODER)
            }),
            "h264_videotoolbox" | "hevc_videotoolbox" => self
                .videotoolbox
                .as_ref()
                .is_some_and(|preflight| preflight.encoders.iter().any(|item| item == encoder)),
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

/// Failure to prove the `FFmpeg` binaries or configured accelerator at startup.
#[derive(Debug, thiserror::Error)]
pub enum FfmpegPreflightError {
    #[error("ffmpeg preflight failed: {0}")]
    Failed(String),
}

pub const FFMPEG_BIN_ENV: &str = "VOOM_FFMPEG_BIN";
pub const FFPROBE_BIN_ENV: &str = "VOOM_FFPROBE_BIN";
const DEFAULT_FFMPEG_BIN: &str = "ffmpeg";
const DEFAULT_FFPROBE_BIN: &str = "ffprobe";

/// Resolves process configuration and runs the selected preflight path.
///
/// # Errors
///
/// Returns an error when environment configuration conflicts or any required capability cannot be
/// proven.
pub fn preflight_from_process_env() -> Result<FfmpegPreflight, FfmpegPreflightError> {
    let ffmpeg =
        std::env::var_os(FFMPEG_BIN_ENV).unwrap_or_else(|| OsString::from(DEFAULT_FFMPEG_BIN));
    let ffprobe =
        std::env::var_os(FFPROBE_BIN_ENV).unwrap_or_else(|| OsString::from(DEFAULT_FFPROBE_BIN));
    let ffmpeg_path = resolve_binary(&ffmpeg);
    let ffprobe_path = resolve_binary(&ffprobe);
    let nvidia = nvidia_config_from_process_env()?;
    let vaapi = vaapi_config_from_process_env()?;
    let videotoolbox = videotoolbox_config_from_process_env()?;
    // One worker binds one accelerator (ADR 0049 §5), so a second configured backend
    // is a supervisor error rather than a preference to resolve here. The tuple match
    // means a fourth backend cannot be added without deciding this rule again.
    match (nvidia, vaapi, videotoolbox) {
        (Some(config), None, None) => preflight_with_nvidia(&ffmpeg_path, &ffprobe_path, &config),
        (None, Some(config), None) => preflight_with_vaapi(&ffmpeg_path, &ffprobe_path, &config),
        (None, None, Some(config)) => {
            preflight_with_videotoolbox(&ffmpeg_path, &ffprobe_path, &config)
        }
        (None, None, None) => preflight_with_paths(&ffmpeg_path, &ffprobe_path),
        _ => Err(FfmpegPreflightError::Failed(
            "NVIDIA, VAAPI, and VideoToolbox configurations are mutually exclusive; one worker \
             binds one accelerator, so run one worker per device"
                .to_owned(),
        )),
    }
}

fn nvidia_config_from_process_env() -> Result<Option<NvidiaPreflightConfig>, FfmpegPreflightError> {
    let device = std::env::var(NVIDIA_DEVICE_ENV).ok();
    let sessions = std::env::var(NVIDIA_MAX_SESSIONS_ENV).ok();
    let Some(device_uuid) = device else {
        if sessions.is_some() {
            return Err(FfmpegPreflightError::Failed(format!(
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
            FfmpegPreflightError::Failed(format!(
                "{NVIDIA_MAX_SESSIONS_ENV} must be an integer in 1..=16: {error}"
            ))
        })?;
    if !(1..=16).contains(&max_sessions) {
        return Err(FfmpegPreflightError::Failed(format!(
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
-> Result<Option<VideoToolboxPreflightConfig>, FfmpegPreflightError> {
    let resource_id = std::env::var(VIDEOTOOLBOX_RESOURCE_ID_ENV).ok();
    let sessions = std::env::var(VIDEOTOOLBOX_MAX_SESSIONS_ENV).ok();
    let Some(resource_id) = resource_id else {
        if sessions.is_some() {
            return Err(FfmpegPreflightError::Failed(format!(
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
            FfmpegPreflightError::Failed(format!(
                "{VIDEOTOOLBOX_MAX_SESSIONS_ENV} must be an integer in 1..=16: {error}"
            ))
        })?;
    if !(1..=16).contains(&max_sessions) {
        return Err(FfmpegPreflightError::Failed(format!(
            "{VIDEOTOOLBOX_MAX_SESSIONS_ENV} must be in 1..=16"
        )));
    }
    Ok(Some(VideoToolboxPreflightConfig {
        resource_id,
        max_sessions,
    }))
}

fn validate_resource_id(resource_id: &str) -> Result<(), FfmpegPreflightError> {
    if resource_id.len() == 64
        && resource_id
            .chars()
            .all(|character| character.is_ascii_hexdigit() && !character.is_ascii_uppercase())
    {
        return Ok(());
    }
    Err(FfmpegPreflightError::Failed(
        "VideoToolbox resource ID must be a lowercase SHA-256 digest".to_owned(),
    ))
}

/// Proves the required shared `FFmpeg` and `FFprobe` versions, encoders, and muxers.
///
/// # Errors
///
/// Returns an error when either binary is unavailable or a required shared capability is absent.
pub fn preflight_with_paths(
    ffmpeg_path: &Path,
    ffprobe_path: &Path,
) -> Result<FfmpegPreflight, FfmpegPreflightError> {
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
        FfmpegPreflightError::Failed(
            "ffmpeg does not advertise required libx265 encoder".to_owned(),
        )
    })?;
    let svtav1_encoder = parse_token(&encoders, "libsvtav1").ok_or_else(|| {
        FfmpegPreflightError::Failed(
            "ffmpeg does not advertise required libsvtav1 encoder".to_owned(),
        )
    })?;
    let libaom_encoder = parse_token(&encoders, "libaom-av1").unwrap_or_default();
    let aac_encoder = parse_token(&encoders, "aac").ok_or_else(|| {
        FfmpegPreflightError::Failed("ffmpeg does not advertise required aac encoder".to_owned())
    })?;
    let opus_encoder = parse_token(&encoders, "libopus").ok_or_else(|| {
        FfmpegPreflightError::Failed(
            "ffmpeg does not advertise required libopus encoder".to_owned(),
        )
    })?;
    let muxers = command_text(
        "ffmpeg -hide_banner -muxers",
        command_output(Command::new(ffmpeg_path).arg("-hide_banner").arg("-muxers")),
    )?;
    let matroska_muxer = parse_token(&muxers, "matroska").ok_or_else(|| {
        FfmpegPreflightError::Failed("ffmpeg does not advertise required matroska muxer".to_owned())
    })?;
    let mp4_muxer = parse_token(&muxers, "mp4").ok_or_else(|| {
        FfmpegPreflightError::Failed("ffmpeg does not advertise required mp4 muxer".to_owned())
    })?;
    let ogg_muxer = parse_token(&muxers, "ogg").ok_or_else(|| {
        FfmpegPreflightError::Failed("ffmpeg does not advertise required ogg muxer".to_owned())
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
        vaapi: None,
        videotoolbox: None,
    })
}

fn vaapi_config_from_process_env() -> Result<Option<VaapiPreflightConfig>, FfmpegPreflightError> {
    let sessions = std::env::var(VAAPI_MAX_SESSIONS_ENV).ok();
    vaapi_config_from_env_values(
        std::env::var(VAAPI_DEVICE_ENV).ok(),
        sessions.as_deref(),
        std::env::var_os(DRI_ROOT_ENV),
        std::env::var_os(DRM_SYSFS_ROOT_ENV),
    )
}

#[cfg(test)]
#[path = "preflight_test.rs"]
mod tests;
