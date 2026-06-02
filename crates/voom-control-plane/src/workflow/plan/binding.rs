use serde_json::Map;
use serde_json::{Value, json};
use std::path::Path;
use voom_core::OperationKind;
use voom_core::{FileLocationId, FileVersionId};
use voom_plan::audio::{AudioOperationPayload, AudioOperationType};
use voom_plan::remux::RemuxOperationPayload;
use voom_worker_protocol::{
    TranscodeVideoExpectedFacts, TranscodeVideoInput, TranscodeVideoOutput, TranscodeVideoProfile,
    TranscodeVideoRequest,
};

use crate::transcode::stage::{OutputName, output_file_name};
use crate::workflow::execution::timing::EffectiveTiming;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchContext {
    pub branch_id: String,
    pub path: String,
    pub probe_codec: Option<String>,
    pub source_file: Option<Value>,
}

pub fn render_default_payload(
    operation: OperationKind,
    branch: &BranchContext,
    timing: EffectiveTiming,
) -> Result<Value, BindingError> {
    render_default_payload_with_fan_out(operation, branch, timing, 3)
}

pub fn render_default_payload_with_fan_out(
    operation: OperationKind,
    branch: &BranchContext,
    timing: EffectiveTiming,
    fan_out_count: usize,
) -> Result<Value, BindingError> {
    let mut payload = match operation {
        OperationKind::ScanLibrary => json!({
            "path": "/library",
            "fan_out_count": fan_out_count,
        }),
        OperationKind::ProbeFile => {
            let mut payload = json!({ "path": branch.path });
            if let Some(codec) = &branch.probe_codec {
                payload["codec"] = json!(codec);
            }
            payload
        }
        OperationKind::HashFile
        | OperationKind::IdentifyMedia
        | OperationKind::BackUpFile
        | OperationKind::VerifyArtifact
        | OperationKind::ExtractAudio
        | OperationKind::TranscodeAudio
        | OperationKind::TranscribeAudio
        | OperationKind::DeleteArtifact => json!({ "path": branch.path }),
        OperationKind::ScoreQuality => {
            let codec = branch.probe_codec.as_ref().ok_or_else(|| {
                BindingError::new(format!(
                    "probe codec missing for branch `{}`",
                    branch.branch_id
                ))
            })?;
            json!({
                "path": branch.path,
                "profile": "default",
                "codec": codec,
            })
        }
        OperationKind::Remux => json!({
            "path": branch.path,
            "container": "mkv",
        }),
        OperationKind::TranscodeVideo => render_default_transcode_video_payload(branch)?,
        OperationKind::CommitArtifact => json!({
            "path": branch.path,
            "reason": "quality_regression",
        }),
        OperationKind::SyncExternalSystem => json!({
            "path": branch.path,
            "system": "plex",
            "action": "refresh",
        }),
        OperationKind::EditTracks => json!({
            "path": branch.path,
            "holder": "manual",
            "reason": "playback",
        }),
    };

    let Some(object) = payload.as_object_mut() else {
        return Err(BindingError::new("rendered payload must be a JSON object"));
    };
    object.insert("operation".to_owned(), json!(operation.as_str()));
    object.insert("branch_id".to_owned(), json!(branch.branch_id));
    object.insert("duration_ms".to_owned(), json!(timing.duration_ms));
    object.insert(
        "progress_interval_ms".to_owned(),
        json!(timing.progress_interval_ms),
    );
    Ok(payload)
}

