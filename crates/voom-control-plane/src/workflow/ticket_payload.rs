use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use voom_worker_protocol::OperationKind;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkflowTicketPayload {
    pub workflow_id: String,
    pub plan_id: String,
    pub node_id: String,
    pub branch_id: String,
    pub operation: OperationKind,
    pub rendered_payload: Value,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowTicketPayloadError {
    detail: String,
}

impl WorkflowTicketPayload {
    #[must_use]
    pub fn new_for_test(
        workflow_id: &str,
        plan_id: &str,
        node_id: &str,
        branch_id: &str,
        operation: OperationKind,
        rendered_payload: Value,
    ) -> Self {
        Self {
            workflow_id: workflow_id.to_owned(),
            plan_id: plan_id.to_owned(),
            node_id: node_id.to_owned(),
            branch_id: branch_id.to_owned(),
            operation,
            rendered_payload,
        }
    }

    pub fn to_ticket_payload(&self) -> Result<Value, WorkflowTicketPayloadError> {
        let mut value = serde_json::to_value(self).map_err(|e| {
            WorkflowTicketPayloadError::new(format!("workflow ticket payload encode: {e}"))
        })?;
        let operation = operation_name(self.operation);
        let Some(rendered_payload) = value
            .get_mut("rendered_payload")
            .and_then(serde_json::Value::as_object_mut)
        else {
            return Err(WorkflowTicketPayloadError::new(
                "rendered_payload must be a JSON object",
            ));
        };
        rendered_payload
            .entry("operation".to_owned())
            .or_insert_with(|| json!(operation));
        Ok(value)
    }

    pub fn parse_ticket(
        ticket_kind: &str,
        payload: Value,
    ) -> Result<Self, WorkflowTicketPayloadError> {
        let ticket_operation = ticket_operation(ticket_kind)?;
        let parsed: Self = serde_json::from_value(payload).map_err(|e| {
            WorkflowTicketPayloadError::new(format!("workflow ticket payload decode: {e}"))
        })?;
        if ticket_operation != parsed.operation {
            return Err(operation_mismatch(
                "ticket kind suffix",
                ticket_operation,
                parsed.operation,
            ));
        }

        let rendered_operation = parsed
            .rendered_payload
            .get("operation")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| {
                WorkflowTicketPayloadError::new("rendered_payload.operation is required")
            })?;
        let rendered_operation = parse_operation_name(rendered_operation)?;
        if rendered_operation != parsed.operation {
            return Err(operation_mismatch(
                "rendered_payload.operation",
                rendered_operation,
                parsed.operation,
            ));
        }

        Ok(parsed)
    }
}

impl WorkflowTicketPayloadError {
    fn new(detail: impl Into<String>) -> Self {
        Self {
            detail: detail.into(),
        }
    }
}

impl std::fmt::Display for WorkflowTicketPayloadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.detail)
    }
}

impl std::error::Error for WorkflowTicketPayloadError {}

pub(crate) fn operation_name(operation: OperationKind) -> &'static str {
    match operation {
        OperationKind::ScanLibrary => "scan_library",
        OperationKind::ProbeFile => "probe_file",
        OperationKind::HashFile => "hash_file",
        OperationKind::IdentifyMedia => "identify_media",
        OperationKind::ScoreQuality => "score_quality",
        OperationKind::SyncExternalSystem => "sync_external_system",
        OperationKind::BackUpFile => "back_up_file",
        OperationKind::Remux => "remux",
        OperationKind::TranscodeVideo => "transcode_video",
        OperationKind::EditTracks => "edit_tracks",
        OperationKind::ExtractAudio => "extract_audio",
        OperationKind::TranscribeAudio => "transcribe_audio",
        OperationKind::VerifyArtifact => "verify_artifact",
        OperationKind::CommitArtifact => "commit_artifact",
        OperationKind::DeleteArtifact => "delete_artifact",
    }
}

fn ticket_operation(ticket_kind: &str) -> Result<OperationKind, WorkflowTicketPayloadError> {
    let Some(operation) = ticket_kind.strip_prefix("synthetic.workflow.operation.") else {
        return Err(WorkflowTicketPayloadError::new(format!(
            "workflow ticket kind `{ticket_kind}` must start with synthetic.workflow.operation."
        )));
    };
    parse_operation_name(operation)
}

fn parse_operation_name(operation: &str) -> Result<OperationKind, WorkflowTicketPayloadError> {
    serde_json::from_value(Value::String(operation.to_owned())).map_err(|e| {
        WorkflowTicketPayloadError::new(format!("unknown workflow operation `{operation}`: {e}"))
    })
}

fn operation_mismatch(
    source: &str,
    source_operation: OperationKind,
    payload_operation: OperationKind,
) -> WorkflowTicketPayloadError {
    WorkflowTicketPayloadError::new(format!(
        "operation mismatch: {source} `{}` does not match payload operation `{}`",
        operation_name(source_operation),
        operation_name(payload_operation)
    ))
}
