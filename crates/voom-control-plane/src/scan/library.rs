//! `voom scan --root <id>`: fail-closed scan of a configured library root.
//!
//! A disabled root or disabled parent library yields `RootScanOutcome::Blocked`
//! with **no discovery, no worker launch, no persistence** — the daemon-readiness
//! fail-closed contract (ADR 0027). An enabled root scans its canonical path
//! honoring the root's extension allowlist.

use std::path::PathBuf;

use voom_core::{LibraryId, LibraryRootId, VoomError};

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
    pub library_root_id: LibraryRootId,
    pub reason: RootBlockReason,
    pub canonical_path: PathBuf,
}

/// The disabled resource that blocked the scan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RootBlockReason {
    RootDisabled,
    LibraryDisabled,
}

impl RootBlockReason {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::RootDisabled => "root_disabled",
            Self::LibraryDisabled => "library_disabled",
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
        root_id: LibraryRootId,
    ) -> Result<RootScanOutcome, ScanCommandError> {
        let root = self
            .get_library_root(root_id)
            .await
            .map_err(|e| lookup_error(&e))?
            .ok_or_else(|| {
                lookup_error(&VoomError::NotFound(format!(
                    "library root {root_id} not found"
                )))
            })?;
        let library = self
            .get_library(root.library_id)
            .await
            .map_err(|e| lookup_error(&e))?
            .ok_or_else(|| {
                lookup_error(&VoomError::Internal(format!(
                    "library {} for root {root_id} not found",
                    root.library_id
                )))
            })?;

        let canonical_path = PathBuf::from(&root.canonical_path);
        if !root.enabled {
            return Ok(RootScanOutcome::Blocked(RootScanBlocked {
                library_id: root.library_id,
                library_root_id: root_id,
                reason: RootBlockReason::RootDisabled,
                canonical_path,
            }));
        }
        if !library.enabled {
            return Ok(RootScanOutcome::Blocked(RootScanBlocked {
                library_id: root.library_id,
                library_root_id: root_id,
                reason: RootBlockReason::LibraryDisabled,
                canonical_path,
            }));
        }

        let input = ScanPathInput {
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
