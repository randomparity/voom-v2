use std::fmt::{Display, Formatter};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde::{Serialize, de::DeserializeOwned};
use time::OffsetDateTime;
use voom_core::{
    ErrorCode, FailureClass, LeaseId, expected_output_pixel_format, nvidia_decoder_for_video_codec,
};
use voom_worker_protocol::{
    AudioExpectedFacts, AudioObservedFacts, ExtractAudioOutputDescriptor, ExtractAudioOutputResult,
    ExtractAudioRequest, ExtractAudioResult, ExtractAudioStatus, OperationDispatch,
    OperationFuture, OperationHandler, OperationKind, OperationRequest, OperationResponse,
    ProgressFrame, ProtocolError, TranscodeAudioRequest, TranscodeAudioResult,
    TranscodeAudioStatus, TranscodeVideoExpectedFacts, TranscodeVideoObservedFacts,
    TranscodeVideoProfile, TranscodeVideoRequest, TranscodeVideoResult, TranscodeVideoStatus,
    VideoHardwareAssignment, vaapi_hardware_token, validate_extract_audio_request,
    validate_extract_audio_result,
};

use crate::ffmpeg::{
    FfmpegConfig, FfmpegError, InputProbe, VaapiDeviceBinding, VideoTranscodeInput, probe_input,
    run_ffmpeg_extract_audio, run_ffmpeg_transcode, run_ffmpeg_transcode_audio,
};
use crate::observe::{ObserveError, observe_file_facts};

const PROVIDER: &str = "ffmpeg";
const MAX_PROGRESS_INTERVAL: Duration = Duration::from_secs(5);

#[derive(Clone, Copy)]
struct StreamingOperation {
    lease_id: LeaseId,
    accepted_at: OffsetDateTime,
    progress_idle_deadline_ms: u32,
    started_message: &'static str,
    active_message: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TranscodeVideoError {
    ConfigInvalid {
        message: String,
        payload: serde_json::Value,
    },
    ArtifactUnavailable {
        message: String,
        payload: serde_json::Value,
    },
    ArtifactChecksumMismatch {
        message: String,
        payload: serde_json::Value,
    },
    ExternalSystemUnavailable {
        message: String,
        payload: serde_json::Value,
    },
    MalformedMedia {
        message: String,
        payload: serde_json::Value,
    },
    MalformedWorkerResult {
        message: String,
        payload: serde_json::Value,
    },
}

pub type TranscodeAudioError = TranscodeVideoError;
pub type ExtractAudioError = TranscodeVideoError;

impl TranscodeVideoError {
    #[must_use]
    pub const fn failure_class(&self) -> FailureClass {
        match self {
            Self::ConfigInvalid { .. } | Self::MalformedWorkerResult { .. } => {
                FailureClass::MalformedWorkerResult
            }
            Self::ArtifactUnavailable { .. } => FailureClass::ArtifactUnavailable,
            Self::ArtifactChecksumMismatch { .. } => FailureClass::ArtifactChecksumMismatch,
            Self::ExternalSystemUnavailable { .. } => FailureClass::ExternalSystemUnavailable,
            Self::MalformedMedia { .. } => FailureClass::MalformedMedia,
        }
    }

    #[must_use]
    pub const fn error_code(&self) -> ErrorCode {
        match self {
            Self::ConfigInvalid { .. } => ErrorCode::ConfigInvalid,
            _ => self.failure_class().into_error_code(),
        }
    }

    #[must_use]
    pub fn payload(&self) -> serde_json::Value {
        match self {
            Self::ConfigInvalid { payload, .. }
            | Self::ArtifactUnavailable { payload, .. }
            | Self::ArtifactChecksumMismatch { payload, .. }
            | Self::ExternalSystemUnavailable { payload, .. }
            | Self::MalformedMedia { payload, .. }
            | Self::MalformedWorkerResult { payload, .. } => payload.clone(),
        }
    }
}

impl Display for TranscodeVideoError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ConfigInvalid { message, .. } => write!(f, "config invalid: {message}"),
            Self::ArtifactUnavailable { message, .. } => {
                write!(f, "artifact unavailable: {message}")
            }
            Self::ArtifactChecksumMismatch { message, .. } => {
                write!(f, "artifact checksum mismatch: {message}")
            }
            Self::ExternalSystemUnavailable { message, .. } => {
                write!(f, "external system unavailable: {message}")
            }
            Self::MalformedMedia { message, .. } => {
                write!(f, "malformed media: {message}")
            }
            Self::MalformedWorkerResult { message, .. } => {
                write!(f, "malformed worker result: {message}")
            }
        }
    }
}

impl std::error::Error for TranscodeVideoError {}

#[must_use]
pub fn handle_operation(req: OperationRequest) -> OperationFuture {
    handle_operation_with_config(req, None)
}

#[must_use]
pub fn operation_handler(config: FfmpegConfig) -> OperationHandler {
    Arc::new(move |req| handle_operation_with_config(req, Some(config.clone())))
}

