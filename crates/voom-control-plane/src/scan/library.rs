//! `voom scan --root <id>`: fail-closed scan of a configured library root.
//!
//! A disabled root or disabled parent library yields `RootScanOutcome::Blocked`
//! with **no discovery, no worker launch, no persistence** — the daemon-readiness
//! fail-closed contract (ADR 0027). An enabled root scans its canonical path
//! honoring the root's extension allowlist.

use std::path::PathBuf;

use voom_core::{LibraryId, StorageRootId, VoomError};
use voom_store::repo::library::library_roots::RootAvailabilityReason;

use super::{
    ScanCommandError, ScanMode, ScanPathInput, ScanReport, ScanSummary, command_error_from_voom,
};
use crate::ControlPlane;

/// Outcome of `scan_library_root`. `Blocked` means the root was not scanned
/// because it (or its library) is disabled.
#[derive(Debug)]
pub enum RootScanOutcome {
    Scanned(ScanReport),
    Blocked(RootScanBlocked),
}

/// Why a root scan was refused, plus the identifiers an operator needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RootScanBlocked {
    pub library_id: LibraryId,
    pub storage_root_id: StorageRootId,
    pub reason: RootBlockReason,
    pub provider_locator: String,
}

/// The disabled resource that blocked the scan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RootBlockReason {
    LibraryDisabled,
    RootDisabled,
    RootUnassigned,
    RootNotActive,
    OwnerRegistered,
    OwnerStale,
    OwnerRetired,
    LocalNodeUnconfigured,
    OwnerNotLocal,
}

impl RootBlockReason {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::RootDisabled => "root_disabled",
            Self::LibraryDisabled => "library_disabled",
            Self::RootUnassigned => "root_unassigned",
            Self::RootNotActive => "root_not_active",
            Self::OwnerRegistered => "owner_registered",
            Self::OwnerStale => "owner_stale",
            Self::OwnerRetired => "owner_retired",
            Self::LocalNodeUnconfigured => "local_node_unconfigured",
            Self::OwnerNotLocal => "owner_not_local",
        }
    }
}

impl RootBlockReason {
    pub(super) fn from_availability(reason: RootAvailabilityReason) -> Option<Self> {
        match reason {
            RootAvailabilityReason::Available => None,
            RootAvailabilityReason::LibraryDisabled => Some(Self::LibraryDisabled),
            RootAvailabilityReason::RootDisabled => Some(Self::RootDisabled),
            RootAvailabilityReason::RootUnassigned => Some(Self::RootUnassigned),
            RootAvailabilityReason::RootNotActive => Some(Self::RootNotActive),
            RootAvailabilityReason::OwnerRegistered => Some(Self::OwnerRegistered),
            RootAvailabilityReason::OwnerStale => Some(Self::OwnerStale),
            RootAvailabilityReason::OwnerRetired => Some(Self::OwnerRetired),
        }
    }
}

impl ControlPlane {
    /// Scan a configured library root. Fail-closed: a disabled root or library
    /// returns `Blocked` without touching the filesystem or persisting rows.
    ///
    /// # Errors
    /// Returns a `NOT_FOUND` `ScanCommandError` for a missing root, and any
    /// error from the underlying scan when the root is enabled.
    pub async fn scan_library_root(
        &self,
        root_id: StorageRootId,
    ) -> Result<RootScanOutcome, ScanCommandError> {
        let effective = self
            .effective_library_root(root_id)
            .await
            .map_err(|e| lookup_error(&e))?
            .ok_or_else(|| {
                lookup_error(&VoomError::NotFound(format!(
                    "library root {root_id} not found"
                )))
            })?;
        let root = effective.root;
        let provider_locator = root.provider_locator.as_str().to_owned();
        if let Some(reason) = RootBlockReason::from_availability(effective.reason) {
            return Ok(RootScanOutcome::Blocked(RootScanBlocked {
                library_id: root.library_id,
                storage_root_id: root_id,
                reason,
                provider_locator,
            }));
        }
        let owner = root.owner_node_id.ok_or_else(|| {
            lookup_error(&VoomError::database(format!(
                "available storage root {root_id} has no owner"
            )))
        })?;
        let Some(local_node_id) = self.local_node_id else {
            return Ok(RootScanOutcome::Blocked(RootScanBlocked {
                library_id: root.library_id,
                storage_root_id: root_id,
                reason: RootBlockReason::LocalNodeUnconfigured,
                provider_locator,
            }));
        };
        if owner != local_node_id {
            return Ok(RootScanOutcome::Blocked(RootScanBlocked {
                library_id: root.library_id,
                storage_root_id: root_id,
                reason: RootBlockReason::OwnerNotLocal,
                provider_locator,
            }));
        }
        let configured_path = PathBuf::from(root.provider_locator.as_str());
        let metadata = tokio::fs::symlink_metadata(&configured_path)
            .await
            .map_err(|error| {
                lookup_error(&VoomError::Config(format!(
                    "cannot inspect storage root {root_id}: {error}"
                )))
            })?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(lookup_error(&VoomError::Config(format!(
                "storage root {root_id} must resolve from a non-symlink directory"
            ))));
        }
        let canonical_path = tokio::fs::canonicalize(&configured_path)
            .await
            .map_err(|error| {
                lookup_error(&VoomError::Config(format!(
                    "cannot canonicalize storage root {root_id}: {error}"
                )))
            })?;

        let input = ScanPathInput {
            storage_root_id: root_id,
            root_path: canonical_path.clone(),
            path: canonical_path,
            extension_allowlist: root.extension_allowlist,
        };
        self.scan_path(input).await.map(RootScanOutcome::Scanned)
    }
}

/// Convert a pre-scan lookup `VoomError` into a `ScanCommandError` with an empty
/// report (no discovery ran).
fn lookup_error(err: &VoomError) -> ScanCommandError {
    command_error_from_voom(err, empty_report())
}

fn empty_report() -> ScanReport {
    ScanReport {
        path: PathBuf::new(),
        mode: ScanMode::Directory,
        summary: ScanSummary::default(),
        files: Vec::new(),
        skipped: Vec::new(),
    }
}

#[cfg(test)]
#[path = "library_test.rs"]
mod tests;
