use crate::{compile_policy, parse_policy_source};

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
fn rejects_unknown_bare_dependency() {
    assert!(
        codes("policy \"p\" { phase a { depends_on: missing } }")
            .contains(&"unknown_phase_reference".to_owned())
    );
}

#[test]
fn rejects_depends_on_with_extra_tokens_after_list() {
    assert!(
        codes("policy \"p\" { phase a {} phase b { depends_on: [a] later } }")
            .contains(&"unknown_phase_statement_or_operation".to_owned())
    );
}

#[test]
fn accepts_hevc_and_av1_named_and_inline() {
    assert!(codes("policy \"p\" { phase a { transcode video to hevc } }").is_empty());
    assert!(codes("policy \"p\" { phase a { transcode video to av1 } }").is_empty());
    assert!(
        codes(
            "policy \"p\" { phase a { transcode video to hevc using profile \"hevc-archive\" } }"
        )
        .is_empty()
    );
    assert!(
        codes(
            "policy \"p\" { phase a { transcode video to av1 { encoder: libsvtav1 crf: 28 preset: 6 } } }"
        )
        .is_empty()
    );
}

#[test]
fn rejects_invalid_inline_profiles() {
    assert!(
        codes("policy \"p\" { phase a { transcode video to av1 { crf: 28 preset: 6 } } }")
            .contains(&"invalid_video_profile_setting".to_owned())
    );
    assert!(
        codes(
            "policy \"p\" { phase a { transcode video to av1 { encoder: libx265 crf: 20 preset: medium } } }"
        )
        .contains(&"invalid_video_profile_setting".to_owned())
    );
    assert!(
        codes(
            "policy \"p\" { phase a { transcode video to hevc { encoder: libx265 crf: 60 preset: medium } } }"
        )
        .contains(&"invalid_video_profile_setting".to_owned())
    );
    assert!(
        codes(
            "policy \"p\" { phase a { transcode video to av1 { encoder: libsvtav1 crf: 30 preset: medium } } }"
        )
        .contains(&"invalid_video_profile_setting".to_owned())
    );
    assert!(
        codes(
            "policy \"p\" { phase a { transcode video to av1 { encoder: libsvtav1 crf: 30 preset: 6 bogus: 1 } } }"
        )
        .contains(&"invalid_video_profile_setting".to_owned())
    );
    assert!(
        codes(
            "policy \"p\" { phase a { transcode video to hevc { encoder: libx265 crf: 20 preset: slow codec_profile: main pixel_format: yuv420p10le } } }"
        )
        .contains(&"invalid_video_profile_setting".to_owned())
    );
    // duplicate key
    assert!(
        codes(
            "policy \"p\" { phase a { transcode video to av1 { encoder: libsvtav1 crf: 20 crf: 28 preset: 6 } } }"
        )
        .contains(&"invalid_video_profile_setting".to_owned())
    );
}

#[test]
fn rejects_using_profile_with_inline_body() {
    assert!(
        codes(
            "policy \"p\" { phase a { transcode video to hevc using profile \"x\" { crf: 20 } } }"
        )
        .contains(&"unsupported_transcode_shape".to_owned())
    );
}

#[test]
fn rejects_unknown_codec() {
    assert!(
        codes("policy \"p\" { phase a { transcode video to vp9 } }")
            .contains(&"unsupported_transcode_shape".to_owned())
    );
}

#[test]
fn accepts_sprint14_audio_operations() {
    assert!(
        compile_policy("policy \"p\" { phase a { transcode audio to aac where lang in [eng] } }")
            .is_ok()
    );
    assert!(
        compile_policy("policy \"p\" { phase a { transcode audio to opus where codec in [aac] } }")
            .is_ok()
    );
    assert!(compile_policy("policy \"p\" { phase a { extract audio where commentary } }").is_ok());
}

#[test]
fn rejects_unsupported_transcode_shapes() {
    assert!(
        codes("policy \"p\" { phase a { transcode video to av1 {} } }")
            .contains(&"invalid_video_profile_setting".to_owned())
    );
    assert!(
        codes("policy \"p\" { phase a { transcode video to hevc using profile \"small\" {} } }")
            .contains(&"unsupported_transcode_shape".to_owned())
    );
    assert!(
        codes("policy \"p\" { phase a { transcode audio to flac where lang in [eng] } }")
            .contains(&"unsupported_transcode_shape".to_owned())
    );
}

