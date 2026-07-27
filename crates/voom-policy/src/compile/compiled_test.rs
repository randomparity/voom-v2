use super::*;

#[test]
fn source_hash_uses_exact_bytes() {
    let a = source_hash("policy \"p\" { phase a {} }");
    let b = source_hash("policy \"p\" {\n phase a {}\n}");
    assert_ne!(a, b);
}

#[test]
fn compiled_json_is_deterministic() {
    let policy = CompiledPolicy::minimal_for_test("p", "hash");
    let first = deterministic_json(&policy).unwrap();
    let second = deterministic_json(&policy).unwrap();
    assert_eq!(first, second);
}

#[test]
fn compiled_policy_rejects_unknown_root_fields() {
    let policy = CompiledPolicy::minimal_for_test("p", "hash");
    let mut json = deterministic_json(&policy).unwrap();
    json["future_root"] = serde_json::json!(true);

    let error = serde_json::from_value::<CompiledPolicy>(json).unwrap_err();

    assert!(error.to_string().contains("unknown field `future_root`"));
}

fn assert_tagged_wire_contract<T>(wires: &[serde_json::Value])
where
    T: serde::de::DeserializeOwned + serde::Serialize + std::fmt::Debug,
{
    for wire in wires {
        let decoded: T = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(serde_json::to_value(decoded).unwrap(), *wire);

        let mut unknown = wire.clone();
        unknown["future_field"] = serde_json::json!(true);
        let error = serde_json::from_value::<T>(unknown).unwrap_err();
        assert!(
            error.to_string().contains("unknown field `future_field`"),
            "{wire}: {error}"
        );
    }
}

#[test]
fn all_compiled_operation_variants_preserve_wire_and_reject_unknown_fields() {
    let wires = [
        serde_json::json!({"type": "set_container", "container": "mkv"}),
        serde_json::json!({"type": "keep_tracks", "target": "audio", "filter": null}),
        serde_json::json!({"type": "remove_tracks", "target": "audio", "filter": null}),
        serde_json::json!({
            "type": "reorder_tracks",
            "targets": ["audio"],
            "head_filter": {"type": "commentary"}
        }),
        serde_json::json!({
            "type": "set_defaults",
            "target": "audio",
            "strategy": "first",
            "filter": {"type": "language_in", "values": ["eng"]}
        }),
        serde_json::json!({"type": "clear_track_actions", "target": "audio"}),
        serde_json::json!({"type": "clear_tags"}),
        serde_json::json!({
            "type": "set_tag",
            "key": "title",
            "value": {"type": "string", "value": "Example"}
        }),
        serde_json::json!({"type": "delete_tag", "key": "title"}),
        serde_json::json!({
            "type": "transcode_video",
            "target_codec": "hevc",
            "container": "mkv",
            "profile": {"named": "default-hevc"},
            "resolved_profile": {
                "name": "default-hevc",
                "target_codec": "hevc",
                "encoder": "libx265",
                "crf": 23,
                "preset": "medium"
            }
        }),
        serde_json::json!({
            "type": "transcode_audio",
            "target_codec": "aac",
            "container": "mkv",
            "filter": null
        }),
        serde_json::json!({
            "type": "extract_audio",
            "target_codec": "opus",
            "container": "ogg",
            "filter": null
        }),
        serde_json::json!({
            "type": "synthesize_audio",
            "target_codec": "aac",
            "container": "mkv",
            "target_channels": 2,
            "filter": null
        }),
        serde_json::json!({"type": "verify_artifact"}),
        serde_json::json!({
            "type": "conditional",
            "condition": {"type": "predicate", "name": "ready"},
            "operations": [{"type": "clear_tags"}]
        }),
        serde_json::json!({
            "type": "rules",
            "mode": "first",
            "rules": [{
                "name": "ready-rule",
                "condition": {"type": "predicate", "name": "ready"},
                "operations": [{"type": "clear_tags"}]
            }]
        }),
    ];

    assert_eq!(wires.len(), 16);
    assert_tagged_wire_contract::<CompiledOperation>(&wires);
}

