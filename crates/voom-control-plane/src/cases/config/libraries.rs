//! `ControlPlane` wrappers over `SqliteLibraryRepo` for the `voom library` /
//! `voom library root` CLI, including atomic lifecycle facts.

use voom_core::{LibraryId, NodeId, StorageRootId, StorageRootState, VoomError};
use voom_events::payload::{
    StorageRootActivatedPayload, StorageRootCreatedPayload, StorageRootOwnerAssignedPayload,
    StorageRootReactivatedPayload, StorageRootRetiredPayload, StorageRootValidationLostPayload,
};
use voom_events::{Event, SubjectType};
use voom_store::repo::library::libraries::{Library, LibraryUpdate, NewLibrary};
use voom_store::repo::library::library_roots::{
    EffectiveLibraryRoot, LibraryRoot, LibraryRootUpdate, NewLibraryRoot,
};

use crate::ControlPlane;
use crate::cases::{append_event, begin_tx, commit_tx};

impl ControlPlane {
    /// Create a library.
    ///
    /// # Errors
    /// Returns `Conflict` for a duplicate slug; propagates repository errors.
    pub async fn create_library(&self, input: NewLibrary) -> Result<Library, VoomError> {
        self.libraries
            .create_library(input, self.clock().now())
            .await
    }

    /// Get a library by id.
    ///
    /// # Errors
    /// Propagates repository errors.
    pub async fn get_library(&self, id: LibraryId) -> Result<Option<Library>, VoomError> {
        self.libraries.get_library(id).await
    }

    /// List libraries in creation order.
    ///
    /// # Errors
    /// Propagates repository errors.
    pub async fn list_libraries(&self) -> Result<Vec<Library>, VoomError> {
        self.libraries.list_libraries().await
    }

    /// Apply a partial update to a library.
    ///
    /// # Errors
    /// Returns `NotFound` for a missing id; propagates repository errors.
    pub async fn update_library(
        &self,
        id: LibraryId,
        update: LibraryUpdate,
    ) -> Result<Library, VoomError> {
        self.libraries
            .update_library(id, update, self.clock().now())
            .await
    }

    /// Enable or disable a library.
    ///
    /// # Errors
    /// Returns `NotFound` for a missing id; propagates repository errors.
    pub async fn set_library_enabled(
        &self,
        id: LibraryId,
        enabled: bool,
    ) -> Result<Library, VoomError> {
        self.libraries
            .set_library_enabled(id, enabled, self.clock().now())
            .await
    }

    /// Set or clear a library's default quality scoring profile by name. `None`
    /// clears the default. A named profile must exist and be active (not
    /// retired), since a library should not default to a retired profile.
    ///
    /// # Errors
    /// Returns `NotFound` for a missing library or an unknown/retired profile;
    /// propagates repository errors.
    pub async fn set_library_default_scoring_profile(
        &self,
        id: LibraryId,
        profile_name: Option<&str>,
    ) -> Result<Library, VoomError> {
        if let Some(name) = profile_name {
            match self.quality_scoring_profiles.get_by_name(name).await? {
                Some(profile) if profile.retired_at.is_none() => {}
                Some(_) => {
                    return Err(VoomError::NotFound(format!(
                        "quality scoring profile {name:?} is retired"
                    )));
                }
                None => {
                    return Err(VoomError::NotFound(format!(
                        "quality scoring profile {name:?} not found"
                    )));
                }
            }
        }
        self.libraries
            .set_default_scoring_profile(id, profile_name, self.clock().now())
            .await
    }

    /// Delete a library with no durable roots. Root history is retained, so a
    /// library with any root must be retained too.
    ///
    /// # Errors
    /// Propagates repository errors.
    pub async fn delete_library(&self, id: LibraryId) -> Result<bool, VoomError> {
        self.libraries.delete_library(id).await
    }

    /// Create a configured node-owned library root and its lifecycle fact in
    /// one transaction.
    ///
    /// # Errors
    /// Returns `NotFound` for a missing library or owner and `Conflict` for a
    /// retired owner or duplicate owner/provider/locator tuple.
    pub async fn create_library_root(
        &self,
        input: NewLibraryRoot,
    ) -> Result<LibraryRoot, VoomError> {
        let now = self.clock().now();
        let mut tx = begin_tx(&self.pool).await?;
        let result = async {
            let root = self
                .libraries
                .create_library_root_in_tx(&mut tx, input, now)
                .await?;
            append_event(
                &self.events,
                &mut tx,
                SubjectType::StorageRoot,
                Some(root.id.0),
                now,
                Event::StorageRootCreated(StorageRootCreatedPayload {
                    storage_root_id: root.id,
                    library_id: root.library_id,
                    owner_node_id: root.owner_node_id.ok_or_else(|| {
                        VoomError::database("new storage root has no owner".to_owned())
                    })?,
                    provider_kind: root.provider_kind,
                    state: root.state,
                    root_epoch: root.root_epoch,
                }),
            )
            .await?;
            Ok(root)
        }
        .await;
        match result {
            Ok(root) => {
                commit_tx(tx).await?;
                Ok(root)
            }
            Err(error) => {
                tx.rollback().await.map_err(|rollback_error| {
                    VoomError::database_context("library root create rollback", rollback_error)
                })?;
                Err(error)
            }
        }
    }

    /// Get a library root by id.
    ///
    /// # Errors
    /// Propagates repository errors.
    pub async fn get_library_root(
        &self,
        id: StorageRootId,
    ) -> Result<Option<LibraryRoot>, VoomError> {
        self.libraries.get_library_root(id).await
    }

