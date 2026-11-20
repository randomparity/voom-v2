//! Bench-only (#898): time the commit coordinator's `stream_copy_with_hash` on the node
//! agent's runtime shape. The shared #838 runner describes the input and output lines; its
//! `<seconds>` here covers one copy, flush, fsync and removal of the copy.
//!
//! Usage: `copy-commit <concurrency> <scratch-dir>`. Each copy is written to a new file in
//! `<scratch-dir>`, which must be on the source's filesystem, and removed before the file's
//! line is written. An `err` line names the failure; the harness refuses a run that leaves
//! anything in `<scratch-dir>`.

#[path = "../../voom-ffprobe-worker/examples/support/observe_runner.rs"]
mod observe_runner;

use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use voom_core::VoomError;
use voom_node_agent::commit::stream_copy_with_hash;

#[tokio::main(flavor = "multi_thread")]
async fn main() -> io::Result<()> {
    let scratch = Arc::new(scratch_dir()?);
    let next = Arc::new(AtomicU64::new(0));
    observe_runner::run(move |path| {
        let temp = scratch.join(format!("copy-{}.tmp", next.fetch_add(1, Ordering::Relaxed)));
        copy_once(path, temp)
    })
    .await
}

async fn copy_once(path: PathBuf, temp: PathBuf) -> observe_runner::Observed {
    let copied = stream_copy_with_hash(&path, &temp).await;
    let removed = tokio::fs::remove_file(&temp).await;
    let facts = copied.map_err(|error| match error {
        VoomError::ArtifactUnavailable(_) => "artifact_unavailable",
        VoomError::CommitFailure(_) => "commit_failure",
        _ => "other",
    })?;
    removed.map_err(|_| "remove_failed")?;
    Ok((facts.size_bytes, facts.content_hash))
}

fn scratch_dir() -> io::Result<PathBuf> {
    std::env::args()
        .nth(2)
        .map(PathBuf::from)
        .filter(|dir| dir.is_dir())
        .ok_or_else(|| io::Error::other("usage: copy-commit <concurrency> <scratch-dir>"))
}