#[test]
fn all_track_filter_variants_preserve_wire_and_reject_unknown_fields() {
    let wires = [
        serde_json::json!({"type": "language_in", "values": ["eng"]}),
        serde_json::json!({"type": "codec_in", "values": ["aac"]}),
        serde_json::json!({"type": "channels", "op": "gte", "value": 2}),
        serde_json::json!({"type": "commentary"}),
        serde_json::json!({"type": "forced"}),
        serde_json::json!({"type": "default"}),
        serde_json::json!({"type": "font"}),
        serde_json::json!({"type": "title_contains", "value": "Director"}),
        serde_json::json!({"type": "title_matches", "value": "^Director"}),
        serde_json::json!({
            "type": "not",
            "inner": {"type": "commentary"}
        }),
        serde_json::json!({
            "type": "and",
            "filters": [{"type": "commentary"}]
        }),
        serde_json::json!({
            "type": "or",
            "filters": [{"type": "forced"}]
        }),
    ];

    assert_eq!(wires.len(), 12);
    assert_tagged_wire_contract::<TrackFilter>(&wires);
}

#[test]
fn all_compiled_condition_variants_preserve_wire_and_reject_unknown_fields() {
    let wires = [
        serde_json::json!({"type": "exists", "target": "audio", "filter": null}),
        serde_json::json!({
            "type": "count",
            "target": "audio",
            "op": "gte",
            "value": 1
        }),
        serde_json::json!({
            "type": "field_comparison",
            "path": ["media", "container"],
            "op": "eq",
            "value": {"type": "string", "value": "mkv"}
        }),
        serde_json::json!({"type": "field_exists", "path": ["media", "container"]}),
        serde_json::json!({"type": "predicate", "name": "ready"}),
        serde_json::json!({
            "type": "not",
            "inner": {"type": "predicate", "name": "ready"}
        }),
        serde_json::json!({
            "type": "and",
            "conditions": [{"type": "predicate", "name": "ready"}]
        }),
        serde_json::json!({
            "type": "or",
            "conditions": [{"type": "predicate", "name": "ready"}]
        }),
    ];

    assert_eq!(wires.len(), 8);
    assert_tagged_wire_contract::<CompiledCondition>(&wires);
}

#[test]
fn all_compiled_value_variants_preserve_wire_and_reject_unknown_fields() {
    let wires = [
        serde_json::json!({"type": "string", "value": "mkv"}),
        serde_json::json!({"type": "number", "value": "2"}),
        serde_json::json!({"type": "boolean", "value": true}),
        serde_json::json!({"type": "field_path", "path": ["media", "container"]}),
        serde_json::json!({
            "type": "list",
            "values": [
                {"type": "string", "value": "mkv"},
                {"type": "list", "values": [{"type": "boolean", "value": true}]}
            ]
        }),
    ];

    assert_eq!(wires.len(), 5);
    assert_tagged_wire_contract::<CompiledValue>(&wires);
}

#[test]
fn reachable_compiled_structs_reject_unknown_fields() {
    let phase_error = serde_json::from_value::<CompiledPhase>(serde_json::json!({
        "name": "normalize",
        "depends_on": [],
        "run_if": null,
        "skip_if": null,
        "on_error": null,
        "operations": [],
        "future_phase": true
    }))
    .unwrap_err();
    assert!(
        phase_error
            .to_string()
            .contains("unknown field `future_phase`")
    );

    let provenance_error = serde_json::from_value::<PolicyProvenance>(serde_json::json!({
        "compiler": "voom-policy",
        "format": "sprint4-v2",
        "flags": {},
        "future_provenance": true
    }))
    .unwrap_err();
    assert!(
        provenance_error
            .to_string()
            .contains("unknown field `future_provenance`")
    );

    let rule_error = serde_json::from_value::<CompiledRule>(serde_json::json!({
        "name": "ready-rule",
        "condition": null,
        "operations": [],
        "future_rule": true
    }))
    .unwrap_err();
    assert!(
        rule_error
            .to_string()
            .contains("unknown field `future_rule`")
    );
}

