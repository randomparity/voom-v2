use std::path::Path;

use voom_core::{ArtifactHandleId, ArtifactLocationId, FileLocationId, VoomError};
use voom_worker_protocol::{RemuxResult, RemuxSelection};

use super::ExecuteRemuxInput;
use crate::ControlPlane;

#[must_use]
pub fn remux_events_are_noop() -> bool {
    true
}

pub fn record_started(
    _cp: &ControlPlane,
    _input: &ExecuteRemuxInput,
    _source_location_id: FileLocationId,
    _selection: &RemuxSelection,
    _staging_path: &Path,
) -> Result<(), VoomError> {
    // Temporary until Task 8 adds remux-specific event kinds. Generic
    // artifact/media-snapshot events still record durable transitions through
    // commit::record_staged_remux and commit::record_result_snapshot.
    Ok(())
}

pub fn record_succeeded(
    _cp: &ControlPlane,
    _input: &ExecuteRemuxInput,
    _source_location_id: FileLocationId,
    _artifact_handle_id: ArtifactHandleId,
    _artifact_location_id: ArtifactLocationId,
    _result: &RemuxResult,
) -> Result<(), VoomError> {
    Ok(())
}

#[cfg(test)]
#[path = "events_test.rs"]
mod tests;