fn handle_operation_with_config(
    req: OperationRequest,
    config: Option<FfmpegConfig>,
) -> OperationFuture {
    Box::pin(async move {
        let lease_id = req.lease_id;
        let accepted_at = OffsetDateTime::now_utc();
        let progress_idle_deadline_ms = req.progress_idle_deadline_ms;
        let operation = req.operation;
        if !matches!(
            operation,
            OperationKind::TranscodeVideo
                | OperationKind::TranscodeAudio
                | OperationKind::ExtractAudio
        ) {
            return Err(ProtocolError::UnknownOperation {
                name: format!("{operation:?}"),
            });
        }
        let Some(config) = config else {
            return error_dispatch(
                lease_id,
                accepted_at,
                &config_invalid("preflight", "ffmpeg config was not provided".to_owned()),
                0,
            );
        };

        match operation {
            OperationKind::TranscodeVideo => {
                let payload = match decode_payload::<TranscodeVideoRequest>(
                    req.payload,
                    lease_id,
                    accepted_at,
                    "transcode_video",
                )? {
                    Ok(payload) => payload,
                    Err(dispatch) => return Ok(dispatch),
                };
                stream_operation(
                    StreamingOperation {
                        lease_id,
                        accepted_at,
                        progress_idle_deadline_ms,
                        started_message: "video transcode started",
                        active_message: "video transcode in progress",
                    },
                    async move { handle_transcode_video(&payload, &config).await },
                )
            }
            OperationKind::TranscodeAudio => {
                let payload = match decode_payload::<TranscodeAudioRequest>(
                    req.payload,
                    lease_id,
                    accepted_at,
                    "transcode_audio",
                )? {
                    Ok(payload) => payload,
                    Err(dispatch) => return Ok(dispatch),
                };
                stream_operation(
                    StreamingOperation {
                        lease_id,
                        accepted_at,
                        progress_idle_deadline_ms,
                        started_message: "audio transcode started",
                        active_message: "audio transcode in progress",
                    },
                    async move { handle_transcode_audio(&payload, &config).await },
                )
            }
            OperationKind::ExtractAudio => {
                let payload = match decode_payload::<ExtractAudioRequest>(
                    req.payload,
                    lease_id,
                    accepted_at,
                    "extract_audio",
                )? {
                    Ok(payload) => payload,
                    Err(dispatch) => return Ok(dispatch),
                };
                stream_operation(
                    StreamingOperation {
                        lease_id,
                        accepted_at,
                        progress_idle_deadline_ms,
                        started_message: "audio extraction started",
                        active_message: "audio extraction in progress",
                    },
                    async move { handle_extract_audio(&payload, &config).await },
                )
            }
            _ => unreachable!("unsupported operation returned before config validation"),
        }
    })
}

pub async fn handle_transcode_video(
    request: &TranscodeVideoRequest,
    config: &FfmpegConfig,
) -> Result<TranscodeVideoResult, TranscodeVideoError> {
    if request.output.overwrite {
        return Err(config_invalid(
            "request",
            "overwrite must be false".to_owned(),
        ));
    }
    validate_request_contract(request)?;
    validate_encoder_available(request, config)?;
    let input_path = PathBuf::from(&request.input.path);
    let output_path = PathBuf::from(&request.output.path);
    let input_pre = prepare_video_operation(
        &input_path,
        &output_path,
        Path::new(&request.output.staging_root),
        &request.input.expected,
    )
    .await?;

    // Probe input to learn source dimensions and, for copy_video, to
    // revalidate the source satisfies the profile's constraints.
    let input_probe = probe_input(config, &input_path)
        .await
        .map_err(TranscodeVideoError::from)?;
    if let Some(expected_codec) = &request.input.video_codec
        && !codec_tokens_match(&input_probe.codec, expected_codec)
    {
        return Err(malformed_worker_result(
            "input.video_codec",
            format!(
                "source codec `{}` does not match expected `{expected_codec}`",
                input_probe.codec
            ),
        ));
    }
    if let Some(expected_pixel_format) = &request.input.video_pixel_format
        && input_probe.pixel_format != *expected_pixel_format
    {
        return Err(malformed_worker_result(
            "input.video_pixel_format",
            format!(
                "source pixel format `{}` does not match expected `{expected_pixel_format}`",
                input_probe.pixel_format
            ),
        ));
    }
    if input_probe.video_stream_count > 1 {
        // The transcode maps only 0:v:0; a source with multiple video streams
        // would silently drop the rest. Fail loud rather than lose data.
        return Err(TranscodeVideoError::from(FfmpegError::UnsupportedInput(
            format!(
                "source has {} video streams; transcode_video supports exactly one",
                input_probe.video_stream_count
            ),
        )));
    }

    if request.copy_video {
        validate_copy_video_preconditions(request, &input_probe)?;
    }
    validate_video_hardware_binding(request, config, &input_probe)?;

    let probe = run_ffmpeg_transcode(
        config,
        request,
        VideoTranscodeInput {
            width: input_probe.width,
            height: input_probe.height,
            codec: &input_probe.codec,
            forced_subtitle_ordinals: &input_probe.forced_subtitle_ordinals,
        },
    )
    .await
    .map_err(TranscodeVideoError::from)?;
    let (input_post, output) =
        finalize_video_operation(&input_path, &output_path, &input_pre).await?;

    Ok(TranscodeVideoResult {
        status: TranscodeVideoStatus::Transcoded,
        provider: PROVIDER.to_owned(),
        provider_version: config.provider_version.clone(),
        input_pre,
        input_post,
        output,
        output_container: probe.container,
        output_video_codec: probe.video_codec,
        output_width: probe.width,
        output_height: probe.height,
        output_pixel_format: probe.pixel_format,
        hardware_assignment: request.hardware_assignment.clone(),
        copied_video: request.copy_video,
    })
}

/// Shared pre-ffmpeg flow for video transcode: validate the output path
/// against the staging root, require the output to not yet exist, observe the
/// input file, and verify it matches the request's expected facts.
async fn prepare_video_operation(
    input_path: &Path,
    output_path: &Path,
    staging_root: &Path,
    expected: &TranscodeVideoExpectedFacts,
) -> Result<TranscodeVideoObservedFacts, TranscodeVideoError> {
    reject_option_like_path("input_path", input_path)?;
    reject_option_like_path("output_path", output_path)?;
    validate_staging_path(staging_root, output_path)?;
    validate_output_missing(output_path).await?;
    let input_pre = observe_file_facts(input_path)
        .await
        .map_err(TranscodeVideoError::from)?;
    verify_expected_facts("input_pre", &input_pre, expected)?;
    Ok(input_pre)
}

/// Shared post-ffmpeg flow for video transcode: re-observe the input and
/// confirm it was untouched while the operation ran, then observe the output.
/// Returns `(input_post, output)`.
async fn finalize_video_operation(
    input_path: &Path,
    output_path: &Path,
    input_pre: &TranscodeVideoObservedFacts,
) -> Result<(TranscodeVideoObservedFacts, TranscodeVideoObservedFacts), TranscodeVideoError> {
    let input_post = observe_file_facts(input_path)
        .await
        .map_err(TranscodeVideoError::from)?;
    verify_observed_match("input_post", input_pre, &input_post)?;
    let output = observe_file_facts(output_path)
        .await
        .map_err(TranscodeVideoError::from)?;
    Ok((input_post, output))
}

