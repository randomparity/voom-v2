//! Bench-only (#838): time this crate's `observe_file_facts` on the worker's runtime shape.
//! The shared runner describes the input and output lines.

#[path = "support/observe_runner.rs"]
mod observe_runner;

use voom_ffprobe_worker::{WorkerError, observe_file_facts};

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> std::io::Result<()> {
    observe_runner::run(|path| async move {
        match Box::pin(observe_file_facts(&path)).await {
            Ok(facts) => Ok((facts.size_bytes, facts.content_hash)),
            Err(WorkerError::ArtifactUnavailable(_)) => Err("artifact_unavailable"),
            Err(WorkerError::ArtifactChecksumMismatch(_)) => Err("artifact_checksum_mismatch"),
            Err(WorkerError::MalformedWorkerResult(_)) => Err("malformed_worker_result"),
            Err(WorkerError::MalformedMedia(_)) => Err("malformed_media"),
        }
    })
    .await
}