#[test]
fn rejects_unsupported_extract_shapes() {
    assert!(
        codes("policy \"p\" { phase a { extract subtitles where forced } }")
            .contains(&"unknown_phase_statement_or_operation".to_owned())
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
fn warns_for_metadata_requires_tools() {
    let ast = parse_policy_source(
        "policy \"p\" { metadata { requires_tools: [ffmpeg] } phase a { container mkv } }",
    )
    .unwrap();

    let result = validate_policy_ast("", &ast);

    assert!(
        result
            .diagnostics
            .iter()
            .any(|d| d.code == "metadata_requires_tools_deferred")
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
fn rejects_unknown_core_field_path_below_valid_root() {
    assert!(
        codes(
            "policy \"p\" { phase a { when video.not_a_policy_input_fact == true { container mkv } } }"
        )
        .contains(&"invalid_core_field_path".to_owned())
    );
}

#[test]
fn rejects_unknown_core_field_path_extra_segments() {
    assert!(
        codes(
            "policy \"p\" { phase a { when video.codec.no_such_fact == true { container mkv } } }"
        )
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
fn rejects_invalid_language_filter_alias() {
    assert!(
        codes("policy \"p\" { phase a { keep audio where language in [english] } }")
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
fn rejects_nested_clear_tags_after_set_tag_in_same_phase() {
    assert!(
        codes("policy \"p\" { phase a { set_tag \"title\" identity.title when exists audio { clear_tags } } }")
            .contains(&"tag_ordering_error".to_owned())
    );
}

#[test]
fn rejects_nested_tag_operation_conflict_in_same_phase() {
    assert!(
        codes(
            "policy \"p\" { phase a { when exists audio { set_tag \"title\" identity.title } delete_tag \"title\" } }"
        )
        .contains(&"ambiguous_tag_operation_conflict".to_owned())
    );
}

#[test]
fn accepts_rules_first_mode() {
    let diagnostics = codes("policy \"p\" { phase a { rules first { rule \"r\" {} } } }");

    assert!(!diagnostics.contains(&"invalid_rule_match_mode".to_owned()));
}

#[test]
fn rejects_rules_with_extra_mode_tokens() {
    assert!(
        codes("policy \"p\" { phase a { rules first all { rule \"r\" {} } } }")
            .contains(&"unknown_phase_statement_or_operation".to_owned())
    );
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
fn rejects_container_with_extra_tokens() {
    assert!(
        codes("policy \"p\" { phase a { container mkv mp4 } }")
            .contains(&"unknown_phase_statement_or_operation".to_owned())
    );
}

#[test]
fn rejects_keep_without_track_target() {
    assert!(
        codes("policy \"p\" { phase a { keep } }").contains(&"invalid_track_target".to_owned())
    );
}

#[test]
fn rejects_keep_with_extra_tokens_without_where() {
    assert!(
        codes("policy \"p\" { phase a { keep audio garbage } }")
            .contains(&"unknown_phase_statement_or_operation".to_owned())
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
fn rejects_defaults_with_extra_tokens() {
    assert!(
        codes("policy \"p\" { phase a { defaults audio first forced } }")
            .contains(&"unknown_phase_statement_or_operation".to_owned())
    );
}

#[test]
fn rejects_on_error_without_value() {
    assert!(
        codes("policy \"p\" { phase a { on_error: } }")
            .contains(&"invalid_on_error_value".to_owned())
    );
}

#[test]
fn rejects_on_error_with_extra_tokens() {
    assert!(
        codes("policy \"p\" { phase a { on_error abort retry } }")
            .contains(&"unknown_phase_statement_or_operation".to_owned())
    );
}

#[test]
fn rejects_unsupported_transcode_inside_rule_block() {
    assert!(
        codes("policy \"p\" { phase a { rules first { rule \"r\" { transcode video to vp9 } } } }")
            .contains(&"unsupported_transcode_shape".to_owned())
    );
}

#[test]
fn reports_nested_when_diagnostic_once() {
    let diagnostics =
        codes("policy \"p\" { phase a { when exists audio { transcode video to vp9 } } }");

    assert_eq!(
        diagnostics
            .iter()
            .filter(|code| *code == "unsupported_transcode_shape")
            .count(),
        1
    );
}

#[test]
fn rejects_set_tag_without_value() {
    assert!(
        codes("policy \"p\" { phase a { set_tag \"title\" } }")
            .contains(&"unknown_phase_statement_or_operation".to_owned())
    );
}

#[test]
fn rejects_set_tag_with_extra_tokens_after_value() {
    assert!(
        codes("policy \"p\" { phase a { set_tag \"title\" \"one\" \"two\" } }")
            .contains(&"unknown_phase_statement_or_operation".to_owned())
    );
}

#[test]
fn rejects_delete_tag_without_key() {
    assert!(
        codes("policy \"p\" { phase a { delete_tag } }")
            .contains(&"unknown_phase_statement_or_operation".to_owned())
    );
}

#[test]
fn rejects_delete_tag_with_extra_tokens() {
    assert!(
        codes("policy \"p\" { phase a { delete_tag \"title\" identity.title } }")
            .contains(&"unknown_phase_statement_or_operation".to_owned())
    );
}

#[test]
fn rejects_clear_tags_with_extra_tokens() {
    assert!(
        codes("policy \"p\" { phase a { clear_tags now } }")
            .contains(&"unknown_phase_statement_or_operation".to_owned())
    );
}

#[test]
fn rejects_actions_without_clear_verb() {
    assert!(
        codes("policy \"p\" { phase a { actions audio retain } }")
            .contains(&"unknown_phase_statement_or_operation".to_owned())
    );
}

#[test]
fn rejects_actions_with_extra_tokens() {
    assert!(
        codes("policy \"p\" { phase a { actions audio clear now } }")
            .contains(&"unknown_phase_statement_or_operation".to_owned())
    );
}

#[test]
fn rejects_order_without_tracks_keyword() {
    assert!(
        codes("policy \"p\" { phase a { order [video, audio] } }")
            .contains(&"unknown_phase_statement_or_operation".to_owned())
    );
}

#[test]
fn rejects_order_with_extra_tokens_after_list() {
    assert!(
        codes("policy \"p\" { phase a { order tracks [video, audio] later } }")
            .contains(&"unknown_phase_statement_or_operation".to_owned())
    );
}

#[test]
fn rejects_unknown_track_filter_predicate() {
    assert!(
        codes("policy \"p\" { phase a { keep audio where banana } }")
            .contains(&"unknown_phase_statement_or_operation".to_owned())
    );
}

#[test]
fn accepts_channel_count_track_filter() {
    assert!(codes("policy \"p\" { phase a { keep audio where channels >= 6 } }").is_empty());
}

#[test]
fn rejects_unknown_boolean_track_filter_branch() {
    assert!(
        codes("policy \"p\" { phase a { keep audio where lang in [eng] or banana } }")
            .contains(&"unknown_phase_statement_or_operation".to_owned())
    );
}

#[test]
fn rejects_malformed_audio_filter_tails() {
    assert!(
        codes("policy \"p\" { phase a { transcode audio to aac where lang in [eng] garbage } }")
            .contains(&"unknown_phase_statement_or_operation".to_owned())
    );
    assert!(
        codes("policy \"p\" { phase a { extract audio where commentary and } }")
            .contains(&"unknown_phase_statement_or_operation".to_owned())
    );
}

#[test]
fn rejects_invalid_exists_condition_target() {
    assert!(
        codes("policy \"p\" { phase a { when exists banana { container mkv } } }")
            .contains(&"invalid_track_target".to_owned())
    );
}

#[test]
fn rejects_condition_comparison_without_value() {
    assert!(
        codes("policy \"p\" { phase a { when video.codec == { container mkv } } }")
            .contains(&"unknown_phase_statement_or_operation".to_owned())
    );
}

#[test]
fn rejects_condition_comparison_with_unquoted_extra_value_tokens() {
    assert!(
        codes("policy \"p\" { phase a { when video.codec == hevc extra { container mkv } } }")
            .contains(&"unknown_phase_statement_or_operation".to_owned())
    );
}

#[test]
fn rejects_invalid_skip_condition_target() {
    assert!(
        codes("policy \"p\" { phase a { skip when exists banana container mkv } }")
            .contains(&"invalid_track_target".to_owned())
    );
}
