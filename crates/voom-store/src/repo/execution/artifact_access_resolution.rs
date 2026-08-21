//! Artifact access resolution repository.
//!
//! This module provides read-only resolution of artifact-access declarations
//! against persisted storage state. It proves one common configured owner for
//! a ticket's declared artifact access using stable IDs and epochs only.

use sqlx::{Executor, Row, Sqlite};
use voom_core::ids::{ArtifactHandleId, FileLocationId, StorageRootId};

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

/// A resolved storage root reference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedRoot {
    pub storage_root_id: StorageRootId,
    pub owner_node_id: i64,
    pub state: RootState,
    pub root_epoch: i64,
}

/// A resolved file location reference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedLocation {
    pub file_location_id: FileLocationId,
    pub storage_root_id: StorageRootId,
    pub owner_node_id: i64,
    pub state: LocationState,
}

/// A resolved artifact handle reference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedArtifact {
    pub artifact_handle_id: ArtifactHandleId,
    pub storage_root_id: StorageRootId,
    pub file_location_id: Option<FileLocationId>,
    pub owner_node_id: i64,
}

/// Valid storage root state from the database.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RootState {
    Active,
    Configured,
}

/// Valid file location address state from the database.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocationState {
    Rooted,
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
    /// A database access or consistency error occurred.
    DatabaseError(String),
}

