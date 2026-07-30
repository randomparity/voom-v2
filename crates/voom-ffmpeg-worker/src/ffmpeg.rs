use std::collections::BTreeSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use serde_json::Value;
use thiserror::Error;
use tokio::process::Command;
use tokio::time::{Duration, timeout};
use voom_core::{VAAPI_VIDEO_DECODERS, VideoEncoderBackend, nvidia_decoder_for_video_codec};
use voom_worker_protocol::{
    AudioDispositionFact, AudioOutputStreamFact, AudioStreamRef, ExtractAudioRequest,
    NvidiaVideoAcceleratorDescriptor, TranscodeAudioRequest, TranscodeVideoProfile,
    TranscodeVideoRequest, VaapiVideoAcceleratorDescriptor, VideoToolboxVideoAcceleratorDescriptor,
};

/// The video encoders advertised by every ffmpeg build voom supports.
pub const ALL_VIDEO_ENCODERS: [&str; 7] = [
    "libx265",
    "libsvtav1",
    "libaom-av1",
    "hevc_nvenc",
    "hevc_vaapi",
    "h264_videotoolbox",
    "hevc_videotoolbox",
];

const VAAPI_HEVC_ENCODER: &str = "hevc_vaapi";
const NVENC_HEVC_ENCODER: &str = "hevc_nvenc";

/// The VAAPI device this worker bound itself to.
///
/// `render_node` is the node preflight resolved the configured PCI address to and
/// then ran its capability probes on, carried here so command generation names the
/// same device the probe proved. Re-resolving the address per command would
/// duplicate that lookup and could disagree with it; a hardcoded
/// `/dev/dri/renderD128` would be wrong on any host whose enumeration differs
/// (ADR 0052 §1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VaapiDeviceBinding {
    pub render_node: PathBuf,
    pub descriptor: VaapiVideoAcceleratorDescriptor,
}

/// The one accelerator a worker is bound to, if any.
///
/// One worker binds one device, so this is a single enum rather than one `Option`
/// per backend: a config that could hold both would make "which device did this
/// worker bind" ambiguous exactly where the argv builder must not guess.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AcceleratorBinding {
    Nvidia(NvidiaVideoAcceleratorDescriptor),
    Vaapi(VaapiDeviceBinding),
    VideoToolbox(VideoToolboxVideoAcceleratorDescriptor),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FfmpegConfig {
    pub ffmpeg_path: PathBuf,
    pub ffprobe_path: PathBuf,
    pub provider_version: String,
    pub process_timeout: Duration,
    accelerator: Option<AcceleratorBinding>,
    available_video_encoders: BTreeSet<String>,
}

impl FfmpegConfig {
    /// Builds a config that assumes every supported video encoder is available.
    ///
    /// Callers wiring a real ffmpeg build (e.g. `main`) must narrow this with
    /// [`FfmpegConfig::with_available_video_encoders`] using the preflight
    /// capabilities so a request naming an absent encoder fails loud.
    #[must_use]
    pub fn new(
        ffmpeg_path: PathBuf,
        ffprobe_path: PathBuf,
        provider_version: String,
        process_timeout: Duration,
    ) -> Self {
        Self {
            ffmpeg_path,
            ffprobe_path,
            provider_version,
            process_timeout,
            accelerator: None,
            available_video_encoders: ALL_VIDEO_ENCODERS
                .iter()
                .map(|encoder| (*encoder).to_owned())
                .collect(),
        }
    }

    /// Restricts the config to the given set of available video encoders.
    #[must_use]
    pub fn with_available_video_encoders(
        mut self,
        encoders: impl IntoIterator<Item = String>,
    ) -> Self {
        self.available_video_encoders = encoders.into_iter().collect();
        self
    }

    /// Returns true when the named video encoder is available in this build.
    #[must_use]
    pub fn has_video_encoder(&self, encoder: &str) -> bool {
        self.available_video_encoders.contains(encoder)
    }

    /// Binds this worker configuration to one accelerator descriptor.
    #[must_use]
    pub fn with_accelerator(mut self, accelerator: NvidiaVideoAcceleratorDescriptor) -> Self {
        self.accelerator = Some(AcceleratorBinding::Nvidia(accelerator));
        self
    }

    /// Binds this worker configuration to one VAAPI render node and its
    /// probe-proven capability.
    #[must_use]
    pub fn with_vaapi_device(mut self, binding: VaapiDeviceBinding) -> Self {
        self.accelerator = Some(AcceleratorBinding::Vaapi(binding));
        self
    }

    /// Binds the config to the `VideoToolbox` host this worker proved at startup.
    #[must_use]
    pub fn with_videotoolbox_device(
        mut self,
        descriptor: VideoToolboxVideoAcceleratorDescriptor,
    ) -> Self {
        self.accelerator = Some(AcceleratorBinding::VideoToolbox(descriptor));
        self
    }

    /// The accelerator this worker bound, if any. `None` means a software worker.
    #[must_use]
    pub const fn accelerator(&self) -> Option<&AcceleratorBinding> {
        self.accelerator.as_ref()
    }

    /// The bound NVIDIA device, or `None` on any other worker.
    #[must_use]
    pub const fn nvidia(&self) -> Option<&NvidiaVideoAcceleratorDescriptor> {
        match &self.accelerator {
            Some(AcceleratorBinding::Nvidia(descriptor)) => Some(descriptor),
            Some(AcceleratorBinding::Vaapi(_) | AcceleratorBinding::VideoToolbox(_)) | None => None,
        }
    }

    /// The bound VAAPI device, or `None` on any other worker.
    #[must_use]
    pub const fn vaapi(&self) -> Option<&VaapiDeviceBinding> {
        match &self.accelerator {
            Some(AcceleratorBinding::Vaapi(binding)) => Some(binding),
            Some(AcceleratorBinding::Nvidia(_) | AcceleratorBinding::VideoToolbox(_)) | None => {
                None
            }
        }
    }

    /// The bound `VideoToolbox` host, or `None` on any other worker.
    #[must_use]
    pub const fn videotoolbox(&self) -> Option<&VideoToolboxVideoAcceleratorDescriptor> {
        match &self.accelerator {
            Some(AcceleratorBinding::VideoToolbox(descriptor)) => Some(descriptor),
            Some(AcceleratorBinding::Nvidia(_) | AcceleratorBinding::Vaapi(_)) | None => None,
        }
    }
}

#[derive(Debug, Error)]
pub enum FfmpegError {
    #[error("ffmpeg failed: {0}")]
    FfmpegFailed(String),
    #[error("ffprobe failed: {0}")]
    FfprobeFailed(String),
    #[error("malformed media: {0}")]
    MalformedMedia(String),
    #[error("output facts mismatch: {0}")]
    OutputFactsMismatch(String),
    #[error("unsupported input: {0}")]
    UnsupportedInput(String),
}

/// Diagnostics that mean the *input bytes* are structurally unusable regardless
/// of the ffmpeg build — a permanent `FailureClass::MalformedMedia`, not a
/// transient tool failure. Deliberately narrow (precision over recall): a missed
/// signature degrades to the pre-existing retriable `FfmpegFailed`/`FfprobeFailed`
/// mapping, whereas a false positive would wrongly condemn a transient failure.
/// See `docs/adr/0024`. Kept in sync with the ffprobe worker's copy.
fn is_malformed_media_stderr(stderr: &str) -> bool {
    const SIGNATURES: [&str; 4] = [
        "invalid data found when processing input",
        "moov atom not found",
        "error opening input",
        "header missing",
    ];
    let lowered = stderr.to_ascii_lowercase();
    SIGNATURES
        .iter()
        .any(|signature| lowered.contains(signature))
}

/// Classify a non-zero process exit. A structural-input-fault stderr on a real
/// exit (not a signal kill) is a permanent [`FfmpegError::MalformedMedia`];
/// anything else takes the caller's transient constructor
/// (`FfmpegFailed`/`FfprobeFailed`).
fn classify_process_failure(
    output: &std::process::Output,
    transient: impl Fn(String) -> FfmpegError,
) -> FfmpegError {
    let message = command_error(output);
    if output.status.code().is_some()
        && is_malformed_media_stderr(&String::from_utf8_lossy(&output.stderr))
    {
        return FfmpegError::MalformedMedia(message);
    }
    transient(message)
}

