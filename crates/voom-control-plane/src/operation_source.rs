//! Shared source-selection protocol for media operations.
//!
//! Operation modules keep thin wrappers so call sites retain domain names while
//! local-path, snapshot, and canonicalization invariants stay in one place.

use std::io::ErrorKind;
use std::path::PathBuf;

use serde_json::Value;
use voom_core::{
    FileLocationId, FileVersionId, MediaSnapshotId, ProviderRelativeLocator, StorageRootId,
    VoomError,
};
use voom_store::repo::library::library_roots::EffectiveLibraryRoot;
use voom_store::repo::media::identity::{
    FileLocation, FileLocationAddress, FileLocationRepo, FileVersion, FileVersionRepo,
    MediaSnapshot, MediaSnapshotRepo,
};

use crate::ControlPlane;
use crate::artifact::fs::{canonical_existing_file_no_symlink, canonical_new_leaf_no_symlink};

#[derive(Debug, Clone)]
pub(crate) struct SelectedSource {
    pub(crate) version: FileVersion,
    pub(crate) location: FileLocation,
    pub(crate) canonical_path: PathBuf,
}

pub(crate) async fn select_local_source(
    cp: &ControlPlane,
    operation_label: &'static str,
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
    let canonical_path = resolve_rooted_existing_path(cp, operation_label, &location).await?;
    Ok(SelectedSource {
        version,
        location,
        canonical_path,
    })
}