#[test]
fn compiled_run_if_keeps_the_published_predicate_wire_shape() {
    let json = serde_json::json!({
        "type": "predicate",
        "name": "modified normalize"
    });

    let gate: CompiledRunIf = serde_json::from_value(json.clone()).unwrap();
    assert_eq!(
        gate,
        CompiledRunIf {
            trigger: RunIfTrigger::Modified,
            phase: "normalize".to_owned(),
        }
    );
    assert_eq!(serde_json::to_value(gate).unwrap(), json);
}

#[test]
fn compiled_run_if_rejects_noncanonical_predicate_names() {
    for name in ["modified", "completed inspect extra", "changed inspect"] {
        let error = serde_json::from_value::<CompiledRunIf>(serde_json::json!({
            "type": "predicate",
            "name": name
        }))
        .unwrap_err();

        assert!(error.to_string().contains("compiled run_if"));
    }
}

#[test]
fn compiled_run_if_rejects_unknown_fields_through_its_strict_wire() {
    let error = serde_json::from_value::<CompiledRunIf>(serde_json::json!({
        "type": "predicate",
        "name": "modified normalize",
        "future_run_if": true
    }))
    .unwrap_err();

    assert!(error.to_string().contains("unknown field `future_run_if`"));
}

#[test]
fn published_config_lowers_to_typed_defaults_and_fills_omitted_phases() {
    let policy = crate::compile_policy(
        "policy \"p\" { \
         config { languages: [\"eng\", \"und\"] on_error: continue } \
         phase inherits {} \
         phase overrides { depends_on: [inherits] on_error: abort } \
         }",
    )
    .unwrap()
    .policy;

    assert_eq!(policy.config.languages, ["eng", "und"]);
    assert_eq!(policy.config.on_error, Some(ErrorStrategy::Continue));
    assert_eq!(policy.phases[0].on_error, Some(ErrorStrategy::Continue));
    assert_eq!(policy.phases[1].on_error, Some(ErrorStrategy::Abort));
    let json = deterministic_json(&policy).unwrap();
    assert_eq!(
        json["config"]["languages"],
        serde_json::json!(["eng", "und"])
    );
    assert_eq!(json["config"]["on_error"], "continue");
}

#[test]
fn execution_default_application_is_idempotent() {
    let mut policy = CompiledPolicy::minimal_for_test("p", "hash");
    policy.config.on_error = Some(ErrorStrategy::Continue);
    policy.phases.push(CompiledPhase {
        name: "a".to_owned(),
        depends_on: Vec::new(),
        run_if: None,
        skip_if: None,
        on_error: None,
        operations: Vec::new(),
    });

    policy.apply_execution_defaults();
    let once = policy.clone();
    policy.apply_execution_defaults();

    assert_eq!(policy, once);
    assert_eq!(policy.phases[0].on_error, Some(ErrorStrategy::Continue));
}

#[test]
fn legacy_compiled_config_deserializes_and_applies_defaults() {
    let json = include_str!("../../fixtures/compiled/legacy-policy-config-v2.json");
    let mut policy: CompiledPolicy = serde_json::from_str(json).unwrap();

    assert_eq!(policy.config.languages, ["eng", "und"]);
    assert_eq!(policy.config.on_error, Some(ErrorStrategy::Continue));
    assert_eq!(policy.phases[0].on_error, None);
    policy.apply_execution_defaults();
    assert_eq!(policy.phases[0].on_error, Some(ErrorStrategy::Continue));
    assert_eq!(policy.phases[1].on_error, Some(ErrorStrategy::Abort));
}

