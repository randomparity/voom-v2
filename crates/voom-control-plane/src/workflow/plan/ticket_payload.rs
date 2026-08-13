use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use voom_core::{
    ArtifactAccessDeclaration, FileLocationId, NormalizedTicketOperation, OperationKind,
    StorageRootId, TicketOperation,
};

use crate::workflow::execution::timing::EffectiveTiming;
use crate::workflow::plan::access_declaration::{TicketStorageSource, declaration_for};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowTicketPayload {
    pub workflow_id: String,
    pub plan_id: String,
    pub node_id: String,
    pub branch_id: String,
    pub operation: OperationKind,
    pub rendered_payload: Value,
    pub timing: EffectiveTiming,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_file: Option<Value>,
    /// What this ticket intends to open. Required for a byte-touching operation
    /// and forbidden for every other, enforced identically on encode and decode.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub declared_artifact_access: Option<ArtifactAccessDeclaration>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowTicketPayloadError {
    detail: String,
}

impl WorkflowTicketPayload {
    /// A payload for tests, valid for its operation.
    ///
    /// A byte-touching operation gets the fixture source stamped into
    /// `rendered_payload` and the matching canonical declaration, so callers
    /// testing something other than the access gate do not have to construct one.
    #[must_use]
    pub fn new_for_test(
        workflow_id: &str,
        plan_id: &str,
        node_id: &str,
        branch_id: &str,
        operation: OperationKind,
        rendered_payload: Value,
    ) -> Self {
        let timing = EffectiveTiming::for_test(25, 10);
        let mut rendered_payload = rendered_payload;
        let mut declared_artifact_access = None;
        if operation.is_byte_touching()
            && let Some(object) = rendered_payload.as_object_mut()
        {
            // Respect a source the caller already put in the payload; several
            // fixtures pass real seeded ids and would break if these overwrote
            // them. Otherwise supply a placeholder so the payload is valid.
            let root = object
                .get("source_storage_root_id")
                .and_then(Value::as_u64)
                .unwrap_or(3);
            let location = object
                .get("source_location_id")
                .and_then(Value::as_u64)
                .unwrap_or(7);
            object.insert("source_storage_root_id".to_owned(), json!(root));
            object.insert("source_location_id".to_owned(), json!(location));
            let source = TicketStorageSource::Location {
                storage_root_id: StorageRootId(root),
                file_location_id: FileLocationId(location),
            };
            declared_artifact_access = declaration_for(operation, Some(&source)).ok().flatten();
        }
        Self {
            workflow_id: workflow_id.to_owned(),
            plan_id: plan_id.to_owned(),
            node_id: node_id.to_owned(),
            branch_id: branch_id.to_owned(),
            operation,
            rendered_payload,
            timing,
            source_file: None,
            declared_artifact_access,
        }
    }

    pub fn to_ticket_payload(&self) -> Result<Value, WorkflowTicketPayloadError> {
        self.validate_artifact_access()?;
        let mut value = serde_json::to_value(self).map_err(|e| {
            WorkflowTicketPayloadError::new(format!("workflow ticket payload encode: {e}"))
        })?;
        let operation = self.operation.as_str();
        let Some(rendered_payload) = value
            .get_mut("rendered_payload")
            .and_then(serde_json::Value::as_object_mut)
        else {
            return Err(WorkflowTicketPayloadError::new(
                "rendered_payload must be a JSON object",
            ));
        };
        match rendered_payload.get("operation").and_then(Value::as_str) {
            Some(rendered_operation) if rendered_operation == operation => {}
            Some(rendered_operation) => {
                let rendered_operation = parse_operation_name(rendered_operation)?;
                return Err(operation_mismatch(
                    "rendered_payload.operation",
                    rendered_operation,
                    self.operation,
                ));
            }
            None => {
                rendered_payload.insert("operation".to_owned(), json!(operation));
            }
        }
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
        parsed.validate_artifact_access()?;

        Ok(parsed)
    }