/// Before emitting `-c:v copy`, confirm the source satisfies all constraints
/// the profile imposes. Fails loudly on any mismatch — never silently
/// re-encodes or copies a non-conforming stream.
fn validate_copy_video_preconditions(
    request: &TranscodeVideoRequest,
    probe: &InputProbe,
) -> Result<(), TranscodeVideoError> {
    let profile = &request.profile;
    validate_copy_codec(&request.output.video_codec, probe)?;
    validate_copy_dimensions(profile, probe)?;
    validate_copy_pixel_format(profile, probe)?;
    validate_copy_codec_profile(profile, probe)?;
    validate_copy_codec_level(profile, probe)?;
    Ok(())
}

fn validate_copy_codec(target_codec: &str, probe: &InputProbe) -> Result<(), TranscodeVideoError> {
    if codec_tokens_match(&probe.codec, target_codec) {
        return Ok(());
    }
    Err(malformed_worker_result(
        "copy_video",
        format!(
            "copy_video requested but source codec `{}` != target `{}`",
            probe.codec, target_codec
        ),
    ))
}

/// Compares two codec tokens for copy-precondition equality. Resolves known
/// aliases (e.g. `h265` -> `hevc`) via `canonical_video_codec` so an
/// `h265`-spelled probe matches an `hevc` target — mirroring control-plane
/// `decide_copy_video`. Falls back to a normalized literal compare only when
/// either side is an unrecognized codec.
fn codec_tokens_match(source: &str, target: &str) -> bool {
    if let (Some(source_canonical), Some(target_canonical)) = (
        voom_worker_protocol::canonical_video_codec(source),
        voom_worker_protocol::canonical_video_codec(target),
    ) {
        return source_canonical == target_canonical;
    }
    voom_worker_protocol::normalize_codec_token(source)
        == voom_worker_protocol::normalize_codec_token(target)
}

fn validate_copy_dimensions(
    profile: &TranscodeVideoProfile,
    probe: &InputProbe,
) -> Result<(), TranscodeVideoError> {
    if let Some(max_w) = profile.max_width
        && probe.width > max_w
    {
        return Err(malformed_worker_result(
            "copy_video",
            format!(
                "copy_video source width {} exceeds profile cap {}",
                probe.width, max_w
            ),
        ));
    }
    if let Some(max_h) = profile.max_height
        && probe.height > max_h
    {
        return Err(malformed_worker_result(
            "copy_video",
            format!(
                "copy_video source height {} exceeds profile cap {}",
                probe.height, max_h
            ),
        ));
    }
    Ok(())
}

/// An unknown (empty) probe value under a constraint is non-conforming — we
/// cannot prove the stream matches, so fail loudly.
/// `-c:v copy` runs no encoder, so a stream copy under a hardware profile is not a
/// hardware operation and must be allowed when the source already conforms. The source's
/// pixel format is a *file* format, so it is compared against the format a conforming
/// output file carries — not the profile's `pixel_format`, which for a hardware profile
/// names the surface the encoder would have consumed. Comparing the surface refused every
/// legitimate copy under a VAAPI profile.
fn validate_copy_pixel_format(
    profile: &TranscodeVideoProfile,
    probe: &InputProbe,
) -> Result<(), TranscodeVideoError> {
    let required_pf = expected_output_pixel_format(profile).map_err(|error| {
        malformed_worker_result("copy_video", format!("copy_video precondition: {error}"))
    })?;
    let Some(required_pf) = required_pf else {
        return Ok(());
    };
    if probe.pixel_format.is_empty() {
        return Err(malformed_worker_result(
            "copy_video",
            format!(
                "copy_video requires pixel_format `{required_pf}` but source pixel_format is unknown"
            ),
        ));
    }
    if probe.pixel_format != required_pf {
        return Err(malformed_worker_result(
            "copy_video",
            format!(
                "copy_video source pixel_format `{}` != required `{}`",
                probe.pixel_format, required_pf
            ),
        ));
    }
    Ok(())
}

/// An unknown (None) probe value under a constraint is non-conforming — fail
/// loudly rather than copy blind.
fn validate_copy_codec_profile(
    profile: &TranscodeVideoProfile,
    probe: &InputProbe,
) -> Result<(), TranscodeVideoError> {
    let Some(required_cp) = &profile.codec_profile else {
        return Ok(());
    };
    let Some(source_cp) = &probe.codec_profile else {
        return Err(malformed_worker_result(
            "copy_video",
            format!(
                "copy_video requires codec_profile `{required_cp}` but source codec_profile is unknown"
            ),
        ));
    };
    if voom_worker_protocol::normalize_codec_token(source_cp)
        != voom_worker_protocol::normalize_codec_token(required_cp)
    {
        return Err(malformed_worker_result(
            "copy_video",
            format!("copy_video source codec_profile `{source_cp}` != required `{required_cp}`"),
        ));
    }
    Ok(())
}

/// An unknown (None) probe value under a constraint is non-conforming — fail
/// loudly rather than copy blind.
fn validate_copy_codec_level(
    profile: &TranscodeVideoProfile,
    probe: &InputProbe,
) -> Result<(), TranscodeVideoError> {
    let Some(required_cl) = &profile.codec_level else {
        return Ok(());
    };
    let Some(source_cl) = &probe.codec_level else {
        return Err(malformed_worker_result(
            "copy_video",
            format!(
                "copy_video requires codec_level `{required_cl}` but source codec_level is unknown"
            ),
        ));
    };
    if voom_worker_protocol::normalize_codec_token(source_cl)
        != voom_worker_protocol::normalize_codec_token(required_cl)
    {
        return Err(malformed_worker_result(
            "copy_video",
            format!("copy_video source codec_level `{source_cl}` != required `{required_cl}`"),
        ));
    }
    Ok(())
}

