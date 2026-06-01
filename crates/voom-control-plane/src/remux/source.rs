use serde_json::Value;
use voom_core::{FileLocationId, FileVersionId, VoomError};
use voom_store::repo::identity::MediaSnapshot;

use crate::ControlPlane;

pub(crate) use crate::operation_source::SelectedSource;

pub(crate) async fn select_source(
    cp: &ControlPlane,
    file_version_id: FileVersionId,
    source_location_id: Option<FileLocationId>,
) -> Result<SelectedSource, VoomError> {
    crate::operation_source::select_local_source(cp, "remux", file_version_id, source_location_id)
        .await
}

pub(crate) async fn read_media_snapshot(
    cp: &ControlPlane,
    file_version_id: FileVersionId,
    operation_payload: &Value,
) -> Result<MediaSnapshot, VoomError> {
    crate::operation_source::read_required_media_snapshot(
        cp,
        "remux",
        file_version_id,
        operation_payload,
    )
    .await
}

#[cfg(test)]
#[path = "source_test.rs"]
mod tests;
