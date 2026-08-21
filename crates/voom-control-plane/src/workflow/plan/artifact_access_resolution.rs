//! Artifact access resolution.
//! methods from voom-store, enforcing single common owner and stable locator-free
//! error evidence.

use sqlx::SqliteConnection;
use voom_core::{
    artifact_access_declaration::{ArtifactAccessDeclaration, ArtifactAccessTarget},
    ids::{ArtifactHandleId, FileLocationId, StorageRootId},
};
use voom_store::repo::execution::artifact_access_resolution::{
    AccessResolutionError as RepoAccessResolutionError, ResolvedLocation, ResolvedRoot,
    resolve_active_incarnation, resolve_file_location, resolve_storage_root,
};

/// Result of resolving a ticket's artifact access declaration.
///
/// A successful resolution proves that all referenced storage exists, is active,
/// belongs to one common owner, and has valid epochs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessResolution {
    /// The common owner node ID for all referenced storage.
    pub owner_node_id: i64,
    /// The active incarnation ID for the owner node.
    pub owner_incarnation_id: String,
    /// Resolved storage root references.
    pub resolved_roots: Vec<ResolvedRoot>,
    /// Resolved file location references.
    pub resolved_locations: Vec<ResolvedLocation>,
    /// Resolved artifact handle references.
    pub resolved_artifacts: Vec<ResolvedArtifact>,
}

/// A resolved artifact handle reference.
#[derive(Debug, Clone, PartialEq, Eq)]
#[expect(clippy::struct_field_names)]
pub struct ResolvedArtifact {
    pub artifact_handle_id: ArtifactHandleId,
    pub storage_root_id: StorageRootId,
    pub file_location_id: Option<FileLocationId>,
    pub owner_node_id: i64,
}

/// Errors that can occur during artifact access resolution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccessResolutionError {
    /// No entry matches the given storage root ID.
    StorageRootNotFound { storage_root_id: StorageRootId },
    /// No entry matches the given file location ID.
    FileLocationNotFound { file_location_id: FileLocationId },
    /// The file location's `storage_root_id` does not match the declared root.
    LocationRootInvalid {
        file_location_id: FileLocationId,
        storage_root_id: StorageRootId,
    },
    /// A storage root is not in a valid state (inactive, stale, retired).
    InvalidRootState {
        storage_root_id: StorageRootId,
        state: String,
    },
    /// A storage root carries a negative (corrupt) epoch.
    InvalidRootEpoch {
        storage_root_id: StorageRootId,
        root_epoch: i64,
    },
    /// A file location is not in a valid state.
    InvalidLocationState {
        file_location_id: FileLocationId,
        state: String,
    },
    /// References have different owner nodes.
    MixedOwner {
        first_owner: i64,
        conflicting_owner: i64,
    },
    /// No active incarnation found for the owner node.
    NoActiveIncarnation { owner_node_id: i64 },
    /// Database error during resolution.
    DatabaseError(String),
}

impl std::fmt::Display for AccessResolutionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AccessResolutionError::StorageRootNotFound { storage_root_id } => {
                write!(f, "storage root {} not found", storage_root_id.0)
            }
            AccessResolutionError::FileLocationNotFound { file_location_id } => {
                write!(f, "file location {} not found", file_location_id.0)
            }
            AccessResolutionError::LocationRootInvalid {
                file_location_id,
                storage_root_id,
            } => {
                write!(
                    f,
                    "file location {} is not in declared storage root {}",
                    file_location_id.0, storage_root_id.0
                )
            }
            AccessResolutionError::InvalidRootState {
                storage_root_id,
                state,
            } => {
                write!(
                    f,
                    "storage root {} is not in a valid state: {}",
                    storage_root_id.0, state
                )
            }
            AccessResolutionError::InvalidLocationState {
                file_location_id,
                state,
            } => {
                write!(
                    f,
                    "file location {file_location_id} has invalid state: {state}"
                )
            }
            AccessResolutionError::InvalidRootEpoch {
                storage_root_id,
                root_epoch,
            } => {
                write!(
                    f,
                    "storage root {storage_root_id} has invalid epoch: {root_epoch}"
                )
            }
            AccessResolutionError::MixedOwner {
                first_owner,
                conflicting_owner,
            } => {
                write!(
                    f,
                    "references have different owner nodes: first={first_owner}, conflicting={conflicting_owner}"
                )
            }
            AccessResolutionError::NoActiveIncarnation { owner_node_id } => {
                write!(
                    f,
                    "no active incarnation found for owner node {owner_node_id}"
                )
            }
            AccessResolutionError::DatabaseError(msg) => {
                write!(f, "database error: {msg}")
            }
        }
    }
}

impl std::error::Error for AccessResolutionError {}