/// Facts probed from the output file after a successful transcode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputProbe {
    pub container: String,
    pub video_codec: String,
    pub width: u32,
    pub height: u32,
    pub pixel_format: String,
}

/// Facts probed from the input file before transcoding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputProbe {
    pub width: u32,
    pub height: u32,
    pub codec: String,
    pub pixel_format: String,
    pub codec_profile: Option<String>,
    pub codec_level: Option<String>,
    pub video_stream_count: u32,
    pub forced_subtitle_ordinals: Vec<usize>,
}

/// Input facts needed to build a device-correct transcode command.
#[derive(Debug, Clone, Copy)]
pub struct VideoTranscodeInput<'a> {
    pub width: u32,
    pub height: u32,
    pub codec: &'a str,
    pub forced_subtitle_ordinals: &'a [usize],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioOutputProbe {
    pub container: String,
    pub audio_codecs: Vec<String>,
    pub selected_output_streams: Vec<AudioOutputStreamFact>,
    pub output_language: Option<String>,
    pub output_title: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceAudioFact {
    pub snapshot_stream_id: Option<String>,
    pub provider_stream_index: u32,
    pub audio_ordinal: usize,
    pub codec: String,
    pub language: Option<String>,
    pub title: Option<String>,
    pub default: Option<bool>,
    pub disposition: Option<AudioDispositionFact>,
    pub channels: Option<u64>,
}

pub const DEFAULT_PROCESS_TIMEOUT: Duration = Duration::from_hours(2);

/// Returns the video codec arguments for the given profile.
///
/// When `copy_video` is true, emits `-c:v copy` regardless of encoder.
/// Otherwise branches on `profile.encoder` to emit the per-encoder flags.
///
/// # Errors
/// Returns `FfmpegError::OutputFactsMismatch` for an unrecognized encoder.
/// The contract validation in the handler rejects unknown encoders before
/// reaching here; this arm is defensive and must never silently pass through.
pub fn video_codec_args(
    profile: &TranscodeVideoProfile,
    copy_video: bool,
) -> Result<Vec<OsString>, FfmpegError> {
    if copy_video {
        return Ok(vec![OsString::from("-c:v"), OsString::from("copy")]);
    }
    match profile.encoder.as_str() {
        "libx265" => video_codec_args_x265(profile),
        "libsvtav1" => video_codec_args_svtav1(profile),
        "libaom-av1" => video_codec_args_libaom(profile),
        NVENC_HEVC_ENCODER => video_codec_args_nvenc(profile),
        VAAPI_HEVC_ENCODER => video_codec_args_vaapi(profile),
        "h264_videotoolbox" | "hevc_videotoolbox" => video_codec_args_videotoolbox(profile),
        other => Err(FfmpegError::OutputFactsMismatch(format!(
            "unknown video encoder `{other}`"
        ))),
    }
}

fn video_codec_args_x265(profile: &TranscodeVideoProfile) -> Result<Vec<OsString>, FfmpegError> {
    let mut args = vec![
        OsString::from("-c:v"),
        OsString::from("libx265"),
        OsString::from("-crf"),
        required_quality(profile.crf, "crf", &profile.encoder)?,
        OsString::from("-preset"),
        required_preset(profile)?,
    ];
    if let Some(tune) = &profile.tune {
        args.push(OsString::from("-tune"));
        args.push(OsString::from(tune));
    }
    if let Some(codec_profile) = &profile.codec_profile {
        args.push(OsString::from("-profile:v"));
        args.push(OsString::from(codec_profile));
    }
    if let Some(level) = &profile.codec_level {
        args.push(OsString::from("-level"));
        args.push(OsString::from(level));
    }
    append_pixel_format_arg(&mut args, profile);
    Ok(args)
}

fn video_codec_args_svtav1(profile: &TranscodeVideoProfile) -> Result<Vec<OsString>, FfmpegError> {
    let mut args = vec![
        OsString::from("-c:v"),
        OsString::from("libsvtav1"),
        OsString::from("-crf"),
        required_quality(profile.crf, "crf", &profile.encoder)?,
        OsString::from("-preset"),
        required_preset(profile)?,
    ];
    if let Some(codec_profile) = &profile.codec_profile {
        args.push(OsString::from("-profile:v"));
        args.push(OsString::from(codec_profile));
    }
    // tune and level go via -svtav1-params for libsvtav1
    let mut svt_params: Vec<String> = Vec::new();
    if let Some(tune) = &profile.tune {
        svt_params.push(format!("tune={tune}"));
    }
    if let Some(level) = &profile.codec_level {
        svt_params.push(format!("level={level}"));
    }
    if !svt_params.is_empty() {
        args.push(OsString::from("-svtav1-params"));
        args.push(OsString::from(svt_params.join(":")));
    }
    append_pixel_format_arg(&mut args, profile);
    Ok(args)
}

fn video_codec_args_libaom(profile: &TranscodeVideoProfile) -> Result<Vec<OsString>, FfmpegError> {
    let mut args = vec![
        OsString::from("-c:v"),
        OsString::from("libaom-av1"),
        OsString::from("-crf"),
        required_quality(profile.crf, "crf", &profile.encoder)?,
        OsString::from("-b:v"),
        OsString::from("0"),
        OsString::from("-cpu-used"),
        required_preset(profile)?,
    ];
    if let Some(tune) = &profile.tune {
        args.push(OsString::from("-tune"));
        args.push(OsString::from(tune));
    }
    if let Some(codec_profile) = &profile.codec_profile {
        args.push(OsString::from("-profile:v"));
        args.push(OsString::from(codec_profile));
    }
    append_pixel_format_arg(&mut args, profile);
    Ok(args)
}

fn video_codec_args_nvenc(profile: &TranscodeVideoProfile) -> Result<Vec<OsString>, FfmpegError> {
    let mut args = vec![
        OsString::from("-c:v"),
        OsString::from("hevc_nvenc"),
        OsString::from("-rc"),
        OsString::from("vbr"),
        OsString::from("-cq"),
        required_quality(profile.cq, "cq", &profile.encoder)?,
        OsString::from("-b:v"),
        OsString::from("0"),
        OsString::from("-preset"),
        required_preset(profile)?,
    ];
    if let Some(tune) = &profile.tune {
        args.push(OsString::from("-tune"));
        args.push(OsString::from(tune));
    }
    if let Some(codec_profile) = &profile.codec_profile {
        args.push(OsString::from("-profile:v"));
        args.push(OsString::from(codec_profile));
    }
    if let Some(level) = &profile.codec_level {
        args.push(OsString::from("-level"));
        args.push(OsString::from(level));
    }
    Ok(args)
}

/// `hevc_vaapi` encoder arguments (spec §7).
///
/// `-rc_mode CQP` is always stated: `auto` is `FFmpeg`'s default, so relying on it
/// would let rate control move with an `FFmpeg` or driver upgrade (ADR 0052 §5).
/// `-profile:v` carries the operator's `codec_profile` **by name** — the option is
/// int-typed but has named constants, so `FFmpeg` resolves `main10` and rejects an
/// unknown name, which is exactly the behavior we want and needs no mapping table
/// here. It is emitted only when set, because the uploaded surface format already
/// selects the profile (spec §2.2) and inventing one would state a choice the
/// operator did not make. There is no `-preset` (the encoder has none), no
/// `-b:v 0` (CQP needs none), and no `-pix_fmt`: a VAAPI pixel format names a
/// hardware surface and is carried by the upload filter.
fn video_codec_args_vaapi(profile: &TranscodeVideoProfile) -> Result<Vec<OsString>, FfmpegError> {
    if let Some(level) = &profile.codec_level {
        return Err(FfmpegError::OutputFactsMismatch(format!(
            "encoder `{VAAPI_HEVC_ENCODER}` accepts no codec_level; got `{level}`"
        )));
    }
    let mut args = vec![
        OsString::from("-c:v"),
        OsString::from(VAAPI_HEVC_ENCODER),
        OsString::from("-rc_mode"),
        OsString::from("CQP"),
        OsString::from("-qp"),
        required_quality(profile.qp, "qp", &profile.encoder)?,
    ];
    if let Some(codec_profile) = &profile.codec_profile {
        args.push(OsString::from("-profile:v"));
        args.push(OsString::from(codec_profile));
    }
    Ok(args)
}

