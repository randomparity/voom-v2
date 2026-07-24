#![expect(
    clippy::panic,
    reason = "integration test setup should fail loudly with fixture context"
)]

use std::collections::BTreeSet;

use voom_policy::{CompiledOperation, CompiledPolicy, compile_policy};

const FIXTURE_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/policies/");
const MATRIX_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/policies/published-grammar-coverage.md"
);
const POLICY_FILES: [&str; 4] = [
    "published-grammar-core.voom",
    "published-grammar-tracks.voom",
    "published-grammar-audio.voom",
    "published-grammar-control-flow.voom",
];
const UNPUBLISHED_FORMS: [&str; 10] = [
    "extends ",
    "set_tag ",
    "delete_tag ",
    "clear_tags",
    "actions ",
    "lang ",
    "languages audio",
    "title matches",
    "attachments ",
    "subtitles ",
];
const CORE_FORMS: [&str; 6] = [
    "metadata {\n    requires_tools: [ffmpeg, ffprobe, mkvtoolnix]",
    "languages: [\"eng\", \"und\"]",
    "on_error: abort",
    "depends_on: [containerize]",
    "transcode video to hevc using profile \"default-hevc\"",
    "verify artifact",
];
const TRACK_FORMS: [&str; 19] = [
    "keep audio where (language == \"eng\" and channels >= 2) or default",
    "remove audio where commentary",
    "keep subtitle where language in [\"eng\", \"und\"] or forced",
    "remove subtitle where title contains \"Signs\" and not forced",
    "keep attachment where font",
    "remove attachment where not font",
    "order tracks [video, audio, subtitle, attachment]",
    "order tracks [video, audio, subtitle, attachment] where default",
    "order tracks where forced",
    "defaults audio first",
    "defaults subtitle best",
    "defaults audio none",
    "defaults subtitle preserve",
    "defaults audio where language == \"eng\"",
    "defaults subtitle where forced",
    "when video.width < 640 {\n      remove subtitle",
    "when video.width < 1280 {\n      defaults audio none",
    "when count subtitle == 1 {\n      defaults subtitle best",
    "when count subtitle > 1 {\n      defaults subtitle preserve",
];
const AUDIO_FORMS: [&str; 10] = [
    "transcode audio to aac",
    "transcode audio to opus where language in [\"eng\", \"und\"] and not commentary",
    "transcode audio to eac3 where channels >= 6",
    "synthesize audio from channels >= 6 {\n      codec aac",
    "synthesize audio from channels >= 6 {\n      codec opus",
    "synthesize audio from channels >= 6 {\n      codec eac3",
    "channels 2",
    "extract audio where codec in [\"aac\"]",
    "extract audio\n",
    "verify artifact",
];
const CONTROL_FORMS: [&str; 20] = [
    "on_error: continue",
    "not media.container == mkv",
    "not video.codec == hevc",
    "media.duration_millis != 0",
    "video.width >= 1280",
    "video.height <= 2160",
    "video.bitrate > 1000000",
    "run_if completed inspect",
    "skip when not exists audio",
    "on_error: abort",
    "rules first",
    "rule \"transcode video\" when not video.codec == hevc",
    "run_if modified normalize",
    "skip when count audio < 2",
    "rules all",
    "rule \"retain subtitles\" when exists subtitle",
    "rule \"default audio\" when count audio >= 2",
    "rule \"retain forced subtitles\" when count subtitle > 1",
    "keep subtitle\n",
    "keep subtitle where forced or default",
];
const CORE_COMPILED: [&str; 4] = [
    "set_container|container=mkv",
    "transcode_video|container=mkv|profile=present|target_codec=hevc",
    "verify_artifact",
    "phase:encode|depends_on=[\"containerize\"]",
];
const TRACK_COMPILED: [&str; 22] = [
    "keep_tracks|filter=present|target=audio",
    "remove_tracks|filter=present|target=audio",
    "keep_tracks|filter=present|target=subtitle",
    "remove_tracks|filter=present|target=subtitle",
    "remove_tracks|filter=null|target=subtitle",
    "keep_tracks|filter=present|target=attachment",
    "remove_tracks|filter=present|target=attachment",
    "reorder_tracks|targets=[\"video\",\"audio\",\"subtitle\",\"attachment\"]",
    "reorder_tracks|head_filter=present|targets=[\"video\",\"audio\",\"subtitle\",\"attachment\"]",
    "reorder_tracks|head_filter=present|targets=[]",
    "set_defaults|strategy=first|target=audio",
    "set_defaults|strategy=best|target=subtitle",
    "set_defaults|strategy=none|target=audio",
    "set_defaults|strategy=preserve|target=subtitle",
    "set_defaults|filter=present|strategy=preserve|target=audio",
    "set_defaults|filter=present|strategy=preserve|target=subtitle",
    "field_comparison|op=lt|path=[\"video\",\"width\"]|value=present",
    "count|op=eq|target=subtitle|value=1",
    "count|op=gt|target=subtitle|value=1",
    "language_in",
    "title_contains",
    "font",
];
const AUDIO_COMPILED: [&str; 11] = [
    "transcode_audio|container=mkv|filter=null|target_codec=aac",
    "transcode_audio|container=mkv|filter=present|target_codec=opus",
    "transcode_audio|container=mkv|filter=present|target_codec=eac3",
    "synthesize_audio|container=mkv|filter=present|target_channels=2|target_codec=aac",
    "synthesize_audio|container=mkv|filter=present|target_channels=2|target_codec=opus",
    "synthesize_audio|container=mkv|filter=present|target_channels=2|target_codec=eac3",
    "extract_audio|container=ogg|filter=present|target_codec=opus",
    "extract_audio|container=ogg|filter=null|target_codec=opus",
    "language_in",
    "codec_in",
    "channels|op=gte|value=6",
];
const CONTROL_COMPILED: [&str; 18] = [
    "phase:inspect|on_error=continue",
    "phase:normalize|on_error=abort",
    "predicate|name=completed inspect",
    "predicate|name=modified normalize",
    "field_comparison|op=eq|path=[\"media\",\"container\"]|value=present",
    "field_comparison|op=eq|path=[\"video\",\"codec\"]|value=present",
    "field_comparison|op=ne|path=[\"media\",\"duration_millis\"]|value=present",
    "field_comparison|op=gte|path=[\"video\",\"width\"]|value=present",
    "field_comparison|op=lte|path=[\"video\",\"height\"]|value=present",
    "field_comparison|op=gt|path=[\"video\",\"bitrate\"]|value=present",
    "exists|filter=null|target=subtitle",
    "count|op=gte|target=audio|value=2",
    "count|op=gt|target=subtitle|value=1",
    "conditional|condition=present|operations=present",
    "rules|mode=first|rules=present",
    "rules|mode=all|rules=present",
    "keep_tracks|filter=null|target=subtitle",
    "keep_tracks|filter=present|target=subtitle",
];

