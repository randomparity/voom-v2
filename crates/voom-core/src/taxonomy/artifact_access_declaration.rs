//! Canonical declaration of the storage a byte-touching ticket intends to access.
//!
//! Routing evidence only. Every target names stable IDs — never a path, provider
//! locator, mount name, or host string — so nothing here can be mistaken for proof
//! of locality (ADR 0050, ADR 0069). Rights state intent; this module exposes
//! nothing that resolves, authorizes, or performs anything.
//!
//! Construction and deserialization run the same validation, so a declaration has
//! exactly one accepted encoding. **Variant and field declaration order below is
//! durable wire contract**: `ArtifactAccessTarget`'s derived `Ord` decides whether a
//! persisted payload is canonical, and reordering silently invalidates every stored
//! declaration.
//!
//! Two sibling tests guard that, and it takes both. `frozen_canonical_encoding_…`
//! pins the serialized form by comparing *strings* — a `Value` comparison would not,
//! because the workspace builds `serde_json` with `preserve_order` and `IndexMap`'s
//! `PartialEq` ignores key order. `within_variant_field_order_…` pins the derived
//! `Ord`, which the frozen fixture alone cannot reach: it holds one entry per
//! variant, so no comparison in it ever gets past the variant discriminant.

use std::collections::HashSet;

use serde::{Deserialize, Deserializer, Serialize};

use crate::VoomError;
use crate::taxonomy::ids::{ArtifactHandleId, FileLocationId, StorageRootId};

/// Intended byte access for one storage reference. Canonical order is declaration
/// order: `read < write < delete`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactAccessRight {
    Read,
    Write,
    Delete,
}

impl ArtifactAccessRight {
    pub const ALL: &'static [Self] = &[Self::Read, Self::Write, Self::Delete];

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Write => "write",
            Self::Delete => "delete",
        }
    }

    /// Parse the stable wire token produced by [`Self::as_str`].
    #[must_use]
    pub fn from_wire(token: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|right| right.as_str() == token)
    }
}

impl std::fmt::Display for ArtifactAccessRight {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A whole storage root: enumeration, or a write whose artifact is not yet named.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StorageRootAccess {
    pub storage_root_id: StorageRootId,
}

/// An existing live location inside a root.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileLocationAccess {
    pub storage_root_id: StorageRootId,
    pub file_location_id: FileLocationId,
}

/// A materialized artifact handle. The location and root are required: an existing
/// handle that cannot say where its bytes are has no encoding.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExistingArtifactAccess {
    pub artifact_handle_id: ArtifactHandleId,
    pub storage_root_id: StorageRootId,
    pub file_location_id: FileLocationId,
}

/// An output handle whose bytes do not exist yet. The target root is required, and
/// no location may be named, because none exists until commit creates one.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlannedArtifactAccess {
    pub artifact_handle_id: ArtifactHandleId,
    pub target_storage_root_id: StorageRootId,
}

/// Closed vocabulary of what a declaration entry may address.
///
/// Newtype variants over `deny_unknown_fields` content structs, per ADR 0013 — an
/// inline tagged struct-variant is a silent no-op and is forbidden for durable enums.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ArtifactAccessTarget {
    StorageRoot(StorageRootAccess),
    FileLocation(FileLocationAccess),
    ExistingArtifact(ExistingArtifactAccess),
    PlannedArtifact(PlannedArtifactAccess),
}

impl ArtifactAccessTarget {
    /// The file location this target names, if any.
    #[must_use]
    pub const fn file_location_id(&self) -> Option<FileLocationId> {
        match self {
            Self::StorageRoot(_) | Self::PlannedArtifact(_) => None,
            Self::FileLocation(target) => Some(target.file_location_id),
            Self::ExistingArtifact(target) => Some(target.file_location_id),
        }
    }

    /// The artifact handle this target names, if any.
    #[must_use]
    pub const fn artifact_handle_id(&self) -> Option<ArtifactHandleId> {
        match self {
            Self::StorageRoot(_) | Self::FileLocation(_) => None,
            Self::ExistingArtifact(target) => Some(target.artifact_handle_id),
            Self::PlannedArtifact(target) => Some(target.artifact_handle_id),
        }
    }

    /// The storage root this target is scoped to.
    #[must_use]
    pub const fn storage_root_id(&self) -> StorageRootId {
        match self {
            Self::StorageRoot(target) => target.storage_root_id,
            Self::FileLocation(target) => target.storage_root_id,
            Self::ExistingArtifact(target) => target.storage_root_id,
            Self::PlannedArtifact(target) => target.target_storage_root_id,
        }
    }