fn video_codec_args_videotoolbox(
    profile: &TranscodeVideoProfile,
) -> Result<Vec<OsString>, FfmpegError> {
    let bitrate_kbps = profile.bitrate_kbps.ok_or_else(|| {
        FfmpegError::OutputFactsMismatch(format!(
            "encoder `{}` requires `bitrate_kbps`",
            profile.encoder
        ))
    })?;
    let mut args = vec![
        OsString::from("-c:v"),
        OsString::from(&profile.encoder),
        OsString::from("-allow_sw"),
        OsString::from("0"),
        OsString::from("-b:v"),
        OsString::from(format!("{bitrate_kbps}k")),
    ];
    if let Some(codec_profile) = &profile.codec_profile {
        args.push(OsString::from("-profile:v"));
        args.push(OsString::from(codec_profile));
    }
    if let Some(level) = &profile.codec_level {
        args.push(OsString::from("-level"));
        args.push(OsString::from(level));
    }
    Ok(args)
}

/// Every encoder reaching this module has a speed knob, so a missing `preset` is a
/// contract violation to report — never a value to substitute.
fn required_preset(profile: &TranscodeVideoProfile) -> Result<OsString, FfmpegError> {
    let Some(preset) = &profile.preset else {
        return Err(FfmpegError::OutputFactsMismatch(format!(
            "encoder `{}` requires `preset`",
            profile.encoder
        )));
    };
    Ok(OsString::from(preset))
}

fn required_quality(
    value: Option<u8>,
    field: &str,
    encoder: &str,
) -> Result<OsString, FfmpegError> {
    let Some(value) = value else {
        return Err(FfmpegError::OutputFactsMismatch(format!(
            "encoder `{encoder}` requires `{field}`"
        )));
    };
    Ok(OsString::from(value.to_string()))
}

fn append_pixel_format_arg(args: &mut Vec<OsString>, profile: &TranscodeVideoProfile) {
    if let Some(pixel_format) = &profile.pixel_format {
        args.push(OsString::from("-pix_fmt"));
        args.push(OsString::from(pixel_format));
    }
}

/// Returns container/format arguments for the given container and video codec.
///
/// - `mkv` → `-f matroska`
/// - `mp4` + `h264` → `-f mp4 -tag:v avc1`
/// - `mp4` + `hevc` → `-f mp4 -tag:v hvc1`
/// - `mp4` + `av1` → `-f mp4 -tag:v av01`
///
/// # Errors
/// Returns `FfmpegError::OutputFactsMismatch` for an mp4 container with a video
/// codec that has no defined mp4 tag, or for any container other than mkv/mp4.
/// `validate_request_contract` already gates the container to mkv/mp4, so an
/// unsupported container here means an upstream contract was bypassed; we fail
/// loud rather than pass an unvalidated `-f <container>` to ffmpeg.
pub fn container_args(container: &str, codec: &str) -> Result<Vec<OsString>, FfmpegError> {
    match container {
        "mkv" => Ok(vec![OsString::from("-f"), OsString::from("matroska")]),
        "mp4" => {
            let tag = match codec {
                "h264" => "avc1",
                "hevc" => "hvc1",
                "av1" => "av01",
                other => {
                    return Err(FfmpegError::OutputFactsMismatch(format!(
                        "unsupported mp4 video codec {other}"
                    )));
                }
            };
            Ok(vec![
                OsString::from("-f"),
                OsString::from("mp4"),
                OsString::from("-tag:v"),
                OsString::from(tag),
            ])
        }
        other => Err(FfmpegError::OutputFactsMismatch(format!(
            "unsupported transcode_video output container `{other}` (mkv or mp4)"
        ))),
    }
}

/// Returns the scale filter arguments for aspect-preserving downscale-only.
///
/// Only emits `-vf scale=...` when the source dimensions exceed the profile's
/// caps. A missing cap is treated as unbounded so a single-dimension cap is
/// honored independently (matching policy validation and the planner, which
/// treat `max_width` and `max_height` as independent). The filter forces even
/// dimensions (required by most codecs).
#[must_use]
pub fn scale_args(profile: &TranscodeVideoProfile, src_w: u32, src_h: u32) -> Vec<OsString> {
    software_scale_filter(profile, src_w, src_h)
        .map(|filter| vec![OsString::from("-vf"), OsString::from(filter)])
        .unwrap_or_default()
}

fn software_scale_filter(
    profile: &TranscodeVideoProfile,
    src_w: u32,
    src_h: u32,
) -> Option<String> {
    if profile.max_width.is_none() && profile.max_height.is_none() {
        return None;
    }
    let cap_w = profile.max_width.unwrap_or(u32::MAX);
    let cap_h = profile.max_height.unwrap_or(u32::MAX);
    if src_w <= cap_w && src_h <= cap_h {
        return None;
    }
    // Downscale-only, preserve aspect, force even dims.
    // See also: voom-plan/src/planner.rs for the dimension-cap logic.
    Some(format!(
        "scale='min({cap_w},iw)':'min({cap_h},ih)':force_original_aspect_ratio=decrease,\
         scale=trunc(iw/2)*2:trunc(ih/2)*2"
    ))
}

pub async fn run_ffmpeg_transcode(
    config: &FfmpegConfig,
    request: &TranscodeVideoRequest,
    source: VideoTranscodeInput<'_>,
) -> Result<OutputProbe, FfmpegError> {
    let input = Path::new(&request.input.path);
    let output = Path::new(&request.output.path);
    let profile = &request.profile;
    let container = &request.output.container;
    let codec = &request.output.video_codec;

    let mut command = Command::new(&config.ffmpeg_path);
    command.arg("-hide_banner").arg("-nostdin").arg("-n");
    append_hardware_input_args(
        &mut command,
        config,
        profile,
        source.codec,
        request.copy_video,
    )?;
    command
        .arg("-i")
        .arg(input)
        .arg("-map")
        .arg("0:v:0")
        .arg("-map")
        .arg("0:a?")
        .arg("-map")
        .arg("0:s?")
        .arg("-map")
        .arg("0:t?");

    for arg in video_codec_args(profile, request.copy_video)? {
        command.arg(arg);
    }
    for arg in video_filter_args(profile, source.width, source.height, request.copy_video)? {
        command.arg(arg);
    }
    command
        .arg("-c:a")
        .arg("copy")
        .arg("-c:s")
        .arg("copy")
        .arg("-c:t")
        .arg("copy")
        .arg("-map_metadata")
        .arg("0");
    for ordinal in source.forced_subtitle_ordinals {
        command
            .arg(format!("-disposition:s:{ordinal}"))
            .arg("+forced");
    }
    for arg in container_args(container, codec)? {
        command.arg(arg);
    }
    command.arg(output).kill_on_drop(true);

    let process_output = timeout(
        config.process_timeout,
        output_retrying_etxtbsy(&mut command),
    )
    .await
    .map_err(|_| FfmpegError::FfmpegFailed("ffmpeg timed out".to_owned()))?
    .map_err(|err| FfmpegError::FfmpegFailed(err.to_string()))?;
    if !process_output.status.success() {
        return Err(classify_process_failure(
            &process_output,
            FfmpegError::FfmpegFailed,
        ));
    }

    probe_output(config, output, container, codec, profile).await
}

