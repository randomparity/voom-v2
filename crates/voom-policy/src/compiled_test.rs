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
        "policy \"p\" { phase a { transcode audio to aac where lang in [eng, und] } }",
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
        "policy \"p\" { phase a { transcode audio to aac where lang in [eng] or banana } }",
    )
    .unwrap_err();

    assert!(
        err.diagnostics.iter().any(|diagnostic| {
            diagnostic.code.as_str() == "unknown_phase_statement_or_operation"
        })
    );
}
