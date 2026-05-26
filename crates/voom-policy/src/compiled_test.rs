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
fn compiles_sprint12_video_hevc_transcode_operation() {
    let policy = crate::compile_policy("policy \"p\" { phase a { transcode video to hevc {} } }")
        .unwrap()
        .policy;

    assert_eq!(
        policy.phases[0].operations[0],
        CompiledOperation::TranscodeVideo {
            target_codec: "hevc".to_owned(),
            container: "mkv".to_owned(),
            profile: "default-hevc".to_owned(),
        }
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