/// Pre-input arguments that bind the command to a device. A `copy_video` request
/// emits `-c:v copy` and touches no encoder, so it needs no device at all.
fn append_hardware_input_args(
    command: &mut Command,
    config: &FfmpegConfig,
    profile: &TranscodeVideoProfile,
    source_codec: &str,
    copy_video: bool,
) -> Result<(), FfmpegError> {
    if copy_video {
        return Ok(());
    }
    // Dispatch on the encoder's declared backend, never on its name: a name match
    // needs a wildcard the compiler cannot check, and a new hardware encoder falling
    // into it would silently get no device arguments at all.
    let descriptor = voom_core::encoder_descriptor(&profile.encoder).ok_or_else(|| {
        FfmpegError::OutputFactsMismatch(format!("unknown video encoder `{}`", profile.encoder))
    })?;
    match descriptor.backend {
        VideoEncoderBackend::Software => Ok(()),
        VideoEncoderBackend::Nvidia => {
            append_nvidia_input_args(command, config, profile, source_codec)
        }
        VideoEncoderBackend::Vaapi => {
            append_vaapi_input_args(command, config, profile, source_codec)
        }
        VideoEncoderBackend::VideoToolbox => {
            append_videotoolbox_input_args(command, config, profile)
        }
    }
}

/// VAAPI pre-input arguments (spec §7).
///
/// The bound render node is named at open time either way — that naming, not the
/// PCI readback, is where VAAPI binding strength comes from (ADR 0052 §1). A
/// software-decoded source only needs the device (`-vaapi_device`); a VAAPI-decoded
/// one additionally pins the decode hardware and demands hardware output frames,
/// which makes `FFmpeg` error rather than silently decode in software (spec §2.2).
/// Unlike `CUVID` there is no per-codec decoder name to select: `-hwaccel vaapi`
/// plus the codec's own decoder is the whole selection (spec §3).
fn append_vaapi_input_args(
    command: &mut Command,
    config: &FfmpegConfig,
    profile: &TranscodeVideoProfile,
    source_codec: &str,
) -> Result<(), FfmpegError> {
    let Some(binding) = config.vaapi() else {
        return Err(FfmpegError::OutputFactsMismatch(format!(
            "`{VAAPI_HEVC_ENCODER}` request reached an ffmpeg worker with no bound VAAPI device"
        )));
    };
    if !profile.decode.is_vaapi() {
        command.arg("-vaapi_device").arg(&binding.render_node);
        return Ok(());
    }
    if !VAAPI_VIDEO_DECODERS.contains(&source_codec) {
        return Err(FfmpegError::UnsupportedInput(format!(
            "VAAPI decode does not support source codec `{source_codec}`"
        )));
    }
    command
        .arg("-hwaccel")
        .arg("vaapi")
        .arg("-hwaccel_device")
        .arg(&binding.render_node)
        .arg("-hwaccel_output_format")
        .arg("vaapi");
    Ok(())
}

fn append_nvidia_input_args(
    command: &mut Command,
    config: &FfmpegConfig,
    profile: &TranscodeVideoProfile,
    source_codec: &str,
) -> Result<(), FfmpegError> {
    let Some(accelerator) = config.nvidia() else {
        return Err(FfmpegError::OutputFactsMismatch(
            "hevc_nvenc request reached an unbound ffmpeg worker".to_owned(),
        ));
    };
    command.env("CUDA_VISIBLE_DEVICES", &accelerator.device_uuid);
    if profile.decode.is_software() {
        return Ok(());
    }
    let decoder = nvidia_decoder_for_video_codec(source_codec).ok_or_else(|| {
        FfmpegError::UnsupportedInput(format!(
            "NVIDIA decode does not support source codec `{source_codec}`"
        ))
    })?;
    command
        .arg("-hwaccel")
        .arg("cuda")
        .arg("-hwaccel_device")
        .arg("0")
        .arg("-hwaccel_output_format")
        .arg("cuda")
        .arg("-c:v")
        .arg(decoder);
    Ok(())
}

fn append_videotoolbox_input_args(
    command: &mut Command,
    config: &FfmpegConfig,
    profile: &TranscodeVideoProfile,
) -> Result<(), FfmpegError> {
    if config.videotoolbox().is_none() {
        return Err(FfmpegError::OutputFactsMismatch(format!(
            "{} request reached an unbound ffmpeg worker",
            profile.encoder
        )));
    }
    if profile.decode.is_video_toolbox() {
        command
            .arg("-hwaccel")
            .arg("videotoolbox")
            .arg("-hwaccel_output_format")
            .arg("videotoolbox_vld");
    }
    Ok(())
}

fn video_filter_args(
    profile: &TranscodeVideoProfile,
    src_width: u32,
    src_height: u32,
    copy_video: bool,
) -> Result<Vec<OsString>, FfmpegError> {
    if copy_video {
        return Ok(Vec::new());
    }
    let descriptor = voom_core::encoder_descriptor(&profile.encoder).ok_or_else(|| {
        FfmpegError::OutputFactsMismatch(format!("unknown video encoder `{}`", profile.encoder))
    })?;
    match descriptor.backend {
        VideoEncoderBackend::Software => Ok(scale_args(profile, src_width, src_height)),
        VideoEncoderBackend::Nvidia => nvenc_filter_args(profile, src_width, src_height),
        VideoEncoderBackend::Vaapi => vaapi_filter_args(profile, src_width, src_height),
        VideoEncoderBackend::VideoToolbox => {
            videotoolbox_filter_args(profile, src_width, src_height)
        }
    }
}

/// VAAPI frame transfers are explicit in both directions (spec §7).
///
/// A software-decoded source uploads with `format=<surface>,hwupload`. A
/// VAAPI-decoded source is already in hardware frames and takes **no** filter:
/// inserting one would download and re-upload every frame, and restating the
/// surface format the decoder chose is not this layer's call.
fn vaapi_filter_args(
    profile: &TranscodeVideoProfile,
    src_width: u32,
    src_height: u32,
) -> Result<Vec<OsString>, FfmpegError> {
    let surface_format = vaapi_surface_format(profile)?;
    if exceeds_dimension_caps(profile, src_width, src_height) {
        return Err(FfmpegError::OutputFactsMismatch(format!(
            "profile `{}` caps output at {}x{} but the source is {src_width}x{src_height}, and \
             `{VAAPI_HEVC_ENCODER}` has no verified scale filter in this slice",
            profile.name,
            profile.max_width.unwrap_or(u32::MAX),
            profile.max_height.unwrap_or(u32::MAX),
        )));
    }
    if profile.decode.is_vaapi() {
        return Ok(Vec::new());
    }
    Ok(vec![
        OsString::from("-vf"),
        OsString::from(format!("format={surface_format},hwupload")),
    ])
}

/// A VAAPI `pixel_format` names a hardware **surface** format, not the software
/// format the other encoders take. `nv12` and `p010` are the two the `HEVC_VAAPI`
/// descriptor allows and the two spec §2.2 verified end to end.
fn vaapi_surface_format(profile: &TranscodeVideoProfile) -> Result<&str, FfmpegError> {
    match profile.pixel_format.as_deref() {
        None | Some("nv12") => Ok("nv12"),
        Some("p010") => Ok("p010"),
        Some(other) => Err(FfmpegError::OutputFactsMismatch(format!(
            "unsupported VAAPI surface format `{other}` (nv12 or p010)"
        ))),
    }
}

fn nvenc_filter_args(
    profile: &TranscodeVideoProfile,
    src_width: u32,
    src_height: u32,
) -> Result<Vec<OsString>, FfmpegError> {
    let pixel_format = match profile.pixel_format.as_deref() {
        None | Some("yuv420p") => "nv12",
        Some("yuv420p10le") => "p010le",
        Some(other) => {
            return Err(FfmpegError::OutputFactsMismatch(format!(
                "unsupported NVENC pixel format `{other}`"
            )));
        }
    };
    let scaling = scale_filter(profile, src_width, src_height);
    let filter = if profile.decode.is_nvidia() {
        scaling.map_or_else(
            || format!("scale_cuda=format={pixel_format}"),
            |scale| format!("{scale}:format={pixel_format}"),
        )
    } else {
        let upload = format!("format={pixel_format},hwupload_cuda=device=0");
        scaling.map_or(upload.clone(), |scale| {
            format!("{upload},{scale}:format={pixel_format}")
        })
    };
    Ok(vec![OsString::from("-vf"), OsString::from(filter)])
}

/// True when the source exceeds either dimension cap the profile sets. A missing
/// cap is unbounded, so a single-dimension cap is honored independently — matching
/// `scale_args` and the planner.
fn exceeds_dimension_caps(
    profile: &TranscodeVideoProfile,
    src_width: u32,
    src_height: u32,
) -> bool {
    src_width > profile.max_width.unwrap_or(u32::MAX)
        || src_height > profile.max_height.unwrap_or(u32::MAX)
}

