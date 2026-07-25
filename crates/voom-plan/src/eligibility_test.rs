use std::collections::BTreeMap;

use voom_policy::{
    ComparisonOp, CompiledCondition, CompiledOperation, CompiledPhase, CompiledPolicy,
    CompiledRule, RuleMatchMode, TrackFilter, TrackTarget,
};

use super::*;
use crate::PlanningDiagnosticCode;

fn policy(phases: Vec<CompiledPhase>) -> CompiledPolicy {
    CompiledPolicy {
        policy_name: "stream conditions".to_owned(),
        slug: "stream-conditions".to_owned(),
        source_hash: "source-hash".to_owned(),
        schema_version: 2,
        metadata: BTreeMap::new(),
        config: voom_policy::CompiledConfig::default(),
        phase_order: phases.iter().map(|phase| phase.name.clone()).collect(),
        phases,
        warnings: Vec::new(),
        provenance: voom_policy::PolicyProvenance::default(),
    }
}

fn phase(name: &str, operations: Vec<CompiledOperation>) -> CompiledPhase {
    CompiledPhase {
        name: name.to_owned(),
        depends_on: Vec::new(),
        run_if: None,
        skip_if: None,
        on_error: None,
        operations,
    }
}

fn exists(target: TrackTarget) -> CompiledCondition {
    CompiledCondition::Exists {
        target,
        filter: None,
    }
}

fn count(target: TrackTarget, op: ComparisonOp) -> CompiledCondition {
    CompiledCondition::Count {
        target,
        op,
        value: 1,
    }
}

#[test]
fn published_stream_conditions_are_eligible_on_ordinary_surfaces() {
    let policy = policy(vec![phase(
        "normalize",
        vec![
            CompiledOperation::Conditional {
                condition: exists(TrackTarget::Audio),
                operations: Vec::new(),
            },
            CompiledOperation::Rules {
                mode: RuleMatchMode::All,
                rules: vec![CompiledRule {
                    name: "subtitles".to_owned(),
                    condition: Some(count(TrackTarget::Subtitle, ComparisonOp::Gte)),
                    operations: Vec::new(),
                }],
            },
        ],
    )]);

    assert!(stream_condition_eligibility_diagnostics(&policy).is_empty());
}

#[test]
fn eligibility_collects_unpublished_leaves_in_structural_order() {
    let mut normalize = phase(
        "normalize",
        vec![
            CompiledOperation::Conditional {
                condition: CompiledCondition::And {
                    conditions: vec![
                        exists(TrackTarget::Audio),
                        count(TrackTarget::Audio, ComparisonOp::Contains),
                    ],
                },
                operations: vec![CompiledOperation::Conditional {
                    condition: CompiledCondition::Exists {
                        target: TrackTarget::Audio,
                        filter: Some(TrackFilter::Commentary),
                    },
                    operations: Vec::new(),
                }],
            },
            CompiledOperation::Rules {
                mode: RuleMatchMode::First,
                rules: vec![CompiledRule {
                    name: "nested".to_owned(),
                    condition: Some(count(TrackTarget::Subtitle, ComparisonOp::Eq)),
                    operations: vec![CompiledOperation::Rules {
                        mode: RuleMatchMode::All,
                        rules: vec![CompiledRule {
                            name: "attachments".to_owned(),
                            condition: Some(exists(TrackTarget::Attachment)),
                            operations: Vec::new(),
                        }],
                    }],
                }],
            },
        ],
    );
    normalize.run_if = Some(exists(TrackTarget::Audio));
    normalize.skip_if = Some(exists(TrackTarget::Video));
    let policy = policy(vec![normalize]);

    let diagnostics = stream_condition_eligibility_diagnostics(&policy);
    let messages = diagnostics
        .iter()
        .map(|diagnostic| diagnostic.message.as_str())
        .collect::<Vec<_>>();

    assert_eq!(diagnostics.len(), 5);
    assert!(
        diagnostics
            .iter()
            .all(|diagnostic| diagnostic.code == PlanningDiagnosticCode::InvalidPlanningRequest)
    );
    assert!(messages[0].contains("phase[0:\"normalize\"].run_if"));
    assert!(messages[1].contains("phase[0:\"normalize\"].skip_if"));
    assert!(messages[2].contains("operations[0].condition.and[1]"));
    assert!(messages[3].contains("operations[0].operations[0].condition"));
    assert!(messages[4].contains("operations[1].rules[0].operations[0].rules[0].condition"));
    assert!(
        messages
            .iter()
            .all(|message| message.starts_with("unpublished compiled stream condition at"))
    );
}
