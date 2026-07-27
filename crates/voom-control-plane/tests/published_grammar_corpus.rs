#![expect(
    clippy::panic,
    reason = "integration test setup should fail loudly with fixture context"
)]

use voom_policy::{CompiledOperation, CompiledPolicy, compile_policy, deterministic_json};

const FIXTURE_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/policies/");
const MATRIX_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/policies/published-grammar-coverage.md"
);
const UNPUBLISHED_FORMS: [&str; 13] = [
    "extends ",
    "set_tag ",
    "delete_tag ",
    "clear_tags",
    "actions ",
    "lang ",
    "language == eng",
    "language in [eng",
    "codec in [aac",
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
    "when exists subtitle {\n      defaults subtitle best",
    "when video.width < 1280 and count subtitle >= 2 {\n      defaults audio none",
    "defaults audio none\n      defaults subtitle preserve",
];
const AUDIO_FORMS: [&str; 10] = [
    "transcode audio to aac",
    "transcode audio to opus where language in [\"eng\", \"und\"] and not commentary",
    "transcode audio to eac3 where channels >= 6",
    "synthesize audio from channels >= 6 {\n      codec aac",
    "synthesize audio from channels >= 6 {\n      codec opus",
    "synthesize audio from channels >= 6 {\n      codec eac3",
    "channels 2",
    "extract audio where codec in [\"aac\"] and commentary",
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

struct CorpusFixture {
    source_file: &'static str,
    compiled_file: &'static str,
    required_forms: &'static [&'static str],
}

const CORPUS_FIXTURES: [CorpusFixture; 4] = [
    CorpusFixture {
        source_file: "published-grammar-core.voom",
        compiled_file: "published-grammar-core.compiled.json",
        required_forms: &CORE_FORMS,
    },
    CorpusFixture {
        source_file: "published-grammar-tracks.voom",
        compiled_file: "published-grammar-tracks.compiled.json",
        required_forms: &TRACK_FORMS,
    },
    CorpusFixture {
        source_file: "published-grammar-audio.voom",
        compiled_file: "published-grammar-audio.compiled.json",
        required_forms: &AUDIO_FORMS,
    },
    CorpusFixture {
        source_file: "published-grammar-control-flow.voom",
        compiled_file: "published-grammar-control-flow.compiled.json",
        required_forms: &CONTROL_FORMS,
    },
];

#[test]
fn canonical_published_grammar_policies_compile_and_define_execution_oracles() {
    let matrix = std::fs::read_to_string(MATRIX_PATH)
        .unwrap_or_else(|error| panic!("failed to read {MATRIX_PATH}: {error}"));

    for fixture in &CORPUS_FIXTURES {
        let path = format!("{FIXTURE_DIR}{}", fixture.source_file);
        let source = std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("failed to read {path}: {error}"));
        let output = compile_policy(&source).unwrap_or_else(|error| {
            panic!(
                "{} failed to compile: {:?}",
                fixture.source_file, error.diagnostics
            )
        });

        assert!(
            output
                .policy
                .phases
                .iter()
                .flat_map(|phase| &phase.operations)
                .any(has_mutation),
            "{} must define a mutation",
            fixture.source_file
        );
        assert!(
            output
                .policy
                .phases
                .iter()
                .flat_map(|phase| &phase.operations)
                .any(has_verification),
            "{} must verify an artifact",
            fixture.source_file
        );
        for unpublished in UNPUBLISHED_FORMS {
            assert!(
                !source.contains(unpublished),
                "{} contains unpublished source form `{unpublished}`",
                fixture.source_file
            );
        }
        assert!(
            matrix.contains(fixture.source_file),
            "{} must have a coverage-matrix execution oracle",
            fixture.source_file
        );
        for required in fixture.required_forms {
            assert!(
                source.contains(required),
                "{} is missing published form `{required}`",
                fixture.source_file
            );
        }
        assert_compiled_golden(fixture, &output.policy);
    }

    assert_complete_matrix(&matrix);
}

fn has_mutation(operation: &CompiledOperation) -> bool {
    match operation {
        CompiledOperation::VerifyArtifact(
            voom_policy::compiled::CompiledVerifyArtifactOperation {},
        ) => false,
        CompiledOperation::Conditional(voom_policy::compiled::CompiledConditionalOperation {
            operations,
            ..
        }) => operations.iter().any(has_mutation),
        CompiledOperation::Rules(voom_policy::compiled::CompiledRulesOperation {
            rules, ..
        }) => rules
            .iter()
            .flat_map(|rule| &rule.operations)
            .any(has_mutation),
        _ => true,
    }
}

fn has_verification(operation: &CompiledOperation) -> bool {
    match operation {
        CompiledOperation::VerifyArtifact(
            voom_policy::compiled::CompiledVerifyArtifactOperation {},
        ) => true,
        CompiledOperation::Conditional(voom_policy::compiled::CompiledConditionalOperation {
            operations,
            ..
        }) => operations.iter().any(has_verification),
        CompiledOperation::Rules(voom_policy::compiled::CompiledRulesOperation {
            rules, ..
        }) => rules
            .iter()
            .flat_map(|rule| &rule.operations)
            .any(has_verification),
        _ => false,
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
            let expected_cells = if prefix == 'O' { 8 } else { 7 };
            assert_eq!(
                cells.len(),
                expected_cells,
                "matrix row {id} has the wrong number of cells"
            );
            assert!(
                cells[1..cells.len() - 1]
                    .iter()
                    .all(|cell| !cell.is_empty())
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
    for track_variant in ["T1a", "T1b", "T1c"] {
        assert!(
            matrix.contains(track_variant),
            "matrix is missing observable track variant {track_variant}"
        );
    }
}

fn assert_compiled_golden(fixture: &CorpusFixture, policy: &CompiledPolicy) {
    let path = format!("{FIXTURE_DIR}compiled/{}", fixture.compiled_file);
    let expected = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("failed to read {path}: {error}"));
    let expected: serde_json::Value = serde_json::from_str(&expected)
        .unwrap_or_else(|error| panic!("failed to parse {path}: {error}"));
    let actual = deterministic_json(policy)
        .unwrap_or_else(|error| panic!("failed to serialize {}: {error}", fixture.source_file));
    assert_eq!(
        actual, expected,
        "{} compiled policy differs from {}",
        fixture.source_file, fixture.compiled_file
    );
}