fn videotoolbox_filter_args(
    profile: &TranscodeVideoProfile,
    src_width: u32,
    src_height: u32,
) -> Result<Vec<OsString>, FfmpegError> {
    if profile.decode.is_video_toolbox() {
        let Some((width, height)) = downscale_dimensions(profile, src_width, src_height) else {
            return Ok(Vec::new());
        };
        return Ok(vec![
            OsString::from("-vf"),
            OsString::from(format!("scale_vt=w={width}:h={height}")),
        ]);
    }
    let format = match profile.pixel_format.as_deref() {
        Some("yuv420p") => "nv12",
        Some("yuv420p10le") => "p010le",
        Some(other) => {
            return Err(FfmpegError::OutputFactsMismatch(format!(
                "unsupported VideoToolbox pixel format `{other}`"
            )));
        }
        None => {
            return Err(FfmpegError::OutputFactsMismatch(
                "VideoToolbox profile omitted pixel format".to_owned(),
            ));
        }
    };
    let filter = software_scale_filter(profile, src_width, src_height).map_or_else(
        || format!("format={format}"),
        |scale| format!("{scale},format={format}"),
    );
    Ok(vec![OsString::from("-vf"), OsString::from(filter)])
}

fn downscale_dimensions(
    profile: &TranscodeVideoProfile,
    src_width: u32,
    src_height: u32,
) -> Option<(u32, u32)> {
    let cap_width = profile.max_width.unwrap_or(u32::MAX);
    let cap_height = profile.max_height.unwrap_or(u32::MAX);
    if src_width <= cap_width && src_height <= cap_height {
        return None;
    }
    let width_limited = u64::from(cap_width) * u64::from(src_height)
        <= u64::from(cap_height) * u64::from(src_width);
    let (width, height) = if width_limited {
        let height = u64::from(src_height) * u64::from(cap_width) / u64::from(src_width);
        (cap_width, u32::try_from(height).unwrap_or(cap_height))
    } else {
        let width = u64::from(src_width) * u64::from(cap_height) / u64::from(src_height);
        (u32::try_from(width).unwrap_or(cap_width), cap_height)
    };
    Some((even_dimension(width), even_dimension(height)))
}

fn even_dimension(value: u32) -> u32 {
    (value & !1).max(2)
}

fn scale_filter(
    profile: &TranscodeVideoProfile,
    src_width: u32,
    src_height: u32,
) -> Option<String> {
    let cap_width = profile.max_width.unwrap_or(u32::MAX);
    let cap_height = profile.max_height.unwrap_or(u32::MAX);
    if !exceeds_dimension_caps(profile, src_width, src_height) {
        return None;
    }
    Some(format!(
        "scale_cuda=w='min({cap_width},iw)':h='min({cap_height},ih)':\
         force_original_aspect_ratio=decrease"
    ))
}

pub async fn run_ffmpeg_transcode_audio(
    config: &FfmpegConfig,
    input: &Path,
    output: &Path,
    request: &TranscodeAudioRequest,
) -> Result<AudioOutputProbe, FfmpegError> {
    if request.audio.add_track {
        return run_ffmpeg_synthesize_audio(config, input, output, request).await;
    }
    let source_streams = probe_audio_streams(config, input).await?;
    let selected = selected_source_streams(&source_streams, &request.selection.selected_streams)?;
    let bitrate_kbps_per_channel = voom_worker_protocol::audio_target_bitrate_kbps_per_channel(
        &request.audio.target_codec,
        &request.audio.profile,
    )
    .ok_or_else(|| {
        FfmpegError::OutputFactsMismatch(format!(
            "no audio bitrate defined for codec `{}` profile `{}`",
            request.audio.target_codec, request.audio.profile
        ))
    })?;
    let mut command = Command::new(&config.ffmpeg_path);
    command
        .arg("-hide_banner")
        .arg("-nostdin")
        .arg("-n")
        .arg("-i")
        .arg(input)
        .arg("-map")
        .arg("0")
        .arg("-c")
        .arg("copy");

    for source in &selected {
        command
            .arg(format!("-c:a:{}", source.audio_ordinal))
            .arg(audio_encoder(&request.audio.target_codec)?);
        let channels = source.channels.unwrap_or(2);
        command
            .arg(format!("-b:a:{}", source.audio_ordinal))
            .arg(format!(
                "{}k",
                u64::from(bitrate_kbps_per_channel) * channels
            ));
        append_audio_metadata(&mut command, source.audio_ordinal, source);
    }
    command
        .arg("-map_metadata")
        .arg("0")
        .arg("-f")
        .arg(audio_container_format(&request.output.container)?)
        .arg(output)
        .kill_on_drop(true);

    run_ffmpeg_command(config, command).await?;
    let probe = probe_audio_output(
        config,
        output,
        &request.output.container,
        &request.selection.selected_streams,
        Some(&request.audio.target_codec),
    )
    .await?;
    verify_transcode_audio_probe(
        &selected,
        &request.selection.selected_streams,
        request,
        &probe,
    )?;
    Ok(probe)
}

/// `synthesize audio` (ADR 0026, #276): keep every source stream (`-map 0 -c
/// copy`) and *append* a downmixed companion (`-ac <target_channels>`) per
/// selected source. Each companion is tagged with the request's (new) snapshot
/// stream id so the output probe and lineage can identify the derived track.
async fn run_ffmpeg_synthesize_audio(
    config: &FfmpegConfig,
    input: &Path,
    output: &Path,
    request: &TranscodeAudioRequest,
) -> Result<AudioOutputProbe, FfmpegError> {
    let target_channels = request.audio.target_channels.ok_or_else(|| {
        FfmpegError::OutputFactsMismatch("synthesize audio requires target_channels".to_owned())
    })?;
    let source_streams = probe_audio_streams(config, input).await?;
    let selected = selected_source_streams(&source_streams, &request.selection.selected_streams)?;
    let bitrate_kbps_per_channel = voom_worker_protocol::audio_target_bitrate_kbps_per_channel(
        &request.audio.target_codec,
        &request.audio.profile,
    )
    .ok_or_else(|| {
        FfmpegError::OutputFactsMismatch(format!(
            "no audio bitrate defined for codec `{}` profile `{}`",
            request.audio.target_codec, request.audio.profile
        ))
    })?;
    // Companions are appended after the copied originals, so their output audio
    // ordinal starts at the original audio-stream count.
    let original_audio_count = source_streams.len();

    let mut command = Command::new(&config.ffmpeg_path);
    command
        .arg("-hide_banner")
        .arg("-nostdin")
        .arg("-n")
        .arg("-i")
        .arg(input)
        .arg("-map")
        .arg("0")
        .arg("-map")
        .arg("-0:t?")
        .arg("-c")
        .arg("copy");
    for (offset, source) in selected.iter().enumerate() {
        let out_ordinal = original_audio_count + offset;
        command
            .arg("-map")
            .arg(format!("0:{}", source.provider_stream_index))
            .arg(format!("-c:a:{out_ordinal}"))
            .arg(audio_encoder(&request.audio.target_codec)?)
            .arg(format!("-ac:a:{out_ordinal}"))
            .arg(target_channels.to_string())
            .arg(format!("-b:a:{out_ordinal}"))
            .arg(format!(
                "{}k",
                u64::from(bitrate_kbps_per_channel) * target_channels
            ));
        append_audio_metadata(&mut command, out_ordinal, source);
    }
    command
        .arg("-map")
        .arg("0:t?")
        .arg("-map_metadata")
        .arg("0")
        .arg("-f")
        .arg(audio_container_format(&request.output.container)?)
        .arg(output)
        .kill_on_drop(true);

    run_ffmpeg_command(config, command).await?;
    let probe = probe_audio_output(
        config,
        output,
        &request.output.container,
        &request.selection.selected_streams,
        Some(&request.audio.target_codec),
    )
    .await?;
    verify_synthesize_audio_probe(&selected, request, target_channels, &probe)?;
    Ok(probe)
}

