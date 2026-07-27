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
fn run_if_requires_exactly_one_published_trigger_and_phase() {
    let valid = "policy \"p\" { phase a {} phase b { run_if completed a } }";
    assert!(codes(valid).is_empty());

    for source in [
        "policy \"p\" { phase a { run_if } }",
        "policy \"p\" { phase a {} phase b { run_if changed a } }",
        "policy \"p\" { phase a {} phase b { run_if modified a extra } }",
    ] {
        assert!(
            codes(source).contains(&"invalid_run_if_trigger".to_owned()),
            "{source}"
        );
    }
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
        compile_policy(
            "policy \"p\" { phase a { transcode audio to aac where language in [\"eng\"] } }",
        )
        .is_ok()
    );
    assert!(
        compile_policy(
            "policy \"p\" { phase a { transcode audio to opus where codec in [\"aac\"] } }",
        )
        .is_ok()
    );
    assert!(
        compile_policy(
            "policy \"p\" { phase a { transcode audio to eac3 where language in [\"eng\"] } }",
        )
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
        codes("policy \"p\" { phase a { transcode audio to flac where language in [\"eng\"] } }",)
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
fn accepts_published_metadata_tool_identifiers_without_warning() {
    assert!(
        codes(
            "policy \"p\" { metadata { requires_tools: [ffmpeg, ffprobe, mkvtoolnix] } \
             phase a { container mkv } }"
        )
        .is_empty()
    );
}

#[test]
fn rejects_non_list_metadata_tool_requirements() {
    assert!(
        codes("policy \"p\" { metadata { requires_tools: ffmpeg } phase a { container mkv } }")
            .contains(&"invalid_metadata_requires_tools".to_owned())
    );
}

#[test]
fn rejects_quoted_metadata_tool_requirements() {
    assert!(
        codes(
            "policy \"p\" { metadata { requires_tools: [\"ffmpeg\"] } \
             phase a { container mkv } }"
        )
        .contains(&"invalid_metadata_requires_tools".to_owned())
    );
}

#[test]
fn rejects_unknown_metadata_tool_requirements() {
    assert!(
        codes(
            "policy \"p\" { metadata { requires_tools: [mediainfo] } \
             phase a { container mkv } }"
        )
        .contains(&"invalid_metadata_requires_tools".to_owned())
    );
}

#[test]
fn rejects_repeated_metadata_tool_requirement_settings() {
    assert!(
        codes(
            "policy \"p\" { metadata { requires_tools: [ffmpeg] \
             requires_tools: [mkvtoolnix] } phase a { container mkv } }"
        )
        .contains(&"invalid_metadata_requires_tools".to_owned())
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
        codes("policy \"p\" { config { languages: [\"english\"] } phase a {} }")
            .contains(&"invalid_language_code".to_owned())
    );
}

#[test]
fn rejects_unquoted_config_languages() {
    assert!(
        codes("policy \"p\" { config { languages: [eng, und] } phase a {} }")
            .contains(&"invalid_language_code".to_owned())
    );
}

#[test]
fn rejects_non_list_config_languages() {
    assert!(
        codes("policy \"p\" { config { languages: \"eng\" } phase a {} }")
            .contains(&"invalid_language_code".to_owned())
    );
}

#[test]
fn rejects_config_languages_without_commas() {
    assert!(
        codes("policy \"p\" { config { languages: [\"eng\" \"und\"] } phase a {} }")
            .contains(&"invalid_language_code".to_owned())
    );
}

#[test]
fn rejects_unpublished_config_language_target() {
    let error =
        parse_policy_source("policy \"p\" { config { languages audio: [\"eng\"] } phase a {} }")
            .unwrap_err();
    assert_eq!(error.diagnostics[0].code, "unexpected_token");
}

#[test]
fn rejects_duplicate_config_settings() {
    assert!(
        codes("policy \"p\" { config { languages: [\"eng\"] languages: [\"und\"] } phase a {} }")
            .contains(&"duplicate_config_setting".to_owned())
    );
}

#[test]
fn rejects_invalid_language_filter_alias() {
    assert!(
        codes("policy \"p\" { phase a { keep audio where language in [\"english\"] } }")
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
fn rejects_unpublished_config_on_error_skip() {
    assert!(
        codes("policy \"p\" { config { on_error: skip } phase a {} }")
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
    let diagnostics =
        codes("policy \"p\" { phase a { rules first { rule \"r\" when exists audio {} } } }");

    assert!(!diagnostics.contains(&"invalid_rule_match_mode".to_owned()));
}

#[test]
fn rejects_unpublished_nested_rule_condition() {
    assert!(
        codes("policy \"p\" { phase a { rules first { rule \"r\" { when exists audio {} } } } }")
            .contains(&"unknown_phase_statement_or_operation".to_owned())
    );
}

#[test]
fn rejects_rules_with_extra_mode_tokens() {
    assert!(
        codes("policy \"p\" { phase a { rules first all { rule \"r\" when exists audio {} } } }")
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
fn accepts_defaults_where_track_filter() {
    assert!(
        codes("policy \"p\" { phase a { defaults audio where language in [\"eng\"] } }").is_empty()
    );
    assert!(codes("policy \"p\" { phase a { defaults subtitle where forced } }").is_empty());
}

#[test]
fn rejects_defaults_where_unknown_filter() {
    assert!(
        codes("policy \"p\" { phase a { defaults audio where bogus } }")
            .contains(&"unknown_phase_statement_or_operation".to_owned())
    );
}

#[test]
fn rejects_defaults_where_invalid_language_code() {
    assert!(
        codes("policy \"p\" { phase a { defaults audio where language in [\"english\"] } }")
            .contains(&"invalid_language_code".to_owned())
    );
}

#[test]
fn accepts_order_tracks_where_track_filter() {
    assert!(codes("policy \"p\" { phase a { order tracks where commentary } }").is_empty());
}

#[test]
fn accepts_order_tracks_list_and_where_filter() {
    assert!(
        codes(
            "policy \"p\" { phase a { \
             order tracks [video, audio] where language in [\"eng\"] \
             } }",
        )
        .is_empty()
    );
}

#[test]
fn rejects_order_tracks_where_unknown_filter() {
    assert!(
        codes("policy \"p\" { phase a { order tracks where bogus } }")
            .contains(&"unknown_phase_statement_or_operation".to_owned())
    );
}

#[test]
fn accepts_only_published_phase_on_error_values() {
    for (control, expected, wire_value) in [
        ("on_error: abort", crate::ErrorStrategy::Abort, "abort"),
        (
            "on_error: continue",
            crate::ErrorStrategy::Continue,
            "continue",
        ),
        ("on_error:abort", crate::ErrorStrategy::Abort, "abort"),
        (
            "on_error \t: \t continue",
            crate::ErrorStrategy::Continue,
            "continue",
        ),
    ] {
        let source = format!("policy \"p\" {{ phase a {{ {control} }} }}");
        let policy = compile_policy(&source).unwrap().policy;

        assert_eq!(policy.phases[0].on_error, Some(expected));
        assert_eq!(
            serde_json::to_value(&policy).unwrap()["phases"][0]["on_error"],
            wire_value
        );
    }
}

#[test]
fn rejects_unpublished_phase_on_error_forms() {
    for source in [
        "policy \"p\" { phase a { on_error: } }",
        "policy \"p\" { phase a { on_error: skip } }",
        "policy \"p\" { phase a { on_error skip } }",
        "policy \"p\" { phase a { on_error abort } }",
        "policy \"p\" { phase a { on_error continue } }",
        "policy \"p\" { phase a { on_error abort: continue } }",
        "policy \"p\" { phase a { on_error junk: abort } }",
        "policy \"p\" { phase a { on_error:: abort } }",
        "policy \"p\" { phase a { on_error: \"abort\" } }",
        "policy \"p\" { phase a { on_error: Abort } }",
        "policy \"p\" { phase a { on_error: abort retry } }",
        "policy \"p\" { phase a { on_error\u{a0}: abort } }",
        "policy \"p\" { phase a { on_error:\u{a0}abort } }",
        "policy \"p\" { phase a { on_error: abort\u{a0} } }",
        "policy \"p\" { phase a {\non_error: continue\u{a0}\n} }",
    ] {
        assert!(
            codes(source).contains(&"invalid_on_error_value".to_owned()),
            "{source:?}"
        );
    }
}

#[test]
fn phase_on_error_validation_handles_mismatched_source_without_panicking() {
    let parsed_source = "policy \"p\" { phase a { on_error: abort } }";
    let ast = parse_policy_source(parsed_source).unwrap();
    let span = ast.phases[0].controls[0].span();
    let unaligned_source = format!(
        "{}é{}",
        "x".repeat(span.start - 1),
        "x".repeat(span.end - span.start)
    );

    for source in ["", unaligned_source.as_str()] {
        let result = validate_policy_ast(source, &ast);

        assert!(
            result
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "invalid_on_error_value")
        );
    }
}

#[test]
fn rejects_unsupported_transcode_inside_rule_block() {
    assert!(
        codes(
            "policy \"p\" { phase a { rules first { rule \"r\" when exists audio { transcode video to vp9 } } } }"
        )
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
        codes("policy \"p\" { phase a { keep audio where language in [\"eng\"] or banana } }",)
            .contains(&"unknown_phase_statement_or_operation".to_owned())
    );
}

#[test]
fn rejects_malformed_audio_filter_tails() {
    assert!(
        codes(
            "policy \"p\" { phase a { \
             transcode audio to aac where language in [\"eng\"] garbage \
             } }",
        )
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
fn accepts_published_stream_condition_source_forms() {
    for condition in [
        "exists audio",
        "exists subtitle",
        "count audio == 0",
        "count audio != 1",
        "count audio < 2",
        "count subtitle <= 3",
        "count subtitle > 4",
        "count subtitle >= 5",
    ] {
        assert!(
            codes(&format!(
                "policy \"p\" {{ phase a {{ when {condition} {{ container mkv }} }} }}"
            ))
            .is_empty(),
            "published condition should compile: {condition}"
        );
    }
}

#[test]
fn rejects_unpublished_stream_condition_source_forms() {
    for condition in [
        "exists audio where commentary",
        "exists video",
        "exists attachment",
        "exists subtitles",
        "count video == 1",
        "count attachment == 1",
        "count subtitles == 1",
        "count audio = 1",
        "count audio contains 1",
        "count audio matches 1",
        "count audio == +1",
        "count audio == -1",
        "count audio == ١",
        "count audio == 1 extra",
    ] {
        assert!(
            codes(&format!(
                "policy \"p\" {{ phase a {{ when {condition} {{ container mkv }} }} }}"
            ))
            .contains(&"unknown_phase_statement_or_operation".to_owned()),
            "unpublished condition should fail validation: {condition}"
        );
    }
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

#[test]
fn rejects_comparison_numeric_literal_exceeding_u64() {
    // u64::MAX is 18446744073709551615; this 25-digit literal overflows it.
    // Without a guard it lowers to a Number string that the planner's
    // parse::<u64>() silently drops, so the condition never matches — a silent
    // wrong answer. It must be a hard compile error instead.
    assert!(
        codes("policy \"p\" { phase a { skip when bitrate > 9999999999999999999999999 } }")
            .contains(&"numeric_literal_out_of_range".to_owned())
    );
}

#[test]
fn rejects_count_numeric_literal_exceeding_u64() {
    assert!(
        codes(
            "policy \"p\" { phase a { \
             when count audio > 9999999999999999999999999 { \
             keep audio where language in [\"eng\"] \
             } } }"
        )
        .contains(&"numeric_literal_out_of_range".to_owned())
    );
}

#[test]
fn accepts_in_range_numeric_literal() {
    // A legitimate in-range literal must not trip the overflow guard.
    assert!(
        !codes("policy \"p\" { phase a { skip when bitrate > 1920 } }")
            .contains(&"numeric_literal_out_of_range".to_owned())
    );
}
