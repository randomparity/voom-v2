use crate::{CompiledOperation, TrackFilter};

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