pub async fn run_ffmpeg_extract_audio(
    config: &FfmpegConfig,
    input: &Path,
    output: &Path,
    request: &ExtractAudioRequest,
) -> Result<AudioOutputProbe, FfmpegError> {
    let source_streams = probe_audio_streams(config, input).await?;
    let selected =
        selected_source_streams(&source_streams, std::slice::from_ref(&request.selection))?;
    let source = selected.first().ok_or_else(|| {
        FfmpegError::OutputFactsMismatch("selected audio stream missing".to_owned())
    })?;
    let mut command = Command::new(&config.ffmpeg_path);
    command
        .arg("-hide_banner")
        .arg("-nostdin")
        .arg("-n")
        .arg("-i")
        .arg(input)
        .arg("-map")
        .arg(format!("0:{}", source.provider_stream_index))
        .arg("-c:a")
        .arg(audio_encoder(&request.output.audio_codec)?);
    if request.output.audio_codec == "opus" && source.channels.is_some_and(|channels| channels > 2)
    {
        command.arg("-mapping_family").arg("1");
        if source.channels == Some(6) {
            command.arg("-channel_layout").arg("5.1");
        }
    }
    append_audio_metadata(&mut command, 0, source);
    command
        .arg("-f")
        .arg(audio_container_format(&request.output.container)?)
        .arg(output)
        .kill_on_drop(true);

    run_ffmpeg_command(config, command).await?;
    let probe = probe_audio_output(
        config,
        output,
        &request.output.container,
        std::slice::from_ref(&request.selection),
        Some(&request.output.audio_codec),
    )
    .await?;
    verify_extract_audio_probe(source, request, &probe)?;
    Ok(probe)
}

/// Probes the input file and returns key video stream facts needed for
/// downscale and copy-video revalidation.
pub async fn probe_input(config: &FfmpegConfig, path: &Path) -> Result<InputProbe, FfmpegError> {
    let json = probe_json(config, path).await?;
    let video_stream = json
        .get("streams")
        .and_then(Value::as_array)
        .and_then(|streams| {
            streams
                .iter()
                .find(|s| s.get("codec_type").and_then(Value::as_str) == Some("video"))
        })
        .ok_or_else(|| FfmpegError::FfprobeFailed("no video stream in input".to_owned()))?;

    let width = video_stream
        .get("width")
        .and_then(Value::as_u64)
        .and_then(|v| u32::try_from(v).ok())
        .unwrap_or(0);
    let height = video_stream
        .get("height")
        .and_then(Value::as_u64)
        .and_then(|v| u32::try_from(v).ok())
        .unwrap_or(0);
    let codec = video_stream
        .get("codec_name")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    let pixel_format = video_stream
        .get("pix_fmt")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    let codec_profile = video_stream
        .get("profile")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let codec_level = video_stream
        .get("level")
        .and_then(Value::as_u64)
        .map(|level_int| {
            // ffprobe reports level as integer * 10 (e.g., 40 = 4.0)
            format!("{}.{}", level_int / 10, level_int % 10)
        });

    let video_stream_count = json
        .get("streams")
        .and_then(Value::as_array)
        .map(|streams| {
            streams
                .iter()
                .filter(|s| s.get("codec_type").and_then(Value::as_str) == Some("video"))
                .count()
        })
        .and_then(|n| u32::try_from(n).ok())
        .unwrap_or(0);
    let forced_subtitle_ordinals = json
        .get("streams")
        .and_then(Value::as_array)
        .map(|streams| {
            streams
                .iter()
                .filter(|stream| {
                    stream.get("codec_type").and_then(Value::as_str) == Some("subtitle")
                })
                .enumerate()
                .filter_map(|(ordinal, stream)| {
                    (disposition_bool(stream, "forced") == Some(true)).then_some(ordinal)
                })
                .collect()
        })
        .unwrap_or_default();

    Ok(InputProbe {
        width,
        height,
        codec,
        pixel_format,
        codec_profile,
        codec_level,
        video_stream_count,
        forced_subtitle_ordinals,
    })
}

/// The pixel format `ffprobe` must report for an output conforming to `profile`.
///
/// Delegates to the encoder descriptor, which is where the measured surface-to-file
/// pairings live. The mapping is deliberately not duplicated here: the control plane
/// verifies the same fact about the same result, and two tables would let the worker and
/// the control plane disagree about whether an encode conformed.
fn expected_output_pixel_format(
    profile: &TranscodeVideoProfile,
) -> Result<Option<&str>, FfmpegError> {
    voom_core::expected_output_pixel_format(profile).map_err(FfmpegError::OutputFactsMismatch)
}