fn render_default_transcode_video_payload(branch: &BranchContext) -> Result<Value, BindingError> {
    let profile = TranscodeVideoProfile::default_hevc();
    let staging_root = "/tmp/voom/default-workflow/transcode/staging";
    let output_name = OutputName {
        source_path: &branch.path,
        profile_id: &profile.name,
        codec: &profile.target_codec,
        container: "mkv",
    };
    let request = TranscodeVideoRequest {
        input: TranscodeVideoInput {
            path: source_file_string(branch, "path")?.to_owned(),
            expected: TranscodeVideoExpectedFacts {
                size_bytes: source_file_u64(branch, "size_bytes")?,
                content_hash: source_file_string(branch, "content_hash")?.to_owned(),
                modified_at: source_file_optional_string(branch, "modified_at")?,
                local_file_key: source_file_optional_string(branch, "local_file_key")?,
            },
        },
        output: TranscodeVideoOutput {
            staging_root: staging_root.to_owned(),
            path: format!(
                "{}/{}/{}",
                staging_root,
                branch.branch_id,
                output_file_name(&output_name)
            ),
            container: "mkv".to_owned(),
            video_codec: profile.target_codec.clone(),
            overwrite: true,
        },
        profile,
        copy_video: false,
    };
    serde_json::to_value(request)
        .map_err(|err| BindingError::new(format!("transcode_video payload encode: {err}")))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PolicyFileSource {
    pub file_version_id: FileVersionId,
    pub location_id: Option<FileLocationId>,
}

pub fn render_policy_transcode_payload(
    source: PolicyFileSource,
    operation_payload: &Value,
    staging_root: &Path,
    target_dir: &Path,
    timing: EffectiveTiming,
) -> Result<Value, BindingError> {
    let target_codec = required_string(operation_payload, "target_codec")?;
    let container = required_string(operation_payload, "container")?;
    let profile = required_string(operation_payload, "profile")?;
    // The planner embeds the full typed profile as `resolved_profile` (pinned
    // Phase 5↔6 contract from Task 5.2). Thread it into the ticket payload so
    // the executor can build the TranscodeVideoRequest without re-running
    // resolution at dispatch time.
    let resolved_profile = operation_payload
        .get("resolved_profile")
        .cloned()
        .ok_or_else(|| {
            BindingError::new("transcode_video node payload missing `resolved_profile`")
        })?;
    if !resolved_profile.is_object() {
        return Err(BindingError::new(
            "transcode_video node payload `resolved_profile` must be an object",
        ));
    }
    let mut payload = json!({
        "operation": "transcode_video",
        "target_codec": target_codec,
        "container": container,
        "profile": profile,
        "resolved_profile": resolved_profile,
        "staging_root": staging_root,
        "target_dir": target_dir,
        "duration_ms": timing.duration_ms,
        "progress_interval_ms": timing.progress_interval_ms,
    });
    let Some(object) = payload.as_object_mut() else {
        return Err(BindingError::new("rendered payload must be a JSON object"));
    };
    insert_policy_file_source(object, source);
    Ok(payload)
}

pub fn render_policy_remux_payload(
    source: PolicyFileSource,
    operation_payload: &Value,
    staging_root: &Path,
    target_dir: &Path,
    timing: EffectiveTiming,
) -> Result<Value, BindingError> {
    let remux_payload = RemuxOperationPayload::try_from_execution_value(operation_payload)
        .map_err(|err| BindingError::new(err.to_string()))?
        .into_value();
    let mut payload = json!({
        "operation": "remux",
        "remux": remux_payload,
        "staging_root": staging_root,
        "target_dir": target_dir,
        "duration_ms": timing.duration_ms,
        "progress_interval_ms": timing.progress_interval_ms,
    });
    let Some(object) = payload.as_object_mut() else {
        return Err(BindingError::new("rendered payload must be a JSON object"));
    };
    insert_policy_file_source(object, source);
    Ok(payload)
}

pub fn render_policy_transcode_audio_payload(
    source: PolicyFileSource,
    operation_payload: &Value,
    staging_root: &Path,
    target_dir: &Path,
    timing: EffectiveTiming,
) -> Result<Value, BindingError> {
    render_policy_audio_payload(
        source,
        operation_payload,
        AudioOperationType::TranscodeAudio,
        "transcode_audio",
        staging_root,
        target_dir,
        timing,
    )
}

pub fn render_policy_extract_audio_payload(
    source: PolicyFileSource,
    operation_payload: &Value,
    staging_root: &Path,
    target_dir: &Path,
    timing: EffectiveTiming,
) -> Result<Value, BindingError> {
    render_policy_audio_payload(
        source,
        operation_payload,
        AudioOperationType::ExtractAudio,
        "extract_audio",
        staging_root,
        target_dir,
        timing,
    )
}

fn render_policy_audio_payload(
    source: PolicyFileSource,
    operation_payload: &Value,
    expected_type: AudioOperationType,
    operation: &str,
    staging_root: &Path,
    target_dir: &Path,
    timing: EffectiveTiming,
) -> Result<Value, BindingError> {
    let audio_payload = AudioOperationPayload::try_from_execution_value(operation_payload)
        .map_err(|err| BindingError::new(err.to_string()))?;
    if audio_payload.operation_type != expected_type {
        return Err(BindingError::new(format!(
            "{operation} payload has mismatched type"
        )));
    }
    let mut payload = json!({
        "operation": operation,
        "audio": audio_payload.into_value(),
        "staging_root": staging_root,
        "target_dir": target_dir,
        "duration_ms": timing.duration_ms,
        "progress_interval_ms": timing.progress_interval_ms,
    });
    let Some(object) = payload.as_object_mut() else {
        return Err(BindingError::new("rendered payload must be a JSON object"));
    };
    insert_policy_file_source(object, source);
    Ok(payload)
}

fn insert_policy_file_source(object: &mut Map<String, Value>, source: PolicyFileSource) {
    object.insert(
        "source_file_version_id".to_owned(),
        json!(source.file_version_id),
    );
    if let Some(location_id) = source.location_id {
        object.insert("source_location_id".to_owned(), json!(location_id));
    }
}

#[must_use]
#[cfg(test)]
pub fn branch_context_with_probe_codec(branch_id: &str, codec: &str) -> BranchContext {
    BranchContext {
        branch_id: branch_id.to_owned(),
        path: format!("/library/{branch_id}.mkv"),
        probe_codec: Some(codec.to_owned()),
        source_file: Some(test_source_file(branch_id)),
    }
}

fn required_string<'a>(payload: &'a Value, field: &str) -> Result<&'a str, BindingError> {
    payload
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| BindingError::new(format!("transcode_video payload missing `{field}`")))
}

