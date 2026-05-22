use super::*;

#[test]
fn spanned_preserves_value_and_span() {
    let spanned = Spanned {
        value: "inspect".to_owned(),
        span: crate::SourceSpan::new(7, 14),
    };

    assert_eq!(spanned.value, "inspect");
    assert_eq!(spanned.span, crate::SourceSpan::new(7, 14));
}
