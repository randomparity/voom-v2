use voom_policy::compiled::{
    CompiledAndCondition, CompiledClearTagsOperation, CompiledClearTrackActionsOperation,
    CompiledConditionalOperation, CompiledCountCondition, CompiledDeleteTagOperation,
    CompiledExistsCondition, CompiledExtractAudioOperation, CompiledFieldComparisonCondition,
    CompiledFieldExistsCondition, CompiledKeepTracksOperation, CompiledNotCondition,
    CompiledOrCondition, CompiledPredicateCondition, CompiledRemoveTracksOperation,
    CompiledReorderTracksOperation, CompiledRulesOperation, CompiledSetContainerOperation,
    CompiledSetDefaultsOperation, CompiledSetTagOperation, CompiledSynthesizeAudioOperation,
    CompiledTranscodeAudioOperation, CompiledTranscodeVideoOperation,
    CompiledVerifyArtifactOperation,
};
use voom_policy::{
    ComparisonOp, CompiledCondition, CompiledOperation, CompiledPolicy, TrackTarget,
};

use crate::{PlanningDiagnostic, PlanningDiagnosticCode};

/// Validate that compiled stream conditions use only published executable
/// shapes and placements.
#[must_use]
pub fn stream_condition_eligibility_diagnostics(
    policy: &CompiledPolicy,
) -> Vec<PlanningDiagnostic> {
    let mut diagnostics = Vec::new();
    for (phase_index, phase) in policy.phases.iter().enumerate() {
        let phase_path = format!("phase[{phase_index}:\"{}\"]", phase.name);
        if let Some(condition) = &phase.skip_if {
            visit_condition(
                condition,
                &format!("{phase_path}.skip_if"),
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
            CompiledOperation::Conditional(CompiledConditionalOperation {
                condition,
                operations,
            }) => {
                if condition_uses_stream_facts(condition) || operations_use_stream_facts(operations)
                {
                    return true;
                }
            }
            CompiledOperation::Rules(CompiledRulesOperation { mode: _, rules }) => {
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
            CompiledOperation::SetContainer(CompiledSetContainerOperation { container: _ })
            | CompiledOperation::KeepTracks(CompiledKeepTracksOperation {
                target: _,
                filter: _,
            })
            | CompiledOperation::RemoveTracks(CompiledRemoveTracksOperation {
                target: _,
                filter: _,
            })
            | CompiledOperation::ReorderTracks(CompiledReorderTracksOperation {
                targets: _,
                head_filter: _,
            })
            | CompiledOperation::SetDefaults(CompiledSetDefaultsOperation {
                target: _,
                strategy: _,
                filter: _,
            })
            | CompiledOperation::ClearTrackActions(CompiledClearTrackActionsOperation {
                target: _,
            })
            | CompiledOperation::ClearTags(CompiledClearTagsOperation {})
            | CompiledOperation::SetTag(CompiledSetTagOperation { key: _, value: _ })
            | CompiledOperation::DeleteTag(CompiledDeleteTagOperation { key: _ })
            | CompiledOperation::TranscodeVideo(CompiledTranscodeVideoOperation {
                target_codec: _,
                container: _,
                profile: _,
                resolved_profile: _,
            })
            | CompiledOperation::TranscodeAudio(CompiledTranscodeAudioOperation {
                target_codec: _,
                container: _,
                filter: _,
            })
            | CompiledOperation::ExtractAudio(CompiledExtractAudioOperation {
                target_codec: _,
                container: _,
                filter: _,
            })
            | CompiledOperation::SynthesizeAudio(CompiledSynthesizeAudioOperation {
                target_codec: _,
                container: _,
                target_channels: _,
                filter: _,
            })
            | CompiledOperation::VerifyArtifact(CompiledVerifyArtifactOperation {}) => {}
        }
    }
    false
}

fn condition_uses_stream_facts(condition: &CompiledCondition) -> bool {
    match condition {
        CompiledCondition::Exists(CompiledExistsCondition {
            target: _,
            filter: _,
        })
        | CompiledCondition::Count(CompiledCountCondition {
            target: _,
            op: _,
            value: _,
        }) => true,
        CompiledCondition::Not(CompiledNotCondition { inner }) => {
            condition_uses_stream_facts(inner)
        }
        CompiledCondition::And(CompiledAndCondition { conditions })
        | CompiledCondition::Or(CompiledOrCondition { conditions }) => {
            conditions.iter().any(condition_uses_stream_facts)
        }
        CompiledCondition::FieldComparison(CompiledFieldComparisonCondition {
            path: _,
            op: _,
            value: _,
        })
        | CompiledCondition::FieldExists(CompiledFieldExistsCondition { path: _ })
        | CompiledCondition::Predicate(CompiledPredicateCondition { name: _ }) => false,
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
            CompiledOperation::Conditional(CompiledConditionalOperation {
                condition,
                operations,
            }) => {
                visit_condition(
                    condition,
                    &format!("{operation_path}.condition"),
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
            CompiledOperation::Rules(CompiledRulesOperation { mode: _, rules }) => {
                for (rule_index, rule) in rules.iter().enumerate() {
                    let rule_path = format!("{operation_path}.rules[{rule_index}]");
                    if let Some(condition) = &rule.condition {
                        visit_condition(
                            condition,
                            &format!("{rule_path}.condition"),
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
            CompiledOperation::SetContainer(CompiledSetContainerOperation { container: _ })
            | CompiledOperation::KeepTracks(CompiledKeepTracksOperation {
                target: _,
                filter: _,
            })
            | CompiledOperation::RemoveTracks(CompiledRemoveTracksOperation {
                target: _,
                filter: _,
            })
            | CompiledOperation::ReorderTracks(CompiledReorderTracksOperation {
                targets: _,
                head_filter: _,
            })
            | CompiledOperation::SetDefaults(CompiledSetDefaultsOperation {
                target: _,
                strategy: _,
                filter: _,
            })
            | CompiledOperation::ClearTrackActions(CompiledClearTrackActionsOperation {
                target: _,
            })
            | CompiledOperation::ClearTags(CompiledClearTagsOperation {})
            | CompiledOperation::SetTag(CompiledSetTagOperation { key: _, value: _ })
            | CompiledOperation::DeleteTag(CompiledDeleteTagOperation { key: _ })
            | CompiledOperation::TranscodeVideo(CompiledTranscodeVideoOperation {
                target_codec: _,
                container: _,
                profile: _,
                resolved_profile: _,
            })
            | CompiledOperation::TranscodeAudio(CompiledTranscodeAudioOperation {
                target_codec: _,
                container: _,
                filter: _,
            })
            | CompiledOperation::ExtractAudio(CompiledExtractAudioOperation {
                target_codec: _,
                container: _,
                filter: _,
            })
            | CompiledOperation::SynthesizeAudio(CompiledSynthesizeAudioOperation {
                target_codec: _,
                container: _,
                target_channels: _,
                filter: _,
            })
            | CompiledOperation::VerifyArtifact(CompiledVerifyArtifactOperation {}) => {}
        }
    }
}

fn visit_condition(
    condition: &CompiledCondition,
    path: &str,
    phase_name: &str,
    diagnostics: &mut Vec<PlanningDiagnostic>,
) {
    match condition {
        CompiledCondition::Exists(CompiledExistsCondition { .. })
        | CompiledCondition::Count(CompiledCountCondition { .. }) => {
            if let Some(diagnostic) = unpublished_diagnostic(condition, path, phase_name) {
                diagnostics.push(diagnostic);
            }
        }
        CompiledCondition::Not(CompiledNotCondition { inner }) => {
            visit_condition(inner, &format!("{path}.not"), phase_name, diagnostics);
        }
        CompiledCondition::And(CompiledAndCondition { conditions }) => {
            visit_boolean_children(conditions, path, "and", phase_name, diagnostics);
        }
        CompiledCondition::Or(CompiledOrCondition { conditions }) => {
            visit_boolean_children(conditions, path, "or", phase_name, diagnostics);
        }
        CompiledCondition::FieldComparison(CompiledFieldComparisonCondition {
            path: _,
            op: _,
            value: _,
        })
        | CompiledCondition::FieldExists(CompiledFieldExistsCondition { path: _ })
        | CompiledCondition::Predicate(CompiledPredicateCondition { name: _ }) => {}
    }
}

fn visit_boolean_children(
    conditions: &[CompiledCondition],
    path: &str,
    kind: &str,
    phase_name: &str,
    diagnostics: &mut Vec<PlanningDiagnostic>,
) {
    for (index, condition) in conditions.iter().enumerate() {
        visit_condition(
            condition,
            &format!("{path}.{kind}[{index}]"),
            phase_name,
            diagnostics,
        );
    }
}

fn unpublished_diagnostic(
    condition: &CompiledCondition,
    path: &str,
    phase_name: &str,
) -> Option<PlanningDiagnostic> {
    let detail = match condition {
        CompiledCondition::Exists(CompiledExistsCondition { target, filter }) => {
            if is_published_target(*target) && filter.is_none() {
                return None;
            }
            format!(
                "exists target={} filter={} placement=condition",
                target_name(*target),
                if filter.is_some() {
                    "present"
                } else {
                    "absent"
                }
            )
        }
        CompiledCondition::Count(CompiledCountCondition { target, op, value }) => {
            if is_published_target(*target) && is_numeric_comparison(*op) {
                return None;
            }
            format!(
                "count target={} op={} value={value} placement=condition",
                target_name(*target),
                comparison_name(*op)
            )
        }
        CompiledCondition::FieldComparison(CompiledFieldComparisonCondition {
            path: _,
            op: _,
            value: _,
        })
        | CompiledCondition::FieldExists(CompiledFieldExistsCondition { path: _ })
        | CompiledCondition::Predicate(CompiledPredicateCondition { name: _ })
        | CompiledCondition::Not(CompiledNotCondition { inner: _ })
        | CompiledCondition::And(CompiledAndCondition { conditions: _ })
        | CompiledCondition::Or(CompiledOrCondition { conditions: _ }) => {
            return None;
        }
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
