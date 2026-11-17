//! Bench-only (#838): time this crate's `observe_file_facts` on the worker's runtime shape.
//! The shared runner describes the input and output lines.

#[path = "../../voom-ffprobe-worker/examples/support/observe_runner.rs"]
mod observe_runner;

use voom_ffmpeg_worker::observe::{ObserveError, observe_file_facts};

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> std::io::Result<()> {
    observe_runner::run(|path| async move {
        match Box::pin(observe_file_facts(&path)).await {
            Ok(facts) => Ok((facts.size_bytes, facts.content_hash)),
            Err(ObserveError::ArtifactUnavailable(_)) => Err("artifact_unavailable"),
            Err(ObserveError::ArtifactChecksumMismatch(_)) => Err("artifact_checksum_mismatch"),
        }
    })
    .await
}