pub async fn handle_transcode_audio(
    request: &TranscodeAudioRequest,
    config: &FfmpegConfig,
) -> Result<TranscodeAudioResult, TranscodeAudioError> {
    if request.output.overwrite {
        return Err(config_invalid(
            "request",
            "overwrite must be false".to_owned(),
        ));
    }
    validate_transcode_audio_contract(request)?;
    let input_path = PathBuf::from(&request.input.path);
    let output_path = PathBuf::from(&request.output.path);
    let input_pre = prepare_audio_operation(
        &input_path,
        &output_path,
        Path::new(&request.output.staging_root),
        &request.input.expected,
    )
    .await?;
    let probe = run_ffmpeg_transcode_audio(config, &input_path, &output_path, request)
        .await
        .map_err(TranscodeVideoError::from)?;
    let (input_post, output) =
        finalize_audio_operation(&input_path, &output_path, &input_pre).await?;

    Ok(TranscodeAudioResult {
        status: TranscodeAudioStatus::Transcoded,
        provider: PROVIDER.to_owned(),
        provider_version: config.provider_version.clone(),
        input_pre,
        input_post,
        output,
        output_container: probe.container,
        selected_snapshot_stream_ids: probe
            .selected_output_streams
            .iter()
            .map(|stream| stream.snapshot_stream_id.clone())
            .collect(),
        output_audio_codecs: probe.audio_codecs,
        selected_output_streams: probe.selected_output_streams,
    })
}

pub async fn handle_extract_audio(
    request: &ExtractAudioRequest,
    config: &FfmpegConfig,
) -> Result<ExtractAudioResult, ExtractAudioError> {
    validate_extract_audio_contract(request)?;
    let input_path = PathBuf::from(&request.input.path);
    let targets = extract_audio_targets(request);
    let input_pre = prepare_extract_audio_operation(&input_path, &targets, request).await?;
    let mut outputs = Vec::with_capacity(targets.len());
    for target in &targets {
        outputs.push(execute_extract_audio_target(config, &input_path, request, target).await?);
    }
    let input_post = observe_audio_file_facts(&input_path).await?;
    verify_audio_observed_match("input_post", &input_pre, &input_post)?;
    let Some(first) = outputs.first() else {
        return Err(config_invalid(
            "request",
            "extract_audio must contain at least one output".to_owned(),
        ));
    };
    let result = ExtractAudioResult {
        status: ExtractAudioStatus::Extracted,
        provider: PROVIDER.to_owned(),
        provider_version: config.provider_version.clone(),
        input_pre,
        input_post,
        output: first.output.clone(),
        output_container: first.output_container.clone(),
        output_audio_codec: first.output_audio_codec.clone(),
        selected_snapshot_stream_id: first.selection.snapshot_stream_id.clone(),
        output_language: first.output_language.clone(),
        output_title: first.output_title.clone(),
        outputs: request.outputs.as_ref().map(|_| outputs),
    };
    validate_extract_audio_result(request, &result)
        .map_err(|error| malformed_worker_result("result_contract", error.to_string()))?;
    Ok(result)
}

fn extract_audio_targets(request: &ExtractAudioRequest) -> Vec<ExtractAudioOutputDescriptor> {
    request.outputs.clone().unwrap_or_else(|| {
        vec![ExtractAudioOutputDescriptor {
            output_id: "legacy_singleton".to_owned(),
            selection: request.selection.clone(),
            output: request.output.clone(),
        }]
    })
}

async fn prepare_extract_audio_operation(
    input_path: &Path,
    targets: &[ExtractAudioOutputDescriptor],
    request: &ExtractAudioRequest,
) -> Result<AudioObservedFacts, TranscodeVideoError> {
    reject_option_like_path("input_path", input_path)?;
    for target in targets {
        let output_path = Path::new(&target.output.path);
        reject_option_like_path("output_path", output_path)?;
        validate_staging_path(Path::new(&target.output.staging_root), output_path)?;
        validate_output_missing(output_path).await?;
    }
    let input_pre = observe_audio_file_facts(input_path).await?;
    verify_audio_expected_facts("input_pre", &input_pre, &request.input.expected)?;
    Ok(input_pre)
}

async fn execute_extract_audio_target(
    config: &FfmpegConfig,
    input_path: &Path,
    request: &ExtractAudioRequest,
    target: &ExtractAudioOutputDescriptor,
) -> Result<ExtractAudioOutputResult, TranscodeVideoError> {
    let output_path = Path::new(&target.output.path);
    let singleton = ExtractAudioRequest {
        input: request.input.clone(),
        output: target.output.clone(),
        selection: target.selection.clone(),
        outputs: None,
    };
    let probe = run_ffmpeg_extract_audio(config, input_path, output_path, &singleton)
        .await
        .map_err(TranscodeVideoError::from)?;
    let output = observe_audio_file_facts(output_path).await?;
    Ok(ExtractAudioOutputResult {
        output_id: target.output_id.clone(),
        selection: target.selection.clone(),
        path: target.output.path.clone(),
        output,
        output_container: probe.container,
        output_audio_codec: probe
            .audio_codecs
            .first()
            .cloned()
            .unwrap_or_else(|| target.output.audio_codec.clone()),
        output_language: probe.output_language,
        output_title: probe.output_title,
    })
}

/// Shared pre-ffmpeg flow for audio operations: validate the output path
/// against the staging root, require the output to not yet exist, observe the
/// input file, and verify it matches the request's expected facts.
async fn prepare_audio_operation(
    input_path: &Path,
    output_path: &Path,
    staging_root: &Path,
    expected: &AudioExpectedFacts,
) -> Result<AudioObservedFacts, TranscodeVideoError> {
    reject_option_like_path("input_path", input_path)?;
    reject_option_like_path("output_path", output_path)?;
    validate_staging_path(staging_root, output_path)?;
    validate_output_missing(output_path).await?;
    let input_pre = observe_audio_file_facts(input_path).await?;
    verify_audio_expected_facts("input_pre", &input_pre, expected)?;
    Ok(input_pre)
}

/// Shared post-ffmpeg flow for audio operations: re-observe the input and
/// confirm it was untouched while the operation ran, then observe the output.
/// Returns `(input_post, output)`.
async fn finalize_audio_operation(
    input_path: &Path,
    output_path: &Path,
    input_pre: &AudioObservedFacts,
) -> Result<(AudioObservedFacts, AudioObservedFacts), TranscodeVideoError> {
    let input_post = observe_audio_file_facts(input_path).await?;
    verify_audio_observed_match("input_post", input_pre, &input_post)?;
    let output = observe_audio_file_facts(output_path).await?;
    Ok((input_post, output))
}

