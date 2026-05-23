use crate::span::SourceSpan;

use super::*;

#[test]
fn diagnostic_serializes_stable_fields() {
    let diagnostic = PolicyDiagnostic::error(
        DiagnosticCode::DuplicatePhaseName,
        DiagnosticStage::Validate,
        SourceSpan::new(10, 15),
        SourceLocation { line: 2, column: 5 },
        "duplicate phase name",
    );

    let json = serde_json::to_value(&diagnostic).unwrap();
    assert_eq!(json["code"], "duplicate_phase_name");
    assert_eq!(json["severity"], "error");
    assert_eq!(json["stage"], "validate");
    assert_eq!(json["span"]["start"], 10);
}
