use super::*;

#[test]
fn parses_every_published_leaf() {
    let cases = [
        "language == \"eng\"",
        "language in [\"eng\", \"und\"]",
        "codec in [\"aac\", \"eac3\"]",
        "channels == 2",
        "channels != 2",
        "channels < 2",
        "channels <= 2",
        "channels > 2",
        "channels >= 2",
        "commentary",
        "forced",
        "default",
        "font",
        "title contains \"Director Cut\"",
    ];

    for source in cases {
        assert!(parse_track_filter(source).is_ok(), "rejected `{source}`");
    }
}

#[test]
fn rejects_unpublished_and_malformed_leaves() {
    let cases = [
        "lang in [\"eng\"]",
        "language == eng",
        "language in [eng]",
        "codec in [aac]",
        "title matches \"Signs\"",
        "channels = 2",
        "channels contains 2",
        "channels matches 2",
        "channels >= 18446744073709551616",
        "channels >= ２",
        "language in []",
        "language in [\"eng\",]",
        "language in [\"eng\",, \"und\"]",
        "language in [\"eng\" \"und\"]",
        "language in [\"eng\"] trailing",
        "commentary trailing",
        "title contains \"\"",
        "title contains \"unterminated",
        "title contains \"trailing\" input",
    ];

    for source in cases {
        assert!(parse_track_filter(source).is_err(), "accepted `{source}`");
    }
}

#[test]
fn quoted_token_acceptance_matches_the_published_ascii_alphabet() {
    for byte in 0u8..=127 {
        let ch = char::from(byte);
        let language = format!("language == \"a{ch}b\"");
        let codec = format!("codec in [\"a{ch}b\"]");
        let allowed =
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'-');

        assert_eq!(
            parse_track_filter(&language).is_ok(),
            allowed,
            "language byte {byte}"
        );
        assert_eq!(
            parse_track_filter(&codec).is_ok(),
            allowed,
            "codec byte {byte}"
        );
    }

    for source in [
        "language == \"\"",
        "codec in [\"\"]",
        "language == \"é\"",
        "codec in [\"日本語\"]",
    ] {
        assert!(parse_track_filter(source).is_err(), "accepted `{source}`");
    }
}

#[test]
fn preserves_boolean_precedence_and_nary_source_order() {
    let and = parse_track_filter("commentary and forced and default").unwrap();
    assert_eq!(
        and,
        TrackFilter::And {
            filters: vec![
                TrackFilter::Commentary,
                TrackFilter::Forced,
                TrackFilter::Default,
            ],
        }
    );

    let parsed = parse_track_filter("commentary or forced or default and not font").unwrap();
    let TrackFilter::Or { filters } = parsed else {
        unreachable!("expected top-level or");
    };

    assert_eq!(filters.len(), 3);
    assert_eq!(filters[0], TrackFilter::Commentary);
    assert_eq!(filters[1], TrackFilter::Forced);
    assert_eq!(
        filters[2],
        TrackFilter::And {
            filters: vec![
                TrackFilter::Default,
                TrackFilter::Not {
                    inner: Box::new(TrackFilter::Font),
                },
            ],
        }
    );
}

#[test]
fn rejects_missing_boolean_operands_and_unbalanced_groups() {
    for source in [
        "commentary and",
        "and commentary",
        "commentary and and forced",
        "commentary or or forced",
        "()",
        "(commentary",
        "commentary)",
        "(commentary) trailing",
    ] {
        assert!(parse_track_filter(source).is_err(), "accepted `{source}`");
    }
}

#[test]
fn depth_budget_is_path_local() {
    assert!(parse_track_filter(&nested_not(64)).is_ok());
    assert!(parse_track_filter(&nested_not(65)).is_err());
    assert!(parse_track_filter(&nested_group(64)).is_ok());
    assert!(parse_track_filter(&nested_group(65)).is_err());

    let flat_and = std::iter::repeat_n("commentary", 80)
        .collect::<Vec<_>>()
        .join(" and ");
    let flat_or = std::iter::repeat_n("forced", 80)
        .collect::<Vec<_>>()
        .join(" or ");
    assert!(parse_track_filter(&flat_and).is_ok());
    assert!(parse_track_filter(&flat_or).is_ok());

    let child_at_limit = format!("commentary or {}", nested_not(63));
    let child_past_limit = format!("commentary or {}", nested_not(64));
    assert!(parse_track_filter(&child_at_limit).is_ok());
    assert!(parse_track_filter(&child_past_limit).is_err());
}

#[test]
fn optional_filter_lowering_failure_is_diagnostic() {
    let source = "policy \"p\" {\n  phase a {\n    keep audio where lang in [\"eng\"]\n  }\n}";
    let diagnostics = compile_ast_errors(source);

    assert_eq!(
        serde_json::to_value(diagnostics).unwrap(),
        serde_json::json!([{
            "code": "unknown_phase_statement_or_operation",
            "severity": "error",
            "stage": "compile",
            "span": {"start": 29, "end": 61},
            "location": {"line": 3, "column": 5},
            "message": "validated track filter could not be lowered",
            "suggestion": null,
            "related": []
        }])
    );
}

#[test]
fn required_filter_lowering_failure_is_diagnostic() {
    let source = "policy \"p\" {\n  phase a {\n    \
                  synthesize audio from lang in [\"eng\"] { codec aac channels 2 }\n  }\n}";
    let diagnostics = compile_ast_errors(source);

    assert_eq!(
        serde_json::to_value(diagnostics).unwrap(),
        serde_json::json!([{
            "code": "unknown_phase_statement_or_operation",
            "severity": "error",
            "stage": "compile",
            "span": {"start": 29, "end": 91},
            "location": {"line": 3, "column": 5},
            "message": "validated track filter could not be lowered",
            "suggestion": null,
            "related": []
        }])
    );
}

#[test]
fn malformed_public_filters_report_the_pinned_stage_and_span() {
    let unterminated =
        "policy \"p\" {\n  phase a {\n    keep subtitle where title contains \"broken\n  }\n}";
    let error = crate::compile_policy(unterminated).unwrap_err();
    assert_eq!(error.code(), "POLICY_PARSE_ERROR");
    assert_eq!(
        serde_json::to_value(error.diagnostics).unwrap(),
        serde_json::json!([{
            "code": "unexpected_token",
            "severity": "error",
            "stage": "parse",
            "span": {"start": 64, "end": 65},
            "location": {"line": 3, "column": 40},
            "message": "unterminated string",
            "suggestion": null,
            "related": []
        }])
    );

    let malformed =
        "policy \"p\" {\n  phase a {\n    keep audio where language in [\"eng\",]\n  }\n}";
    let error = crate::compile_policy(malformed).unwrap_err();
    assert_eq!(error.code(), "POLICY_VALIDATION_ERROR");
    assert_eq!(
        serde_json::to_value(error.diagnostics).unwrap(),
        serde_json::json!([{
            "code": "unknown_phase_statement_or_operation",
            "severity": "error",
            "stage": "validate",
            "span": {"start": 29, "end": 66},
            "location": {"line": 3, "column": 5},
            "message": "unknown track filter predicate",
            "suggestion": null,
            "related": []
        }])
    );
}

fn compile_ast_errors(source: &str) -> Vec<crate::PolicyDiagnostic> {
    let ast = crate::parse_policy_source(source).unwrap();
    super::super::lower::compile_ast(source, &ast, Vec::new()).unwrap_err()
}

fn nested_not(count: usize) -> String {
    format!("{}commentary", "not ".repeat(count))
}

fn nested_group(count: usize) -> String {
    format!("{}commentary{}", "(".repeat(count), ")".repeat(count))
}
