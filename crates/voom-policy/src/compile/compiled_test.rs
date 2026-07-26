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

    assert!(error.to_string().contains("config contains unknown field"));
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
        CompiledOperation::TranscodeVideo {
            target_codec: "hevc".to_owned(),
            container: "mkv".to_owned(),
            profile: crate::VideoProfileRef::Named("default-hevc".to_owned()),
            resolved_profile: None,
        }
    );
}

#[test]
fn lowers_bare_hevc_to_named_default() {
    let op = compile_single_op("transcode video to hevc");
    let CompiledOperation::TranscodeVideo {
        target_codec,
        container,
        profile,
        ..
    } = op
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
    let CompiledOperation::TranscodeVideo {
        profile,
        target_codec,
        ..
    } = op
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
    let CompiledOperation::TranscodeVideo { profile, .. } = op else {
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
    let CompiledOperation::TranscodeVideo {
        profile, container, ..
    } = op
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
    let CompiledOperation::TranscodeVideo { profile, .. } = op else {
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
        CompiledOperation::TranscodeAudio {
            target_codec: "aac".to_owned(),
            container: "mkv".to_owned(),
            filter: Some(TrackFilter::LanguageIn {
                values: vec!["eng".to_owned(), "und".to_owned()],
            }),
        }
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
        CompiledOperation::ExtractAudio {
            target_codec: "opus".to_owned(),
            container: "ogg".to_owned(),
            filter: Some(TrackFilter::Commentary),
        }
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