impl std::fmt::Display for AccessResolutionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AccessResolutionError::StorageRootNotFound { storage_root_id } => {
                write!(f, "storage root {storage_root_id} not found")
            }
            AccessResolutionError::FileLocationNotFound { file_location_id } => {
                write!(f, "file location {file_location_id} not found")
            }
            AccessResolutionError::LocationRootInvalid {
                file_location_id,
                storage_root_id,
            } => {
                write!(
                    f,
                    "file location {file_location_id} does not belong to declared root {storage_root_id}"
                )
            }
            AccessResolutionError::InvalidRootState {
                storage_root_id,
                state,
            } => {
                write!(
                    f,
                    "storage root {storage_root_id} has invalid state: {state}"
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

impl From<sqlx::Error> for AccessResolutionError {
    fn from(err: sqlx::Error) -> Self {
        AccessResolutionError::DatabaseError(err.to_string())
    }
}

/// Resolve a storage root by ID.
///
/// Returns the root's owner, state, and epoch, or an error if not found or invalid.
pub async fn resolve_storage_root<'e, E>(
    executor: E,
    storage_root_id: StorageRootId,
) -> Result<ResolvedRoot, AccessResolutionError>
where
    E: Executor<'e, Database = Sqlite>,
{
    let row = sqlx::query(
        r"
        SELECT id, owner_node_id, state, root_epoch
        FROM library_roots
        WHERE id = ?1
        ",
    )
    .bind(i64::try_from(storage_root_id.0).map_err(|_| {
        AccessResolutionError::DatabaseError("storage_root_id overflow".to_string())
    })?)
    .fetch_optional(executor)
    .await?
    .ok_or(AccessResolutionError::StorageRootNotFound { storage_root_id })?;

    let owner_node_id: i64 = row.try_get("owner_node_id").map_err(|e| {
        AccessResolutionError::DatabaseError(format!("Failed to read owner_node_id: {e}"))
    })?;

    let state: String = row
        .try_get("state")
        .map_err(|e| AccessResolutionError::DatabaseError(format!("Failed to read state: {e}")))?;

    let root_state = match state.as_str() {
        "active" => RootState::Active,
        "configured" => RootState::Configured,
        _ => {
            return Err(AccessResolutionError::InvalidRootState {
                storage_root_id,
                state,
            });
        }
    };

    let root_epoch: i64 = row.try_get("root_epoch").map_err(|e| {
        AccessResolutionError::DatabaseError(format!("Failed to read root_epoch: {e}"))
    })?;

    Ok(ResolvedRoot {
        storage_root_id,
        owner_node_id,
        state: root_state,
        root_epoch,
    })
}

/// Resolve a file location by ID, enforcing the `storage_root_id` constraint.
///
/// Returns the location's storage root, owner, and state, or an error if not found
/// or if the `storage_root_id` does not match the declared root.
pub async fn resolve_file_location<'e, E>(
    executor: E,
    file_location_id: FileLocationId,
    declared_storage_root_id: StorageRootId,
) -> Result<ResolvedLocation, AccessResolutionError>
where
    E: Executor<'e, Database = Sqlite>,
{
    let row = sqlx::query(
        r"
        SELECT fl.id AS id, fl.storage_root_id AS storage_root_id,
               fl.address_state AS address_state, fl.retired_at AS retired_at,
               lr.owner_node_id AS owner_node_id, lr.state AS root_state
        FROM file_locations fl
        LEFT JOIN library_roots lr ON lr.id = fl.storage_root_id
        WHERE fl.id = ?1
        ",
    )
    .bind(i64::try_from(file_location_id.0).map_err(|_| {
        AccessResolutionError::DatabaseError("file_location_id overflow".to_string())
    })?)
    .fetch_optional(executor)
    .await?
    .ok_or(AccessResolutionError::FileLocationNotFound { file_location_id })?;

    let storage_root_id: i64 = row.try_get("storage_root_id").map_err(|e| {
        AccessResolutionError::DatabaseError(format!("Failed to read storage_root_id: {e}"))
    })?;

    let storage_root_id = StorageRootId(u64::try_from(storage_root_id).map_err(|_| {
        AccessResolutionError::DatabaseError("storage_root_id overflow".to_string())
    })?);

    if storage_root_id != declared_storage_root_id {
        return Err(AccessResolutionError::LocationRootInvalid {
            file_location_id,
            storage_root_id: declared_storage_root_id,
        });
    }

    let state: String = row.try_get("address_state").map_err(|e| {
        AccessResolutionError::DatabaseError(format!("Failed to read address_state: {e}"))
    })?;
    if row
        .try_get::<Option<String>, _>("retired_at")
        .map_err(|e| {
            AccessResolutionError::DatabaseError(format!("Failed to read retired_at: {e}"))
        })?
        .is_some()
    {
        return Err(AccessResolutionError::InvalidLocationState {
            file_location_id,
            state: "retired".to_owned(),
        });
    }

    let location_state = match state.as_str() {
        "rooted" => LocationState::Rooted,
        _ => {
            return Err(AccessResolutionError::InvalidLocationState {
                file_location_id,
                state,
            });
        }
    };

    // The declared root must exist and be live; its owner is the location's owner.
    let owner_node_id: Option<i64> = row.try_get("owner_node_id").map_err(|e| {
        AccessResolutionError::DatabaseError(format!("Failed to read owner_node_id: {e}"))
    })?;
    let Some(owner_node_id) = owner_node_id else {
        return Err(AccessResolutionError::StorageRootNotFound { storage_root_id });
    };
    let root_state: String = row
        .try_get("root_state")
        .map_err(|e| AccessResolutionError::DatabaseError(format!("Failed to read state: {e}")))?;
    match root_state.as_str() {
        "active" | "configured" => {}
        _ => {
            return Err(AccessResolutionError::InvalidRootState {
                storage_root_id,
                state: root_state,
            });
        }
    }

    Ok(ResolvedLocation {
        file_location_id,
        storage_root_id,
        owner_node_id,
        state: location_state,
    })
}

/// Resolve the active incarnation for a given owner node.
pub async fn resolve_active_incarnation<'e, E>(
    executor: E,
    owner_node_id: i64,
) -> Result<String, AccessResolutionError>
where
    E: Executor<'e, Database = Sqlite>,
{
    let row = sqlx::query(
        r"
        SELECT incarnation_id
        FROM node_incarnations
        WHERE node_id = ?1 AND status = 'active'
        ORDER BY incarnation_id DESC
        LIMIT 1
        ",
    )
    .bind(owner_node_id)
    .fetch_optional(executor)
    .await?
    .ok_or(AccessResolutionError::NoActiveIncarnation { owner_node_id })?;

    let incarnation_id: String = row.try_get("incarnation_id").map_err(|e| {
        AccessResolutionError::DatabaseError(format!("Failed to read incarnation_id: {e}"))
    })?;

    Ok(incarnation_id)
}

#[cfg(test)]
#[path = "artifact_access_resolution_test.rs"]
mod tests;