    /// List library roots, optionally filtered to one library.
    ///
    /// # Errors
    /// Propagates repository errors.
    pub async fn list_library_roots(
        &self,
        library_id: Option<LibraryId>,
    ) -> Result<Vec<LibraryRoot>, VoomError> {
        self.libraries.list_library_roots(library_id).await
    }

    /// Read a root with the parent-library and owner-node availability overlay.
    pub async fn effective_library_root(
        &self,
        id: StorageRootId,
    ) -> Result<Option<EffectiveLibraryRoot>, VoomError> {
        self.libraries.effective_library_root(id).await
    }

    /// Apply a partial update to a library root.
    ///
    /// # Errors
    /// Returns `NotFound` for a missing id; propagates repository errors.
    pub async fn update_library_root(
        &self,
        id: StorageRootId,
        update: LibraryRootUpdate,
    ) -> Result<LibraryRoot, VoomError> {
        self.libraries
            .update_library_root(id, update, self.clock().now())
            .await
    }

    /// Enable or disable a library root.
    ///
    /// # Errors
    /// Returns `NotFound` for a missing id; propagates repository errors.
    pub async fn set_library_root_enabled(
        &self,
        id: StorageRootId,
        enabled: bool,
    ) -> Result<LibraryRoot, VoomError> {
        self.libraries
            .set_library_root_enabled(id, enabled, self.clock().now())
            .await
    }

    /// Assign a migrated, never-activated root to a non-retired owner.
    pub async fn assign_library_root_owner(
        &self,
        id: StorageRootId,
        owner_node_id: NodeId,
    ) -> Result<LibraryRoot, VoomError> {
        let now = self.clock().now();
        let mut tx = begin_tx(&self.pool).await?;
        let root = self
            .libraries
            .assign_library_root_owner_in_tx(&mut tx, id, owner_node_id, now)
            .await?;
        append_event(
            &self.events,
            &mut tx,
            SubjectType::StorageRoot,
            Some(id.0),
            now,
            Event::StorageRootOwnerAssigned(StorageRootOwnerAssignedPayload {
                storage_root_id: id,
                owner_node_id,
                state: root.state,
                root_epoch: root.root_epoch,
            }),
        )
        .await?;
        commit_tx(tx).await?;
        Ok(root)
    }

    /// Record successful owner validation. Revalidation from unavailable emits
    /// a reactivation fact; first activation emits an activation fact.
    pub async fn activate_library_root(
        &self,
        id: StorageRootId,
        activation_identity: String,
    ) -> Result<LibraryRoot, VoomError> {
        let now = self.clock().now();
        let mut tx = begin_tx(&self.pool).await?;
        let prior = self
            .libraries
            .get_library_root_in_tx(&mut tx, id)
            .await?
            .ok_or_else(|| VoomError::NotFound(format!("library root {id} not found")))?;
        let root = self
            .libraries
            .activate_library_root_in_tx(&mut tx, id, activation_identity.clone(), now)
            .await?;
        let owner_node_id = required_root_owner(&root)?;
        let event = if prior.state == StorageRootState::Unavailable {
            Event::StorageRootReactivated(StorageRootReactivatedPayload {
                storage_root_id: id,
                owner_node_id,
                activation_identity,
                root_epoch: root.root_epoch,
            })
        } else {
            Event::StorageRootActivated(StorageRootActivatedPayload {
                storage_root_id: id,
                owner_node_id,
                activation_identity,
                root_epoch: root.root_epoch,
            })
        };
        append_event(
            &self.events,
            &mut tx,
            SubjectType::StorageRoot,
            Some(id.0),
            now,
            event,
        )
        .await?;
        commit_tx(tx).await?;
        Ok(root)
    }

    /// Mark an active root unavailable after owner validation is lost.
    pub async fn mark_library_root_unavailable(
        &self,
        id: StorageRootId,
        reason: String,
    ) -> Result<LibraryRoot, VoomError> {
        if reason.trim().is_empty() {
            return Err(VoomError::Config(
                "storage root validation-loss reason must not be empty".to_owned(),
            ));
        }
        let now = self.clock().now();
        let mut tx = begin_tx(&self.pool).await?;
        let root = self
            .libraries
            .mark_library_root_unavailable_in_tx(&mut tx, id, now)
            .await?;
        let owner_node_id = required_root_owner(&root)?;
        append_event(
            &self.events,
            &mut tx,
            SubjectType::StorageRoot,
            Some(id.0),
            now,
            Event::StorageRootValidationLost(StorageRootValidationLostPayload {
                storage_root_id: id,
                owner_node_id,
                reason,
                root_epoch: root.root_epoch,
            }),
        )
        .await?;
        commit_tx(tx).await?;
        Ok(root)
    }

    /// Terminally retire a root while retaining its stable identity and facts.
    pub async fn retire_library_root(&self, id: StorageRootId) -> Result<LibraryRoot, VoomError> {
        let now = self.clock().now();
        let mut tx = begin_tx(&self.pool).await?;
        let root = self
            .libraries
            .retire_library_root_in_tx(&mut tx, id, now)
            .await?;
        append_event(
            &self.events,
            &mut tx,
            SubjectType::StorageRoot,
            Some(id.0),
            now,
            Event::StorageRootRetired(StorageRootRetiredPayload {
                storage_root_id: id,
                owner_node_id: root.owner_node_id,
                root_epoch: root.root_epoch,
            }),
        )
        .await?;
        commit_tx(tx).await?;
        Ok(root)
    }
}

fn required_root_owner(root: &LibraryRoot) -> Result<NodeId, VoomError> {
    root.owner_node_id
        .ok_or_else(|| VoomError::database(format!("storage root {} has no owner", root.id)))
}

#[cfg(test)]
#[path = "libraries_test.rs"]
mod tests;
