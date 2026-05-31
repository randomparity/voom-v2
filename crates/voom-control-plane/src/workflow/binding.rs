use serde_json::Map;
use serde_json::{Value, json};
use std::path::Path;
use voom_core::OperationKind;
use voom_core::{FileLocationId, FileVersionId};
use voom_plan::audio::{AudioOperationPayload, AudioOperationType};
use voom_plan::remux::RemuxOperationPayload;

use super::ticket_payload::operation_name;
use super::timing::EffectiveTiming;

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
        OperationKind::TranscodeVideo => json!({
            "path": branch.path,
            "profile": "default",
            "target_codec": "h265",
        }),
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
    object.insert("operation".to_owned(), json!(operation_name(operation)));
    object.insert("branch_id".to_owned(), json!(branch.branch_id));
    object.insert("duration_ms".to_owned(), json!(timing.duration_ms));
    object.insert(
        "progress_interval_ms".to_owned(),
        json!(timing.progress_interval_ms),
    );
    Ok(payload)
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
pub fn branch_context_with_probe_codec(branch_id: &str, codec: &str) -> BranchContext {
    BranchContext {
        branch_id: branch_id.to_owned(),
        path: format!("/library/{branch_id}.mkv"),
        probe_codec: Some(codec.to_owned()),
        source_file: None,
    }
}

fn required_string<'a>(payload: &'a Value, field: &str) -> Result<&'a str, BindingError> {
    payload
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| BindingError::new(format!("transcode_video payload missing `{field}`")))
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
