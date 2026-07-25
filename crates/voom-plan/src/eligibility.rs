use voom_policy::{
    ComparisonOp, CompiledCondition, CompiledOperation, CompiledPolicy, TrackTarget,
};

use crate::{PlanningDiagnostic, PlanningDiagnosticCode};

#[derive(Clone, Copy, PartialEq, Eq)]
enum ConditionPlacement {
    Ordinary,
    RunIf,
}

impl ConditionPlacement {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Ordinary => "condition",
            Self::RunIf => "run_if",
        }
    }
}

/// Validate that compiled stream conditions use only published executable
/// shapes and placements.
#[must_use]
pub fn stream_condition_eligibility_diagnostics(
    policy: &CompiledPolicy,
) -> Vec<PlanningDiagnostic> {
    let mut diagnostics = Vec::new();
    for (phase_index, phase) in policy.phases.iter().enumerate() {
        let phase_path = format!("phase[{phase_index}:\"{}\"]", phase.name);
        if let Some(condition) = &phase.run_if {
            visit_condition(
                condition,
                &format!("{phase_path}.run_if"),
                ConditionPlacement::RunIf,
                &phase.name,
                &mut diagnostics,
            );
        }
        if let Some(condition) = &phase.skip_if {
            visit_condition(
                condition,
                &format!("{phase_path}.skip_if"),
                ConditionPlacement::Ordinary,
                &phase.name,
                &mut diagnostics,
            );
        }
        visit_operations(
            &phase.operations,
            &format!("{phase_path}.operations"),
            &phase.name,
            &mut diagnostics,
        );
    }
    diagnostics
}

/// Return whether a compiled policy contains an `exists` or `count` leaf.
///
/// Callers that need published-only semantics must first require an empty
/// [`stream_condition_eligibility_diagnostics`] result.
#[must_use]
pub fn policy_uses_stream_conditions(policy: &CompiledPolicy) -> bool {
    for phase in &policy.phases {
        if phase
            .run_if
            .as_ref()
            .is_some_and(condition_uses_stream_facts)
            || phase
                .skip_if
                .as_ref()
                .is_some_and(condition_uses_stream_facts)
            || operations_use_stream_facts(&phase.operations)
        {
            return true;
        }
    }
    false
}

fn operations_use_stream_facts(operations: &[CompiledOperation]) -> bool {
    for operation in operations {
        match operation {
            CompiledOperation::Conditional {
                condition,
                operations,
            } => {
                if condition_uses_stream_facts(condition) || operations_use_stream_facts(operations)
                {
                    return true;
                }
            }
            CompiledOperation::Rules { mode: _, rules } => {
                for rule in rules {
                    if rule
                        .condition
                        .as_ref()
                        .is_some_and(condition_uses_stream_facts)
                        || operations_use_stream_facts(&rule.operations)
                    {
                        return true;
                    }
                }
            }
            CompiledOperation::SetContainer { container: _ }
            | CompiledOperation::KeepTracks {
                target: _,
                filter: _,
            }
            | CompiledOperation::RemoveTracks {
                target: _,
                filter: _,
            }
            | CompiledOperation::ReorderTracks {
                targets: _,
                head_filter: _,
            }
            | CompiledOperation::SetDefaults {
                target: _,
                strategy: _,
                filter: _,
            }
            | CompiledOperation::ClearTrackActions { target: _ }
            | CompiledOperation::ClearTags
            | CompiledOperation::SetTag { key: _, value: _ }
            | CompiledOperation::DeleteTag { key: _ }
            | CompiledOperation::TranscodeVideo {
                target_codec: _,
                container: _,
                profile: _,
                resolved_profile: _,
            }
            | CompiledOperation::TranscodeAudio {
                target_codec: _,
                container: _,
                filter: _,
            }
            | CompiledOperation::ExtractAudio {
                target_codec: _,
                container: _,
                filter: _,
            }
            | CompiledOperation::SynthesizeAudio {
                target_codec: _,
                container: _,
                target_channels: _,
                filter: _,
            }
            | CompiledOperation::VerifyArtifact => {}
        }
    }
    false
}

