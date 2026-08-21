//! The single place an operation's canonical artifact access is decided.
//!
//! Rights are a property of the operation, never a renderer's choice: a renderer
//! free to pick a different rights set for the same operation would make the
//! evidence downstream consumers resolve non-deterministic, and no canonical-form
//! rule would catch it. Both the encode and the decode side of the payload gate
//! compare against this one function, so a persisted declaration is valid exactly
//! when it equals what this would have produced (ADR 0069).

use voom_core::{
    ArtifactAccessDeclaration, ArtifactAccessEntry, ArtifactAccessRight, ArtifactAccessTarget,
    FileLocationAccess, FileLocationId, OperationKind, StorageRootAccess, StorageRootId, VoomError,
};

/// The storage a ticket was rendered against.
///
/// Two variants rather than one struct with an optional location: a single struct
/// with both fields required cannot express a root-addressed ticket without a
/// fabricated `file_location_id`, and a fabricated ID is a live reference to
/// somebody else's location.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TicketStorageSource {
    /// A whole root. What a staged, not-yet-named output addresses.
    Root { storage_root_id: StorageRootId },
    /// A live location inside a root.
    Location {
        storage_root_id: StorageRootId,
        file_location_id: FileLocationId,
    },
}

impl TicketStorageSource {
    const fn storage_root_id(self) -> StorageRootId {
        match self {
            Self::Root { storage_root_id }
            | Self::Location {
                storage_root_id, ..
            } => storage_root_id,
        }
    }
}

/// The canonical declaration for an operation rendered against a source.
///
/// Total over `OperationKind` × source variant: no operation rejects a variant.
/// The three non-byte-touching operations yield `Ok(None)`; a byte-touching one
/// reached with no source at all is a configuration error, because a ticket that
/// opens bytes without saying which is exactly what this change forbids.
///
/// # Errors
///
/// Returns [`VoomError::Config`] when `operation` is byte-touching and `source` is
/// `None`, or when the constructed entries are not canonical — which would be a
/// defect in this function rather than in its caller.
pub(crate) fn declaration_for(
    operation: OperationKind,
    source: Option<&TicketStorageSource>,
) -> Result<Option<ArtifactAccessDeclaration>, VoomError> {
    if !operation.is_byte_touching() {
        return Ok(None);
    }
    let Some(source) = source.copied() else {
        return Err(VoomError::Config(format!(
            "byte-touching operation {} requires a storage source",
            operation.as_str()
        )));
    };
    // Unreachable given the guard above, but propagated rather than panicked:
    // unreachability here is an argument about callers, not a type-level fact.
    let Some(entries) = entries_for(operation, source) else {
        return Err(VoomError::Config(format!(
            "operation {} is byte-touching but has no canonical declaration",
            operation.as_str()
        )));
    };
    ArtifactAccessDeclaration::new(entries).map(Some)
}

fn entries_for(
    operation: OperationKind,
    source: TicketStorageSource,
) -> Option<Vec<ArtifactAccessEntry>> {
    use ArtifactAccessRight::{Delete, Read, Write};

    let root = source.storage_root_id();
    // A scan enumerates a root by definition, so it projects a location source to
    // that location's root — the location is not what the scan addresses.
    let location = match (operation, source) {
        (OperationKind::ScanLibrary, _) | (_, TicketStorageSource::Root { .. }) => None,
        (
            _,
            TicketStorageSource::Location {
                file_location_id, ..
            },
        ) => Some(file_location_id),
    };

    match operation {
        // Read-only against what it addresses.
        OperationKind::ScanLibrary
        | OperationKind::ProbeFile
        | OperationKind::HashFile
        | OperationKind::VerifyArtifact => Some(vec![entry(target(root, location), &[Read])]),
        // Reads a source and writes an output. The output has no location yet, so
        // the write attaches to the root; with no location the two collapse into
        // one entry, which is what keeps the declaration canonical.
        OperationKind::Remux
        | OperationKind::TranscodeVideo
        | OperationKind::TranscodeAudio
        | OperationKind::ExtractAudio
        | OperationKind::EditTracks
        | OperationKind::BackUpFile
        | OperationKind::CommitArtifact => Some(location.map_or_else(
            || vec![entry(root_target(root), &[Read, Write])],
            |location| {
                // storage_root sorts before file_location, so this is canonical
                // as constructed and needs no sort.
                vec![
                    entry(root_target(root), &[Write]),
                    entry(location_target(root, location), &[Read]),
                ]
            },
        )),
        OperationKind::DeleteArtifact => Some(vec![entry(target(root, location), &[Read, Delete])]),
        OperationKind::IdentifyMedia
        | OperationKind::ScoreQuality
        | OperationKind::SyncExternalSystem => None,
    }
}

fn target(root: StorageRootId, location: Option<FileLocationId>) -> ArtifactAccessTarget {
    location.map_or_else(
        || root_target(root),
        |location| location_target(root, location),
    )
}

const fn root_target(storage_root_id: StorageRootId) -> ArtifactAccessTarget {
    ArtifactAccessTarget::StorageRoot(StorageRootAccess { storage_root_id })
}

const fn location_target(
    storage_root_id: StorageRootId,
    file_location_id: FileLocationId,
) -> ArtifactAccessTarget {
    ArtifactAccessTarget::FileLocation(FileLocationAccess {
        storage_root_id,
        file_location_id,
    })
}

fn entry(target: ArtifactAccessTarget, rights: &[ArtifactAccessRight]) -> ArtifactAccessEntry {
    ArtifactAccessEntry {
        target,
        rights: rights.to_vec(),
    }
}

#[cfg(test)]
#[path = "access_declaration_test.rs"]
mod tests;