    /// Report the first zero ID and the field it came from.
    const fn zero_id_field(&self) -> Option<&'static str> {
        match self {
            Self::StorageRoot(target) => {
                if target.storage_root_id.0 == 0 {
                    Some("storage_root_id")
                } else {
                    None
                }
            }
            Self::FileLocation(target) => {
                if target.storage_root_id.0 == 0 {
                    Some("storage_root_id")
                } else if target.file_location_id.0 == 0 {
                    Some("file_location_id")
                } else {
                    None
                }
            }
            Self::ExistingArtifact(target) => {
                if target.artifact_handle_id.0 == 0 {
                    Some("artifact_handle_id")
                } else if target.storage_root_id.0 == 0 {
                    Some("storage_root_id")
                } else if target.file_location_id.0 == 0 {
                    Some("file_location_id")
                } else {
                    None
                }
            }
            Self::PlannedArtifact(target) => {
                if target.artifact_handle_id.0 == 0 {
                    Some("artifact_handle_id")
                } else if target.target_storage_root_id.0 == 0 {
                    Some("target_storage_root_id")
                } else {
                    None
                }
            }
        }
    }
}

/// One target and the access intended on it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactAccessEntry {
    pub target: ArtifactAccessTarget,
    pub rights: Vec<ArtifactAccessRight>,
}

/// A non-empty, canonically ordered set of access entries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct ArtifactAccessDeclaration(Vec<ArtifactAccessEntry>);

impl ArtifactAccessDeclaration {
    /// Build a declaration, enforcing canonical form.
    ///
    /// # Errors
    ///
    /// Returns [`VoomError::Config`] when the entry list is empty, an entry declares
    /// no rights, rights or entries are not strictly ascending, a file location or
    /// artifact handle is named by more than one entry, or any ID is zero.
    pub fn new(entries: Vec<ArtifactAccessEntry>) -> Result<Self, VoomError> {
        validate(&entries).map_err(VoomError::Config)?;
        Ok(Self(entries))
    }

    #[must_use]
    pub fn entries(&self) -> &[ArtifactAccessEntry] {
        &self.0
    }

    /// Every file location this declaration names, in canonical entry order.
    pub fn file_location_ids(&self) -> impl Iterator<Item = FileLocationId> + '_ {
        self.0
            .iter()
            .filter_map(|entry| entry.target.file_location_id())
    }

    /// Every storage root this declaration names, in canonical entry order. Roots may
    /// repeat: reading a source and writing an output into the same root is ordinary.
    pub fn storage_root_ids(&self) -> impl Iterator<Item = StorageRootId> + '_ {
        self.0.iter().map(|entry| entry.target.storage_root_id())
    }
}

impl<'de> Deserialize<'de> for ArtifactAccessDeclaration {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let entries = Vec::<ArtifactAccessEntry>::deserialize(deserializer)?;
        Self::new(entries).map_err(serde::de::Error::custom)
    }
}

fn validate(entries: &[ArtifactAccessEntry]) -> Result<(), String> {
    if entries.is_empty() {
        return Err("artifact access declaration must not be empty".to_owned());
    }

    let mut locations = HashSet::new();
    let mut handles = HashSet::new();
    for (index, entry) in entries.iter().enumerate() {
        validate_rights(index, &entry.rights)?;
        if let Some(field) = entry.target.zero_id_field() {
            return Err(format!("artifact access entry {index} has a zero {field}"));
        }
        if index > 0 && entries[index - 1].target >= entry.target {
            return Err(format!(
                "artifact access entries must be strictly ascending by target; entry {index} \
                 does not follow entry {}",
                index - 1
            ));
        }
        if let Some(location) = entry.target.file_location_id()
            && !locations.insert(location)
        {
            return Err(format!(
                "artifact access declares file location {location} in more than one entry"
            ));
        }
        if let Some(handle) = entry.target.artifact_handle_id()
            && !handles.insert(handle)
        {
            return Err(format!(
                "artifact access declares artifact handle {handle} in more than one entry"
            ));
        }
    }
    Ok(())
}

fn validate_rights(index: usize, rights: &[ArtifactAccessRight]) -> Result<(), String> {
    if rights.is_empty() {
        return Err(format!(
            "artifact access entry {index} must declare at least one right"
        ));
    }
    if rights.windows(2).any(|pair| pair[0] >= pair[1]) {
        return Err(format!(
            "artifact access entry {index} rights must be strictly ascending \
             read < write < delete"
        ));
    }
    Ok(())
}

#[cfg(test)]
#[path = "artifact_access_declaration_test.rs"]
mod tests;
