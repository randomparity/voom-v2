//! Durable owner-local scheduling evidence (issue #477, ADR 0071).
//!
//! The persisted projection of what ADR 0070 proves or rejects at the
//! owner-local gate: the canonical declaration a ticket claimed and the epochs
//! of the roots it references, or the stable per-reference reasons a rejection
//! produced. Every type names stable IDs only — never a path, provider
//! locator, mount name, or host string.
//!
//! Construction and deserialization run the same validation, so each type has
//! exactly one accepted encoding (ADR 0013). Nothing here interprets a
//! root-addressed entry as locality: a `storage_root` entry records that the
//! ticket touches something in that root, nothing more.

use serde::{Deserialize, Serialize};

use super::{
    artifact_access_declaration::{ArtifactAccessDeclaration, ArtifactAccessTarget},
    ids::StorageRootId,
};
use crate::VoomError;

/// The resolved epoch of one referenced storage root.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RootEpoch {
    pub storage_root_id: StorageRootId,
    pub root_epoch: u64,
}

/// The owner-local evidence a **selected** scheduling decision persists: the
/// canonical declaration exactly as validated at the gate, plus the epoch of
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct OwnerAccessEvidence {
    pub declaration: ArtifactAccessDeclaration,
    pub root_epochs: Vec<RootEpoch>,
}

impl OwnerAccessEvidence {
    /// Build owner-local evidence, enforcing canonical form.
    ///
    /// # Errors
    ///
    /// Returns [`VoomError::Config`] when `root_epochs` is empty, is not
    /// strictly ascending by `storage_root_id`, or its root set does not equal
    /// the set of roots `declaration` references.
    pub fn new(
        declaration: ArtifactAccessDeclaration,
        root_epochs: Vec<RootEpoch>,
    ) -> Result<Self, VoomError> {
        validate_root_epochs(&declaration, &root_epochs).map_err(VoomError::Config)?;
        Ok(Self {
            declaration,
            root_epochs,
        })
    }
}

impl<'de> Deserialize<'de> for OwnerAccessEvidence {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = RawOwnerAccessEvidence::deserialize(deserializer)?;
        Self::new(raw.declaration, raw.root_epochs).map_err(serde::de::Error::custom)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawOwnerAccessEvidence {
    declaration: ArtifactAccessDeclaration,
    root_epochs: Vec<RootEpoch>,
}

fn validate_root_epochs(
    declaration: &ArtifactAccessDeclaration,
    root_epochs: &[RootEpoch],
) -> Result<(), String> {
    if root_epochs.is_empty() {
        return Err("owner access evidence must carry at least one root epoch".to_owned());
    }
    for (index, pair) in root_epochs.windows(2).enumerate() {
        if pair[0].storage_root_id >= pair[1].storage_root_id {
            return Err(format!(
                "owner access evidence root epochs must be strictly ascending by \
                 storage_root_id; epoch {index} does not follow epoch {}",
                index + 1
            ));
        }
    }

    // A declaration's roots may repeat (read a source, write an output in the
    // same root), so compare distinct sorted sets.
    let mut declared: Vec<u64> = declaration.storage_root_ids().map(|id| id.0).collect();
    declared.sort_unstable();
    declared.dedup();
    let epochs: Vec<u64> = root_epochs
        .iter()
        .map(|epoch| epoch.storage_root_id.0)
        .collect();
    if epochs != declared {
        return Err(format!(
            "owner access evidence root epochs {epochs:?} do not match the declared \
             storage roots {declared:?}"
        ));
    }
    Ok(())
}

/// Stable, locator-free reason a single declared reference failed resolution.
///
/// One closed code per resolution failure class (ADR 0070 vocabulary); the codes
/// are durable contract (ADR 0071) and render without paths.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccessReferenceReason {
    StorageRootNotFound,
    FileLocationNotFound,
    LocationRootInvalid,
    InvalidRootState,
    InvalidRootEpoch,
    InvalidLocationState,
    MixedOwner,
    NoActiveIncarnation,
    /// Resolution succeeded but the common owner is not the acquiring node.
    OwnerMismatch,
}

impl AccessReferenceReason {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::StorageRootNotFound => "storage_root_not_found",
            Self::FileLocationNotFound => "file_location_not_found",
            Self::LocationRootInvalid => "location_root_invalid",
            Self::InvalidRootState => "invalid_root_state",
            Self::InvalidRootEpoch => "invalid_root_epoch",
            Self::InvalidLocationState => "invalid_location_state",
            Self::MixedOwner => "mixed_owner",
            Self::NoActiveIncarnation => "no_active_incarnation",
            Self::OwnerMismatch => "owner_mismatch",
        }
    }
}

/// One declared target paired with the stable reason it failed resolution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccessReferenceRejection {
    pub target: ArtifactAccessTarget,
    pub reason: AccessReferenceReason,
}

/// The evidence a **rejected** scheduling decision persists: per-reference
/// reasons for the declared targets, in canonical declaration order, with no
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AccessRejectionEvidence {
    pub references: Vec<AccessReferenceRejection>,
}

impl AccessRejectionEvidence {
    /// Build rejection evidence, enforcing canonical form.
    ///
    /// # Errors
    ///
    /// Returns [`VoomError::Config`] when `references` is empty or its targets
    /// are not strictly ascending (the canonical order of the rejected
    /// declaration).
    pub fn new(references: Vec<AccessReferenceRejection>) -> Result<Self, VoomError> {
        validate_rejections(&references).map_err(VoomError::Config)?;
        Ok(Self { references })
    }
}

impl<'de> Deserialize<'de> for AccessRejectionEvidence {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = RawAccessRejectionEvidence::deserialize(deserializer)?;
        Self::new(raw.references).map_err(serde::de::Error::custom)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawAccessRejectionEvidence {
    references: Vec<AccessReferenceRejection>,
}

fn validate_rejections(references: &[AccessReferenceRejection]) -> Result<(), String> {
    if references.is_empty() {
        return Err("access rejection evidence must carry at least one reference".to_owned());
    }
    for (index, pair) in references.windows(2).enumerate() {
        if pair[0].target >= pair[1].target {
            return Err(format!(
                "access rejection evidence references must be strictly ascending by \
                 target; reference {index} does not follow reference {}",
                index + 1
            ));
        }
    }
    Ok(())
}

/// The `access_evidence` column vocabulary for scheduler decisions: the
/// discriminator rejects unknown variant names, and each variant is a newtype
/// over a `deny_unknown_fields` content struct (ADR 0013).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "evidence", rename_all = "snake_case")]
pub enum DecisionAccessEvidence {
    Owner(OwnerAccessEvidence),
    Rejected(AccessRejectionEvidence),
}

#[cfg(test)]
#[path = "owner_access_evidence_test.rs"]
mod tests;