/// Rejects a transcode request whose profile names a video encoder this ffmpeg
/// build does not advertise, before any ffmpeg process is launched. A
/// `copy_video` request emits `-c:v copy` and uses no encoder, so it is exempt.
fn validate_encoder_available(
    request: &TranscodeVideoRequest,
    config: &FfmpegConfig,
) -> Result<(), TranscodeVideoError> {
    if request.copy_video {
        return Ok(());
    }
    let encoder = &request.profile.encoder;
    if config.has_video_encoder(encoder) {
        return Ok(());
    }
    Err(config_invalid(
        "transcode_video",
        format!("encoder `{encoder}` is not available in this ffmpeg build"),
    ))
}

/// Each encoder must run on the device the scheduler assigned it, and nowhere else.
/// Dispatch is on the encoder's declared backend rather than on its name so a new
/// hardware encoder cannot fall into a wildcard arm and be validated as software
/// work on an unbound worker — the silent software fallback issue #409 forbids.
fn validate_video_hardware_binding(
    request: &TranscodeVideoRequest,
    config: &FfmpegConfig,
    source: &InputProbe,
) -> Result<(), TranscodeVideoError> {
    let descriptor = voom_core::encoder_descriptor(&request.profile.encoder).ok_or_else(|| {
        config_invalid(
            "transcode_video",
            format!("unknown video encoder `{}`", request.profile.encoder),
        )
    })?;
    match descriptor.backend {
        voom_core::VideoEncoderBackend::Software => validate_software_binding(request, config),
        voom_core::VideoEncoderBackend::Nvidia => {
            validate_nvidia_binding(request, config, &source.codec)
        }
        voom_core::VideoEncoderBackend::Vaapi => validate_vaapi_binding(request, config, source),
        voom_core::VideoEncoderBackend::VideoToolbox => {
            validate_videotoolbox_binding(request, config, source)
        }
    }
}

/// Software video work belongs on an unbound worker: a device-bound one would
/// occupy the accelerator with an encode any worker could have run (ADR 0049 §5).
fn validate_software_binding(
    request: &TranscodeVideoRequest,
    config: &FfmpegConfig,
) -> Result<(), TranscodeVideoError> {
    let software_assignment = match &request.hardware_assignment {
        None | Some(VideoHardwareAssignment::Software(_)) => true,
        Some(
            VideoHardwareAssignment::Nvidia(_)
            | VideoHardwareAssignment::Vaapi(_)
            | VideoHardwareAssignment::VideoToolbox(_),
        ) => false,
    };
    if config.accelerator().is_none() && software_assignment {
        return Ok(());
    }
    Err(config_invalid(
        "transcode_video",
        "software video work requires an unbound software worker".to_owned(),
    ))
}

/// and a `vaapi`-decode profile must name a codec that actually decoded on it at
/// startup (ADR 0052 §2). Every failure here is terminal: there is no software
/// encoder to fall back to.
fn validate_vaapi_binding(
    request: &TranscodeVideoRequest,
    config: &FfmpegConfig,
    source: &InputProbe,
) -> Result<(), TranscodeVideoError> {
    let Some(VideoHardwareAssignment::Vaapi(assignment)) = &request.hardware_assignment else {
        return Err(config_invalid(
            "transcode_video",
            "hevc_vaapi requires scheduler assignment; start a configured worker with: \
             voom worker run-local --kind ffmpeg --vaapi-device <pci-address>"
                .to_owned(),
        ));
    };
    let Some(binding) = config.vaapi() else {
        return Err(config_invalid(
            "transcode_video",
            "assigned VAAPI work reached an unbound worker".to_owned(),
        ));
    };
    let bound_address = &binding.descriptor.pci_address;
    if &assignment.pci_address != bound_address
        || assignment.hardware_token != vaapi_hardware_token(bound_address)
    {
        return Err(config_invalid(
            "transcode_video",
            format!(
                "VAAPI assignment {} ({}) does not match worker {} ({bound_address})",
                assignment.hardware_token,
                assignment.pci_address,
                vaapi_hardware_token(bound_address),
            ),
        ));
    }
    if !request.profile.decode.is_vaapi() {
        return Ok(());
    }
    validate_vaapi_decoder_probed(binding, &source.codec)?;
    validate_vaapi_bit_depth(request, &source.pixel_format)
}

/// VAAPI hardware decode is the one path that carries the *decoder's* surface
/// format straight into the encoder: `vaapi_filter_args` emits no `-vf` there, on
/// purpose, so that frames never leave the GPU. Nothing downstream can therefore
/// reconcile a source bit depth the profile disagrees with — `hevc_vaapi` reports
/// only `No usable encoding profile found`, which reaches the operator as an
/// opaque worker crash carrying an `FFmpeg` dump.
///
/// Software decode needs no such check: its `format=<surface>,hwupload` filter
/// converts the frame to the profile's surface before upload, so a depth change
/// there is the operator asking for exactly that.
fn validate_vaapi_bit_depth(
    request: &TranscodeVideoRequest,
    source_pixel_format: &str,
) -> Result<(), TranscodeVideoError> {
    // `vaapi_surface_format` defaults an absent `pixel_format` to nv12, so this
    // must default identically: reading it as "no declared depth" would reject
    // every profile that leaves the field unset.
    let surface_format = request.profile.pixel_format.as_deref().unwrap_or("nv12");
    let source_depth = video_pixel_format_depth(source_pixel_format);
    if source_depth.is_some() && source_depth == video_pixel_format_depth(surface_format) {
        return Ok(());
    }
    Err(config_invalid(
        "transcode_video",
        format!(
            "VAAPI decode source pixel format `{source_pixel_format}` is incompatible with \
             profile surface format `{surface_format}`; a hardware-decoded source reaches the \
             encoder at its own bit depth, so pair a 10-bit source with `p010`/`main10` and an \
             8-bit source with `nv12`/`main`, or set the profile to software decode to convert \
             the frame on upload"
        ),
    ))
}

fn validate_vaapi_decoder_probed(
    binding: &VaapiDeviceBinding,
    source_codec: &str,
) -> Result<(), TranscodeVideoError> {
    if binding
        .descriptor
        .decoders
        .iter()
        .any(|decoder| decoder == source_codec)
    {
        return Ok(());
    }
    Err(config_invalid(
        "transcode_video",
        format!(
            "VAAPI device {} did not probe a decoder for source codec `{source_codec}`",
            binding.descriptor.pci_address
        ),
    ))
}

