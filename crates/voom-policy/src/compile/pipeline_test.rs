use crate::{CompiledCondition, CompiledOperation, TrackFilter};

use super::*;

#[test]
fn compile_policy_preserves_sprint12_video_hevc_transcode() {
    let out = compile_policy("policy \"p\" { phase a { transcode video to hevc } }").unwrap();

    assert_eq!(
        out.policy.phases[0].operations[0],
        CompiledOperation::TranscodeVideo {
            target_codec: "hevc".to_owned(),
            container: "mkv".to_owned(),
            profile: crate::VideoProfileRef::Named("default-hevc".to_owned()),
            resolved_profile: None,
        }
    );
}

#[test]
fn compile_policy_lowers_defaults_where_to_filter_addressed_default() {
    let out = compile_policy(
        "policy \"p\" { phase a { defaults audio where lang in [eng] and not commentary } }",
    )
    .unwrap();

    let CompiledOperation::SetDefaults {
        target,
        strategy,
        filter: Some(TrackFilter::And { filters }),
    } = &out.policy.phases[0].operations[0]
    else {
        unreachable!("expected filter-addressed default");
    };

    assert_eq!(*target, crate::TrackTarget::Audio);
    // In filter mode the strategy is inert (ADR 0023): lowered to Preserve so
    // an unresolved filter never applies a group-wide default.
    assert_eq!(*strategy, crate::DefaultStrategy::Preserve);
    assert_eq!(filters.len(), 2);
}

#[test]
fn compile_policy_keeps_strategy_default_without_filter() {
    let out = compile_policy("policy \"p\" { phase a { defaults audio first } }").unwrap();

    assert_eq!(
        out.policy.phases[0].operations[0],
        CompiledOperation::SetDefaults {
            target: crate::TrackTarget::Audio,
            strategy: crate::DefaultStrategy::First,
            filter: None,
        }
    );
}

#[test]
fn compile_policy_lowers_order_tracks_where_to_head_filter() {
    let out = compile_policy(
        "policy \"p\" { phase a { order tracks [video, audio] where lang in [eng] } }",
    )
    .unwrap();

    let CompiledOperation::ReorderTracks {
        targets,
        head_filter: Some(TrackFilter::LanguageIn { values }),
    } = &out.policy.phases[0].operations[0]
    else {
        unreachable!("expected head-filter reorder");
    };

    assert_eq!(targets.len(), 2);
    assert_eq!(values, &["eng".to_owned()]);
}

#[test]
fn compile_policy_keeps_group_order_without_head_filter() {
    let out = compile_policy("policy \"p\" { phase a { order tracks [video, audio] } }").unwrap();

    assert_eq!(
        out.policy.phases[0].operations[0],
        CompiledOperation::ReorderTracks {
            targets: vec![crate::TrackTarget::Video, crate::TrackTarget::Audio],
            head_filter: None,
        }
    );
}

#[test]
fn compile_policy_produces_phase_order() {
    let out = compile_policy("policy \"p\" { phase a {} phase b { depends_on: [a] } }").unwrap();
    assert_eq!(out.policy.phase_order, ["a", "b"]);
}

#[test]
fn compile_policy_topologically_sorts_phase_order() {
    let out = compile_policy("policy \"p\" { phase b { depends_on: [a] } phase a {} }").unwrap();

    assert_eq!(out.policy.phase_order, ["a", "b"]);
}

#[test]
fn compile_policy_preserves_boolean_track_filters() {
    let out =
        compile_policy("policy \"p\" { phase a { keep audio where lang in [eng] or commentary } }")
            .unwrap();
    let CompiledOperation::KeepTracks {
        filter: Some(TrackFilter::Or { filters }),
        ..
    } = &out.policy.phases[0].operations[0]
    else {
        unreachable!("expected boolean track filter");
    };

    assert_eq!(filters.len(), 2);
    assert!(matches!(filters[0], TrackFilter::LanguageIn { .. }));
    assert!(matches!(filters[1], TrackFilter::Commentary));
}

