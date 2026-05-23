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