fn condition_uses_stream_facts(condition: &CompiledCondition) -> bool {
    match condition {
        CompiledCondition::Exists {
            target: _,
            filter: _,
        }
        | CompiledCondition::Count {
            target: _,
            op: _,
            value: _,
        } => true,
        CompiledCondition::Not { inner } => condition_uses_stream_facts(inner),
        CompiledCondition::And { conditions } | CompiledCondition::Or { conditions } => {
            conditions.iter().any(condition_uses_stream_facts)
        }
        CompiledCondition::FieldComparison {
            path: _,
            op: _,
            value: _,
        }
        | CompiledCondition::FieldExists { path: _ }
        | CompiledCondition::Predicate { name: _ } => false,
    }
}

fn visit_operations(
    operations: &[CompiledOperation],
    path: &str,
    phase_name: &str,
    diagnostics: &mut Vec<PlanningDiagnostic>,
) {
    for (index, operation) in operations.iter().enumerate() {
        let operation_path = format!("{path}[{index}]");
        match operation {
            CompiledOperation::Conditional {
                condition,
                operations,
            } => {
                visit_condition(
                    condition,
                    &format!("{operation_path}.condition"),
                    ConditionPlacement::Ordinary,
                    phase_name,
                    diagnostics,
                );
                visit_operations(
                    operations,
                    &format!("{operation_path}.operations"),
                    phase_name,
                    diagnostics,
                );
            }
            CompiledOperation::Rules { mode: _, rules } => {
                for (rule_index, rule) in rules.iter().enumerate() {
                    let rule_path = format!("{operation_path}.rules[{rule_index}]");
                    if let Some(condition) = &rule.condition {
                        visit_condition(
                            condition,
                            &format!("{rule_path}.condition"),
                            ConditionPlacement::Ordinary,
                            phase_name,
                            diagnostics,
                        );
                    }
                    visit_operations(
                        &rule.operations,
                        &format!("{rule_path}.operations"),
                        phase_name,
                        diagnostics,
                    );
                }
            }
            CompiledOperation::SetContainer { container: _ }
            | CompiledOperation::KeepTracks {
                target: _,
                filter: _,
            }
            | CompiledOperation::RemoveTracks {
                target: _,
                filter: _,
            }
            | CompiledOperation::ReorderTracks {
                targets: _,
                head_filter: _,
            }
            | CompiledOperation::SetDefaults {
                target: _,
                strategy: _,
                filter: _,
            }
            | CompiledOperation::ClearTrackActions { target: _ }
            | CompiledOperation::ClearTags
            | CompiledOperation::SetTag { key: _, value: _ }
            | CompiledOperation::DeleteTag { key: _ }
            | CompiledOperation::TranscodeVideo {
                target_codec: _,
                container: _,
                profile: _,
                resolved_profile: _,
            }
            | CompiledOperation::TranscodeAudio {
                target_codec: _,
                container: _,
                filter: _,
            }
            | CompiledOperation::ExtractAudio {
                target_codec: _,
                container: _,
                filter: _,
            }
            | CompiledOperation::SynthesizeAudio {
                target_codec: _,
                container: _,
                target_channels: _,
                filter: _,
            }
            | CompiledOperation::VerifyArtifact => {}
        }
    }
}

fn visit_condition(
    condition: &CompiledCondition,
    path: &str,
    placement: ConditionPlacement,
    phase_name: &str,
    diagnostics: &mut Vec<PlanningDiagnostic>,
) {
    match condition {
        CompiledCondition::Exists { .. } | CompiledCondition::Count { .. } => {
            if let Some(diagnostic) = unpublished_diagnostic(condition, path, placement, phase_name)
            {
                diagnostics.push(diagnostic);
            }
        }
        CompiledCondition::Not { inner } => visit_condition(
            inner,
            &format!("{path}.not"),
            placement,
            phase_name,
            diagnostics,
        ),
        CompiledCondition::And { conditions } => {
            visit_boolean_children(conditions, path, "and", placement, phase_name, diagnostics);
        }
        CompiledCondition::Or { conditions } => {
            visit_boolean_children(conditions, path, "or", placement, phase_name, diagnostics);
        }
        CompiledCondition::FieldComparison {
            path: _,
            op: _,
            value: _,
        }
        | CompiledCondition::FieldExists { path: _ }
        | CompiledCondition::Predicate { name: _ } => {}
    }
}

