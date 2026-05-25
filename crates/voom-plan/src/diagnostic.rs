#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanningDiagnosticSeverity {
    Error,
    Warning,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanningDiagnosticCode {
    MissingPolicyInputTarget,
    UnsupportedOperationForSprint5,
    InsufficientSnapshotFacts,
    UnsupportedMediaShape,
    AmbiguousTargetSelection,
    EmptyPolicyPhases,
    EmptyInputSet,
    InvalidPlanningRequest,
    DeterministicSerializationFailure,
}

impl PlanningDiagnosticCode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::MissingPolicyInputTarget => "missing_policy_input_target",
            Self::UnsupportedOperationForSprint5 => "unsupported_operation_for_sprint5",
            Self::InsufficientSnapshotFacts => "insufficient_snapshot_facts",
            Self::UnsupportedMediaShape => "unsupported_media_shape",
            Self::AmbiguousTargetSelection => "ambiguous_target_selection",
            Self::EmptyPolicyPhases => "empty_policy_phases",
            Self::EmptyInputSet => "empty_input_set",
            Self::InvalidPlanningRequest => "invalid_planning_request",
            Self::DeterministicSerializationFailure => "deterministic_serialization_failure",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PlanningDiagnostic {
    pub severity: PlanningDiagnosticSeverity,
    pub code: PlanningDiagnosticCode,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<voom_policy::TargetRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phase_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub operation_kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suggestion: Option<String>,
}

impl PlanningDiagnostic {
    #[must_use]
    pub fn error(code: PlanningDiagnosticCode, message: impl Into<String>) -> Self {
        Self {
            severity: PlanningDiagnosticSeverity::Error,
            code,
            message: message.into(),
            target: None,
            phase_name: None,
            operation_kind: None,
            suggestion: None,
        }
    }

    #[must_use]
    pub fn warning(code: PlanningDiagnosticCode, message: impl Into<String>) -> Self {
        Self {
            severity: PlanningDiagnosticSeverity::Warning,
            code,
            message: message.into(),
            target: None,
            phase_name: None,
            operation_kind: None,
            suggestion: None,
        }
    }

    #[must_use]
    pub fn with_target(mut self, target: voom_policy::TargetRef) -> Self {
        self.target = Some(target);
        self
    }

    #[must_use]
    pub fn with_phase(mut self, phase_name: impl Into<String>) -> Self {
        self.phase_name = Some(phase_name.into());
        self
    }

    #[must_use]
    pub fn with_operation_kind(mut self, operation_kind: impl Into<String>) -> Self {
        self.operation_kind = Some(operation_kind.into());
        self
    }
}

#[cfg(test)]
#[path = "diagnostic_test.rs"]
mod tests;
