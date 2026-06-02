//! Typed payload structs paired with `EventKind` via the `Event` sum type.
//! Sprint 1 M1 subset.

mod artifact;
mod commit;
mod execution;
mod media_identity;
mod policy;
mod system;
mod use_leases;
mod workers;

pub use artifact::*;
pub use commit::*;
pub use execution::*;
pub use media_identity::*;
pub use policy::*;
pub use system::*;
pub use use_leases::*;
pub use workers::*;

use serde::{Deserialize, Serialize};

use crate::kind::EventKind;

// --- sum type --------------------------------------------------------------

/// Sum type pairing each `EventKind` with its typed payload.
/// The compiler prevents writers from emitting a payload that doesn't
/// match the kind.
///
/// The `tag` column uses the dotted wire format produced by
/// `EventKind::as_str()`. Every variant carries an explicit
/// `#[serde(rename = "...")]` matching `as_str()` exactly so the
/// JSON round-trip cannot drift from what the `events.kind` column
/// stores. Do NOT use `rename_all` here — it would produce `snake_case`
/// strings (e.g. `"schema_initialized"`) that don't match the wire
/// format.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "payload")]
pub enum Event {
    #[serde(rename = "schema.initialized")]
    SchemaInitialized(SchemaInitializedPayload),
    #[serde(rename = "job.opened")]
    JobOpened(JobOpenedPayload),
    #[serde(rename = "job.succeeded")]
    JobSucceeded(JobSucceededPayload),
    #[serde(rename = "job.failed")]
    JobFailed(JobFailedPayload),
    #[serde(rename = "job.cancelled")]
    JobCancelled(JobCancelledPayload),
    #[serde(rename = "ticket.created")]
    TicketCreated(TicketCreatedPayload),
    #[serde(rename = "ticket.ready")]
    TicketReady(TicketReadyPayload),
    #[serde(rename = "ticket.leased")]
    TicketLeased(TicketLeasedPayload),
    #[serde(rename = "ticket.succeeded")]
    TicketSucceeded(TicketSucceededPayload),
    #[serde(rename = "ticket.failed_retriable")]
    TicketFailedRetriable(TicketFailedRetriablePayload),
    #[serde(rename = "ticket.failed_terminal")]
    TicketFailedTerminal(TicketFailedTerminalPayload),
    #[serde(rename = "ticket.requeued_after_lease_expiry")]
    TicketRequeuedAfterLeaseExpiry(TicketRequeuedAfterLeaseExpiryPayload),
    #[serde(rename = "ticket.requeued_after_force_release")]
    TicketRequeuedAfterForceRelease(TicketRequeuedAfterForceReleasePayload),
    #[serde(rename = "lease.acquired")]
    LeaseAcquired(LeaseAcquiredPayload),
    #[serde(rename = "lease.released")]
    LeaseReleased(LeaseReleasedPayload),
    #[serde(rename = "lease.expired")]
    LeaseExpired(LeaseExpiredPayload),
    #[serde(rename = "lease.force_released")]
    LeaseForceReleased(LeaseForceReleasedPayload),
    #[serde(rename = "node.registered")]
    NodeRegistered(NodeRegisteredPayload),
    #[serde(rename = "node.heartbeat_recorded")]
    NodeHeartbeatRecorded(NodeHeartbeatRecordedPayload),
    #[serde(rename = "node.marked_stale")]
    NodeMarkedStale(NodeMarkedStalePayload),
    #[serde(rename = "node.retired")]
    NodeRetired(NodeRetiredPayload),
    #[serde(rename = "worker.registered")]
    WorkerRegistered(WorkerRegisteredPayload),
    #[serde(rename = "worker.linked_to_node")]
    WorkerLinkedToNode(WorkerLinkedToNodePayload),
    #[serde(rename = "worker.capability_recorded")]
    WorkerCapabilityRecorded(WorkerCapabilityRecordedPayload),
    #[serde(rename = "worker.grant_recorded")]
    WorkerGrantRecorded(WorkerGrantRecordedPayload),
    #[serde(rename = "worker.retired")]
    WorkerRetired(WorkerRetiredPayload),
    #[serde(rename = "artifact_handle.created")]
    ArtifactHandleCreated(ArtifactHandleCreatedPayload),
    #[serde(rename = "artifact_location.recorded")]
    ArtifactLocationRecorded(ArtifactLocationRecordedPayload),
    #[serde(rename = "artifact_location.retired")]
    ArtifactLocationRetired(ArtifactLocationRetiredPayload),
    #[serde(rename = "artifact_lineage.recorded")]
    ArtifactLineageRecorded(ArtifactLineageRecordedPayload),
    #[serde(rename = "artifact.staged")]
    ArtifactStaged(ArtifactStagedPayload),
    #[serde(rename = "artifact.verification_started")]
    ArtifactVerificationStarted(ArtifactVerificationStartedPayload),
    #[serde(rename = "artifact.verification_succeeded")]
    ArtifactVerificationSucceeded(ArtifactVerificationSucceededPayload),
    #[serde(rename = "artifact.verification_failed")]
    ArtifactVerificationFailed(ArtifactVerificationFailedPayload),
    #[serde(rename = "artifact.commit_started")]
    ArtifactCommitStarted(ArtifactCommitStartedPayload),
    #[serde(rename = "artifact.commit_completed")]
    ArtifactCommitCompleted(ArtifactCommitCompletedPayload),
    #[serde(rename = "artifact.commit_failed_pre_mutation")]
    ArtifactCommitFailedPreMutation(ArtifactCommitFailedPreMutationPayload),
    #[serde(rename = "artifact.commit_recovery_required")]
    ArtifactCommitRecoveryRequired(ArtifactCommitRecoveryRequiredPayload),
    #[serde(rename = "artifact.transcode_started")]
    ArtifactTranscodeStarted(ArtifactTranscodeStartedPayload),
    #[serde(rename = "artifact.transcode_progress")]
    ArtifactTranscodeProgress(ArtifactTranscodeProgressPayload),
    #[serde(rename = "artifact.transcode_succeeded")]
    ArtifactTranscodeSucceeded(ArtifactTranscodeSucceededPayload),
    #[serde(rename = "artifact.transcode_failed")]
    ArtifactTranscodeFailed(ArtifactTranscodeFailedPayload),
    #[serde(rename = "artifact.remux_started")]
    ArtifactRemuxStarted(ArtifactRemuxStartedPayload),
    #[serde(rename = "artifact.remux_progress")]
    ArtifactRemuxProgress(ArtifactRemuxProgressPayload),
    #[serde(rename = "artifact.remux_succeeded")]
    ArtifactRemuxSucceeded(ArtifactRemuxSucceededPayload),
    #[serde(rename = "artifact.remux_failed")]
    ArtifactRemuxFailed(ArtifactRemuxFailedPayload),
    #[serde(rename = "artifact.audio_transcode_started")]
    ArtifactAudioTranscodeStarted(ArtifactAudioTranscodeStartedPayload),
    #[serde(rename = "artifact.audio_transcode_progress")]
    ArtifactAudioTranscodeProgress(ArtifactAudioTranscodeProgressPayload),
    #[serde(rename = "artifact.audio_transcode_succeeded")]
    ArtifactAudioTranscodeSucceeded(ArtifactAudioTranscodeSucceededPayload),
    #[serde(rename = "artifact.audio_transcode_failed")]
    ArtifactAudioTranscodeFailed(ArtifactAudioTranscodeFailedPayload),
    #[serde(rename = "artifact.audio_extract_started")]
    ArtifactAudioExtractStarted(ArtifactAudioExtractStartedPayload),
    #[serde(rename = "artifact.audio_extract_progress")]
    ArtifactAudioExtractProgress(ArtifactAudioExtractProgressPayload),
    #[serde(rename = "artifact.audio_extract_succeeded")]
    ArtifactAudioExtractSucceeded(ArtifactAudioExtractSucceededPayload),
    #[serde(rename = "artifact.audio_extract_failed")]
    ArtifactAudioExtractFailed(ArtifactAudioExtractFailedPayload),
    #[serde(rename = "issue.opened")]
    IssueOpened(IssueLifecyclePayload),
    #[serde(rename = "issue.updated")]
    IssueUpdated(IssueLifecyclePayload),
    #[serde(rename = "issue.resolved")]
    IssueResolved(IssueLifecyclePayload),
    #[serde(rename = "media_work.created")]
    MediaWorkCreated(MediaWorkCreatedPayload),
    #[serde(rename = "media_variant.created")]
    MediaVariantCreated(MediaVariantCreatedPayload),
    #[serde(rename = "asset_bundle.created")]
    AssetBundleCreated(AssetBundleCreatedPayload),
    #[serde(rename = "asset_bundle.member_added")]
    AssetBundleMemberAdded(AssetBundleMemberAddedPayload),
    #[serde(rename = "asset_bundle.member_removed")]
    AssetBundleMemberRemoved(AssetBundleMemberRemovedPayload),
    #[serde(rename = "file_asset.created")]
    FileAssetCreated(FileAssetCreatedPayload),
    #[serde(rename = "file_version.created")]
    FileVersionCreated(FileVersionCreatedPayload),
    #[serde(rename = "file_location.recorded")]
    FileLocationRecorded(FileLocationRecordedPayload),
    #[serde(rename = "file_location.aliased")]
    FileLocationAliased(FileLocationAliasedPayload),
    #[serde(rename = "file_location.retired_by_move")]
    FileLocationRetiredByMove(FileLocationRetiredByMovePayload),
    #[serde(rename = "file_location.recorded_by_move")]
    FileLocationRecordedByMove(FileLocationRecordedByMovePayload),
    #[serde(rename = "identity_evidence.recorded")]
    IdentityEvidenceRecorded(IdentityEvidenceRecordedPayload),
    #[serde(rename = "identity_evidence.accepted")]
    IdentityEvidenceAccepted(IdentityEvidenceAcceptedPayload),
    #[serde(rename = "identity_evidence.superseded")]
    IdentityEvidenceSuperseded(IdentityEvidenceSupersededPayload),
    #[serde(rename = "media_snapshot.recorded")]
    MediaSnapshotRecorded(MediaSnapshotRecordedPayload),
    #[serde(rename = "use_lease.acquired")]
    UseLeaseAcquired(UseLeaseAcquiredPayload),
    #[serde(rename = "use_lease.released")]
    UseLeaseReleased(UseLeaseReleasedPayload),
    #[serde(rename = "use_lease.expired")]
    UseLeaseExpired(UseLeaseExpiredPayload),
    #[serde(rename = "use_lease.force_released")]
    UseLeaseForceReleased(UseLeaseForceReleasedPayload),
    #[serde(rename = "use_lease.recovered_stale_issuer")]
    UseLeaseRecoveredStaleIssuer(UseLeaseRecoveredStaleIssuerPayload),
    #[serde(rename = "use_lease.reanchored_by_move")]
    UseLeaseReanchoredByMove(UseLeaseReanchoredByMovePayload),
    #[serde(rename = "commit.intent_recorded")]
    CommitIntentRecorded(CommitIntentRecordedPayload),
    #[serde(rename = "commit.aborted_by_use_lease")]
    CommitAbortedByUseLease(CommitAbortedByUseLeasePayload),
    #[serde(rename = "commit.aborted_by_stale_evidence")]
    CommitAbortedByStaleEvidence(CommitAbortedByStaleEvidencePayload),
    #[serde(rename = "commit.aborted_by_closure_incomplete")]
    CommitAbortedByClosureIncomplete(CommitAbortedByClosureIncompletePayload),
    #[serde(rename = "commit.aborted_by_pending_commit")]
    CommitAbortedByPendingCommit(CommitAbortedByPendingCommitPayload),
    #[serde(rename = "commit.authorized")]
    CommitAuthorized(CommitAuthorizedPayload),
    #[serde(rename = "commit.aborted_by_closure_grew")]
    CommitAbortedByClosureGrew(CommitAbortedByClosureGrewPayload),
    #[serde(rename = "commit.completed")]
    CommitCompleted(CommitCompletedPayload),
    #[serde(rename = "commit.aborted_pre_mutation")]
    CommitAbortedPreMutation(CommitAbortedPreMutationPayload),
    #[serde(rename = "commit.aborted_post_mutation")]
    CommitAbortedPostMutation(CommitAbortedPostMutationPayload),
    #[serde(rename = "commit.recovery_required")]
    CommitRecoveryRequired(CommitRecoveryRequiredPayload),
    #[serde(rename = "commit.forced_override")]
    CommitForcedOverride(CommitForcedOverridePayload),
}