fn validate_nvidia_binding(
    request: &TranscodeVideoRequest,
    config: &FfmpegConfig,
    source_codec: &str,
) -> Result<(), TranscodeVideoError> {
    let Some(VideoHardwareAssignment::Nvidia(assignment)) = &request.hardware_assignment else {
        return Err(config_invalid(
            "transcode_video",
            "hevc_nvenc requires scheduler assignment; start a configured worker with: \
             voom worker run-local --kind ffmpeg --nvidia-device GPU-<uuid>"
                .to_owned(),
        ));
    };
    let Some(accelerator) = config.nvidia() else {
        return Err(config_invalid(
            "transcode_video",
            "assigned NVIDIA work reached an unbound worker".to_owned(),
        ));
    };
    if assignment.hardware_token != accelerator.hardware_token
        || assignment.device_uuid != accelerator.device_uuid
    {
        return Err(config_invalid(
            "transcode_video",
            format!(
                "NVIDIA assignment {} ({}) does not match worker {} ({})",
                assignment.hardware_token,
                assignment.device_uuid,
                accelerator.hardware_token,
                accelerator.device_uuid
            ),
        ));
    }
    if !request.profile.decode.is_nvidia() {
        return Ok(());
    }
    let Some(decoder) = nvidia_decoder_for_video_codec(source_codec) else {
        return Err(config_invalid(
            "transcode_video",
            format!("NVIDIA decode does not support source codec `{source_codec}`"),
        ));
    };
    if !accelerator.decoders.iter().any(|item| item == decoder) {
        return Err(config_invalid(
            "transcode_video",
            format!(
                "NVIDIA device {} did not probe decoder `{decoder}`",
                accelerator.device_uuid
            ),
        ));
    }
    Ok(())
}

fn validate_videotoolbox_binding(
    request: &TranscodeVideoRequest,
    config: &FfmpegConfig,
    source: &InputProbe,
) -> Result<(), TranscodeVideoError> {
    let Some(VideoHardwareAssignment::VideoToolbox(assignment)) = &request.hardware_assignment
    else {
        return Err(config_invalid(
            "transcode_video",
            format!(
                "{} requires a VideoToolbox scheduler assignment",
                request.profile.encoder
            ),
        ));
    };
    let Some(accelerator) = config.videotoolbox() else {
        return Err(config_invalid(
            "transcode_video",
            "assigned VideoToolbox work reached an unbound worker".to_owned(),
        ));
    };
    if assignment.hardware_token != accelerator.hardware_token
        || assignment.resource_id != accelerator.resource_id
    {
        return Err(config_invalid(
            "transcode_video",
            "VideoToolbox assignment does not match the bound host resource".to_owned(),
        ));
    }
    if !accelerator
        .encoders
        .iter()
        .any(|encoder| encoder == &request.profile.encoder)
    {
        return Err(config_invalid(
            "transcode_video",
            format!(
                "VideoToolbox host did not prove encoder `{}`",
                request.profile.encoder
            ),
        ));
    }
    if !request.profile.decode.is_video_toolbox() {
        return Ok(());
    }
    let decoder_matches = accelerator.decoders.iter().any(|decoder| {
        codec_tokens_match(&decoder.codec, &source.codec)
            && decoder
                .pixel_formats
                .iter()
                .any(|pixel_format| pixel_format == &source.pixel_format)
    });
    if !decoder_matches {
        return Err(config_invalid(
            "transcode_video",
            format!(
                "VideoToolbox host did not prove decoder `{}/{}`",
                source.codec, source.pixel_format
            ),
        ));
    }
    validate_videotoolbox_bit_depth(request, &source.pixel_format)
}

fn validate_videotoolbox_bit_depth(
    request: &TranscodeVideoRequest,
    source_pixel_format: &str,
) -> Result<(), TranscodeVideoError> {
    let source_depth = video_pixel_format_depth(source_pixel_format);
    let output_depth = request
        .profile
        .pixel_format
        .as_deref()
        .and_then(video_pixel_format_depth);
    if source_depth.is_some() && source_depth == output_depth {
        return Ok(());
    }
    Err(config_invalid(
        "transcode_video",
        format!(
            "VideoToolbox decode source pixel format `{source_pixel_format}` is incompatible \
             with output pixel format `{}`",
            request.profile.pixel_format.as_deref().unwrap_or("unknown")
        ),
    ))
}

/// Maps both the *file* formats a probe reports and the *surface* formats a
/// hardware profile names onto a bit depth, which is the only property the two
/// vocabularies share. `p010` is VAAPI's spelling of the surface `p010le` names.
fn video_pixel_format_depth(pixel_format: &str) -> Option<u8> {
    match pixel_format {
        "yuv420p" | "nv12" => Some(8),
        "yuv420p10le" | "p010le" | "p010" => Some(10),
        _ => None,
    }
}

fn validate_request_contract(request: &TranscodeVideoRequest) -> Result<(), TranscodeVideoError> {
    if !voom_worker_protocol::is_supported_transcode_video_container(&request.output.container) {
        return Err(config_invalid(
            "request",
            format!(
                "transcode_video output container `{}` is not supported (mkv or mp4)",
                request.output.container
            ),
        ));
    }
    if !voom_worker_protocol::is_supported_transcode_video_codec(&request.output.video_codec) {
        return Err(config_invalid(
            "request",
            format!(
                "transcode_video output codec `{}` is not supported (hevc or av1)",
                request.output.video_codec
            ),
        ));
    }
    if let Err(reason) = voom_worker_protocol::validate_profile_against_descriptor(&request.profile)
    {
        return Err(config_invalid(
            "request",
            format!(
                "transcode_video profile `{}` failed encoder descriptor validation: {reason}",
                request.profile.name,
            ),
        ));
    }
    Ok(())
}

