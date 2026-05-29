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

#[test]
fn parses_multiple_phase_statements_separated_by_spaces() {
    let ast =
        parse_policy_source("policy \"p\" { phase inspect { container mkv keep audio } }").unwrap();

    assert_eq!(ast.phases[0].operations.len(), 2);
    assert_eq!(ast.phases[0].operations[0].keyword().value, "container");
    assert_eq!(ast.phases[0].operations[1].keyword().value, "keep");
}

#[test]
fn keeps_skip_when_as_one_phase_control() {
    let ast = parse_policy_source(
        "policy \"p\" { phase inspect { skip when video.codec == h264 container mkv } }",
    )
    .unwrap();

    assert_eq!(ast.phases[0].controls.len(), 1);
    assert_eq!(ast.phases[0].controls[0].keyword().value, "skip");
    assert_eq!(ast.phases[0].operations.len(), 1);
    assert_eq!(ast.phases[0].operations[0].keyword().value, "container");
}

#[test]
fn parses_multiple_metadata_settings_separated_by_spaces() {
    let ast = parse_policy_source(
        "policy \"p\" { metadata { version: \"1\" description: \"x\" } phase inspect {} }",
    )
    .unwrap();

    assert_eq!(ast.metadata.len(), 2);
    assert_eq!(ast.metadata[0].key.value, "version");
    assert_eq!(ast.metadata[1].key.value, "description");
}

#[test]
fn parses_transcode_inline_settings_body() {
    let src = "policy \"p\" { phase a { transcode video to av1 { encoder: libsvtav1 crf: 28 preset: 6 } } }";
    let ast = parse_policy_source(src).unwrap();
    let op = &ast.phases[0].operations[0];
    let crate::StatementAst::TranscodeInline { settings, .. } = op else {
        panic!("expected TranscodeInline, got {op:?}");
    };
    let keys: Vec<&str> = settings.iter().map(|s| s.key.value.as_str()).collect();
    assert_eq!(keys, vec!["encoder", "crf", "preset"]);
}

#[test]
fn parses_bare_transcode_as_raw() {
    let src = "policy \"p\" { phase a { transcode video to hevc } }";
    let ast = parse_policy_source(src).unwrap();
    assert!(matches!(
        ast.phases[0].operations[0],
        crate::StatementAst::Raw { .. }
    ));
}