impl Event {
    /// The `EventKind` that pairs with this payload. Derived by exhaustive
    /// match so a new variant is a compile error until both `EventKind` and
    /// the `as_str()` table grow to match.
    #[must_use]
    pub const fn kind(&self) -> EventKind {
        match self {
            Self::SchemaInitialized(_) => EventKind::SchemaInitialized,
            Self::JobOpened(_) => EventKind::JobOpened,
            Self::JobSucceeded(_) => EventKind::JobSucceeded,
            Self::JobFailed(_) => EventKind::JobFailed,
            Self::JobCancelled(_) => EventKind::JobCancelled,
            Self::TicketCreated(_) => EventKind::TicketCreated,
            Self::TicketReady(_) => EventKind::TicketReady,
            Self::TicketLeased(_) => EventKind::TicketLeased,
            Self::TicketSucceeded(_) => EventKind::TicketSucceeded,
            Self::TicketFailedRetriable(_) => EventKind::TicketFailedRetriable,
            Self::TicketFailedTerminal(_) => EventKind::TicketFailedTerminal,
            Self::TicketRequeuedAfterLeaseExpiry(_) => EventKind::TicketRequeuedAfterLeaseExpiry,
            Self::TicketRequeuedAfterForceRelease(_) => EventKind::TicketRequeuedAfterForceRelease,
            Self::LeaseAcquired(_) => EventKind::LeaseAcquired,
            Self::LeaseReleased(_) => EventKind::LeaseReleased,
            Self::LeaseExpired(_) => EventKind::LeaseExpired,
            Self::LeaseForceReleased(_) => EventKind::LeaseForceReleased,
            Self::NodeRegistered(_) => EventKind::NodeRegistered,
            Self::NodeHeartbeatRecorded(_) => EventKind::NodeHeartbeatRecorded,
            Self::NodeMarkedStale(_) => EventKind::NodeMarkedStale,
            Self::NodeRetired(_) => EventKind::NodeRetired,
            Self::WorkerRegistered(_) => EventKind::WorkerRegistered,
            Self::WorkerLinkedToNode(_) => EventKind::WorkerLinkedToNode,
            Self::WorkerCapabilityRecorded(_) => EventKind::WorkerCapabilityRecorded,
            Self::WorkerGrantRecorded(_) => EventKind::WorkerGrantRecorded,
            Self::WorkerRetired(_) => EventKind::WorkerRetired,
            Self::ArtifactHandleCreated(_) => EventKind::ArtifactHandleCreated,
            Self::ArtifactLocationRecorded(_) => EventKind::ArtifactLocationRecorded,
            Self::ArtifactLocationRetired(_) => EventKind::ArtifactLocationRetired,
            Self::ArtifactLineageRecorded(_) => EventKind::ArtifactLineageRecorded,
            Self::ArtifactStaged(_) => EventKind::ArtifactStaged,
            Self::ArtifactVerificationStarted(_) => EventKind::ArtifactVerificationStarted,
            Self::ArtifactVerificationSucceeded(_) => EventKind::ArtifactVerificationSucceeded,
            Self::ArtifactVerificationFailed(_) => EventKind::ArtifactVerificationFailed,
            Self::ArtifactCommitStarted(_) => EventKind::ArtifactCommitStarted,
            Self::ArtifactCommitCompleted(_) => EventKind::ArtifactCommitCompleted,
            Self::ArtifactCommitFailedPreMutation(_) => EventKind::ArtifactCommitFailedPreMutation,
            Self::ArtifactCommitRecoveryRequired(_) => EventKind::ArtifactCommitRecoveryRequired,
            Self::ArtifactTranscodeStarted(_) => EventKind::ArtifactTranscodeStarted,
            Self::ArtifactTranscodeProgress(_) => EventKind::ArtifactTranscodeProgress,
            Self::ArtifactTranscodeSucceeded(_) => EventKind::ArtifactTranscodeSucceeded,
            Self::ArtifactTranscodeFailed(_) => EventKind::ArtifactTranscodeFailed,
            Self::ArtifactRemuxStarted(_) => EventKind::ArtifactRemuxStarted,
            Self::ArtifactRemuxProgress(_) => EventKind::ArtifactRemuxProgress,
            Self::ArtifactRemuxSucceeded(_) => EventKind::ArtifactRemuxSucceeded,
            Self::ArtifactRemuxFailed(_) => EventKind::ArtifactRemuxFailed,
            Self::ArtifactAudioTranscodeStarted(_) => EventKind::ArtifactAudioTranscodeStarted,
            Self::ArtifactAudioTranscodeProgress(_) => EventKind::ArtifactAudioTranscodeProgress,
            Self::ArtifactAudioTranscodeSucceeded(_) => EventKind::ArtifactAudioTranscodeSucceeded,
            Self::ArtifactAudioTranscodeFailed(_) => EventKind::ArtifactAudioTranscodeFailed,
            Self::ArtifactAudioExtractStarted(_) => EventKind::ArtifactAudioExtractStarted,
            Self::ArtifactAudioExtractProgress(_) => EventKind::ArtifactAudioExtractProgress,
            Self::ArtifactAudioExtractSucceeded(_) => EventKind::ArtifactAudioExtractSucceeded,
            Self::ArtifactAudioExtractFailed(_) => EventKind::ArtifactAudioExtractFailed,
            Self::IssueOpened(_) => EventKind::IssueOpened,
            Self::IssueUpdated(_) => EventKind::IssueUpdated,
            Self::IssueResolved(_) => EventKind::IssueResolved,
            Self::MediaWorkCreated(_) => EventKind::MediaWorkCreated,
            Self::MediaVariantCreated(_) => EventKind::MediaVariantCreated,
            Self::AssetBundleCreated(_) => EventKind::AssetBundleCreated,
            Self::AssetBundleMemberAdded(_) => EventKind::AssetBundleMemberAdded,
            Self::AssetBundleMemberRemoved(_) => EventKind::AssetBundleMemberRemoved,
            Self::FileAssetCreated(_) => EventKind::FileAssetCreated,
            Self::FileVersionCreated(_) => EventKind::FileVersionCreated,
            Self::FileLocationRecorded(_) => EventKind::FileLocationRecorded,
            Self::FileLocationAliased(_) => EventKind::FileLocationAliased,
            Self::FileLocationRetiredByMove(_) => EventKind::FileLocationRetiredByMove,
            Self::FileLocationRecordedByMove(_) => EventKind::FileLocationRecordedByMove,
            Self::IdentityEvidenceRecorded(_) => EventKind::IdentityEvidenceRecorded,
            Self::IdentityEvidenceAccepted(_) => EventKind::IdentityEvidenceAccepted,
            Self::IdentityEvidenceSuperseded(_) => EventKind::IdentityEvidenceSuperseded,
            Self::MediaSnapshotRecorded(_) => EventKind::MediaSnapshotRecorded,
            Self::UseLeaseAcquired(_) => EventKind::UseLeaseAcquired,
            Self::UseLeaseReleased(_) => EventKind::UseLeaseReleased,
            Self::UseLeaseExpired(_) => EventKind::UseLeaseExpired,
            Self::UseLeaseForceReleased(_) => EventKind::UseLeaseForceReleased,
            Self::UseLeaseRecoveredStaleIssuer(_) => EventKind::UseLeaseRecoveredStaleIssuer,
            Self::UseLeaseReanchoredByMove(_) => EventKind::UseLeaseReanchoredByMove,
            Self::CommitIntentRecorded(_) => EventKind::CommitIntentRecorded,
            Self::CommitAbortedByUseLease(_) => EventKind::CommitAbortedByUseLease,
            Self::CommitAbortedByStaleEvidence(_) => EventKind::CommitAbortedByStaleEvidence,
            Self::CommitAbortedByClosureIncomplete(_) => {
                EventKind::CommitAbortedByClosureIncomplete
            }
            Self::CommitAbortedByPendingCommit(_) => EventKind::CommitAbortedByPendingCommit,
            Self::CommitAuthorized(_) => EventKind::CommitAuthorized,
            Self::CommitAbortedByClosureGrew(_) => EventKind::CommitAbortedByClosureGrew,
            Self::CommitCompleted(_) => EventKind::CommitCompleted,
            Self::CommitAbortedPreMutation(_) => EventKind::CommitAbortedPreMutation,
            Self::CommitAbortedPostMutation(_) => EventKind::CommitAbortedPostMutation,
            Self::CommitRecoveryRequired(_) => EventKind::CommitRecoveryRequired,
            Self::CommitForcedOverride(_) => EventKind::CommitForcedOverride,
        }
    }
}

#[cfg(test)]
#[path = "mod_test.rs"]
mod tests;