#[test]
fn compiled_policy_without_config_field_remains_readable() {
    let policy = CompiledPolicy::minimal_for_test("p", "hash");
    let mut json = deterministic_json(&policy).unwrap();
    json.as_object_mut().unwrap().remove("config");

    let decoded: CompiledPolicy = serde_json::from_value(json).unwrap();

    assert_eq!(decoded.config, CompiledConfig::default());
}

#[test]
fn historical_compiled_title_matches_filter_remains_readable() {
    let json = serde_json::json!({
        "type": "title_matches",
        "value": "^Director"
    });

    let filter: TrackFilter = serde_json::from_value(json.clone()).unwrap();

    assert_eq!(
        filter,
        TrackFilter::TitleMatches(crate::compiled::TitleMatchesTrackFilter {
            value: "^Director".to_owned(),
        })
    );
    assert_eq!(serde_json::to_value(filter).unwrap(), json);
}

#[test]
fn compiled_config_rejects_invalid_typed_and_legacy_languages() {
    let cases = [
        serde_json::json!({"languages": ["EN"]}),
        serde_json::json!({"languages": ["en"]}),
        serde_json::json!({"languages": [7]}),
        serde_json::json!({"languages": "languages audio: [EN]"}),
        serde_json::json!({"languages": "language_preferences: [eng]"}),
        serde_json::json!({"languages": "languages: [eng] trailing"}),
        serde_json::json!({"languages": "languages: [\"eng]"}),
    ];

    for value in cases {
        let error = serde_json::from_value::<CompiledConfig>(value).unwrap_err();
        assert!(error.to_string().contains("config.languages"));
    }
}

#[test]
fn legacy_compiled_config_reads_previously_published_skip_value() {
    let config: CompiledConfig =
        serde_json::from_value(serde_json::json!({"on_error": "on_error skip"})).unwrap();

    assert_eq!(config.on_error, Some(ErrorStrategy::Skip));
}

#[test]
fn compiled_config_reads_previous_language_targets_and_whitespace() {
    let cases = [
        (
            serde_json::json!({
                "languages": "languages subtitle : [eng, \"und\"]",
                "on_error": "on_error\tcontinue"
            }),
            ErrorStrategy::Continue,
        ),
        (
            serde_json::json!({
                "languages": "languages : [\"eng\"]",
                "on_error": "on_error \t: \t abort"
            }),
            ErrorStrategy::Abort,
        ),
    ];

    for (value, strategy) in cases {
        let config: CompiledConfig = serde_json::from_value(value).unwrap();
        assert_eq!(config.languages[0], "eng");
        assert_eq!(config.on_error, Some(strategy));
    }
}

#[test]
fn compiled_config_rejects_unknown_durable_fields() {
    let error =
        serde_json::from_value::<CompiledConfig>(serde_json::json!({"renamed": "continue"}))
            .unwrap_err();

    assert!(error.to_string().contains("unknown field `renamed`"));
}

#[test]
fn compiled_config_rejects_present_null_compatibility_fields() {
    for value in [
        serde_json::json!({"languages": null}),
        serde_json::json!({"on_error": null}),
    ] {
        assert!(serde_json::from_value::<CompiledConfig>(value).is_err());
    }
}

#[test]
fn configured_languages_do_not_rewrite_explicit_track_filters() {
    let without_config = compile_single_op("defaults audio where language == \"spa\"");
    let policy = crate::compile_policy(
        "policy \"p\" { config { languages: [\"eng\"] } \
         phase a { defaults audio where language == \"spa\" } }",
    )
    .unwrap()
    .policy;

    assert_eq!(policy.phases[0].operations[0], without_config);
}

#[test]
fn required_tools_preserve_source_order_and_remove_duplicates() {
    let policy = crate::compile_policy(
        "policy \"p\" { metadata { \
         requires_tools: [ffprobe, ffmpeg, ffprobe, mkvtoolnix] \
         } phase a { container mkv } }",
    )
    .unwrap()
    .policy;

    assert_eq!(
        policy.required_tools().unwrap(),
        vec![
            PolicyTool::Ffprobe,
            PolicyTool::Ffmpeg,
            PolicyTool::Mkvtoolnix,
        ]
    );
    assert!(policy.warnings.is_empty());
}