#[test]
fn compile_policy_preserves_quoted_title_filter_with_boolean_words() {
    let out = compile_policy(
        "policy \"p\" { phase a { keep subtitle where title contains \"Director or Commentary\" } }",
    )
    .unwrap();

    assert_eq!(
        out.policy.phases[0].operations[0],
        crate::CompiledOperation::KeepTracks {
            target: crate::TrackTarget::Subtitle,
            filter: Some(crate::TrackFilter::TitleContains {
                value: "Director or Commentary".to_owned(),
            }),
        }
    );
}

#[test]
fn compile_policy_preserves_boolean_conditions() {
    let out = compile_policy(
        "policy \"p\" { phase a { when exists audio or exists subtitle { container mkv } } }",
    )
    .unwrap();
    let CompiledOperation::Conditional {
        condition: CompiledCondition::Or { conditions },
        ..
    } = &out.policy.phases[0].operations[0]
    else {
        unreachable!("expected boolean condition");
    };

    assert_eq!(conditions.len(), 2);
    assert!(matches!(conditions[0], CompiledCondition::Exists { .. }));
    assert!(matches!(conditions[1], CompiledCondition::Exists { .. }));
}

#[test]
fn compile_policy_preserves_parenthesized_boolean_conditions() {
    let out = compile_policy(
        "policy \"p\" { phase a { when (exists audio or exists subtitle) and exists video { container mkv } } }",
    )
    .unwrap();
    let CompiledOperation::Conditional {
        condition: CompiledCondition::And { conditions },
        ..
    } = &out.policy.phases[0].operations[0]
    else {
        unreachable!("expected parenthesized boolean condition");
    };

    assert_eq!(conditions.len(), 2);
    assert!(matches!(conditions[0], CompiledCondition::Or { .. }));
    assert!(matches!(conditions[1], CompiledCondition::Exists { .. }));
}

#[test]
fn compile_policy_preserves_parenthesized_boolean_track_filters() {
    let out = compile_policy(
        "policy \"p\" { phase a { keep audio where (lang in [eng] or commentary) and not forced } }",
    )
    .unwrap();
    let CompiledOperation::KeepTracks {
        filter: Some(TrackFilter::And { filters }),
        ..
    } = &out.policy.phases[0].operations[0]
    else {
        unreachable!("expected parenthesized boolean track filter");
    };

    assert_eq!(filters.len(), 2);
    assert!(matches!(filters[0], TrackFilter::Or { .. }));
    assert!(matches!(filters[1], TrackFilter::Not { .. }));
}

#[test]
fn compile_policy_preserves_quoted_condition_comparison_value() {
    let out = compile_policy(
        "policy \"p\" { phase a { when video.title contains \"Director or Commentary\" { clear_tags } } }",
    )
    .unwrap();

    assert_eq!(
        out.policy.phases[0].operations[0],
        CompiledOperation::Conditional {
            condition: CompiledCondition::FieldComparison {
                path: vec!["video".to_owned(), "title".to_owned()],
                op: crate::ComparisonOp::Contains,
                value: crate::CompiledValue::String {
                    value: "Director or Commentary".to_owned(),
                },
            },
            operations: vec![CompiledOperation::ClearTags],
        }
    );
}

#[test]
fn compile_policy_preserves_channel_count_track_filter() {
    let out =
        compile_policy("policy \"p\" { phase a { keep audio where channels >= 6 } }").unwrap();

    assert_eq!(
        out.policy.phases[0].operations[0],
        CompiledOperation::KeepTracks {
            target: crate::TrackTarget::Audio,
            filter: Some(TrackFilter::Channels {
                op: crate::ComparisonOp::Gte,
                value: 6,
            }),
        }
    );
}

