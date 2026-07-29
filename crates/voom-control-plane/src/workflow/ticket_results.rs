use voom_core::ids::ArtifactVerificationId;
use voom_core::{
    ArtifactHandleId, ArtifactLocationId, FileLocationId, FileVersionId, MediaSnapshotId, VoomError,
};

#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PolicyVerificationTicketResult {
    pub(crate) source_file_version_id: FileVersionId,
    pub(crate) source_location_id: FileLocationId,
    pub(crate) source_media_snapshot_id: MediaSnapshotId,
    pub(crate) artifact_handle_id: ArtifactHandleId,
    pub(crate) artifact_location_id: ArtifactLocationId,
    pub(crate) artifact_verification_id: ArtifactVerificationId,
    pub(crate) status: PolicyVerificationTicketStatus,
    pub(crate) path: String,
    pub(crate) expected_size_bytes: u64,
    pub(crate) expected_checksum: String,
    pub(crate) observed_size_bytes: Option<u64>,
    pub(crate) observed_checksum: Option<String>,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PolicyVerificationTicketStatus {
    Verified,
}

#[derive(Debug)]
pub(crate) enum OrderedTicketResult {
    Outputs(Vec<serde_json::Value>),
    Scalar(serde_json::Value),
}

pub(crate) fn ordered_ticket_result(result: &str) -> Result<OrderedTicketResult, VoomError> {
    let value: serde_json::Value = serde_json::from_str(result)
        .map_err(|error| VoomError::database(format!("ticket result is malformed: {error}")))?;
    let outputs = value
        .get("outputs")
        .and_then(serde_json::Value::as_array)
        .filter(|outputs| !outputs.is_empty());
    match outputs {
        Some(outputs) => Ok(OrderedTicketResult::Outputs(outputs.clone())),
        None => Ok(OrderedTicketResult::Scalar(value)),
    }
}

#[cfg(test)]
#[path = "ticket_results_test.rs"]
mod tests;
