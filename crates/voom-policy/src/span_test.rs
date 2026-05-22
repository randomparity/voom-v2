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
