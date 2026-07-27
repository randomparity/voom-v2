//! Real CLI/process-boundary acceptance for the complete published grammar
//! corpus.

#![expect(
    clippy::unwrap_used,
    reason = "integration tests fail loudly with scenario and child-process diagnostics"
)]

#[path = "support/media_inspect.rs"]
mod media_inspect;
#[path = "support/process.rs"]
mod process;
#[path = "support/published_grammar_media.rs"]
mod published_grammar_media;

#[test]
fn published_grammar_corpus_is_executable() {
    let _corpus = published_grammar_media::generate_and_validate_all().unwrap();
}