#[test]
fn compile_policy_preserves_quoted_tag_value_with_dot_as_string() {
    let out =
        compile_policy("policy \"p\" { phase a { set_tag \"title\" \"Movie.Name\" } }").unwrap();
    let CompiledOperation::SetTag { value, .. } = &out.policy.phases[0].operations[0] else {
        unreachable!("expected set_tag operation");
    };

    assert_eq!(
        *value,
        crate::CompiledValue::String {
            value: "Movie.Name".to_owned()
        }
    );
}

// ---- Issue #271: V1 grammar conformance ----------------------------------
//
// Each production below is quoted verbatim from the DSL V1 grammar in
// docs/specs/voom-control-plane-design.md (lines 640-692). The suite pins the
// three forms fixed by #271 (`language == <token>`, the `media.*` field-path
// root, and the optional `where` on transcode/extract audio) plus the
// already-working productions, so grammar drift in either direction fails a
// test. `verify artifact` is covered by the #273 forms below.

/// Diagnostic codes produced by a policy that fails to compile.
fn compile_error_codes(source: &str) -> Vec<String> {
    let err = compile_policy(source).unwrap_err();
    err.diagnostics.into_iter().map(|d| d.code).collect()
}

/// Assert a policy body compiles with no diagnostics at all.
fn assert_compiles_clean(body: &str) {
    let source = format!("policy \"p\" {{ phase a {{ {body} }} }}");
    let out = compile_policy(&source)
        .unwrap_or_else(|err| panic!("`{body}` failed to compile: {:?}", err.diagnostics));
    assert!(
        out.diagnostics.is_empty(),
        "`{body}` compiled with diagnostics: {:?}",
        out.diagnostics
    );
}

fn single_op(body: &str) -> CompiledOperation {
    let source = format!("policy \"p\" {{ phase a {{ {body} }} }}");
    compile_policy(&source)
        .unwrap_or_else(|err| panic!("`{body}` failed to compile: {:?}", err.diagnostics))
        .policy
        .phases[0]
        .operations[0]
        .clone()
}

// Form 1 — `language == <quoted-token>` (track-filter).

#[test]
fn conformance_language_equals_lowers_to_single_language_in() {
    assert_eq!(
        single_op("keep audio where language == \"eng\""),
        CompiledOperation::KeepTracks {
            target: crate::TrackTarget::Audio,
            filter: Some(TrackFilter::LanguageIn {
                values: vec!["eng".to_owned()],
            }),
        }
    );
}

#[test]
fn conformance_language_equals_accepts_bare_token() {
    assert_eq!(
        single_op("keep audio where language == eng"),
        CompiledOperation::KeepTracks {
            target: crate::TrackTarget::Audio,
            filter: Some(TrackFilter::LanguageIn {
                values: vec!["eng".to_owned()],
            }),
        }
    );
}

#[test]
fn conformance_spec_example_language_equals_and_not_commentary() {
    // The spec's own example (design doc line ~590).
    let CompiledOperation::KeepTracks {
        filter: Some(TrackFilter::And { filters }),
        ..
    } = single_op("keep audio where language == \"eng\" and not commentary")
    else {
        unreachable!("expected boolean track filter");
    };
    assert_eq!(filters.len(), 2);
    assert_eq!(
        filters[0],
        TrackFilter::LanguageIn {
            values: vec!["eng".to_owned()],
        }
    );
    assert!(matches!(filters[1], TrackFilter::Not { .. }));
}

#[test]
fn conformance_language_equals_rejects_invalid_code() {
    assert!(
        compile_error_codes(
            "policy \"p\" { phase a { keep audio where language == \"english\" } }"
        )
        .contains(&"invalid_language_code".to_owned())
    );
}

// Form 2 — `media.container` / `media.duration_millis` (condition).

