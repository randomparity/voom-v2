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

#[test]
fn durable_diagnostic_structs_reject_unknown_fields() {
    let diagnostic = serde_json::json!({
        "code": "duplicate_phase_name",
        "severity": "error",
        "stage": "validate",
        "span": {"start": 10, "end": 15},
        "location": {"line": 2, "column": 5},
        "message": "duplicate phase name",
        "suggestion": null,
        "related": [],
        "future_diagnostic": true
    });
    let diagnostic_error = serde_json::from_value::<PolicyDiagnostic>(diagnostic).unwrap_err();
    assert!(
        diagnostic_error
            .to_string()
            .contains("unknown field `future_diagnostic`")
    );

    let related = serde_json::json!({
        "span": {"start": 10, "end": 15},
        "location": {"line": 2, "column": 5},
        "message": "first declared here",
        "future_related": true
    });
    let related_error = serde_json::from_value::<RelatedSpan>(related).unwrap_err();
    assert!(
        related_error
            .to_string()
            .contains("unknown field `future_related`")
    );
}