fn visit_boolean_children(
    conditions: &[CompiledCondition],
    path: &str,
    kind: &str,
    placement: ConditionPlacement,
    phase_name: &str,
    diagnostics: &mut Vec<PlanningDiagnostic>,
) {
    for (index, condition) in conditions.iter().enumerate() {
        visit_condition(
            condition,
            &format!("{path}.{kind}[{index}]"),
            placement,
            phase_name,
            diagnostics,
        );
    }
}

fn unpublished_diagnostic(
    condition: &CompiledCondition,
    path: &str,
    placement: ConditionPlacement,
    phase_name: &str,
) -> Option<PlanningDiagnostic> {
    let detail = match condition {
        CompiledCondition::Exists { target, filter } => {
            if placement == ConditionPlacement::Ordinary
                && is_published_target(*target)
                && filter.is_none()
            {
                return None;
            }
            format!(
                "exists target={} filter={} placement={}",
                target_name(*target),
                if filter.is_some() {
                    "present"
                } else {
                    "absent"
                },
                placement.as_str()
            )
        }
        CompiledCondition::Count { target, op, value } => {
            if placement == ConditionPlacement::Ordinary
                && is_published_target(*target)
                && is_numeric_comparison(*op)
            {
                return None;
            }
            format!(
                "count target={} op={} value={value} placement={}",
                target_name(*target),
                comparison_name(*op),
                placement.as_str()
            )
        }
        CompiledCondition::FieldComparison {
            path: _,
            op: _,
            value: _,
        }
        | CompiledCondition::FieldExists { path: _ }
        | CompiledCondition::Predicate { name: _ }
        | CompiledCondition::Not { inner: _ }
        | CompiledCondition::And { conditions: _ }
        | CompiledCondition::Or { conditions: _ } => return None,
    };
    Some(
        PlanningDiagnostic::error(
            PlanningDiagnosticCode::InvalidPlanningRequest,
            format!("unpublished compiled stream condition at {path}: {detail}"),
        )
        .with_phase(phase_name),
    )
}

const fn is_published_target(target: TrackTarget) -> bool {
    match target {
        TrackTarget::Audio | TrackTarget::Subtitle => true,
        TrackTarget::Video | TrackTarget::Attachment => false,
    }
}

const fn is_numeric_comparison(op: ComparisonOp) -> bool {
    match op {
        ComparisonOp::Eq
        | ComparisonOp::Ne
        | ComparisonOp::Lt
        | ComparisonOp::Lte
        | ComparisonOp::Gt
        | ComparisonOp::Gte => true,
        ComparisonOp::Contains | ComparisonOp::Matches => false,
    }
}

const fn target_name(target: TrackTarget) -> &'static str {
    match target {
        TrackTarget::Video => "video",
        TrackTarget::Audio => "audio",
        TrackTarget::Subtitle => "subtitle",
        TrackTarget::Attachment => "attachment",
    }
}

const fn comparison_name(op: ComparisonOp) -> &'static str {
    match op {
        ComparisonOp::Eq => "eq",
        ComparisonOp::Ne => "ne",
        ComparisonOp::Lt => "lt",
        ComparisonOp::Lte => "lte",
        ComparisonOp::Gt => "gt",
        ComparisonOp::Gte => "gte",
        ComparisonOp::Contains => "contains",
        ComparisonOp::Matches => "matches",
    }
}

#[cfg(test)]
#[path = "eligibility_test.rs"]
mod tests;