async fn probe_output(
    config: &FfmpegConfig,
    path: &Path,
    expected_container: &str,
    expected_codec: &str,
    profile: &TranscodeVideoProfile,
) -> Result<OutputProbe, FfmpegError> {
    let json = probe_json(config, path).await?;
    let container = json
        .pointer("/format/format_name")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let video_stream = json
        .get("streams")
        .and_then(Value::as_array)
        .and_then(|streams| {
            streams
                .iter()
                .find(|s| s.get("codec_type").and_then(Value::as_str) == Some("video"))
        });

    let actual_codec = video_stream
        .and_then(|s| s.get("codec_name"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let width = video_stream
        .and_then(|s| s.get("width"))
        .and_then(Value::as_u64)
        .and_then(|v| u32::try_from(v).ok())
        .unwrap_or(0);
    let height = video_stream
        .and_then(|s| s.get("height"))
        .and_then(Value::as_u64)
        .and_then(|v| u32::try_from(v).ok())
        .unwrap_or(0);
    let pixel_format = video_stream
        .and_then(|s| s.get("pix_fmt"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();

    let probe_container = match expected_container {
        "mkv" => "matroska",
        "mp4" => "mp4",
        other => other,
    };
    if !container.split(',').any(|name| name == probe_container) {
        return Err(FfmpegError::OutputFactsMismatch(format!(
            "expected {expected_container} output, got container={container}"
        )));
    }
    // Map ffprobe codec names to canonical forms for comparison
    let canonical_actual = canonical_output_codec(actual_codec);
    if canonical_actual != expected_codec {
        return Err(FfmpegError::OutputFactsMismatch(format!(
            "expected {expected_codec} codec, got {actual_codec}"
        )));
    }
    // Validate dimension caps when set
    if let Some(max_w) = profile.max_width
        && width > max_w
    {
        return Err(FfmpegError::OutputFactsMismatch(format!(
            "output width {width} exceeds cap {max_w}"
        )));
    }
    if let Some(max_h) = profile.max_height
        && height > max_h
    {
        return Err(FfmpegError::OutputFactsMismatch(format!(
            "output height {height} exceeds cap {max_h}"
        )));
    }
    // Validate pixel format when constrained. An unknown (empty) output
    // pixel_format under a constraint is non-conforming — fail fast, matching
    // validate_copy_video_preconditions.
    if let Some(expected_pf) = expected_output_pixel_format(profile)? {
        if pixel_format.is_empty() {
            return Err(FfmpegError::OutputFactsMismatch(format!(
                "expected pixel_format {expected_pf}, but output pixel_format is unknown"
            )));
        }
        if pixel_format != expected_pf {
            return Err(FfmpegError::OutputFactsMismatch(format!(
                "expected pixel_format {expected_pf}, got {pixel_format}"
            )));
        }
    }

    Ok(OutputProbe {
        container: expected_container.to_owned(),
        video_codec: expected_codec.to_owned(),
        width,
        height,
        pixel_format,
    })
}

/// Maps ffprobe codec names to canonical voom-worker-protocol forms.
fn canonical_output_codec(codec: &str) -> &str {
    match codec {
        "hevc" | "h265" => "hevc",
        "av1" => "av1",
        other => other,
    }
}

fn is_text_file_busy(err: &std::io::Error) -> bool {
    // ETXTBSY: attempted to exec a file currently open for writing. Transient in
    // a multithreaded process when a sibling thread's fork briefly inherited a
    // writable fd to a freshly written executable; it clears once that child
    // exec()s. 26 on Linux and macOS.
    const ETXTBSY: i32 = 26;
    err.raw_os_error() == Some(ETXTBSY)
}

/// Run `command` to completion, retrying a bounded number of times on ETXTBSY
/// with a short backoff. See [`is_text_file_busy`] for why the condition is
/// transient. A real ffmpeg binary is never rewritten, so this only fires in
/// tests that stub the executable.
async fn output_retrying_etxtbsy(command: &mut Command) -> std::io::Result<std::process::Output> {
    const MAX_ATTEMPTS: u32 = 5;
    let mut attempt: u32 = 0;
    loop {
        attempt += 1;
        match command.output().await {
            Err(err) if is_text_file_busy(&err) && attempt < MAX_ATTEMPTS => {
                tokio::time::sleep(Duration::from_millis(10 * u64::from(attempt))).await;
            }
            other => return other,
        }
    }
}

async fn run_ffmpeg_command(
    config: &FfmpegConfig,
    mut command: Command,
) -> Result<(), FfmpegError> {
    let process_output = timeout(
        config.process_timeout,
        output_retrying_etxtbsy(&mut command),
    )
    .await
    .map_err(|_| FfmpegError::FfmpegFailed("ffmpeg timed out".to_owned()))?
    .map_err(|err| FfmpegError::FfmpegFailed(err.to_string()))?;
    if !process_output.status.success() {
        return Err(classify_process_failure(
            &process_output,
            FfmpegError::FfmpegFailed,
        ));
    }
    Ok(())
}

async fn probe_json(config: &FfmpegConfig, path: &Path) -> Result<Value, FfmpegError> {
    let mut command = Command::new(&config.ffprobe_path);
    command
        .arg("-v")
        .arg("error")
        .arg("-print_format")
        .arg("json")
        .arg("-show_format")
        .arg("-show_streams")
        .arg(path)
        .kill_on_drop(true);
    let output = timeout(
        config.process_timeout,
        output_retrying_etxtbsy(&mut command),
    )
    .await
    .map_err(|_| FfmpegError::FfprobeFailed("ffprobe timed out".to_owned()))?
    .map_err(|err| FfmpegError::FfprobeFailed(err.to_string()))?;
    if !output.status.success() {
        return Err(classify_process_failure(
            &output,
            FfmpegError::FfprobeFailed,
        ));
    }
    serde_json::from_slice(&output.stdout)
        .map_err(|err| FfmpegError::FfprobeFailed(format!("invalid ffprobe JSON: {err}")))
}

async fn probe_audio_streams(
    config: &FfmpegConfig,
    path: &Path,
) -> Result<Vec<SourceAudioFact>, FfmpegError> {
    let json = probe_json(config, path).await?;
    Ok(audio_stream_values(&json)
        .enumerate()
        .filter_map(|(audio_ordinal, stream)| {
            Some(SourceAudioFact {
                snapshot_stream_id: None,
                provider_stream_index: u32::try_from(stream.get("index")?.as_u64()?).ok()?,
                audio_ordinal,
                codec: stream
                    .get("codec_name")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                language: stream_tag(stream, "language"),
                title: stream_tag(stream, "title"),
                default: disposition_bool(stream, "default"),
                disposition: Some(AudioDispositionFact {
                    default: disposition_bool(stream, "default"),
                    forced: disposition_bool(stream, "forced"),
                    commentary: disposition_bool(stream, "comment"),
                }),
                channels: stream.get("channels").and_then(Value::as_u64),
            })
        })
        .collect())
}

async fn probe_audio_output(
    config: &FfmpegConfig,
    path: &Path,
    expected_container: &str,
    selected_refs: &[AudioStreamRef],
    expected_codec: Option<&str>,
) -> Result<AudioOutputProbe, FfmpegError> {
    let json = probe_json(config, path).await?;
    let container = json
        .pointer("/format/format_name")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if !container
        .split(',')
        .any(|name| name == audio_probe_container(expected_container))
    {
        return Err(FfmpegError::OutputFactsMismatch(format!(
            "expected {expected_container} output, got {container}"
        )));
    }
    let audio_streams: Vec<&Value> = audio_stream_values(&json).collect();
    let selected_output_streams =
        selected_output_streams(&audio_streams, selected_refs, expected_codec);
    let audio_codecs = selected_output_streams
        .iter()
        .map(|stream| stream.codec.clone())
        .collect();
    let first_selected = selected_output_streams.first();
    Ok(AudioOutputProbe {
        container: expected_container.to_owned(),
        audio_codecs,
        output_language: first_selected.and_then(|stream| stream.language.clone()),
        output_title: first_selected.and_then(|stream| stream.title.clone()),
        selected_output_streams,
    })
}

fn selected_source_streams(
    source_streams: &[SourceAudioFact],
    selected_refs: &[AudioStreamRef],
) -> Result<Vec<SourceAudioFact>, FfmpegError> {
    selected_refs
        .iter()
        .map(|selected| {
            let mut source = source_streams
                .iter()
                .find(|stream| stream.provider_stream_index == selected.provider_stream_index)
                .cloned()
                .ok_or_else(|| {
                    FfmpegError::OutputFactsMismatch(format!(
                        "selected audio stream {} was not present in input probe",
                        selected.provider_stream_index
                    ))
                })?;
            source.snapshot_stream_id = Some(selected.snapshot_stream_id.clone());
            Ok(source)
        })
        .collect()
}

fn selected_output_streams(
    audio_streams: &[&Value],
    selected_refs: &[AudioStreamRef],
    expected_codec: Option<&str>,
) -> Vec<AudioOutputStreamFact> {
    let has_snapshot_tags = audio_streams
        .iter()
        .any(|stream| stream_tag(stream, "snapshot_stream_id").is_some());
    if has_snapshot_tags {
        return audio_streams
            .iter()
            .filter_map(|stream| {
                let snapshot_stream_id = stream_tag(stream, "snapshot_stream_id")?;
                if !selected_refs
                    .iter()
                    .any(|selected| selected.snapshot_stream_id == snapshot_stream_id)
                {
                    return None;
                }
                audio_output_stream_fact(stream, snapshot_stream_id, expected_codec)
            })
            .collect();
    }
    selected_refs
        .iter()
        .filter_map(|selected| {
            let stream = audio_streams.iter().find(|stream| {
                stream
                    .get("index")
                    .and_then(Value::as_u64)
                    .and_then(|index| u32::try_from(index).ok())
                    == Some(selected.provider_stream_index)
            })?;
            audio_output_stream_fact(stream, selected.snapshot_stream_id.clone(), expected_codec)
        })
        .collect()
}

fn audio_output_stream_fact(
    stream: &Value,
    snapshot_stream_id: String,
    expected_codec: Option<&str>,
) -> Option<AudioOutputStreamFact> {
    let codec = stream
        .get("codec_name")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    if expected_codec.is_some_and(|expected| codec != expected) {
        return None;
    }
    Some(AudioOutputStreamFact {
        snapshot_stream_id,
        output_provider_stream_index: stream
            .get("index")
            .and_then(Value::as_u64)
            .and_then(|index| u32::try_from(index).ok())
            .unwrap_or_default(),
        codec,
        language: stream_tag(stream, "language"),
        title: stream_tag(stream, "title"),
        default: disposition_bool(stream, "default"),
        disposition: Some(AudioDispositionFact {
            default: disposition_bool(stream, "default"),
            forced: disposition_bool(stream, "forced"),
            commentary: disposition_bool(stream, "comment"),
        }),
        channels: stream.get("channels").and_then(Value::as_u64),
    })
}

fn audio_stream_values(json: &Value) -> impl Iterator<Item = &Value> {
    json.get("streams")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|stream| stream.get("codec_type").and_then(Value::as_str) == Some("audio"))
}

fn stream_tag(stream: &Value, tag: &str) -> Option<String> {
    stream
        .get("tags")
        .and_then(Value::as_object)
        .and_then(|tags| {
            tags.iter()
                .find(|(key, _)| key.eq_ignore_ascii_case(tag))
                .map(|(_, value)| value)
        })
        .and_then(Value::as_str)
        .map(str::to_owned)
}

fn disposition_bool(stream: &Value, key: &str) -> Option<bool> {
    stream
        .get("disposition")
        .and_then(|disposition| disposition.get(key))
        .and_then(Value::as_i64)
        .map(|value| value != 0)
}

fn append_audio_metadata(
    command: &mut Command,
    output_audio_ordinal: usize,
    source: &SourceAudioFact,
) {
    if let Some(language) = &source.language {
        command
            .arg(format!("-metadata:s:a:{output_audio_ordinal}"))
            .arg(format!("language={language}"));
    }
    if let Some(title) = &source.title {
        command
            .arg(format!("-metadata:s:a:{output_audio_ordinal}"))
            .arg(format!("title={title}"));
    }
    if let Some(snapshot_stream_id) = &source.snapshot_stream_id {
        command
            .arg(format!("-metadata:s:a:{output_audio_ordinal}"))
            .arg(format!("snapshot_stream_id={snapshot_stream_id}"));
    }
    if let Some(disposition) = audio_disposition_arg(source) {
        command
            .arg(format!("-disposition:a:{output_audio_ordinal}"))
            .arg(disposition);
    }
}

fn audio_disposition_arg(source: &SourceAudioFact) -> Option<String> {
    let disposition = source.disposition.as_ref()?;
    let mut flags = Vec::new();
    if disposition.default == Some(true) || source.default == Some(true) {
        flags.push("default");
    }
    if disposition.forced == Some(true) {
        flags.push("forced");
    }
    if disposition.commentary == Some(true) {
        flags.push("comment");
    }
    Some(if flags.is_empty() {
        "0".to_owned()
    } else {
        flags.join("+")
    })
}

fn audio_encoder(codec: &str) -> Result<&'static str, FfmpegError> {
    match codec {
        "aac" => Ok("aac"),
        "opus" => Ok("libopus"),
        "eac3" => Ok("eac3"),
        other => Err(FfmpegError::OutputFactsMismatch(format!(
            "unsupported audio codec: {other}"
        ))),
    }
}

fn audio_container_format(container: &str) -> Result<&'static str, FfmpegError> {
    match container {
        "mkv" => Ok("matroska"),
        "ogg" => Ok("ogg"),
        other => Err(FfmpegError::OutputFactsMismatch(format!(
            "unsupported audio container: {other}"
        ))),
    }
}

fn audio_probe_container(container: &str) -> &str {
    match container {
        "mkv" => "matroska",
        other => other,
    }
}

fn verify_transcode_audio_probe(
    selected_sources: &[SourceAudioFact],
    selected_refs: &[AudioStreamRef],
    request: &TranscodeAudioRequest,
    probe: &AudioOutputProbe,
) -> Result<(), FfmpegError> {
    if probe.selected_output_streams.len() != selected_refs.len() {
        return Err(FfmpegError::OutputFactsMismatch(
            "selected output stream count mismatch".to_owned(),
        ));
    }
    let observed_ids: Vec<&str> = probe
        .selected_output_streams
        .iter()
        .map(|stream| stream.snapshot_stream_id.as_str())
        .collect();
    let expected_ids: Vec<&str> = selected_refs
        .iter()
        .map(|stream| stream.snapshot_stream_id.as_str())
        .collect();
    if observed_ids != expected_ids {
        return Err(FfmpegError::OutputFactsMismatch(
            "selected output stream order mismatch".to_owned(),
        ));
    }
    for ((source, expected), output) in selected_sources
        .iter()
        .zip(selected_refs)
        .zip(&probe.selected_output_streams)
    {
        if output.snapshot_stream_id != expected.snapshot_stream_id {
            return Err(FfmpegError::OutputFactsMismatch(
                "selected snapshot stream id mismatch".to_owned(),
            ));
        }
        if output.codec != request.audio.target_codec {
            return Err(FfmpegError::OutputFactsMismatch(
                "selected audio codec mismatch".to_owned(),
            ));
        }
        verify_preserved_audio_metadata(source, output)?;
    }
    Ok(())
}

/// Verify a `synthesize audio` output (ADR 0026). Unlike the transcode verifier,
/// the companion's channel count is expected to *differ* from the source (it is
/// a downmix): each synthesized stream must carry the requested snapshot id and
/// target codec, exactly `target_channels` channels, and preserve the source
/// language.
fn verify_synthesize_audio_probe(
    selected_sources: &[SourceAudioFact],
    request: &TranscodeAudioRequest,
    target_channels: u64,
    probe: &AudioOutputProbe,
) -> Result<(), FfmpegError> {
    let selected_refs = &request.selection.selected_streams;
    if probe.selected_output_streams.len() != selected_refs.len() {
        return Err(FfmpegError::OutputFactsMismatch(
            "synthesized output stream count mismatch".to_owned(),
        ));
    }
    let observed_ids: Vec<&str> = probe
        .selected_output_streams
        .iter()
        .map(|stream| stream.snapshot_stream_id.as_str())
        .collect();
    let expected_ids: Vec<&str> = selected_refs
        .iter()
        .map(|stream| stream.snapshot_stream_id.as_str())
        .collect();
    if observed_ids != expected_ids {
        return Err(FfmpegError::OutputFactsMismatch(
            "synthesized output stream order mismatch".to_owned(),
        ));
    }
    for (source, output) in selected_sources.iter().zip(&probe.selected_output_streams) {
        if output.codec != request.audio.target_codec {
            return Err(FfmpegError::OutputFactsMismatch(
                "synthesized audio codec mismatch".to_owned(),
            ));
        }
        if output.channels != Some(target_channels) {
            return Err(FfmpegError::OutputFactsMismatch(
                "synthesized audio channel count is not the requested downmix".to_owned(),
            ));
        }
        if source.language != output.language {
            return Err(FfmpegError::OutputFactsMismatch(
                "synthesized audio language was not preserved".to_owned(),
            ));
        }
    }
    Ok(())
}

fn verify_extract_audio_probe(
    source: &SourceAudioFact,
    request: &ExtractAudioRequest,
    probe: &AudioOutputProbe,
) -> Result<(), FfmpegError> {
    if probe.selected_output_streams.len() != 1 {
        return Err(FfmpegError::OutputFactsMismatch(
            "extract_audio selected output count mismatch".to_owned(),
        ));
    }
    let output = &probe.selected_output_streams[0];
    if output.snapshot_stream_id != request.selection.snapshot_stream_id {
        return Err(FfmpegError::OutputFactsMismatch(
            "extract_audio selected snapshot stream id mismatch".to_owned(),
        ));
    }
    if probe.container != "ogg" || output.codec != "opus" {
        return Err(FfmpegError::OutputFactsMismatch(
            "extract_audio expected opus in ogg".to_owned(),
        ));
    }
    if source.language.is_some() && source.language != output.language {
        return Err(FfmpegError::OutputFactsMismatch(
            "extract_audio language was not preserved".to_owned(),
        ));
    }
    if source.title.is_some() && source.title != output.title {
        return Err(FfmpegError::OutputFactsMismatch(
            "extract_audio title was not preserved".to_owned(),
        ));
    }
    Ok(())
}

fn verify_preserved_audio_metadata(
    source: &SourceAudioFact,
    output: &AudioOutputStreamFact,
) -> Result<(), FfmpegError> {
    if source.language != output.language {
        return Err(FfmpegError::OutputFactsMismatch(
            "selected audio language mismatch".to_owned(),
        ));
    }
    if source.title != output.title {
        return Err(FfmpegError::OutputFactsMismatch(
            "selected audio title mismatch".to_owned(),
        ));
    }
    if source.default != output.default {
        return Err(FfmpegError::OutputFactsMismatch(
            "selected audio default disposition mismatch".to_owned(),
        ));
    }
    if source.disposition != output.disposition {
        return Err(FfmpegError::OutputFactsMismatch(
            "selected audio disposition mismatch".to_owned(),
        ));
    }
    if source.channels.is_some() && source.channels != output.channels {
        return Err(FfmpegError::OutputFactsMismatch(
            "selected audio channel count mismatch".to_owned(),
        ));
    }
    Ok(())
}

fn command_error(output: &std::process::Output) -> String {
    format!(
        "status {}: {}{}",
        output
            .status
            .code()
            .map_or_else(|| "signal".to_owned(), |code| code.to_string()),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

#[cfg(test)]
#[path = "ffmpeg_test.rs"]
mod tests;
