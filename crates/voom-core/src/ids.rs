use serde::{Deserialize, Serialize};

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
define_id!(JobId);
define_id!(EventId);
define_id!(ArtifactHandleId);
define_id!(ArtifactLocationId);

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

// Issue layer (M3 issues table; the `IssueId` newtype lands in M1 so
// `TicketFailedTerminal` event payloads can already carry the optional
// auto-opened issue id — it serializes as `null` in M1 because no
// `issues` table exists yet).
define_id!(IssueId);

// --- M3 (use leases) ---
define_id!(UseLeaseId);

// --- M3 Phase 2 (commit safety gate) ---
define_id!(CommitId);

#[cfg(test)]
#[path = "ids_test.rs"]
mod tests;
