//! Strongly-typed ID newtypes for every persistent entity in the workspace.
//!
//! All IDs are database-generated (`ROWID` / `last_insert_rowid`). They are
//! stored as bare `u64` values and are **not validated on construction** — a
//! value of `0` or any other `u64` is accepted without error. This is
//! intentional: the application never constructs raw IDs by hand; it only
//! round-trips values emitted by `SQLite`.

use rand::TryRngCore;
use serde::{Deserialize, Serialize};

use crate::VoomError;

const INCARNATION_ID_BYTES: usize = 16;
const INCARNATION_ID_HEX_LEN: usize = INCARNATION_ID_BYTES * 2;

/// Defines a strongly-typed `u64` ID newtype.
///
/// The inner value is database-generated and unvalidated by design. No
/// boundary checks are applied; all valid `u64` values are accepted so that
/// values returned by `SQLite` can be stored without loss.
macro_rules! define_id {
    ($name:ident) => {
        #[derive(
            Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
        )]
        #[serde(transparent)]
        pub struct $name(pub u64);

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "{}", self.0)
            }
        }
    };
}

// Execution layer (Sprint 0 + M1).
define_id!(TicketId);
define_id!(LeaseId);
define_id!(WorkerId);
define_id!(NodeId);

/// Random identity for one node-agent process lifetime.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NodeIncarnationId([u8; INCARNATION_ID_BYTES]);

impl NodeIncarnationId {
    /// Generate a fresh incarnation ID from the operating-system RNG.
    pub fn generate() -> Result<Self, VoomError> {
        let mut bytes = [0_u8; INCARNATION_ID_BYTES];
        rand::rngs::OsRng
            .try_fill_bytes(&mut bytes)
            .map_err(|error| {
                VoomError::Internal(format!("generate node incarnation id from OS RNG: {error}"))
            })?;
        Ok(Self(bytes))
    }

    /// Parse persisted text, reporting corruption as a database error.
    pub fn parse_database(field: &str, value: &str) -> Result<Self, VoomError> {
        parse_incarnation_id(value)
            .map_err(|message| VoomError::database(format!("{field} {message}")))
    }
}

impl std::fmt::Debug for NodeIncarnationId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(self, f)
    }
}

impl std::fmt::Display for NodeIncarnationId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for byte in self.0 {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

impl std::str::FromStr for NodeIncarnationId {
    type Err = VoomError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        parse_incarnation_id(value).map_err(VoomError::Config)
    }
}

impl Serialize for NodeIncarnationId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for NodeIncarnationId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        value.parse().map_err(serde::de::Error::custom)
    }
}

fn parse_incarnation_id(value: &str) -> Result<NodeIncarnationId, String> {
    if value.len() != INCARNATION_ID_HEX_LEN {
        return Err(format!(
            "must be {INCARNATION_ID_HEX_LEN} lowercase hexadecimal characters, got length {}",
            value.len()
        ));
    }
    let mut bytes = [0_u8; INCARNATION_ID_BYTES];
    for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
        let high = lowercase_hex_value(pair[0]).ok_or_else(|| invalid_incarnation_id(value))?;
        let low = lowercase_hex_value(pair[1]).ok_or_else(|| invalid_incarnation_id(value))?;
        bytes[index] = (high << 4) | low;
    }
    Ok(NodeIncarnationId(bytes))
}

const fn lowercase_hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    }
}

fn invalid_incarnation_id(value: &str) -> String {
    format!("must contain only lowercase hexadecimal characters, got {value:?}")
}
define_id!(JobId);
define_id!(EventId);
define_id!(ArtifactHandleId);
define_id!(ArtifactLocationId);
define_id!(ArtifactVerificationId);
define_id!(ArtifactCommitRecordId);

// Identity layer (M2).
//
// Sprint 0's placeholder `MediaId` (a single u64 newtype standing in for
// the yet-to-be-split identity layers) is removed in M2 because every
// Sprint 1 caller wants the specific layer: `MediaWorkId` for the
// logical title, `MediaVariantId` for a retained version,
// `FileAssetId` for managed file lineage, etc.
define_id!(MediaWorkId);
define_id!(MediaVariantId);
define_id!(BundleId);
define_id!(FileAssetId);
define_id!(FileVersionId);
define_id!(FileLocationId);
define_id!(EvidenceId);
define_id!(MediaSnapshotId);

// Policy input layer (Sprint 3).
define_id!(PolicyInputSetId);
define_id!(PolicySyntheticTargetId);

// Policy registry layer (Sprint 4).
define_id!(PolicyDocumentId);
define_id!(PolicyVersionId);

// Issue layer (M3 issues table; the `IssueId` newtype lands in M1 so
// `TicketFailedTerminal` event payloads can already carry the optional
// auto-opened issue id — it serializes as `null` in M1 because no
// `issues` table exists yet).
define_id!(IssueId);

// --- M3 (use leases) ---
define_id!(UseLeaseId);

// --- M3 Phase 2 (commit safety gate) ---
define_id!(CommitId);

// --- Sprint 17 (backups) ---
define_id!(BackupId);

// --- Library config (Sprint 17, T11) ---
define_id!(LibraryId);
define_id!(LibraryRootId);
define_id!(StorageRootId);
define_id!(ScanSessionId);

// --- External system (Sprint 17, T15) ---
define_id!(ExternalSystemId);
define_id!(ExternalPathMappingId);
define_id!(ExternalSystemLinkId);

#[cfg(test)]
#[path = "ids_test.rs"]
mod tests;
