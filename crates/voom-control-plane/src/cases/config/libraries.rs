//! `ControlPlane` wrappers over `SqliteLibraryRepo` for the `voom library` /
//! `voom library root` CLI. Thin delegation plus the no-overlap rule that keeps
//! root ownership unambiguous (ADR 0027).

use std::path::Path;

use voom_core::{LibraryId, LibraryRootId, VoomError};
use voom_store::repo::library::libraries::{Library, LibraryUpdate, NewLibrary};
use voom_store::repo::library::library_roots::{LibraryRoot, LibraryRootUpdate, NewLibraryRoot};

use crate::ControlPlane;

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

    /// Delete a library (its roots cascade). Returns whether a row was removed.
    ///
    /// # Errors
    /// Propagates repository errors.
    pub async fn delete_library(&self, id: LibraryId) -> Result<bool, VoomError> {
        self.libraries.delete_library(id).await
    }

    /// Create a library root. `input.canonical_path` must already be
    /// canonicalized by the caller. Rejects a path that overlaps an existing
    /// root (component-wise ancestor-or-descendant) so no file is claimed by
    /// two roots.
    ///
    /// # Errors
    /// Returns `NotFound` for a missing library, `Conflict` for a duplicate or
    /// overlapping `canonical_path`; propagates repository errors.
    pub async fn create_library_root(
        &self,
        input: NewLibraryRoot,
    ) -> Result<LibraryRoot, VoomError> {
        for existing in self.libraries.list_library_roots(None).await? {
            if paths_overlap(&existing.canonical_path, &input.canonical_path) {
                return Err(VoomError::Conflict(format!(
                    "library root path {:?} overlaps existing root {} at {:?}",
                    input.canonical_path, existing.id, existing.canonical_path
                )));
            }
        }
        self.libraries
            .create_library_root(input, self.clock().now())
            .await
    }

    /// Get a library root by id.
    ///
    /// # Errors
    /// Propagates repository errors.
    pub async fn get_library_root(
        &self,
        id: LibraryRootId,
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

    /// Apply a partial update to a library root.
    ///
    /// # Errors
    /// Returns `NotFound` for a missing id; propagates repository errors.
    pub async fn update_library_root(
        &self,
        id: LibraryRootId,
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
        id: LibraryRootId,
        enabled: bool,
    ) -> Result<LibraryRoot, VoomError> {
        self.libraries
            .set_library_root_enabled(id, enabled, self.clock().now())
            .await
    }

    /// Delete a library root. Returns whether a row was removed.
    ///
    /// # Errors
    /// Propagates repository errors.
    pub async fn delete_library_root(&self, id: LibraryRootId) -> Result<bool, VoomError> {
        self.libraries.delete_library_root(id).await
    }
}

/// True when either path is a component-wise prefix of the other (equal,
/// ancestor, or descendant). Both must be canonical absolute paths. Component
/// comparison — not string prefix — so `/media/movies` and
/// `/media/movies-adult` do **not** overlap.
pub(crate) fn paths_overlap(a: &str, b: &str) -> bool {
    let (a, b) = (Path::new(a), Path::new(b));
    a.starts_with(b) || b.starts_with(a)
}

#[cfg(test)]
#[path = "libraries_test.rs"]
mod tests;