#[test]
fn required_tools_accept_legacy_canonical_json_strings() {
    let mut policy = CompiledPolicy::minimal_for_test("p", "hash");
    policy.metadata.insert(
        "requires_tools".to_owned(),
        serde_json::json!(["mkvtoolnix", "ffmpeg"]),
    );

    assert_eq!(
        policy.required_tools().unwrap(),
        vec![PolicyTool::Mkvtoolnix, PolicyTool::Ffmpeg]
    );
}

#[test]
fn required_tools_reject_malformed_legacy_json() {
    let cases = [
        (
            serde_json::json!("ffmpeg"),
            "metadata.requires_tools must be an array",
        ),
        (
            serde_json::json!(["ffmpeg", 7]),
            "metadata.requires_tools[1] must be a string",
        ),
        (
            serde_json::json!(["mediainfo"]),
            "metadata.requires_tools[0] contains unknown tool `mediainfo`",
        ),
    ];

    for (value, message) in cases {
        let mut policy = CompiledPolicy::minimal_for_test("p", "hash");
        policy.metadata.insert("requires_tools".to_owned(), value);
        assert_eq!(policy.required_tools().unwrap_err().to_string(), message);
    }
}

#[test]
fn required_tools_do_not_add_a_compiled_policy_field() {
    let policy = crate::compile_policy(
        "policy \"p\" { metadata { requires_tools: [ffmpeg] } \
         phase a { container mkv } }",
    )
    .unwrap()
    .policy;
    let json = deterministic_json(&policy).unwrap();

    assert!(json.get("required_tools").is_none());
    assert_eq!(
        json["metadata"]["requires_tools"],
        serde_json::json!(["ffmpeg"])
    );
    assert_eq!(json["schema_version"], 2);
}

fn compile_single_op(operation: &str) -> CompiledOperation {
    let source = format!("policy \"p\" {{ phase a {{ {operation} }} }}");
    let policy = crate::compile_policy(&source).unwrap().policy;
    policy.phases[0].operations[0].clone()
}

#[test]
fn compiles_sprint12_video_hevc_transcode_operation() {
    assert_eq!(
        compile_single_op("transcode video to hevc"),
        CompiledOperation::TranscodeVideo(crate::compiled::CompiledTranscodeVideoOperation {
            target_codec: "hevc".to_owned(),
            container: "mkv".to_owned(),
            profile: crate::VideoProfileRef::Named("default-hevc".to_owned()),
            resolved_profile: None,
        })
    );
}

#[test]
fn lowers_bare_hevc_to_named_default() {
    let op = compile_single_op("transcode video to hevc");
    let CompiledOperation::TranscodeVideo(crate::compiled::CompiledTranscodeVideoOperation {
        target_codec,
        container,
        profile,
        ..
    }) = op
    else {
        panic!("expected TranscodeVideo");
    };
    assert_eq!(target_codec, "hevc");
    assert_eq!(container, "mkv");
    assert_eq!(
        profile,
        crate::VideoProfileRef::Named("default-hevc".to_owned())
    );
}

#[test]
fn lowers_bare_av1_to_named_default() {
    let op = compile_single_op("transcode video to av1");
    let CompiledOperation::TranscodeVideo(crate::compiled::CompiledTranscodeVideoOperation {
        profile,
        target_codec,
        ..
    }) = op
    else {
        panic!("expected TranscodeVideo");
    };
    assert_eq!(target_codec, "av1");
    assert_eq!(
        profile,
        crate::VideoProfileRef::Named("default-av1".to_owned())
    );
}

