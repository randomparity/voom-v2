use super::*;

#[test]
fn registry_covers_every_failure_class_once() {
    validate_registry().unwrap();
}

#[test]
fn missing_failure_class_fails_validation() {
    let fixtures = registry()
        .iter()
        .copied()
        .filter(|fixture| fixture.class != voom_core::FailureClass::WorkerTimeout)
        .collect::<Vec<_>>();
    let err = validate_registry_with(&fixtures).unwrap_err();
    assert!(err.to_string().contains("missing"));
}

#[test]
fn duplicate_failure_class_fails_validation() {
    let mut fixtures = registry().to_vec();
    fixtures.push(registry()[0]);
    let err = validate_registry_with(&fixtures).unwrap_err();
    assert!(err.to_string().contains("duplicate"));
}

#[test]
fn every_fixture_matches_failure_class_error_code_and_retry_mapping() {
    for fixture in registry() {
        assert_eq!(fixture.code, fixture.class.into_error_code());
        assert_eq!(fixture.retry, fixture.class.retry_class());
    }
}
