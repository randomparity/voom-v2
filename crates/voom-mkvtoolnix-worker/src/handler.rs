use std::collections::BTreeSet;
use std::fmt::{Display, Formatter};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::Utc;
use voom_core::{ErrorCode, FailureClass, LeaseId};
use voom_worker_protocol::{
    OperationDispatch, OperationFuture, OperationHandler, OperationKind, OperationRequest,
    OperationResponse, ProgressFrame, ProtocolError, RemuxExpectedFacts, RemuxObservedFacts,
    RemuxRequest, RemuxResult, RemuxStatus, RemuxStreamRef, RemuxTrackGroup,
};

use crate::mkvmerge::{identify_output, identify_tracks, run_mkvmerge_remux};
use crate::observe::{ObserveError, observe_file_facts};
use crate::preflight::{MkvmergeConfig, MkvtoolnixError};

const PROVIDER: &str = "mkvtoolnix";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MkvtoolnixWorkerError {
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

impl MkvtoolnixWorkerError {
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

impl Display for MkvtoolnixWorkerError {
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

impl std::error::Error for MkvtoolnixWorkerError {}

#[must_use]
pub fn handle_operation(req: OperationRequest) -> OperationFuture {
    handle_operation_with_config(req, None)
}

#[must_use]
pub fn operation_handler(config: MkvmergeConfig) -> OperationHandler {
    Arc::new(move |req| handle_operation_with_config(req, Some(config.clone())))
}

fn handle_operation_with_config(
    req: OperationRequest,
    config: Option<MkvmergeConfig>,
) -> OperationFuture {
    Box::pin(async move {
        if req.operation != OperationKind::Remux {
            return Err(ProtocolError::UnknownOperation {
                name: format!("{:?}", req.operation),
            });
        }

        let lease_id = req.lease_id;
        let accepted_at = Utc::now();
        let payload = match serde_json::from_value::<RemuxRequest>(req.payload) {
            Ok(payload) => payload,
            Err(err) => {
                return error_dispatch(
                    lease_id,
                    accepted_at,
                    &malformed_worker_result(
                        "decode_request",
                        format!("remux payload decode: {err}"),
                    ),
                    0,
                );
            }
        };
        let Some(config) = config else {
            return error_dispatch(
                lease_id,
                accepted_at,
                &config_invalid("preflight", "mkvmerge config was not provided".to_owned()),
                0,
            );
        };

        let started = progress_frame(lease_id, accepted_at);
        match Box::pin(handle_remux(&payload, &config)).await {
            Ok(result) => success_dispatch(lease_id, accepted_at, started, result),
            Err(err) => error_dispatch_with_progress(lease_id, accepted_at, started, &err),
        }
    })
}

pub async fn handle_remux(
    request: &RemuxRequest,
    config: &MkvmergeConfig,
) -> Result<RemuxResult, MkvtoolnixWorkerError> {
    validate_request_contract(request)?;
    let input_path = PathBuf::from(&request.input.path);
    let output_path = PathBuf::from(&request.output.path);
    validate_staging_path(Path::new(&request.output.staging_root), &output_path)?;
    if tokio::fs::try_exists(&output_path)
        .await
        .map_err(|err| config_invalid("output_path", err.to_string()))?
    {
        return Err(config_invalid(
            "output_path",
            "output path already exists".to_owned(),
        ));
    }

    let input_pre = observe_file_facts(&input_path)
        .await
        .map_err(MkvtoolnixWorkerError::from)?;
    verify_expected_facts("input_pre", &input_pre, &request.input.expected)?;
    let input_mapping = identify_tracks(config, &input_path)
        .await
        .map_err(MkvtoolnixWorkerError::from)?;
    if !request.selection.keep_streams.iter().any(|stream| {
        input_mapping
            .track_for_provider_index(stream.provider_stream_index)
            .is_some_and(|track| track.kind.matches_group(RemuxTrackGroup::Video))
    }) {
        return Err(config_invalid(
            "selection",
            "selection must include at least one video stream".to_owned(),
        ));
    }
    validate_all_source_video_streams_kept(request, &input_mapping)?;
    run_mkvmerge_remux(config, request, &input_mapping)
        .await
        .map_err(MkvtoolnixWorkerError::from)?;
    let input_post = observe_file_facts(&input_path)
        .await
        .map_err(MkvtoolnixWorkerError::from)?;
    verify_observed_match("input_post", &input_pre, &input_post)?;
    let output = observe_file_facts(&output_path)
        .await
        .map_err(MkvtoolnixWorkerError::from)?;
    let output_probe = identify_output(config, &output_path)
        .await
        .map_err(MkvtoolnixWorkerError::from)?;
    validate_output_selection(request, &input_mapping, &output_probe.mapping)?;

    Ok(RemuxResult {
        status: RemuxStatus::Remuxed,
        provider: PROVIDER.to_owned(),
        provider_version: config.provider_version.clone(),
        input_pre,
        input_post,
        output,
        output_container: voom_worker_protocol::REMUX_CONTAINER_MKV.to_owned(),
        kept_snapshot_stream_ids: request
            .selection
            .keep_streams
            .iter()
            .map(|stream| stream.snapshot_stream_id.clone())
            .collect(),
        default_snapshot_stream_ids: request
            .selection
            .default_streams
            .iter()
            .map(|stream| stream.snapshot_stream_id.clone())
            .collect(),
    })
}

fn validate_request_contract(request: &RemuxRequest) -> Result<(), MkvtoolnixWorkerError> {
    if request.output.overwrite {
        return Err(config_invalid(
            "request",
            "overwrite must be false".to_owned(),
        ));
    }
    if !voom_worker_protocol::is_supported_remux_container(&request.output.container) {
        return Err(config_invalid(
            "request",
            "remux output container must be mkv".to_owned(),
        ));
    }
    if request.selection.keep_streams.is_empty() {
        return Err(config_invalid(
            "selection",
            "selection must include at least one video stream".to_owned(),
        ));
    }
    reject_duplicate_refs("keep_streams", &request.selection.keep_streams)?;
    reject_duplicate_refs("default_streams", &request.selection.default_streams)?;
    reject_duplicate_refs(
        "clear_default_streams",
        &request.selection.clear_default_streams,
    )?;
    Ok(())
}

fn reject_duplicate_refs(
    field: &str,
    streams: &[RemuxStreamRef],
) -> Result<(), MkvtoolnixWorkerError> {
    let mut snapshot_ids = BTreeSet::new();
    let mut provider_indexes = BTreeSet::new();
    for stream in streams {
        if !snapshot_ids.insert(stream.snapshot_stream_id.as_str()) {
            return Err(config_invalid(
                "selection",
                format!("duplicate snapshot_stream_id in {field}"),
            ));
        }
        if !provider_indexes.insert(stream.provider_stream_index) {
            return Err(config_invalid(
                "selection",
                format!("duplicate provider_stream_index in {field}"),
            ));
        }
    }
    Ok(())
}

fn validate_staging_path(
    staging_root: &Path,
    output_path: &Path,
) -> Result<(), MkvtoolnixWorkerError> {
    let root = canonical_existing_dir_no_symlink(staging_root)?;
    if staging_root != root {
        return Err(config_invalid(
            "output_path",
            "staging root must be canonical".to_owned(),
        ));
    }
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

fn validate_all_source_video_streams_kept(
    request: &RemuxRequest,
    input_mapping: &crate::mkvmerge::MkvmergeTrackMapping,
) -> Result<(), MkvtoolnixWorkerError> {
    let kept_provider_indexes = request
        .selection
        .keep_streams
        .iter()
        .map(|stream| stream.provider_stream_index)
        .collect::<BTreeSet<_>>();
    let missing_video_indexes = input_mapping
        .provider_indexes_for_group(RemuxTrackGroup::Video)
        .into_iter()
        .filter(|provider_index| !kept_provider_indexes.contains(provider_index))
        .collect::<Vec<_>>();
    if missing_video_indexes.is_empty() {
        return Ok(());
    }
    Err(config_invalid(
        "selection",
        format!(
            "unsupported media shape: must keep all source video streams; missing provider indexes {missing_video_indexes:?}"
        ),
    ))
}

fn canonical_existing_dir_no_symlink(path: &Path) -> Result<PathBuf, MkvtoolnixWorkerError> {
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
    observed: &RemuxObservedFacts,
    expected: &RemuxExpectedFacts,
) -> Result<(), MkvtoolnixWorkerError> {
    if observed.size_bytes == expected.size_bytes && observed.content_hash == expected.content_hash
    {
        if expected.modified_at.is_some() && observed.modified_at != expected.modified_at {
            return Err(MkvtoolnixWorkerError::ArtifactChecksumMismatch {
                message: "input facts differ from expected modified_at".to_owned(),
                payload: serde_json::json!({"stage": stage, "expected": expected, "observed": observed}),
            });
        }
        if expected.local_file_key.is_some() && observed.local_file_key != expected.local_file_key {
            return Err(MkvtoolnixWorkerError::ArtifactChecksumMismatch {
                message: "input facts differ from expected local_file_key".to_owned(),
                payload: serde_json::json!({"stage": stage, "expected": expected, "observed": observed}),
            });
        }
        return Ok(());
    }
    Err(MkvtoolnixWorkerError::ArtifactChecksumMismatch {
        message: "input facts differ from expected size/hash".to_owned(),
        payload: serde_json::json!({"stage": stage, "expected": expected, "observed": observed}),
    })
}

fn verify_observed_match(
    stage: &str,
    before: &RemuxObservedFacts,
    after: &RemuxObservedFacts,
) -> Result<(), MkvtoolnixWorkerError> {
    if before.size_bytes == after.size_bytes && before.content_hash == after.content_hash {
        Ok(())
    } else {
        Err(MkvtoolnixWorkerError::ArtifactChecksumMismatch {
            message: "input changed while remux was running".to_owned(),
            payload: serde_json::json!({"stage": stage, "before": before, "after": after}),
        })
    }
}

fn validate_output_selection(
    request: &RemuxRequest,
    input_mapping: &crate::mkvmerge::MkvmergeTrackMapping,
    output_mapping: &crate::mkvmerge::MkvmergeTrackMapping,
) -> Result<(), MkvtoolnixWorkerError> {
    if output_mapping.track_count() != request.selection.keep_streams.len() {
        return Err(malformed_worker_result(
            "output_probe",
            format!(
                "selected stream mismatch: expected {} output tracks, got {}",
                request.selection.keep_streams.len(),
                output_mapping.track_count()
            ),
        ));
    }

    let saw_video = (0..request.selection.keep_streams.len()).any(|output_index| {
        u32::try_from(output_index)
            .ok()
            .and_then(|provider_index| output_mapping.track_for_provider_index(provider_index))
            .is_some_and(|track| track.kind.matches_group(RemuxTrackGroup::Video))
    });
    if !saw_video {
        return Err(malformed_worker_result(
            "output_probe",
            "output must include at least one video track".to_owned(),
        ));
    }

    for (output_index, kept_stream) in request.selection.keep_streams.iter().enumerate() {
        let expected_track = input_mapping
            .track_for_provider_index(kept_stream.provider_stream_index)
            .ok_or_else(|| {
                malformed_worker_result(
                    "output_probe",
                    format!(
                        "selected stream mismatch: missing input track for provider index {}",
                        kept_stream.provider_stream_index
                    ),
                )
            })?;
        let output_track = u32::try_from(output_index)
            .ok()
            .and_then(|provider_index| output_mapping.track_for_provider_index(provider_index))
            .ok_or_else(|| {
                malformed_worker_result(
                    "output_probe",
                    format!(
                        "selected stream mismatch: missing output track at index {output_index}"
                    ),
                )
            })?;
        if output_track.kind != expected_track.kind {
            return Err(malformed_worker_result(
                "output_probe",
                format!(
                    "selected stream mismatch: expected {:?} for {}, got {:?}",
                    expected_track.kind, kept_stream.snapshot_stream_id, output_track.kind
                ),
            ));
        }
    }

    let expected_default = request
        .selection
        .default_streams
        .iter()
        .map(|stream| stream.snapshot_stream_id.clone())
        .collect::<Vec<_>>();
    let actual_default = request
        .selection
        .keep_streams
        .iter()
        .enumerate()
        .filter_map(|(index, stream)| {
            let provider_index = u32::try_from(index).ok()?;
            let track = output_mapping.track_for_provider_index(provider_index)?;
            track.default.then(|| stream.snapshot_stream_id.clone())
        })
        .collect::<Vec<_>>();
    if actual_default != expected_default {
        return Err(malformed_worker_result(
            "output_probe",
            format!(
                "default stream mismatch: expected {expected_default:?}, got {actual_default:?}"
            ),
        ));
    }
    Ok(())
}

fn success_dispatch(
    lease_id: LeaseId,
    accepted_at: chrono::DateTime<chrono::Utc>,
    progress: ProgressFrame,
    result: RemuxResult,
) -> Result<OperationDispatch, ProtocolError> {
    let payload = serde_json::to_value(result).map_err(|err| ProtocolError::InvalidPayload {
        detail: format!("remux result encode: {err}"),
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
    err: &MkvtoolnixWorkerError,
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
    err: &MkvtoolnixWorkerError,
) -> Result<OperationDispatch, ProtocolError> {
    Ok(OperationDispatch::buffered(
        OperationResponse {
            lease_id,
            accepted_at,
        },
        body_from_frames(&[progress, error_frame(lease_id, err, 1)])?,
    ))
}

fn progress_frame(lease_id: LeaseId, emitted_at: chrono::DateTime<chrono::Utc>) -> ProgressFrame {
    ProgressFrame::Progress {
        lease_id,
        seq: 0,
        emitted_at,
        percent: None,
        message: Some("remux started".to_owned()),
        payload: Some(serde_json::json!({"provider": PROVIDER})),
    }
}

fn error_frame(lease_id: LeaseId, err: &MkvtoolnixWorkerError, seq: u64) -> ProgressFrame {
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

impl From<ObserveError> for MkvtoolnixWorkerError {
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

impl From<MkvtoolnixError> for MkvtoolnixWorkerError {
    fn from(value: MkvtoolnixError) -> Self {
        match value {
            MkvtoolnixError::ConfigInvalid(message) => config_invalid("mkvmerge", message),
            MkvtoolnixError::OutputFactsMismatch(message) => {
                malformed_worker_result("output_probe", message)
            }
            MkvtoolnixError::Preflight(message)
            | MkvtoolnixError::MkvmergeFailed(message)
            | MkvtoolnixError::IdentifyFailed(message) => Self::ExternalSystemUnavailable {
                payload: serde_json::json!({"stage": "mkvmerge", "message": message}),
                message,
            },
        }
    }
}

fn config_invalid(stage: &str, message: String) -> MkvtoolnixWorkerError {
    MkvtoolnixWorkerError::ConfigInvalid {
        payload: serde_json::json!({"stage": stage, "message": message}),
        message,
    }
}

fn malformed_worker_result(stage: &str, message: String) -> MkvtoolnixWorkerError {
    MkvtoolnixWorkerError::MalformedWorkerResult {
        payload: serde_json::json!({"stage": stage, "message": message}),
        message,
    }
}

#[cfg(test)]
#[path = "handler_test.rs"]
mod tests;
