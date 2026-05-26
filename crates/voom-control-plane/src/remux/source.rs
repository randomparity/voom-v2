use voom_core::{FileLocationId, FileVersionId, VoomError};
use voom_store::repo::identity::{
    FileLocation, FileLocationKind, FileVersion, IdentityRepo, MediaSnapshot,
};

use crate::ControlPlane;

#[derive(Debug, Clone)]
pub struct SelectedSource {
    pub version: FileVersion,
    pub location: FileLocation,
}

pub async fn select_source(
    cp: &ControlPlane,
    file_version_id: FileVersionId,
    source_location_id: Option<FileLocationId>,
) -> Result<SelectedSource, VoomError> {
    let version = cp
        .identity
        .get_file_version(file_version_id)
        .await?
        .ok_or_else(|| VoomError::NotFound(format!("file_version {file_version_id}")))?;
    if version.retired_at.is_some() {
        return Err(VoomError::NotFound(format!(
            "file_version {file_version_id} is retired"
        )));
    }
    let location = select_location(cp, file_version_id, source_location_id).await?;
    Ok(SelectedSource { version, location })
}

pub async fn read_media_snapshot(
    cp: &ControlPlane,
    file_version_id: FileVersionId,
) -> Result<MediaSnapshot, VoomError> {
    cp.identity
        .list_media_snapshots_by_version(file_version_id)
        .await?
        .into_iter()
        .next_back()
        .ok_or_else(|| VoomError::NotFound(format!("media snapshot for {file_version_id}")))
}

async fn select_location(
    cp: &ControlPlane,
    file_version_id: FileVersionId,
    source_location_id: Option<FileLocationId>,
) -> Result<FileLocation, VoomError> {
    if let Some(id) = source_location_id {
        let location = cp
            .identity
            .get_file_location(id)
            .await?
            .ok_or_else(|| VoomError::NotFound(format!("file_location {id}")))?;
        require_live_local_location(&location, file_version_id)?;
        return Ok(location);
    }
    let local_locations = cp
        .identity
        .list_live_file_locations_by_version(file_version_id)
        .await?
        .into_iter()
        .filter(|location| location.kind == FileLocationKind::LocalPath)
        .collect::<Vec<_>>();
    match local_locations.as_slice() {
        [location] => Ok(location.clone()),
        [] => Err(VoomError::Config(format!(
            "file_version {file_version_id} has no live local source locations"
        ))),
        _ => Err(VoomError::Config(format!(
            "file_version {file_version_id} has multiple live local source locations"
        ))),
    }
}

fn require_live_local_location(
    location: &FileLocation,
    file_version_id: FileVersionId,
) -> Result<(), VoomError> {
    if location.file_version_id != file_version_id {
        return Err(VoomError::Config(format!(
            "file_location {} belongs to file_version {}, expected {file_version_id}",
            location.id, location.file_version_id
        )));
    }
    if location.retired_at.is_some() {
        return Err(VoomError::NotFound(format!(
            "file_location {} is retired",
            location.id
        )));
    }
    if location.kind != FileLocationKind::LocalPath {
        return Err(VoomError::Config(format!(
            "file_location {} must be local_path",
            location.id
        )));
    }
    Ok(())
}

#[cfg(test)]
#[path = "source_test.rs"]
mod tests;