#[test]
fn conformance_media_container_condition_compiles_and_lowers() {
    let CompiledOperation::Conditional { condition, .. } =
        single_op("when media.container == mkv { container mkv }")
    else {
        unreachable!("expected conditional");
    };
    assert_eq!(
        condition,
        CompiledCondition::FieldComparison {
            path: vec!["media".to_owned(), "container".to_owned()],
            op: crate::ComparisonOp::Eq,
            value: crate::CompiledValue::String {
                value: "mkv".to_owned(),
            },
        }
    );
}

#[test]
fn conformance_media_duration_millis_condition_compiles_and_lowers() {
    let CompiledOperation::Conditional { condition, .. } =
        single_op("when media.duration_millis > 1000 { container mkv }")
    else {
        unreachable!("expected conditional");
    };
    assert_eq!(
        condition,
        CompiledCondition::FieldComparison {
            path: vec!["media".to_owned(), "duration_millis".to_owned()],
            op: crate::ComparisonOp::Gt,
            value: crate::CompiledValue::Number {
                value: "1000".to_owned(),
            },
        }
    );
}

#[test]
fn conformance_unknown_media_field_still_rejected() {
    assert!(
        compile_error_codes(
            "policy \"p\" { phase a { when media.bogus == mkv { container mkv } } }"
        )
        .contains(&"invalid_core_field_path".to_owned())
    );
}

// Form 3 — optional `where` on `transcode audio` / `extract audio`.

#[test]
fn conformance_transcode_audio_without_where_selects_all() {
    assert_eq!(
        single_op("transcode audio to aac"),
        CompiledOperation::TranscodeAudio {
            target_codec: "aac".to_owned(),
            container: "mkv".to_owned(),
            filter: None,
        }
    );
}

#[test]
fn conformance_extract_audio_without_where_selects_all() {
    assert_eq!(
        single_op("extract audio"),
        CompiledOperation::ExtractAudio {
            target_codec: "opus".to_owned(),
            container: "ogg".to_owned(),
            filter: None,
        }
    );
}

// Form 4 (#273) — `verify artifact`. The spec production takes no arguments;
// it compiles to the fieldless `VerifyArtifact` operation.

#[test]
fn conformance_verify_artifact_compiles_and_lowers() {
    assert_eq!(
        single_op("verify artifact"),
        CompiledOperation::VerifyArtifact
    );
}

#[test]
fn conformance_verify_artifact_serializes_with_snake_case_tag() {
    let value = serde_json::to_value(CompiledOperation::VerifyArtifact).unwrap();
    assert_eq!(value, serde_json::json!({ "type": "verify_artifact" }));
}

#[test]
fn conformance_verify_without_artifact_target_is_rejected() {
    assert_eq!(
        compile_error_codes("policy \"p\" { phase a { verify } }"),
        vec!["unknown_phase_statement_or_operation".to_owned()]
    );
}

#[test]
fn conformance_verify_artifact_rejects_extra_arguments() {
    assert_eq!(
        compile_error_codes("policy \"p\" { phase a { verify artifact now } }"),
        vec!["unknown_phase_statement_or_operation".to_owned()]
    );
}

// Form 5 (#276) — `synthesize audio from <track-filter> { codec … channels … }`.
// Adds a downmixed companion track; see ADR 0026 and the V1.1 grammar delta.
// `synthesize` is no longer a deferred keyword: a bare `synthesize` is now an
// unknown-shape operation, and the full block form compiles.

#[test]
fn conformance_bare_synthesize_is_unknown_operation() {
    assert_eq!(
        compile_error_codes("policy \"p\" { phase a { synthesize } }"),
        vec!["unknown_phase_statement_or_operation".to_owned()]
    );
}

#[test]
fn conformance_synthesize_audio_downmix_compiles_clean() {
    assert_compiles_clean("synthesize audio from codec in [\"eac3\"] { codec aac  channels 2 }");
}

