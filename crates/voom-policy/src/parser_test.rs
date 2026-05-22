use super::*;

#[test]
fn parses_minimal_policy_with_phase() {
    let ast =
        parse_policy_source("policy \"minimal\" { phase inspect { container mkv } }").unwrap();
    assert_eq!(ast.name.value, "minimal");
    assert_eq!(ast.phases.len(), 1);
    assert_eq!(ast.phases[0].name.value, "inspect");
}

#[test]
fn parses_comments_and_free_form_whitespace() {
    let ast = parse_policy_source(
        "policy \"comments\" {\n// comment\nphase normalize {\n keep audio where lang in [eng, und]\n}\n}",
    )
    .unwrap();
    assert_eq!(ast.phases[0].operations.len(), 1);
}

#[test]
fn reports_parse_diagnostic_for_unclosed_block() {
    let err = parse_policy_source("policy \"broken\" { phase one {").unwrap_err();
    assert_eq!(err.diagnostics[0].code, "unexpected_token");
    assert_eq!(err.diagnostics[0].stage, crate::DiagnosticStage::Parse);
}

#[test]
fn preserves_nested_phase_block_statements() {
    let ast = parse_policy_source(
        "policy \"p\" { phase inspect { when exists audio { keep audio where lang in [eng] } } }",
    )
    .unwrap();

    let StatementAst::Block { statements, .. } = &ast.phases[0].operations[0] else {
        unreachable!("expected when block");
    };
    assert_eq!(statements.len(), 1);
    assert_eq!(statements[0].keyword().value, "keep");
}