pub(crate) async fn read_required_media_snapshot(
    cp: &ControlPlane,
    operation_label: &'static str,
    file_version_id: FileVersionId,
    operation_payload: &Value,
) -> Result<MediaSnapshot, VoomError> {
    let snapshot_id = source_media_snapshot_id(operation_label, operation_payload)?;
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

fn source_media_snapshot_id(
    operation_label: &'static str,
    operation_payload: &Value,
) -> Result<MediaSnapshotId, VoomError> {
    operation_payload
        .get("source_media_snapshot_id")
        .and_then(Value::as_u64)
        .filter(|id| *id > 0)
        .map(MediaSnapshotId)
        .ok_or_else(|| {
            VoomError::Config(format!(
                "{operation_label} operation payload requires source_media_snapshot_id"
            ))
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
        require_live_rooted_location(&location, file_version_id)?;
        return Ok(location);
    }
    let rooted_locations = cp
        .identity
        .list_live_file_locations_by_version(file_version_id)
        .await?
        .into_iter()
        .filter(|location| matches!(location.address, FileLocationAddress::Rooted { .. }))
        .collect::<Vec<_>>();
    match rooted_locations.as_slice() {
        [location] => Ok(location.clone()),
        [] => Err(VoomError::Config(format!(
            "file_version {file_version_id} has no live rooted source locations"
        ))),
        _ => Err(VoomError::Config(format!(
            "file_version {file_version_id} has multiple live rooted source locations"
        ))),
    }
}

fn require_live_rooted_location(
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
    if !matches!(location.address, FileLocationAddress::Rooted { .. }) {
        return Err(VoomError::Config(format!(
            "file_location {} must have a rooted address",
            location.id
        )));
    }
    Ok(())
}

pub(crate) async fn resolve_rooted_existing_path(
    cp: &ControlPlane,
    operation_label: &'static str,
    location: &FileLocation,
) -> Result<PathBuf, VoomError> {
    let (storage_root_id, relative_locator) = location.rooted_address()?;
    resolve_root_relative_existing_path(cp, operation_label, storage_root_id, relative_locator)
        .await
}

pub(crate) async fn resolve_root_relative_existing_path(
    cp: &ControlPlane,
    operation_label: &'static str,
    storage_root_id: StorageRootId,
    relative_locator: &ProviderRelativeLocator,
) -> Result<PathBuf, VoomError> {
    let root_path = require_local_root_path(cp, operation_label, storage_root_id).await?;
    let path = root_path.join(relative_locator.as_str());
    match tokio::fs::symlink_metadata(&path).await {
        Ok(_) => {}
        Err(err) if err.kind() == ErrorKind::NotFound => {
            return Err(VoomError::ArtifactUnavailable(format!(
                "{operation_label} source artifact unavailable: {}: {err}",
                path.display()
            )));
        }
        Err(err) => {
            return Err(VoomError::ArtifactUnavailable(format!(
                "cannot inspect {operation_label} source artifact {}: {err}",
                path.display()
            )));
        }
    }
    let canonical = canonical_existing_file_no_symlink(&path)
        .await
        .map_err(|err| match err {
            VoomError::Config(message)
                if message.contains("artifact path must exist")
                    || message.contains("cannot canonicalize artifact path") =>
            {
                VoomError::ArtifactUnavailable(message)
            }
            other => other,
        })?;
    require_contained(operation_label, storage_root_id, &root_path, &canonical)?;
    Ok(canonical)
}

pub(crate) async fn resolve_artifact_target(
    cp: &ControlPlane,
    operation_label: &'static str,
    source_storage_root_id: StorageRootId,
    requested_target: &std::path::Path,
) -> Result<(StorageRootId, ProviderRelativeLocator, PathBuf), VoomError> {
    let (target_storage_root_id, root_path) =
        artifact_target_root(cp, operation_label, source_storage_root_id).await?;
    let canonical_target = canonical_new_leaf_no_symlink(requested_target).await?;
    rooted_target_address(
        operation_label,
        target_storage_root_id,
        &root_path,
        canonical_target,
    )
}

pub(crate) async fn resolve_exact_artifact_recovery_target(
    cp: &ControlPlane,
    operation_label: &'static str,
    source_storage_root_id: StorageRootId,
    target_storage_root_id: StorageRootId,
    target_relative_locator: &ProviderRelativeLocator,
    requested_target: &std::path::Path,
) -> Result<PathBuf, VoomError> {
    let source = cp
        .effective_library_root(source_storage_root_id)
        .await?
        .ok_or_else(|| VoomError::NotFound(format!("storage root {source_storage_root_id}")))?;
    let target = cp
        .effective_library_root(target_storage_root_id)
        .await?
        .ok_or_else(|| VoomError::NotFound(format!("storage root {target_storage_root_id}")))?;
    if target.root.library_id != source.root.library_id {
        return Err(VoomError::database(format!(
            "{operation_label} persisted target root {target_storage_root_id} belongs to library \
             {}, expected {} from source root {source_storage_root_id}",
            target.root.library_id, source.root.library_id
        )));
    }
    let root_path = require_effective_local_root_path(cp, operation_label, &target).await?;
    let canonical_target = match tokio::fs::symlink_metadata(requested_target).await {
        Ok(_) => canonical_existing_file_no_symlink(requested_target).await?,
        Err(error) if error.kind() == ErrorKind::NotFound => {
            canonical_new_leaf_no_symlink(requested_target).await?
        }
        Err(error) => {
            return Err(VoomError::ArtifactUnavailable(format!(
                "cannot inspect {operation_label} recovery target {}: {error}",
                requested_target.display()
            )));
        }
    };
    let (_, resolved_relative_locator, canonical_target) = rooted_target_address(
        operation_label,
        target_storage_root_id,
        &root_path,
        canonical_target,
    )?;
    if &resolved_relative_locator != target_relative_locator {
        return Err(VoomError::database(format!(
            "{operation_label} persisted target address ({target_storage_root_id}, {}) does not \
             match target path {}",
            target_relative_locator.as_str(),
            requested_target.display()
        )));
    }
    Ok(canonical_target)
}

async fn artifact_target_root(
    cp: &ControlPlane,
    operation_label: &'static str,
    source_storage_root_id: StorageRootId,
) -> Result<(StorageRootId, PathBuf), VoomError> {
    let source = cp
        .effective_library_root(source_storage_root_id)
        .await?
        .ok_or_else(|| VoomError::NotFound(format!("storage root {source_storage_root_id}")))?;
    let source_library_id = source.root.library_id;
    let target_storage_root_id = source
        .root
        .default_output_root_id
        .unwrap_or(source_storage_root_id);
    let target = if target_storage_root_id == source_storage_root_id {
        source
    } else {
        cp.effective_library_root(target_storage_root_id)
            .await?
            .ok_or_else(|| VoomError::NotFound(format!("storage root {target_storage_root_id}")))?
    };
    if target.root.library_id != source_library_id {
        return Err(VoomError::database(format!(
            "storage root {source_storage_root_id} default output root {target_storage_root_id} \
             belongs to library {}, expected {source_library_id}",
            target.root.library_id
        )));
    }
    let root_path = require_effective_local_root_path(cp, operation_label, &target).await?;
    Ok((target_storage_root_id, root_path))
}

fn rooted_target_address(
    operation_label: &'static str,
    target_storage_root_id: StorageRootId,
    root_path: &std::path::Path,
    canonical_target: PathBuf,
) -> Result<(StorageRootId, ProviderRelativeLocator, PathBuf), VoomError> {
    require_contained(
        operation_label,
        target_storage_root_id,
        root_path,
        &canonical_target,
    )?;
    let relative = canonical_target.strip_prefix(root_path).map_err(|_| {
        VoomError::Config(format!(
            "{operation_label} target escaped storage root {target_storage_root_id}"
        ))
    })?;
    let relative = relative.to_str().ok_or_else(|| {
        VoomError::Config(format!(
            "{operation_label} target is not valid UTF-8: {}",
            canonical_target.display()
        ))
    })?;
    Ok((
        target_storage_root_id,
        ProviderRelativeLocator::new(relative.to_owned())?,
        canonical_target,
    ))
}

async fn require_local_root_path(
    cp: &ControlPlane,
    operation_label: &'static str,
    storage_root_id: StorageRootId,
) -> Result<PathBuf, VoomError> {
    let effective = cp
        .effective_library_root(storage_root_id)
        .await?
        .ok_or_else(|| VoomError::NotFound(format!("storage root {storage_root_id}")))?;
    require_effective_local_root_path(cp, operation_label, &effective).await
}

async fn require_effective_local_root_path(
    cp: &ControlPlane,
    operation_label: &'static str,
    effective: &EffectiveLibraryRoot,
) -> Result<PathBuf, VoomError> {
    let storage_root_id = effective.root.id;
    if !effective.available {
        return Err(VoomError::ArtifactUnavailable(format!(
            "{operation_label} storage root {storage_root_id} unavailable: {}",
            effective.reason.as_str()
        )));
    }
    let owner = effective.root.owner_node_id.ok_or_else(|| {
        VoomError::database(format!(
            "active storage root {storage_root_id} has no owner"
        ))
    })?;
    let local = cp.local_node_id.ok_or_else(|| {
        VoomError::Config(format!(
            concat!(
                "{} requires VOOM_LOCAL_NODE_ID to resolve storage root ",
                "{}"
            ),
            operation_label, storage_root_id
        ))
    })?;
    if owner != local {
        return Err(VoomError::ArtifactUnavailable(format!(
            "{operation_label} storage root {storage_root_id} belongs to node {owner}, \
             but this control plane is node {local}"
        )));
    }
    tokio::fs::canonicalize(effective.root.provider_locator.as_str())
        .await
        .map_err(|error| {
            VoomError::ArtifactUnavailable(format!(
                "cannot resolve {operation_label} storage root {storage_root_id}: {error}"
            ))
        })
}

fn require_contained(
    operation_label: &'static str,
    storage_root_id: StorageRootId,
    root_path: &std::path::Path,
    path: &std::path::Path,
) -> Result<(), VoomError> {
    if path.starts_with(root_path) {
        return Ok(());
    }
    Err(VoomError::Config(format!(
        "{operation_label} path escaped storage root {storage_root_id}"
    )))
}

#[cfg(test)]
#[path = "operation_source_test.rs"]
mod tests;
