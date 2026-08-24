//! Real CLI/process-boundary acceptance for the complete published grammar
//! corpus.

#![expect(
    clippy::unwrap_used,
    reason = "integration tests fail loudly with scenario and child-process diagnostics"
)]

#[path = "support/local_worker.rs"]
mod local_worker;
#[path = "support/media_inspect.rs"]
mod media_inspect;
#[path = "support/process.rs"]
mod process;
#[path = "support/published_grammar_execution.rs"]
mod published_grammar_execution;
#[path = "support/published_grammar_media.rs"]
mod published_grammar_media;

#[test]
fn published_grammar_corpus_is_executable() {
    let _workers = published_grammar_execution::prepare_worker_binaries().unwrap();
    let corpus = published_grammar_media::generate_and_validate_all().unwrap();
    published_grammar_execution::execute_core(&corpus.core).unwrap();
    published_grammar_execution::execute_tracks(&corpus.tracks).unwrap();
    published_grammar_execution::execute_audio(&corpus.audio).unwrap();
    published_grammar_execution::execute_control_flow(&corpus.flow).unwrap();
}
