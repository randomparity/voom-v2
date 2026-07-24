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
    }
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
