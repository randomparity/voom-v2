use std::fmt::{Display, Formatter};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::Utc;
use serde::{Serialize, de::DeserializeOwned};
use voom_core::{ErrorCode, FailureClass, LeaseId};
use voom_worker_protocol::{
    AudioExpectedFacts, AudioObservedFacts, ExtractAudioRequest, ExtractAudioResult,
    ExtractAudioStatus, OperationDispatch, OperationFuture, OperationHandler, OperationKind,
    OperationRequest, OperationResponse, ProgressFrame, ProtocolError, TranscodeAudioRequest,
    TranscodeAudioResult, TranscodeAudioStatus, TranscodeVideoExpectedFacts,
    TranscodeVideoObservedFacts, TranscodeVideoProfile, TranscodeVideoRequest,
    TranscodeVideoResult, TranscodeVideoStatus,
};

use crate::ffmpeg::{
    FfmpegConfig, FfmpegError, InputProbe, probe_input, run_ffmpeg_extract_audio,
    run_ffmpeg_transcode, run_ffmpeg_transcode_audio,
};
use crate::observe::{ObserveError, observe_file_facts};

const PROVIDER: &str = "ffmpeg";

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
        let accepted_at = Utc::now();
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
                let started = progress_frame(lease_id, accepted_at, "video transcode started");
                match Box::pin(handle_transcode_video(&payload, &config)).await {
                    Ok(result) => success_dispatch(lease_id, accepted_at, started, result),
                    Err(err) => error_dispatch_with_progress(lease_id, accepted_at, started, &err),
                }
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
                let started = progress_frame(lease_id, accepted_at, "audio transcode started");
                match Box::pin(handle_transcode_audio(&payload, &config)).await {
                    Ok(result) => success_dispatch(lease_id, accepted_at, started, result),
                    Err(err) => error_dispatch_with_progress(lease_id, accepted_at, started, &err),
                }
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
                let started = progress_frame(lease_id, accepted_at, "audio extraction started");
                match Box::pin(handle_extract_audio(&payload, &config)).await {
                    Ok(result) => success_dispatch(lease_id, accepted_at, started, result),
                    Err(err) => error_dispatch_with_progress(lease_id, accepted_at, started, &err),
                }
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

    let probe = run_ffmpeg_transcode(config, request, input_probe.width, input_probe.height)
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
fn validate_copy_pixel_format(
    profile: &TranscodeVideoProfile,
    probe: &InputProbe,
) -> Result<(), TranscodeVideoError> {
    let Some(required_pf) = &profile.pixel_format else {
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
    if &probe.pixel_format != required_pf {
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
    if request.output.overwrite {
        return Err(config_invalid(
            "request",
            "overwrite must be false".to_owned(),
        ));
    }
    validate_extract_audio_contract(request)?;
    let input_path = PathBuf::from(&request.input.path);
    let output_path = PathBuf::from(&request.output.path);
    let input_pre = prepare_audio_operation(
        &input_path,
        &output_path,
        Path::new(&request.output.staging_root),
        &request.input.expected,
    )
    .await?;
    let probe = run_ffmpeg_extract_audio(config, &input_path, &output_path, request)
        .await
        .map_err(TranscodeVideoError::from)?;
    let (input_post, output) =
        finalize_audio_operation(&input_path, &output_path, &input_pre).await?;

    Ok(ExtractAudioResult {
        status: ExtractAudioStatus::Extracted,
        provider: PROVIDER.to_owned(),
        provider_version: config.provider_version.clone(),
        input_pre,
        input_post,
        output,
        output_container: probe.container,
        output_audio_codec: probe
            .audio_codecs
            .first()
            .cloned()
            .unwrap_or_else(|| request.output.audio_codec.clone()),
        selected_snapshot_stream_id: request.selection.snapshot_stream_id.clone(),
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
    if voom_worker_protocol::validate_profile_against_descriptor(&request.profile).is_err() {
        return Err(config_invalid(
            "request",
            format!(
                "transcode_video profile `{}` failed encoder descriptor validation",
                request.profile.name
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
    if !matches!(request.audio.target_codec.as_str(), "aac" | "opus") {
        return Err(config_invalid(
            "request",
            "transcode_audio target codec must be aac or opus".to_owned(),
        ));
    }
    if request.selection.selected_streams.is_empty() {
        return Err(config_invalid(
            "request",
            "transcode_audio must select at least one stream".to_owned(),
        ));
    }
    Ok(())
}

fn validate_extract_audio_contract(request: &ExtractAudioRequest) -> Result<(), ExtractAudioError> {
    if request.output.container != "ogg" || request.output.audio_codec != "opus" {
        return Err(config_invalid(
            "request",
            "extract_audio output must request opus in ogg".to_owned(),
        ));
    }
    Ok(())
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

fn success_dispatch<T: Serialize>(
    lease_id: LeaseId,
    accepted_at: chrono::DateTime<chrono::Utc>,
    progress: ProgressFrame,
    result: T,
) -> Result<OperationDispatch, ProtocolError> {
    let payload = serde_json::to_value(result).map_err(|err| ProtocolError::InvalidPayload {
        detail: format!("operation result encode: {err}"),
    })?;
    let result = ProgressFrame::Result {
        lease_id,
        seq: 1,
        emitted_at: Utc::now(),
        payload,
    };
    Ok(OperationDispatch::buffered(
        OperationResponse {
            lease_id,
            accepted_at,
        },
        body_from_frames(&[progress, result])?,
    ))
}

fn error_dispatch(
    lease_id: LeaseId,
    accepted_at: chrono::DateTime<chrono::Utc>,
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

fn error_dispatch_with_progress(
    lease_id: LeaseId,
    accepted_at: chrono::DateTime<chrono::Utc>,
    progress: ProgressFrame,
    err: &TranscodeVideoError,
) -> Result<OperationDispatch, ProtocolError> {
    Ok(OperationDispatch::buffered(
        OperationResponse {
            lease_id,
            accepted_at,
        },
        body_from_frames(&[progress, error_frame(lease_id, err, 1)])?,
    ))
}

fn progress_frame(
    lease_id: LeaseId,
    emitted_at: chrono::DateTime<chrono::Utc>,
    message: &str,
) -> ProgressFrame {
    ProgressFrame::Progress {
        lease_id,
        seq: 0,
        emitted_at,
        percent: None,
        message: Some(message.to_owned()),
        payload: Some(serde_json::json!({"provider": PROVIDER})),
    }
}

fn decode_payload<T: DeserializeOwned>(
    payload: serde_json::Value,
    lease_id: LeaseId,
    accepted_at: chrono::DateTime<chrono::Utc>,
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
        emitted_at: Utc::now(),
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
