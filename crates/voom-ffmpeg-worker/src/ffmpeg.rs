use std::ffi::OsString;
use std::path::{Path, PathBuf};

use serde_json::Value;
use thiserror::Error;
use tokio::process::Command;
use tokio::time::{Duration, timeout};
use voom_worker_protocol::{
    AudioDispositionFact, AudioOutputStreamFact, AudioStreamRef, ExtractAudioRequest,
    TranscodeAudioRequest, TranscodeVideoProfile, TranscodeVideoRequest,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FfmpegConfig {
    pub ffmpeg_path: PathBuf,
    pub ffprobe_path: PathBuf,
    pub provider_version: String,
    pub process_timeout: Duration,
}

impl FfmpegConfig {
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
        }
    }
}

#[derive(Debug, Error)]
pub enum FfmpegError {
    #[error("ffmpeg failed: {0}")]
    FfmpegFailed(String),
    #[error("ffprobe failed: {0}")]
    FfprobeFailed(String),
    #[error("output facts mismatch: {0}")]
    OutputFactsMismatch(String),
    #[error("unsupported input: {0}")]
    UnsupportedInput(String),
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
        "libx265" => Ok(video_codec_args_x265(profile)),
        "libsvtav1" => Ok(video_codec_args_svtav1(profile)),
        "libaom-av1" => Ok(video_codec_args_libaom(profile)),
        other => Err(FfmpegError::OutputFactsMismatch(format!(
            "unknown video encoder `{other}`"
        ))),
    }
}

fn video_codec_args_x265(profile: &TranscodeVideoProfile) -> Vec<OsString> {
    let mut args = vec![
        OsString::from("-c:v"),
        OsString::from("libx265"),
        OsString::from("-crf"),
        OsString::from(profile.crf.to_string()),
        OsString::from("-preset"),
        OsString::from(&profile.preset),
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
    args
}

fn video_codec_args_svtav1(profile: &TranscodeVideoProfile) -> Vec<OsString> {
    let mut args = vec![
        OsString::from("-c:v"),
        OsString::from("libsvtav1"),
        OsString::from("-crf"),
        OsString::from(profile.crf.to_string()),
        OsString::from("-preset"),
        OsString::from(&profile.preset),
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
    args
}

fn video_codec_args_libaom(profile: &TranscodeVideoProfile) -> Vec<OsString> {
    let mut args = vec![
        OsString::from("-c:v"),
        OsString::from("libaom-av1"),
        OsString::from("-crf"),
        OsString::from(profile.crf.to_string()),
        OsString::from("-b:v"),
        OsString::from("0"),
        OsString::from("-cpu-used"),
        OsString::from(&profile.preset),
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
    args
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
    if profile.max_width.is_none() && profile.max_height.is_none() {
        return Vec::new();
    }
    let cap_w = profile.max_width.unwrap_or(u32::MAX);
    let cap_h = profile.max_height.unwrap_or(u32::MAX);
    if src_w <= cap_w && src_h <= cap_h {
        return Vec::new();
    }
    // Downscale-only, preserve aspect, force even dims.
    // See also: voom-plan/src/planner.rs for the dimension-cap logic.
    let vf = format!(
        "scale='min({cap_w},iw)':'min({cap_h},ih)':force_original_aspect_ratio=decrease,\
         scale=trunc(iw/2)*2:trunc(ih/2)*2"
    );
    vec![OsString::from("-vf"), OsString::from(vf)]
}

pub async fn run_ffmpeg_transcode(
    config: &FfmpegConfig,
    request: &TranscodeVideoRequest,
    src_width: u32,
    src_height: u32,
) -> Result<OutputProbe, FfmpegError> {
    let input = Path::new(&request.input.path);
    let output = Path::new(&request.output.path);
    let profile = &request.profile;
    let container = &request.output.container;
    let codec = &request.output.video_codec;

    let mut command = Command::new(&config.ffmpeg_path);
    command
        .arg("-hide_banner")
        .arg("-nostdin")
        .arg("-n")
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
    for arg in scale_args(profile, src_width, src_height) {
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
    for arg in container_args(container, codec)? {
        command.arg(arg);
    }
    command
        .arg("-progress")
        .arg("pipe:2")
        .arg(output)
        .kill_on_drop(true);

    let process_output = timeout(config.process_timeout, command.output())
        .await
        .map_err(|_| FfmpegError::FfmpegFailed("ffmpeg timed out".to_owned()))?
        .map_err(|err| FfmpegError::FfmpegFailed(err.to_string()))?;
    if !process_output.status.success() {
        return Err(FfmpegError::FfmpegFailed(command_error(&process_output)));
    }

    probe_output(config, output, container, codec, profile).await
}

pub async fn run_ffmpeg_transcode_audio(
    config: &FfmpegConfig,
    input: &Path,
    output: &Path,
    request: &TranscodeAudioRequest,
) -> Result<AudioOutputProbe, FfmpegError> {
    let source_streams = probe_audio_streams(config, input).await?;
    let selected = selected_source_streams(&source_streams, &request.selection.selected_streams)?;
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
        append_audio_metadata(&mut command, source.audio_ordinal, source);
    }
    command
        .arg("-map_metadata")
        .arg("0")
        .arg("-f")
        .arg(audio_container_format(&request.output.container)?)
        .arg("-progress")
        .arg("pipe:2")
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
    append_audio_metadata(&mut command, 0, source);
    command
        .arg("-f")
        .arg(audio_container_format(&request.output.container)?)
        .arg("-progress")
        .arg("pipe:2")
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

    Ok(InputProbe {
        width,
        height,
        codec,
        pixel_format,
        codec_profile,
        codec_level,
        video_stream_count,
    })
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
    if let Some(expected_pf) = &profile.pixel_format {
        if pixel_format.is_empty() {
            return Err(FfmpegError::OutputFactsMismatch(format!(
                "expected pixel_format {expected_pf}, but output pixel_format is unknown"
            )));
        }
        if &pixel_format != expected_pf {
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

async fn run_ffmpeg_command(
    config: &FfmpegConfig,
    mut command: Command,
) -> Result<(), FfmpegError> {
    let process_output = timeout(config.process_timeout, command.output())
        .await
        .map_err(|_| FfmpegError::FfmpegFailed("ffmpeg timed out".to_owned()))?
        .map_err(|err| FfmpegError::FfmpegFailed(err.to_string()))?;
    if !process_output.status.success() {
        return Err(FfmpegError::FfmpegFailed(command_error(&process_output)));
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
    let output = timeout(config.process_timeout, command.output())
        .await
        .map_err(|_| FfmpegError::FfprobeFailed("ffprobe timed out".to_owned()))?
        .map_err(|err| FfmpegError::FfprobeFailed(err.to_string()))?;
    if !output.status.success() {
        return Err(FfmpegError::FfprobeFailed(command_error(&output)));
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
        .and_then(|tags| tags.get(tag))
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