fn source_file(branch: &BranchContext) -> Result<&Value, BindingError> {
    branch.source_file.as_ref().ok_or_else(|| {
        BindingError::new(format!(
            "transcode_video branch `{}` missing source_file facts",
            branch.branch_id
        ))
    })
}

fn source_file_string<'a>(
    branch: &'a BranchContext,
    field: &'static str,
) -> Result<&'a str, BindingError> {
    source_file(branch)?
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| {
            BindingError::new(format!(
                "transcode_video source_file for branch `{}` missing string `{field}`",
                branch.branch_id
            ))
        })
}

fn source_file_optional_string(
    branch: &BranchContext,
    field: &'static str,
) -> Result<Option<String>, BindingError> {
    match source_file(branch)?.get(field) {
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(Value::Null) | None => Ok(None),
        Some(_) => Err(BindingError::new(format!(
            "transcode_video source_file for branch `{}` field `{field}` must be a string",
            branch.branch_id
        ))),
    }
}

fn source_file_u64(branch: &BranchContext, field: &'static str) -> Result<u64, BindingError> {
    source_file(branch)?
        .get(field)
        .and_then(Value::as_u64)
        .ok_or_else(|| {
            BindingError::new(format!(
                "transcode_video source_file for branch `{}` missing unsigned `{field}`",
                branch.branch_id
            ))
        })
}

#[cfg(test)]
fn test_source_file(branch_id: &str) -> Value {
    json!({
        "path": format!("/library/{branch_id}.mkv"),
        "size_bytes": 4_200_000_000_u64,
        "content_hash": format!("blake3:{branch_id}"),
        "local_file_key": format!("/library/{branch_id}.mkv")
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BindingError {
    detail: String,
}

impl BindingError {
    pub(crate) fn new(detail: impl Into<String>) -> Self {
        Self {
            detail: detail.into(),
        }
    }
}

impl std::fmt::Display for BindingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.detail)
    }
}

impl std::error::Error for BindingError {}

#[cfg(test)]
#[path = "binding_test.rs"]
mod tests;
