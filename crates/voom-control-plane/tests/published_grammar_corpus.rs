#![expect(
    clippy::panic,
    reason = "integration test setup should fail loudly with fixture context"
)]

use voom_policy::{CompiledOperation, compile_policy};

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
const TRACK_FORMS: [&str; 16] = [
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
    "when exists subtitle",
    "when count subtitle >= 1",
];
const AUDIO_FORMS: [&str; 9] = [
    "transcode audio to aac",
    "transcode audio to opus where language in [\"eng\", \"und\"] and not commentary",
    "transcode audio to eac3 where channels >= 6",
    "synthesize audio from channels >= 6",
    "codec aac",
    "channels 2",
    "extract audio where codec in [\"aac\"]",
    "extract audio\n",
    "verify artifact",
];
const CONTROL_FORMS: [&str; 17] = [
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
    for (prefix, count) in [('S', 17), ('O', 21), ('C', 15), ('T', 13)] {
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