fn validate_transcode_audio_contract(
    request: &TranscodeAudioRequest,
) -> Result<(), TranscodeAudioError> {
    if request.output.container != "mkv" {
        return Err(config_invalid(
            "request",
            "transcode_audio output must request mkv".to_owned(),
        ));
    }
    if !voom_worker_protocol::is_supported_transcode_audio_codec(&request.audio.target_codec) {
        return Err(config_invalid(
            "request",
            "transcode_audio target codec must be aac, opus, or eac3".to_owned(),
        ));
    }
    if voom_worker_protocol::audio_target_bitrate_kbps_per_channel(
        &request.audio.target_codec,
        &request.audio.profile,
    )
    .is_none()
    {
        return Err(config_invalid(
            "request",
            format!(
                "transcode_audio profile `{}` is not defined for codec `{}`",
                request.audio.profile, request.audio.target_codec
            ),
        ));
    }
    if request.selection.selected_streams.is_empty() {
        return Err(config_invalid(
            "request",
            "transcode_audio must select at least one stream".to_owned(),
        ));
    }
    if request.audio.add_track
        && request
            .audio
            .target_channels
            .is_none_or(|channels| channels == 0)
    {
        return Err(config_invalid(
            "request",
            "synthesize audio (add_track) requires a positive target_channels".to_owned(),
        ));
    }
    Ok(())
}

fn validate_extract_audio_contract(request: &ExtractAudioRequest) -> Result<(), ExtractAudioError> {
    validate_extract_audio_request(request)
        .map_err(|error| config_invalid("request", error.to_string()))
}

async fn validate_output_missing(output_path: &Path) -> Result<(), TranscodeVideoError> {
    match tokio::fs::symlink_metadata(output_path).await {
        Ok(_) => Err(config_invalid(
            "output_path",
            "output path already exists".to_owned(),
        )),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(config_invalid("output_path", err.to_string())),
    }
}

/// Reject a path that ffmpeg would parse as an option rather than a filename.
/// ffmpeg (and the probe/remux tools) treat any argument whose first character
/// is `-` as a flag. Staging paths are absolute and so begin with `/`, but guard
/// input and output paths explicitly so a crafted request can never smuggle an
/// option through a path — and so this stays correct if staging validation is
/// ever refactored. A leading-`-` component *inside* an absolute path is fine:
/// the argument as a whole no longer starts with `-`.
fn reject_option_like_path(field: &str, path: &Path) -> Result<(), TranscodeVideoError> {
    if path.as_os_str().as_encoded_bytes().first() == Some(&b'-') {
        return Err(config_invalid(
            field,
            "path must not begin with '-' (parsed as an option by ffmpeg)".to_owned(),
        ));
    }
    Ok(())
}

fn validate_staging_path(
    staging_root: &Path,
    output_path: &Path,
) -> Result<(), TranscodeVideoError> {
    let root = canonical_existing_dir_no_symlink(staging_root)?;
    let parent = output_path.parent().ok_or_else(|| {
        config_invalid(
            "output_path",
            "output path has no parent directory".to_owned(),
        )
    })?;
    let parent = canonical_existing_dir_no_symlink(parent)?;
    if !parent.starts_with(root) {
        return Err(config_invalid(
            "output_path",
            "output parent escapes staging root".to_owned(),
        ));
    }
    Ok(())
}

fn canonical_existing_dir_no_symlink(path: &Path) -> Result<PathBuf, TranscodeVideoError> {
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|err| config_invalid("path", format!("{}: {err}", path.display())))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(config_invalid(
            "path",
            format!("path is not a non-symlink directory: {}", path.display()),
        ));
    }
    path.canonicalize()
        .map_err(|err| config_invalid("path", format!("{}: {err}", path.display())))
}

fn verify_expected_facts(
    stage: &str,
    observed: &TranscodeVideoObservedFacts,
    expected: &TranscodeVideoExpectedFacts,
) -> Result<(), TranscodeVideoError> {
    if observed.size_bytes == expected.size_bytes && observed.content_hash == expected.content_hash
    {
        Ok(())
    } else {
        Err(TranscodeVideoError::ArtifactChecksumMismatch {
            message: "input facts differ from expected size/hash".to_owned(),
            payload: serde_json::json!({"stage": stage, "expected": expected, "observed": observed}),
        })
    }
}

fn verify_observed_match(
    stage: &str,
    before: &TranscodeVideoObservedFacts,
    after: &TranscodeVideoObservedFacts,
) -> Result<(), TranscodeVideoError> {
    if before.size_bytes == after.size_bytes && before.content_hash == after.content_hash {
        Ok(())
    } else {
        Err(TranscodeVideoError::ArtifactChecksumMismatch {
            message: "input changed while transcode was running".to_owned(),
            payload: serde_json::json!({"stage": stage, "before": before, "after": after}),
        })
    }
}

fn verify_audio_expected_facts(
    stage: &str,
    observed: &AudioObservedFacts,
    expected: &AudioExpectedFacts,
) -> Result<(), TranscodeVideoError> {
    if observed.size_bytes == expected.size_bytes && observed.content_hash == expected.content_hash
    {
        Ok(())
    } else {
        Err(TranscodeVideoError::ArtifactChecksumMismatch {
            message: "input facts differ from expected size/hash".to_owned(),
            payload: serde_json::json!({"stage": stage, "expected": expected, "observed": observed}),
        })
    }
}

fn verify_audio_observed_match(
    stage: &str,
    before: &AudioObservedFacts,
    after: &AudioObservedFacts,
) -> Result<(), TranscodeVideoError> {
    if before.size_bytes == after.size_bytes && before.content_hash == after.content_hash {
        Ok(())
    } else {
        Err(TranscodeVideoError::ArtifactChecksumMismatch {
            message: "input changed while audio operation was running".to_owned(),
            payload: serde_json::json!({"stage": stage, "before": before, "after": after}),
        })
    }
}

async fn observe_audio_file_facts(path: &Path) -> Result<AudioObservedFacts, TranscodeVideoError> {
    let observed = observe_file_facts(path)
        .await
        .map_err(TranscodeVideoError::from)?;
    Ok(AudioObservedFacts {
        size_bytes: observed.size_bytes,
        content_hash: observed.content_hash,
        modified_at: observed.modified_at,
        local_file_key: observed.local_file_key,
    })
}