impl From<RepoAccessResolutionError> for AccessResolutionError {
    fn from(err: RepoAccessResolutionError) -> Self {
        match err {
            RepoAccessResolutionError::StorageRootNotFound { storage_root_id } => {
                AccessResolutionError::StorageRootNotFound { storage_root_id }
            }
            RepoAccessResolutionError::FileLocationNotFound { file_location_id } => {
                AccessResolutionError::FileLocationNotFound { file_location_id }
            }
            RepoAccessResolutionError::LocationRootInvalid {
                file_location_id,
                storage_root_id,
            } => AccessResolutionError::LocationRootInvalid {
                file_location_id,
                storage_root_id,
            },
            RepoAccessResolutionError::InvalidRootState {
                storage_root_id,
                state,
            } => AccessResolutionError::InvalidRootState {
                storage_root_id,
                state,
            },
            RepoAccessResolutionError::InvalidRootEpoch {
                storage_root_id,
                root_epoch,
            } => AccessResolutionError::InvalidRootEpoch {
                storage_root_id,
                root_epoch,
            },
            RepoAccessResolutionError::InvalidLocationState {
                file_location_id,
                state,
            } => AccessResolutionError::InvalidLocationState {
                file_location_id,
                state,
            },
            RepoAccessResolutionError::MixedOwner {
                first_owner,
                conflicting_owner,
            } => AccessResolutionError::MixedOwner {
                first_owner,
                conflicting_owner,
            },
            RepoAccessResolutionError::NoActiveIncarnation { owner_node_id } => {
                AccessResolutionError::NoActiveIncarnation { owner_node_id }
            }
            RepoAccessResolutionError::DatabaseError(msg) => {
                AccessResolutionError::DatabaseError(msg)
            }
        }
    }
}

/// Fold one resolved reference into the running common-owner proof.
///
/// # Errors
/// Returns [`AccessResolutionError::MixedOwner`] when the reference belongs to
/// a different node than an earlier reference.
fn fold_common_owner(
    common_owner: Option<i64>,
    owner_node_id: i64,
) -> Result<i64, AccessResolutionError> {
    match common_owner {
        Some(owner) if owner != owner_node_id => Err(AccessResolutionError::MixedOwner {
            first_owner: owner,
            conflicting_owner: owner_node_id,
        }),
        _ => Ok(owner_node_id),
    }
}

/// Resolve a ticket's artifact access declaration against the database.
///
/// This function performs a read-only resolution that:
/// - Reads all referenced storage roots and file locations using typed repository methods
/// - Validates that each exists and is in a valid state
/// - Checks that all references share one common owner node
/// - Validates epochs and relational bindings
/// - Returns stable error evidence on any failure
///
/// # Errors
///
/// Returns `AccessResolutionError` if:
/// - Any referenced storage root or location does not exist
/// - Any reference is in an invalid state (inactive, stale, retired, mixed-owner)
/// - References have different owner nodes
/// - Database errors occur during resolution
pub async fn resolve_artifact_access(
    executor: &mut SqliteConnection,
    declaration: &ArtifactAccessDeclaration,
) -> Result<AccessResolution, AccessResolutionError> {
    let mut common_owner: Option<i64> = None;
    let mut resolved_roots = Vec::new();
    let mut resolved_locations = Vec::new();
    let mut resolved_artifacts = Vec::new();

    for entry in declaration.entries() {
        match &entry.target {
            ArtifactAccessTarget::StorageRoot(root_access) => {
                let resolved =
                    resolve_storage_root(&mut *executor, root_access.storage_root_id).await?;

                common_owner = Some(fold_common_owner(common_owner, resolved.owner_node_id)?);

                resolved_roots.push(resolved);
            }
            ArtifactAccessTarget::FileLocation(location_access) => {
                let resolved = resolve_file_location(
                    &mut *executor,
                    location_access.file_location_id,
                    location_access.storage_root_id,
                )
                .await?;

                common_owner = Some(fold_common_owner(common_owner, resolved.owner_node_id)?);

                resolved_locations.push(resolved);
            }
            ArtifactAccessTarget::ExistingArtifact(artifact_access) => {
                // Verify artifact exists by checking its location
                let location = resolve_file_location(
                    &mut *executor,
                    artifact_access.file_location_id,
                    artifact_access.storage_root_id,
                )
                .await?;

                common_owner = Some(fold_common_owner(common_owner, location.owner_node_id)?);

                resolved_artifacts.push(ResolvedArtifact {
                    artifact_handle_id: artifact_access.artifact_handle_id,
                    storage_root_id: location.storage_root_id,
                    file_location_id: Some(location.file_location_id),
                    owner_node_id: location.owner_node_id,
                });
            }
            ArtifactAccessTarget::PlannedArtifact(artifact_access) => {
                // Planned artifacts resolve only through their target root
                let root_resolved =
                    resolve_storage_root(&mut *executor, artifact_access.target_storage_root_id)
                        .await?;

                let owner = root_resolved.owner_node_id;

                common_owner = Some(fold_common_owner(common_owner, owner)?);

                // Record as a resolved root reference
                resolved_roots.push(root_resolved);

                // Record the planned artifact with minimal context
                resolved_artifacts.push(ResolvedArtifact {
                    artifact_handle_id: artifact_access.artifact_handle_id,
                    storage_root_id: artifact_access.target_storage_root_id,
                    file_location_id: None,
                    owner_node_id: owner,
                });
            }
        }
    }

    let owner_node_id = common_owner
        .ok_or_else(|| AccessResolutionError::DatabaseError("declaration is empty".to_string()))?;

    let owner_incarnation_id = resolve_active_incarnation(&mut *executor, owner_node_id).await?;

    Ok(AccessResolution {
        owner_node_id,
        owner_incarnation_id,
        resolved_roots,
        resolved_locations,
        resolved_artifacts,
    })
}

#[cfg(test)]
#[path = "artifact_access_resolution_test.rs"]
mod tests;
