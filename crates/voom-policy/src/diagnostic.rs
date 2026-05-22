pub use crate::span::SourceLocation;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DiagnosticCode {
    UnexpectedToken,
    SourceSizeExceeded,
    UnknownTopLevelBlock,
    UnknownPhaseStatementOrOperation,
    DeferredPhaseInheritance,
    DuplicatePhaseName,
    UnknownPhaseReference,
    SelfDependency,
    DependencyCycle,
    InvalidRunIfTrigger,
    InvalidOnErrorValue,
    UnsupportedContainer,
    InvalidTrackTarget,
    InvalidDefaultStrategy,
    InvalidLanguageCode,
    InvalidCoreFieldPath,
    InvalidRuleMatchMode,
    UnknownExtensionNamespace,
    TagOrderingError,
    AmbiguousTagOperationConflict,
    DeferredComposition,
    DeferredExecutionOperation,
}

impl DiagnosticCode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::UnexpectedToken => "unexpected_token",
            Self::SourceSizeExceeded => "source_size_exceeded",
            Self::UnknownTopLevelBlock => "unknown_top_level_block",
            Self::UnknownPhaseStatementOrOperation => "unknown_phase_statement_or_operation",
            Self::DeferredPhaseInheritance => "deferred_phase_inheritance",
            Self::DuplicatePhaseName => "duplicate_phase_name",
            Self::UnknownPhaseReference => "unknown_phase_reference",
            Self::SelfDependency => "self_dependency",
            Self::DependencyCycle => "dependency_cycle",
            Self::InvalidRunIfTrigger => "invalid_run_if_trigger",
            Self::InvalidOnErrorValue => "invalid_on_error_value",
            Self::UnsupportedContainer => "unsupported_container",
            Self::InvalidTrackTarget => "invalid_track_target",
            Self::InvalidDefaultStrategy => "invalid_default_strategy",
            Self::InvalidLanguageCode => "invalid_language_code",
            Self::InvalidCoreFieldPath => "invalid_core_field_path",
            Self::InvalidRuleMatchMode => "invalid_rule_match_mode",
            Self::UnknownExtensionNamespace => "unknown_extension_namespace",
            Self::TagOrderingError => "tag_ordering_error",
            Self::AmbiguousTagOperationConflict => "ambiguous_tag_operation_conflict",
            Self::DeferredComposition => "deferred_composition",
            Self::DeferredExecutionOperation => "deferred_execution_operation",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticSeverity {
    Error,
    Warning,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticStage {
    Parse,
    Validate,
    Compile,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RelatedSpan {
    pub span: crate::span::SourceSpan,
    pub location: crate::span::SourceLocation,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PolicyDiagnostic {
    pub code: String,
    pub severity: DiagnosticSeverity,
    pub stage: DiagnosticStage,
    pub span: crate::span::SourceSpan,
    pub location: crate::span::SourceLocation,
    pub message: String,
    pub suggestion: Option<String>,
    pub related: Vec<RelatedSpan>,
}

impl PolicyDiagnostic {
    #[must_use]
    pub fn error(
        code: DiagnosticCode,
        stage: DiagnosticStage,
        span: crate::span::SourceSpan,
        location: crate::span::SourceLocation,
        message: impl Into<String>,
    ) -> Self {
        Self {
            code: code.as_str().to_owned(),
            severity: DiagnosticSeverity::Error,
            stage,
            span,
            location,
            message: message.into(),
            suggestion: None,
            related: Vec::new(),
        }
    }

    #[must_use]
    pub fn warning(
        code: DiagnosticCode,
        stage: DiagnosticStage,
        span: crate::span::SourceSpan,
        location: crate::span::SourceLocation,
        message: impl Into<String>,
    ) -> Self {
        Self {
            code: code.as_str().to_owned(),
            severity: DiagnosticSeverity::Warning,
            stage,
            span,
            location,
            message: message.into(),
            suggestion: None,
            related: Vec::new(),
        }
    }
}

#[cfg(test)]
#[path = "diagnostic_test.rs"]
mod tests;