fn stream_operation<T, F>(
    context: StreamingOperation,
    operation: F,
) -> Result<OperationDispatch, ProtocolError>
where
    T: Serialize + Send + 'static,
    F: Future<Output = Result<T, TranscodeVideoError>> + Send + 'static,
{
    let response = OperationResponse {
        lease_id: context.lease_id,
        accepted_at: context.accepted_at,
    };
    let (mut writer, dispatch) = OperationDispatch::streaming(response);
    writer.write_frame(&progress_frame(
        context.lease_id,
        0,
        context.accepted_at,
        context.started_message,
    ))?;
    tokio::spawn(async move {
        let interval = progress_interval(context.progress_idle_deadline_ms);
        let mut ticker = tokio::time::interval(interval);
        ticker.tick().await;
        let mut operation = Box::pin(operation);
        let mut seq = 1;
        loop {
            tokio::select! {
                biased;
                result = &mut operation => {
                    let frame = match result {
                        Ok(result) => match result_frame(context.lease_id, seq, result) {
                            Ok(frame) => frame,
                            Err(err) => error_frame(
                                context.lease_id,
                                &malformed_worker_result(
                                    "encode_result",
                                    format!("operation result encode: {err}"),
                                ),
                                seq,
                            ),
                        },
                        Err(err) => error_frame(context.lease_id, &err, seq),
                    };
                    let _ = writer.write_frame(&frame);
                    return;
                }
                _ = ticker.tick() => {
                    let frame = progress_frame(
                        context.lease_id,
                        seq,
                        OffsetDateTime::now_utc(),
                        context.active_message,
                    );
                    if writer.write_frame(&frame).is_err() {
                        return;
                    }
                    seq += 1;
                }
            }
        }
    });
    Ok(dispatch)
}

fn error_dispatch(
    lease_id: LeaseId,
    accepted_at: OffsetDateTime,
    err: &TranscodeVideoError,
    seq: u64,
) -> Result<OperationDispatch, ProtocolError> {
    Ok(OperationDispatch::buffered(
        OperationResponse {
            lease_id,
            accepted_at,
        },
        body_from_frames(&[error_frame(lease_id, err, seq)])?,
    ))
}

fn result_frame<T: Serialize>(
    lease_id: LeaseId,
    seq: u64,
    result: T,
) -> Result<ProgressFrame, ProtocolError> {
    let payload = serde_json::to_value(result).map_err(|err| ProtocolError::InvalidPayload {
        detail: format!("operation result encode: {err}"),
    })?;
    Ok(ProgressFrame::Result {
        lease_id,
        seq,
        emitted_at: OffsetDateTime::now_utc(),
        payload,
    })
}

fn progress_frame(
    lease_id: LeaseId,
    seq: u64,
    emitted_at: OffsetDateTime,
    message: &str,
) -> ProgressFrame {
    ProgressFrame::Progress {
        lease_id,
        seq,
        emitted_at,
        percent: None,
        message: Some(message.to_owned()),
        payload: Some(serde_json::json!({"provider": PROVIDER})),
    }
}

fn progress_interval(progress_idle_deadline_ms: u32) -> Duration {
    let half_deadline_ms = u64::from(progress_idle_deadline_ms)
        .saturating_div(2)
        .max(1);
    Duration::from_millis(half_deadline_ms).min(MAX_PROGRESS_INTERVAL)
}

fn decode_payload<T: DeserializeOwned>(
    payload: serde_json::Value,
    lease_id: LeaseId,
    accepted_at: OffsetDateTime,
    operation: &str,
) -> Result<Result<T, OperationDispatch>, ProtocolError> {
    match serde_json::from_value::<T>(payload) {
        Ok(payload) => Ok(Ok(payload)),
        Err(err) => {
            let worker_err = malformed_worker_result(
                "decode_request",
                format!("{operation} payload decode: {err}"),
            );
            Ok(Err(error_dispatch(lease_id, accepted_at, &worker_err, 0)?))
        }
    }
}

fn error_frame(lease_id: LeaseId, err: &TranscodeVideoError, seq: u64) -> ProgressFrame {
    ProgressFrame::Error {
        lease_id,
        seq,
        emitted_at: OffsetDateTime::now_utc(),
        class: err.failure_class(),
        code: err.error_code(),
        message: err.to_string(),
        payload: Some(err.payload()),
    }
}

fn body_from_frames(frames: &[ProgressFrame]) -> Result<Vec<u8>, ProtocolError> {
    let mut body = Vec::new();
    for frame in frames {
        body.extend_from_slice(&serde_json::to_vec(frame).map_err(|err| {
            ProtocolError::InvalidPayload {
                detail: format!("frame encode: {err}"),
            }
        })?);
        body.push(b'\n');
    }
    Ok(body)
}

impl From<ObserveError> for TranscodeVideoError {
    fn from(value: ObserveError) -> Self {
        match value {
            ObserveError::ArtifactUnavailable(message) => Self::ArtifactUnavailable {
                payload: serde_json::json!({"stage": "observe_file", "message": message}),
                message,
            },
            ObserveError::ArtifactChecksumMismatch(message) => Self::ArtifactChecksumMismatch {
                payload: serde_json::json!({"stage": "observe_file", "message": message}),
                message,
            },
        }
    }
}

impl From<FfmpegError> for TranscodeVideoError {
    fn from(value: FfmpegError) -> Self {
        match value {
            FfmpegError::FfmpegFailed(message) | FfmpegError::FfprobeFailed(message) => {
                Self::ExternalSystemUnavailable {
                    payload: serde_json::json!({"stage": "ffmpeg", "message": message}),
                    message,
                }
            }
            FfmpegError::MalformedMedia(message) => Self::MalformedMedia {
                payload: serde_json::json!({"stage": "ffmpeg", "message": message}),
                message,
            },
            FfmpegError::OutputFactsMismatch(message) => Self::MalformedWorkerResult {
                payload: serde_json::json!({"stage": "output_probe", "message": message}),
                message,
            },
            FfmpegError::UnsupportedInput(message) => Self::ConfigInvalid {
                payload: serde_json::json!({"stage": "input_probe", "message": message}),
                message,
            },
        }
    }
}

fn config_invalid(stage: &str, message: String) -> TranscodeVideoError {
    TranscodeVideoError::ConfigInvalid {
        payload: serde_json::json!({"stage": stage, "message": message}),
        message,
    }
}

fn malformed_worker_result(stage: &str, message: String) -> TranscodeVideoError {
    TranscodeVideoError::MalformedWorkerResult {
        payload: serde_json::json!({"stage": stage, "message": message}),
        message,
    }
}

#[cfg(test)]
#[path = "handler_test.rs"]
mod tests;
