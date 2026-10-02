//! Shared source-selection protocol for media operations.
//!
//! Operation modules keep thin wrappers so call sites retain domain names while
//! local-path, snapshot, and canonicalization invariants stay in one place.

use std::io::ErrorKind;
use std::path::PathBuf;

use voom_core::{FileLocationId, FileVersionId, ProviderRelativeLocator, StorageRootId, VoomError};
use voom_store::repo::library::library_roots::{EffectiveLibraryRoot, LibraryRoot};
use voom_store::repo::media::identity::{FileLocation, FileLocationAddress, FileLocationRepo};

use crate::ControlPlane;
use crate::artifact::fs::{canonical_existing_file_no_symlink, canonical_new_leaf_no_symlink};
use crate::workflow::plan::binding::media_dispatch::DestinationRole;
use crate::workflow::plan::envelope::destination_root;

#[derive(Debug, Clone)]
pub(crate) struct SelectedSource {
    pub(crate) location: FileLocation,
    pub(crate) canonical_path: PathBuf,
}

pub(crate) async fn select_local_source(
    cp: &ControlPlane,
    operation_label: &'static str,
    file_version_id: FileVersionId,
    source_location_id: Option<FileLocationId>,
) -> Result<SelectedSource, VoomError> {
    let location = select_location(cp, file_version_id, source_location_id).await?;
    let canonical_path = resolve_rooted_existing_path(cp, operation_label, &location).await?;
    Ok(SelectedSource {
        location,
        canonical_path,
    })
}

/// Resolve a file version to exactly one live rooted location, failing closed on
/// zero or several.
///
/// The byte-free half of `select_local_source`: it reads identity rows and does
/// not canonicalize, stat, or open anything, which is what makes it reusable on
/// the declaration path.
pub(crate) async fn select_location(
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
    require_live_rooted(location)
}

/// The half of [`require_live_rooted_location`] that asks only about the row
/// itself: is it live, and does it have a rooted address.
///
/// Split out for the caller that already holds the row it looked up by id. There
/// the ownership check above compares the row's `file_version_id` against a value
/// read out of that same row, so it can never fire — and reaching it costs a
/// second read of a row already in hand.
pub(crate) fn require_live_rooted(location: &FileLocation) -> Result<(), VoomError> {
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

/// Resolve a pre-promotion commit target inside the staging root that
/// `destination_root` resolves for the source root (ADR 0097).
///
/// The commit address is scratch awaiting promotion, so it is contained by the
/// registered staging root, never by an output root; an unconfigured staging
/// default fails closed.
pub(crate) async fn resolve_pre_promotion_target(
    cp: &ControlPlane,
    operation_label: &'static str,
    source_storage_root_id: StorageRootId,
    requested_target: &std::path::Path,
) -> Result<(StorageRootId, ProviderRelativeLocator, PathBuf), VoomError> {
    let source = library_root(cp, source_storage_root_id).await?;
    let staging_root_id =
        destination_root(cp, DestinationRole::Staging, source_storage_root_id).await?;
    resolve_target_in_root(
        cp,
        operation_label,
        DestinationRole::Staging,
        &source,
        staging_root_id,
        requested_target,
    )
    .await
}

/// Resolve a durable output target inside the `default_output_root_id` of
/// `storage_root_id` itself (ADR 0097).
///
/// There is no fallback to `storage_root_id`: an unconfigured output default
/// fails closed with the command that configures one.
pub(crate) async fn resolve_output_target(
    cp: &ControlPlane,
    operation_label: &'static str,
    storage_root_id: StorageRootId,
    requested_target: &std::path::Path,
) -> Result<(StorageRootId, ProviderRelativeLocator, PathBuf), VoomError> {
    let root = library_root(cp, storage_root_id).await?;
    let output_root_id = root.default_output_root_id.ok_or_else(|| {
        VoomError::Config(format!(
            "{operation_label}: storage root {storage_root_id} has no default output root; \
             configure one with `voom library root update --root-id {storage_root_id} \
             --output-root <id>`"
        ))
    })?;
    resolve_target_in_root(
        cp,
        operation_label,
        DestinationRole::Output,
        &root,
        output_root_id,
        requested_target,
    )
    .await
}

async fn resolve_target_in_root(
    cp: &ControlPlane,
    operation_label: &'static str,
    role: DestinationRole,
    naming: &LibraryRoot,
    target_storage_root_id: StorageRootId,
    requested_target: &std::path::Path,
) -> Result<(StorageRootId, ProviderRelativeLocator, PathBuf), VoomError> {
    let target = cp
        .effective_library_root(target_storage_root_id)
        .await?
        .ok_or_else(|| VoomError::NotFound(format!("storage root {target_storage_root_id}")))?;
    if target.root.library_id != naming.library_id {
        return Err(VoomError::database(format!(
            "storage root {} default {} root {target_storage_root_id} \
             belongs to library {}, expected {}",
            naming.id,
            role.as_str(),
            target.root.library_id,
            naming.library_id
        )));
    }
    let root_path = require_effective_local_root_path(cp, operation_label, &target).await?;
    let canonical_target = canonical_new_leaf_no_symlink(requested_target).await?;
    rooted_target_address(
        operation_label,
        target_storage_root_id,
        &root_path,
        canonical_target,
    )
}

async fn library_root(
    cp: &ControlPlane,
    storage_root_id: StorageRootId,
) -> Result<LibraryRoot, VoomError> {
    cp.libraries
        .get_library_root(storage_root_id)
        .await?
        .ok_or_else(|| VoomError::NotFound(format!("storage root {storage_root_id}")))
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
        "{operation_label} path escaped storage root {storage_root_id}: {} is not inside {}",
        path.display(),
        root_path.display()
    )))
}

#[cfg(test)]
#[path = "operation_source_test.rs"]
mod tests;
