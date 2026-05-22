use crate::{CompiledCondition, CompiledOperation, TrackFilter};

use super::*;

#[test]
fn compile_policy_returns_validation_error_diagnostics() {
    let err =
        compile_policy("policy \"p\" { phase a { transcode video to hevc {} } }").unwrap_err();
    assert_eq!(
        err.code(),
        voom_core::VoomError::PolicyValidationError("x".to_owned()).code()
    );
    assert!(
        err.diagnostics
            .iter()
            .any(|d| d.code == "deferred_execution_operation")
    );
}

#[test]
fn compile_policy_produces_phase_order() {
    let out = compile_policy("policy \"p\" { phase a {} phase b { depends_on: [a] } }").unwrap();
    assert_eq!(out.policy.phase_order, ["a", "b"]);
}

#[test]
fn compile_policy_topologically_sorts_phase_order() {
    let out = compile_policy("policy \"p\" { phase b { depends_on: [a] } phase a {} }").unwrap();

    assert_eq!(out.policy.phase_order, ["a", "b"]);
}

#[test]
fn compile_policy_preserves_boolean_track_filters() {
    let out =
        compile_policy("policy \"p\" { phase a { keep audio where lang in [eng] or commentary } }")
            .unwrap();
    let CompiledOperation::KeepTracks {
        filter: Some(TrackFilter::Or { filters }),
        ..
    } = &out.policy.phases[0].operations[0]
    else {
        unreachable!("expected boolean track filter");
    };

    assert_eq!(filters.len(), 2);
    assert!(matches!(filters[0], TrackFilter::LanguageIn { .. }));
    assert!(matches!(filters[1], TrackFilter::Commentary));
}

#[test]
fn compile_policy_preserves_quoted_title_filter_with_boolean_words() {
    let out = compile_policy(
        "policy \"p\" { phase a { keep subtitle where title contains \"Director or Commentary\" } }",
    )
    .unwrap();

    assert_eq!(
        out.policy.phases[0].operations[0],
        crate::CompiledOperation::KeepTracks {
            target: crate::TrackTarget::Subtitle,
            filter: Some(crate::TrackFilter::TitleContains {
                value: "Director or Commentary".to_owned(),
            }),
        }
    );
}

#[test]
fn compile_policy_preserves_boolean_conditions() {
    let out = compile_policy(
        "policy \"p\" { phase a { when exists audio or exists subtitle { container mkv } } }",
    )
    .unwrap();
    let CompiledOperation::Conditional {
        condition: CompiledCondition::Or { conditions },
        ..
    } = &out.policy.phases[0].operations[0]
    else {
        unreachable!("expected boolean condition");
    };

    assert_eq!(conditions.len(), 2);
    assert!(matches!(conditions[0], CompiledCondition::Exists { .. }));
    assert!(matches!(conditions[1], CompiledCondition::Exists { .. }));
}

#[test]
fn compile_policy_preserves_quoted_tag_value_with_dot_as_string() {
    let out =
        compile_policy("policy \"p\" { phase a { set_tag \"title\" \"Movie.Name\" } }").unwrap();
    let CompiledOperation::SetTag { value, .. } = &out.policy.phases[0].operations[0] else {
        unreachable!("expected set_tag operation");
    };

    assert_eq!(
        *value,
        crate::CompiledValue::String {
            value: "Movie.Name".to_owned()
        }
    );
}
