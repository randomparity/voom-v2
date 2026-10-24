//! Owner-observed media evidence and exact extraction completion proofs (ADR0109).

use serde::{Deserialize, Serialize};
use voom_core::ids::{ArtifactCommitIntentId, ArtifactCommitRecordId};
use voom_core::{FileLocationId, FileVersionId, LeaseId, ProviderRelativeLocator, WorkerId};

use super::probe_file::{ObservedFileFacts, ProbeFileResult};

/// Existing node-to-child replay identity, also retained in audio attempt evidence.
#[must_use]
pub fn owner_dispatch_idempotency_key(
    incarnation: voom_core::NodeIncarnationId,
    lease: LeaseId,
) -> String {
    format!("{incarnation}-{}", lease.0)
}

/// Bound on a complete extraction proof; admission rejects rather than truncates.
pub const MAX_COMMIT_GROUP_OUTPUTS: usize = 128;

/// The actual child and complete facts observed around its staged-output probe.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OwnerProbeEvidence {
    pub worker_id: WorkerId,
    pub result: ProbeFileResult,
}

/// Independent owner observations of one dispatched media operation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OwnerObservedEvidence {
    pub input_pre: ObservedFileFacts,
    pub input_post: ObservedFileFacts,
    pub outputs: Vec<OwnerObservedOutput>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OwnerObservedOutput {
    pub provider_relative_locator: ProviderRelativeLocator,
    pub facts: ObservedFileFacts,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub probe: Option<OwnerProbeEvidence>,
}

/// Existing durable extraction claim identity, carried unchanged by the owner.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommitGroupIdentity {
    pub operation_id: u64,
    pub generation: u32,
    pub claim_lease_id: LeaseId,
    pub claim_token: String,
    pub output_count: u32,
}

impl std::fmt::Debug for CommitGroupIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CommitGroupIdentity")
            .field("operation_id", &self.operation_id)
            .field("generation", &self.generation)
            .field("claim_lease_id", &self.claim_lease_id)
            .field("claim_token", &"[REDACTED]")
            .field("output_count", &self.output_count)
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpenCommitGroupMember {
    pub group: CommitGroupIdentity,
    pub ordinal: u32,
}

/// Each member supplies its own capability and the facts it reported applied.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommitGroupMemberProof {
    pub intent_id: ArtifactCommitIntentId,
    pub fence_hex: String,
    pub applied: ObservedFileFacts,
}

impl std::fmt::Debug for CommitGroupMemberProof {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CommitGroupMemberProof")
            .field("intent_id", &self.intent_id)
            .field("fence_hex", &"[REDACTED]")
            .field("applied", &self.applied)
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommitGroupProof {
    pub group: CommitGroupIdentity,
    pub members: Vec<CommitGroupMemberProof>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommitGroupResult {
    pub intent_id: ArtifactCommitIntentId,
    pub commit_record_id: ArtifactCommitRecordId,
    pub result_file_version_id: FileVersionId,
    pub result_file_location_id: FileLocationId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommitGroupCompleteOutcome {
    pub operation_id: u64,
    pub results: Vec<CommitGroupResult>,
}

#[cfg(test)]
#[path = "publication_test.rs"]
mod tests;