#[test]
fn canonical_published_grammar_policies_compile_and_define_execution_oracles() {
    let matrix = std::fs::read_to_string(MATRIX_PATH)
        .unwrap_or_else(|error| panic!("failed to read {MATRIX_PATH}: {error}"));

    for fixture in POLICY_FILES {
        let path = format!("{FIXTURE_DIR}{fixture}");
        let source = std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("failed to read {path}: {error}"));
        let output = compile_policy(&source)
            .unwrap_or_else(|error| panic!("{fixture} failed to compile: {:?}", error.diagnostics));

        assert!(
            output
                .policy
                .phases
                .iter()
                .flat_map(|phase| &phase.operations)
                .any(has_mutation),
            "{fixture} must define a mutation"
        );
        assert!(
            output
                .policy
                .phases
                .iter()
                .flat_map(|phase| &phase.operations)
                .any(has_verification),
            "{fixture} must verify an artifact"
        );
        for unpublished in UNPUBLISHED_FORMS {
            assert!(
                !source.contains(unpublished),
                "{fixture} contains unpublished source form `{unpublished}`"
            );
        }
        assert!(
            matrix.contains(fixture),
            "{fixture} must have a coverage-matrix execution oracle"
        );
        for required in required_forms(fixture) {
            assert!(
                source.contains(required),
                "{fixture} is missing published form `{required}`"
            );
        }
        assert_compiled_coverage(fixture, &output.policy);
    }

    assert_complete_matrix(&matrix);
}

fn has_mutation(operation: &CompiledOperation) -> bool {
    match operation {
        CompiledOperation::VerifyArtifact => false,
        CompiledOperation::Conditional { operations, .. } => operations.iter().any(has_mutation),
        CompiledOperation::Rules { rules, .. } => rules
            .iter()
            .flat_map(|rule| &rule.operations)
            .any(has_mutation),
        _ => true,
    }
}

fn has_verification(operation: &CompiledOperation) -> bool {
    match operation {
        CompiledOperation::VerifyArtifact => true,
        CompiledOperation::Conditional { operations, .. } => {
            operations.iter().any(has_verification)
        }
        CompiledOperation::Rules { rules, .. } => rules
            .iter()
            .flat_map(|rule| &rule.operations)
            .any(has_verification),
        _ => false,
    }
}

fn required_forms(fixture: &str) -> &'static [&'static str] {
    match fixture {
        "published-grammar-core.voom" => &CORE_FORMS,
        "published-grammar-tracks.voom" => &TRACK_FORMS,
        "published-grammar-audio.voom" => &AUDIO_FORMS,
        "published-grammar-control-flow.voom" => &CONTROL_FORMS,
        _ => panic!("missing expectations for canonical fixture {fixture}"),
    }
}

