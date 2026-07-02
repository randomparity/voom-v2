use super::*;

#[test]
fn planning_diagnostic_serializes_stable_code() {
    let diagnostic = PlanningDiagnostic::error(
        PlanningDiagnosticCode::UnsupportedOperationForSprint5,
        "track planning is outside Sprint 5",
    )
    .with_phase("normalize")
    .with_operation_kind("keep_tracks");

    let json = serde_json::to_value(&diagnostic).unwrap();
    assert_eq!(json["severity"], "error");
    assert_eq!(json["code"], "unsupported_operation_for_sprint5");
    assert_eq!(json["phase_name"], "normalize");
    assert_eq!(json["operation_kind"], "keep_tracks");
}

#[test]
fn untagged_language_diagnostic_is_a_stable_warning() {
    let diagnostic = PlanningDiagnostic::warning(
        PlanningDiagnosticCode::UntaggedTrackLanguageDefaulted,
        "an untagged audio track was treated as und",
    );

    let json = serde_json::to_value(&diagnostic).unwrap();
    assert_eq!(json["severity"], "warning");
    assert_eq!(json["code"], "untagged_track_language_defaulted");
    assert_eq!(
        PlanningDiagnosticCode::UntaggedTrackLanguageDefaulted.as_str(),
        "untagged_track_language_defaulted"
    );
}
