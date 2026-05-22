use crate::parse_policy_source;

use super::*;

fn codes(source: &str) -> Vec<String> {
    let ast = parse_policy_source(source).unwrap();
    validate_policy_ast(source, &ast)
        .diagnostics
        .into_iter()
        .map(|d| d.code)
        .collect()
}

#[test]
fn rejects_duplicate_phase_names() {
    assert!(
        codes("policy \"p\" { phase a {} phase a {} }")
            .contains(&"duplicate_phase_name".to_owned())
    );
}

#[test]
fn rejects_unknown_dependency() {
    assert!(
        codes("policy \"p\" { phase a { depends_on: [missing] } }")
            .contains(&"unknown_phase_reference".to_owned())
    );
}

#[test]
fn rejects_deferred_execution_operations() {
    assert!(
        codes("policy \"p\" { phase a { transcode video to hevc {} } }")
            .contains(&"deferred_execution_operation".to_owned())
    );
}

#[test]
fn warns_for_unknown_plugin_namespace() {
    let ast =
        parse_policy_source("policy \"p\" { phase a { set_tag \"title\" plugin.radarr.title } }")
            .unwrap();
    let result = validate_policy_ast("", &ast);
    assert!(
        result
            .diagnostics
            .iter()
            .any(|d| d.code == "unknown_extension_namespace")
    );
    assert!(
        result
            .diagnostics
            .iter()
            .all(|d| d.severity == crate::DiagnosticSeverity::Warning)
    );
}

#[test]
fn rejects_unknown_core_field_root() {
    assert!(
        codes("policy \"p\" { phase a { when vidio.codec == hevc { container mkv } } }")
            .contains(&"invalid_core_field_path".to_owned())
    );
}

#[test]
fn rejects_invalid_config_language() {
    assert!(
        codes("policy \"p\" { config { languages audio: [english] } phase a {} }")
            .contains(&"invalid_language_code".to_owned())
    );
}

#[test]
fn rejects_invalid_on_error() {
    assert!(
        codes("policy \"p\" { config { on_error: retry } phase a {} }")
            .contains(&"invalid_on_error_value".to_owned())
    );
}

#[test]
fn rejects_deferred_extends() {
    assert!(
        codes("policy \"p\" { extends \"base\" phase a {} }")
            .contains(&"deferred_composition".to_owned())
    );
}

#[test]
fn rejects_tag_ordering_conflict() {
    assert!(
        codes("policy \"p\" { phase a {\n set_tag \"title\" identity.title\n clear_tags\n } }")
            .contains(&"tag_ordering_error".to_owned())
    );
}

#[test]
fn accepts_rules_first_mode() {
    let diagnostics = codes("policy \"p\" { phase a { rules first { rule \"r\" {} } } }");

    assert!(!diagnostics.contains(&"invalid_rule_match_mode".to_owned()));
}

#[test]
fn rejects_policy_without_phases() {
    let ast = parse_policy_source("policy \"p\" {}").unwrap();
    let result = validate_policy_ast("policy \"p\" {}", &ast);

    assert!(result.has_errors());
}

#[test]
fn rejects_unknown_core_field_root_in_skip_when() {
    assert!(
        codes("policy \"p\" { phase a { skip when vidio.codec == hevc container mkv } }")
            .contains(&"invalid_core_field_path".to_owned())
    );
}

#[test]
fn rejects_container_without_value() {
    assert!(
        codes("policy \"p\" { phase a { container } }")
            .contains(&"unsupported_container".to_owned())
    );
}

#[test]
fn rejects_keep_without_track_target() {
    assert!(
        codes("policy \"p\" { phase a { keep } }").contains(&"invalid_track_target".to_owned())
    );
}

#[test]
fn rejects_defaults_without_strategy() {
    assert!(
        codes("policy \"p\" { phase a { defaults audio } }")
            .contains(&"invalid_default_strategy".to_owned())
    );
}

#[test]
fn rejects_on_error_without_value() {
    assert!(
        codes("policy \"p\" { phase a { on_error: } }")
            .contains(&"invalid_on_error_value".to_owned())
    );
}
