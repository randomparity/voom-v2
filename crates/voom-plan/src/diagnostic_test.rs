use super::*;

#[test]
fn planning_diagnostic_serializes_stable_code() {
    let diagnostic = PlanningDiagnostic::error(
        PlanningDiagnosticCode::UnsupportedOperation,
        "operation is not supported by the planner",
    )
    .with_phase("normalize")
    .with_operation_kind("keep_tracks");

    let json = serde_json::to_value(&diagnostic).unwrap();
    assert_eq!(json["severity"], "error");
    assert_eq!(json["code"], "unsupported_operation");
    assert_eq!(json["message"], "operation is not supported by the planner");
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

#[test]
fn compatibility_diagnostic_codes_are_stable() {
    for (code, expected) in [
        (
            PlanningDiagnosticCode::AudioCompatibilitySourceMissing,
            "audio_compatibility_source_missing",
        ),
        (
            PlanningDiagnosticCode::UnsupportedAudioCompatibilityLayout,
            "unsupported_audio_compatibility_layout",
        ),
        (
            PlanningDiagnosticCode::InvalidAudioCompatibilityAssociation,
            "invalid_audio_compatibility_association",
        ),
    ] {
        assert_eq!(code.as_str(), expected);
        assert_eq!(serde_json::to_value(code).unwrap(), expected);
    }
}
