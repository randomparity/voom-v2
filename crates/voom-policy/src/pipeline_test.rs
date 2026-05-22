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