fn assert_complete_matrix(matrix: &str) {
    for (prefix, count) in [('S', 17), ('O', 23), ('C', 15), ('T', 13)] {
        for number in 1..=count {
            let id = format!("{prefix}{number:02}");
            let marker = format!("| {id} |");
            assert_eq!(matrix.matches(&marker).count(), 1, "matrix row {id}");
            let row = matrix
                .lines()
                .find(|line| line.starts_with(&marker))
                .unwrap_or_else(|| panic!("missing matrix row {id}"));
            let row = row.replace(r"\|", "");
            let cells: Vec<_> = row.split('|').map(str::trim).collect();
            assert_eq!(cells.len(), 7, "matrix row {id} must have five cells");
            assert!(
                cells[1..6].iter().all(|cell| !cell.is_empty()),
                "matrix row {id} has an empty cell"
            );
        }
    }
    assert_eq!(matrix.matches("- Expected mutation:").count(), 4);
    assert_eq!(matrix.matches("- Oracle:").count(), 4);
    for input in ["C1", "T1", "A1", "F1"] {
        assert!(
            matrix.contains(&format!("### {input} —")),
            "matrix is missing generated input {input}"
        );
    }
}

fn assert_compiled_coverage(fixture: &str, policy: &CompiledPolicy) {
    let actual = compiled_coverage(policy);
    for expected in required_compiled_coverage(fixture) {
        assert!(
            actual.contains(*expected),
            "{fixture} does not lower required coverage `{expected}`; actual: {actual:?}"
        );
    }
}

fn required_compiled_coverage(fixture: &str) -> &'static [&'static str] {
    match fixture {
        "published-grammar-core.voom" => &CORE_COMPILED,
        "published-grammar-tracks.voom" => &TRACK_COMPILED,
        "published-grammar-audio.voom" => &AUDIO_COMPILED,
        "published-grammar-control-flow.voom" => &CONTROL_COMPILED,
        _ => panic!("missing compiled expectations for canonical fixture {fixture}"),
    }
}

fn compiled_coverage(policy: &CompiledPolicy) -> BTreeSet<String> {
    let mut coverage = BTreeSet::new();
    for phase in &policy.phases {
        let depends_on = serde_json::to_string(&phase.depends_on)
            .unwrap_or_else(|error| panic!("failed to serialize dependencies: {error}"));
        coverage.insert(format!("phase:{}|depends_on={depends_on}", phase.name));
        if let Some(on_error) = phase.on_error {
            let on_error = serde_json::to_value(on_error)
                .unwrap_or_else(|error| panic!("failed to serialize error strategy: {error}"));
            coverage.insert(format!(
                "phase:{}|on_error={}",
                phase.name,
                on_error.as_str().unwrap_or_default()
            ));
        }
    }
    let value = serde_json::to_value(policy)
        .unwrap_or_else(|error| panic!("failed to serialize compiled policy: {error}"));
    collect_typed_signatures(&value, &mut coverage);
    coverage
}

fn collect_typed_signatures(value: &serde_json::Value, coverage: &mut BTreeSet<String>) {
    match value {
        serde_json::Value::Object(object) => {
            if let Some(kind) = object.get("type").and_then(serde_json::Value::as_str) {
                coverage.insert(kind.to_owned());
                let mut fields: Vec<_> = object
                    .iter()
                    .filter(|(key, _value)| key.as_str() != "type")
                    .map(|(key, value)| format!("{key}={}", direct_value(value)))
                    .collect();
                fields.sort();
                if !fields.is_empty() {
                    coverage.insert(format!("{kind}|{}", fields.join("|")));
                }
            }
            for nested in object.values() {
                collect_typed_signatures(nested, coverage);
            }
        }
        serde_json::Value::Array(values) => {
            for nested in values {
                collect_typed_signatures(nested, coverage);
            }
        }
        serde_json::Value::Null
        | serde_json::Value::Bool(_)
        | serde_json::Value::Number(_)
        | serde_json::Value::String(_) => {}
    }
}

fn direct_value(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Null => "null".to_owned(),
        serde_json::Value::Bool(value) => value.to_string(),
        serde_json::Value::Number(value) => value.to_string(),
        serde_json::Value::String(value) => value.clone(),
        serde_json::Value::Array(values) if values.iter().all(is_scalar) => {
            serde_json::to_string(values)
                .unwrap_or_else(|error| panic!("failed to serialize coverage value: {error}"))
        }
        serde_json::Value::Array(_) | serde_json::Value::Object(_) => "present".to_owned(),
    }
}

const fn is_scalar(value: &serde_json::Value) -> bool {
    matches!(
        value,
        serde_json::Value::Null
            | serde_json::Value::Bool(_)
            | serde_json::Value::Number(_)
            | serde_json::Value::String(_)
    )
}
