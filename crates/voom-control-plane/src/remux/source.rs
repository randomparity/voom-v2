use std::io::ErrorKind;
use std::path::PathBuf;

use serde_json::Value;
use voom_core::{FileLocationId, FileVersionId, MediaSnapshotId, VoomError};
use voom_store::repo::identity::{
    FileLocation, FileLocationKind, FileVersion, IdentityRepo, MediaSnapshot,
};

use crate::ControlPlane;
use crate::artifact::fs::canonical_existing_file_no_symlink;

#[derive(Debug, Clone)]
pub struct SelectedSource {
    pub version: FileVersion,
    pub location: FileLocation,
    pub canonical_path: PathBuf,
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
    let canonical_path = canonical_source_path(&location.value).await?;
    Ok(SelectedSource {
        version,
        location,
        canonical_path,
    })
}

pub async fn read_media_snapshot(
    cp: &ControlPlane,
    file_version_id: FileVersionId,
    operation_payload: &Value,
) -> Result<MediaSnapshot, VoomError> {
    let snapshot_id = source_media_snapshot_id(operation_payload)?;
    let snapshot = cp
        .identity
        .get_media_snapshot(snapshot_id)
        .await?
        .ok_or_else(|| VoomError::NotFound(format!("media_snapshot {snapshot_id}")))?;
    if snapshot.file_version_id != file_version_id {
        return Err(VoomError::Config(format!(
            "media_snapshot {snapshot_id} does not belong to file_version {file_version_id}"
        )));
    }
    Ok(snapshot)
}

fn source_media_snapshot_id(operation_payload: &Value) -> Result<MediaSnapshotId, VoomError> {
    operation_payload
        .get("source_media_snapshot_id")
        .and_then(Value::as_u64)
        .filter(|id| *id > 0)
        .map(MediaSnapshotId)
        .ok_or_else(|| {
            VoomError::Config(
                "remux operation payload requires source_media_snapshot_id".to_owned(),
            )
        })
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

async fn canonical_source_path(path: &str) -> Result<PathBuf, VoomError> {
    match tokio::fs::symlink_metadata(path).await {
        Ok(_) => {}
        Err(err) if err.kind() == ErrorKind::NotFound => {
            return Err(VoomError::ArtifactUnavailable(format!(
                "remux source artifact unavailable: {path}: {err}"
            )));
        }
        Err(err) => {
            return Err(VoomError::ArtifactUnavailable(format!(
                "cannot inspect remux source artifact {path}: {err}"
            )));
        }
    }
    canonical_existing_file_no_symlink(path)
        .await
        .map_err(|err| match err {
            VoomError::Config(message)
                if message.contains("artifact path must exist")
                    || message.contains("cannot canonicalize artifact path") =>
            {
                VoomError::ArtifactUnavailable(message)
            }
            other => other,
        })
}

#[cfg(test)]
#[path = "source_test.rs"]
mod tests;