    /// The one contract binding writers and readers of a ticket's access claim.
    ///
    /// The persisted writer is untrusted, so this is an equality check against
    /// [`declaration_for`], not a shape check: a shape check would bind targets
    /// alone, letting a corrupted row give a `probe_file` ticket `read, write,
    /// delete` on its source and pass every canonical-form rule.
    ///
    /// The root and location are read from `rendered_payload`, never back out of
    /// the declaration being validated — taking them from the declaration's own
    /// entries would make the check circular, and a corrupted row could then name
    /// any root and pass.
    fn validate_artifact_access(&self) -> Result<(), WorkflowTicketPayloadError> {
        let operation = self.operation.as_str();
        if !self.operation.is_byte_touching() {
            return if self.declared_artifact_access.is_some() {
                Err(WorkflowTicketPayloadError::new(format!(
                    "operation {operation} is not byte-touching and must not declare artifact access"
                )))
            } else {
                Ok(())
            };
        }
        let Some(declared) = self.declared_artifact_access.as_ref() else {
            return Err(WorkflowTicketPayloadError::new(format!(
                "operation {operation} is byte-touching and requires declared_artifact_access"
            )));
        };

        let root = self
            .rendered_source_id("source_storage_root_id")
            .filter(|id| *id != 0)
            .ok_or_else(|| {
                WorkflowTicketPayloadError::new(format!(
                    "byte-touching payload for {operation} requires a non-zero \
                     rendered_payload.source_storage_root_id"
                ))
            })?;
        // Absent means the ticket addresses its root. Present-but-unreadable is
        // rejected rather than read as absent: a string, float, or negative id would
        // otherwise fall through to the whole-root shape, which is a legitimate
        // declaration for the operation and so passes every remaining rule.
        let source = match self.rendered_payload.get("source_location_id") {
            None => TicketStorageSource::Root {
                storage_root_id: StorageRootId(root),
            },
            Some(raw) => {
                let location = raw.as_u64().filter(|id| *id != 0).ok_or_else(|| {
                    WorkflowTicketPayloadError::new(
                        "rendered_payload.source_location_id must be a non-zero integer",
                    )
                })?;
                TicketStorageSource::Location {
                    storage_root_id: StorageRootId(root),
                    file_location_id: FileLocationId(location),
                }
            }
        };

        let expected = declaration_for(self.operation, Some(&source)).map_err(|error| {
            WorkflowTicketPayloadError::new(format!(
                "canonical declaration for {operation}: {error}"
            ))
        })?;
        if expected.as_ref() == Some(declared) {
            Ok(())
        } else {
            Err(WorkflowTicketPayloadError::new(format!(
                "declared_artifact_access does not match the canonical declaration for {operation}"
            )))
        }
    }

    fn rendered_source_id(&self, field: &str) -> Option<u64> {
        self.rendered_payload.get(field)?.as_u64()
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

/// The operation a workflow ticket kind names.
///
/// Only the namespaced encoding of a known operation is accepted. A bare
/// `probe_file` is rejected even though it normalizes to a known operation:
/// admitting it would give the kind field a second accepted encoding, which is
/// exactly what this change forbids for the declaration body.
pub(crate) fn ticket_operation(
    ticket_kind: &str,
) -> Result<OperationKind, WorkflowTicketPayloadError> {
    let token = TicketOperation::new(ticket_kind).map_err(|error| {
        WorkflowTicketPayloadError::new(format!("workflow ticket kind `{ticket_kind}`: {error}"))
    })?;
    match token.normalize() {
        NormalizedTicketOperation::Known {
            kind,
            namespaced: true,
        } => Ok(kind),
        NormalizedTicketOperation::Known {
            namespaced: false, ..
        }
        | NormalizedTicketOperation::CustomLocal(_)
        | NormalizedTicketOperation::UnknownNamespaced(_) => {
            Err(WorkflowTicketPayloadError::new(format!(
                "workflow ticket kind `{ticket_kind}` must be a known operation inside \
                 synthetic.workflow.operation."
            )))
        }
    }
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
        source_operation.as_str(),
        payload_operation.as_str()
    ))
}

#[cfg(test)]
#[path = "ticket_payload_test.rs"]
mod tests;
