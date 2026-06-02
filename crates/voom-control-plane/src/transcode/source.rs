use voom_core::{FileLocationId, FileVersionId, VoomError};

use crate::ControlPlane;

pub(crate) use crate::operation_source::SelectedSource;

pub(crate) async fn select_source(
    cp: &ControlPlane,
    file_version_id: FileVersionId,
    source_location_id: Option<FileLocationId>,
) -> Result<SelectedSource, VoomError> {
    crate::operation_source::select_local_source(
        cp,
        "transcode",
        file_version_id,
        source_location_id,
    )
    .await
}

#[cfg(test)]
#[path = "source_test.rs"]
mod tests;
