use super::*;

#[test]
fn line_column_maps_byte_offsets() {
    let source = "policy \"a\" {\n  phase one {}\n}\n";
    let location = line_column(source, 15);
    assert_eq!(location.line, 2);
    assert_eq!(location.column, 3);
}

#[test]
fn span_contains_start_and_end_bytes() {
    let span = SourceSpan::new(2, 5);
    assert_eq!(span.start, 2);
    assert_eq!(span.end, 5);
    assert_eq!(span.len(), 3);
}

#[test]
fn durable_source_positions_reject_unknown_fields() {
    let span_error = serde_json::from_value::<SourceSpan>(serde_json::json!({
        "start": 2,
        "end": 5,
        "future_span": true
    }))
    .unwrap_err();
    assert!(
        span_error
            .to_string()
            .contains("unknown field `future_span`")
    );

    let location_error = serde_json::from_value::<SourceLocation>(serde_json::json!({
        "line": 2,
        "column": 5,
        "future_location": true
    }))
    .unwrap_err();
    assert!(
        location_error
            .to_string()
            .contains("unknown field `future_location`")
    );
}