#[test]
fn lowers_using_profile_to_named() {
    let op = compile_single_op("transcode video to hevc using profile \"hevc-archive\"");
    let CompiledOperation::TranscodeVideo(crate::compiled::CompiledTranscodeVideoOperation {
        profile,
        ..
    }) = op
    else {
        panic!("expected TranscodeVideo");
    };
    assert_eq!(
        profile,
        crate::VideoProfileRef::Named("hevc-archive".to_owned())
    );
}

#[test]
fn lowers_inline_to_inline_settings() {
    let op = compile_single_op(
        "transcode video to av1 { encoder: libsvtav1 crf: 28 preset: 6 output_container: mp4 }",
    );
    let CompiledOperation::TranscodeVideo(crate::compiled::CompiledTranscodeVideoOperation {
        profile,
        container,
        ..
    }) = op
    else {
        panic!("expected TranscodeVideo");
    };
    assert_eq!(container, "mp4");
    let crate::VideoProfileRef::Inline(s) = profile else {
        panic!("expected inline");
    };
    assert_eq!(s.encoder, "libsvtav1");
    assert_eq!(s.crf, 28);
    assert_eq!(s.output_container.as_deref(), Some("mp4"));
}

#[test]
fn freshly_compiled_transcode_omits_resolved_profile_key() {
    let source = "policy \"p\" { phase a { transcode video to hevc } }";
    let policy = crate::compile_policy(source).unwrap().policy;
    let value = crate::deterministic_json(&policy).unwrap();
    let op = &value["phases"][0]["operations"][0];
    assert_eq!(op["type"], "transcode_video");
    assert!(op.get("resolved_profile").is_none());
}

#[test]
fn legacy_bare_string_profile_round_trips_through_compiled_json() {
    let op: CompiledOperation = serde_json::from_value(serde_json::json!({
        "type": "transcode_video",
        "target_codec": "hevc",
        "container": "mkv",
        "profile": "default-hevc"
    }))
    .unwrap();
    let CompiledOperation::TranscodeVideo(crate::compiled::CompiledTranscodeVideoOperation {
        profile,
        ..
    }) = op
    else {
        panic!("expected TranscodeVideo");
    };
    assert_eq!(
        profile,
        crate::VideoProfileRef::Named("default-hevc".to_owned())
    );
}

#[test]
fn compiles_sprint14_audio_aac_transcode_operation() {
    let policy = crate::compile_policy(
        "policy \"p\" { phase a { transcode audio to aac where language in [\"eng\", \"und\"] } }",
    )
    .unwrap()
    .policy;

    assert_eq!(
        policy.phases[0].operations[0],
        CompiledOperation::TranscodeAudio(crate::compiled::CompiledTranscodeAudioOperation {
            target_codec: "aac".to_owned(),
            container: "mkv".to_owned(),
            filter: Some(TrackFilter::LanguageIn(
                crate::compiled::LanguageInTrackFilter {
                    values: vec!["eng".to_owned(), "und".to_owned()],
                }
            )),
        })
    );
}

#[test]
fn compiles_sprint14_audio_extract_operation() {
    let policy =
        crate::compile_policy("policy \"p\" { phase a { extract audio where commentary } }")
            .unwrap()
            .policy;

    assert_eq!(
        policy.phases[0].operations[0],
        CompiledOperation::ExtractAudio(crate::compiled::CompiledExtractAudioOperation {
            target_codec: "opus".to_owned(),
            container: "ogg".to_owned(),
            filter: Some(TrackFilter::Commentary(
                crate::compiled::CommentaryTrackFilter {}
            )),
        })
    );
}

#[test]
fn rejects_invalid_boolean_audio_filter_children() {
    let err = crate::compile_policy(
        "policy \"p\" { phase a { transcode audio to aac where language in [\"eng\"] or banana } }",
    )
    .unwrap_err();

    assert!(
        err.diagnostics.iter().any(|diagnostic| {
            diagnostic.code.as_str() == "unknown_phase_statement_or_operation"
        })
    );
}