#[test]
fn conformance_synthesize_audio_downmix_lowers_to_synthesize_operation() {
    assert_eq!(
        single_op("synthesize audio from channels >= 6 { codec aac  channels 2 }"),
        CompiledOperation::SynthesizeAudio {
            target_codec: "aac".to_owned(),
            container: "mkv".to_owned(),
            target_channels: 2,
            filter: Some(TrackFilter::Channels {
                op: crate::ComparisonOp::Gte,
                value: 6,
            }),
        }
    );
}

#[test]
fn conformance_synthesize_audio_rejects_bad_codec() {
    assert_eq!(
        compile_error_codes(
            "policy \"p\" { phase a { synthesize audio from commentary { codec flac  channels 2 } } }"
        ),
        vec!["unknown_phase_statement_or_operation".to_owned()]
    );
}

#[test]
fn conformance_synthesize_audio_rejects_missing_channels() {
    assert_eq!(
        compile_error_codes(
            "policy \"p\" { phase a { synthesize audio from commentary { codec aac } } }"
        ),
        vec!["unknown_phase_statement_or_operation".to_owned()]
    );
}

#[test]
fn conformance_transcode_audio_with_where_still_lowers_filter() {
    let CompiledOperation::TranscodeAudio {
        filter: Some(filter),
        ..
    } = single_op("transcode audio to aac where language == \"eng\"")
    else {
        unreachable!("expected filter");
    };
    assert_eq!(
        filter,
        TrackFilter::LanguageIn {
            values: vec!["eng".to_owned()],
        }
    );
}

// Regression guard: #271 changes only transcode/extract audio. `keep`/`remove`
// already accept an omitted `where` (pre-existing behavior, out of #271 scope);
// this pins that #271 does not alter it.

#[test]
fn conformance_keep_audio_without_where_unchanged() {
    assert_compiles_clean("keep audio");
}

// The already-working V1 productions still compile clean, so a regression in the
// validator surfaces here too.

#[test]
fn conformance_working_v1_productions_compile_clean() {
    for body in [
        "container mkv",
        "transcode video to hevc",
        "keep audio where language in [eng, und]",
        "keep subtitle where codec in [srt]",
        "remove audio where commentary",
        "keep audio where channels >= 6",
        "keep attachment where font",
        "keep subtitle where title contains \"Director\"",
        "order tracks [video, audio, subtitle]",
        "defaults audio first",
        "defaults subtitle preserve",
        "when exists audio { container mkv }",
        "when count audio >= 2 { container mkv }",
        "when video.codec == hevc { container mkv }",
        "when video.width >= 1920 { container mkv }",
        "verify artifact",
    ] {
        assert_compiles_clean(body);
    }
}

// ---- Issue #292: spec/impl divergence — `order tracks` target list and
// optional `where` on keep/remove --------------------------------------------
//
// The V1 grammar (docs/specs/voom-control-plane-design.md lines 646-648) was
// corrected to match the validator, which is the intended contract:
//
//   keep audio|subtitle|attachment [where <track-filter>]
//   remove audio|subtitle|attachment [where <track-filter>]
//   order tracks [<track-target>, ...]
//
// These cases pin the aligned spec + impl so grammar drift in either direction
// fails a test.

#[test]
fn conformance_order_tracks_requires_target_list() {
    // The base group form is `order tracks [<track-target>, ...]`; a bare
    // `order tracks` with no target list (and no `where` filter) is rejected.
    assert!(
        compile_error_codes("policy \"p\" { phase a { order tracks } }")
            .contains(&"invalid_track_target".to_owned())
    );
}

#[test]
fn conformance_order_tracks_with_target_list_compiles_clean() {
    assert_compiles_clean("order tracks [video, audio]");
}

#[test]
fn conformance_keep_without_where_compiles_clean() {
    // `where` is optional: an omitted filter selects all tracks of the kind.
    assert_compiles_clean("keep audio");
}

#[test]
fn conformance_remove_without_where_compiles_clean() {
    assert_compiles_clean("remove subtitle");
}
