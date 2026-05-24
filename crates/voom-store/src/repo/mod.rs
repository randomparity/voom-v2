//! Repository pattern: trait per storage area, Sqlite impl per trait.

pub mod artifacts;
pub mod bundles;
pub mod commit_safety_gate;
pub(crate) mod common;
pub mod events;
pub mod identity;
pub mod issues;
pub mod jobs;
pub mod leases;
pub mod nodes;
pub mod policies;
pub mod policy_inputs;
pub mod remote_idempotency;
pub mod schema_meta;
pub mod tickets;
pub mod use_leases;
pub mod workers;

pub use artifacts::{
    ArtifactHandle, ArtifactLineage, ArtifactLocation, ArtifactRepo, NewArtifactHandle,
    NewArtifactLineage, NewArtifactLocation, SqliteArtifactRepo,
};
pub use commit_safety_gate::{
    AbortReason, AffectedScopeClosure, AliasResolutionError, AliasResolver, BypassKind,
    ClosureFailure, ClosureMemberDelta, ClosureWarning, CommitGateOutcome, CommitGateResult,
    CommitIntent, CommitIntentState, CommitPermit, CommitTarget, DestructiveCommit, EvidenceDrift,
    EvidenceRevalidationResult, FileLocationProposal, ForcePathToken, MutationOutcome,
    PendingCommitIntent, PrepareOutcome, TargetEpochDrift, TargetMemberKind,
    prepare_destructive_commit, validate_bypass,
};
pub use events::{EventFilter, EventPage, EventRepo, EventRow, Page, SqliteEventRepo};
pub use issues::{
    IssueRepo, PolicyIssueDraft, PolicyIssueMutation, PolicyIssueMutationKind, PolicyIssueRow,
    PolicyIssueStatus, SqliteIssueRepo,
};
pub use jobs::{Job, JobRepo, JobState, NewJob, SqliteJobRepo};
pub use leases::{
    ExpireReport, ForceReleaseOutcome, Lease, LeaseRepo, LeaseState, NewLease, ReleaseReason,
    SqliteLeaseRepo,
};
pub use nodes::{NewNode, Node, NodeAuthRecord, NodeKind, NodeRepo, NodeStatus, SqliteNodeRepo};
pub use policies::{
    CreatedPolicyVersion, NewPolicyDocumentVersion, PolicyDocument, PolicyDocumentSummary,
    PolicyRepo, PolicyVersion, SqlitePolicyRepo,
};
pub use policy_inputs::{
    PolicyBundleTargetInput, PolicyIdentityEvidenceInput, PolicyInputRepo, PolicyInputSet,
    PolicyInputSetSummary, PolicyInputTargetRef, PolicyMediaSnapshotInput,
    PolicyQualityProfileSelection, PolicySyntheticTarget, SqlitePolicyInputRepo,
};
pub use remote_idempotency::{
    IdempotencyOutcome, RemoteIdempotencyInput, RemoteIdempotencyRepo, RemoteMutationReplay,
    SqliteRemoteIdempotencyRepo,
};
pub use schema_meta::{SchemaMetaRepo, SqliteSchemaMetaRepo};
pub use tickets::{NewTicket, SqliteTicketRepo, Ticket, TicketRepo, TicketState};
pub use use_leases::{
    BlockingMode, ExpireReport as UseLeaseExpireReport, IssuerKind, LeaseScope, NewUseLease,
    ReanchorReport, SqliteUseLeaseRepo, UseLease, UseLeaseKind, UseLeaseReleaseReason,
    UseLeaseRepo,
};
pub use workers::{
    Capability, Grant, NewCapability, NewGrant, NewWorker, SqliteWorkerRepo, Worker,
    WorkerInspection, WorkerKind, WorkerNodeContext, WorkerRepo, WorkerStatus,
};

/// Marker trait so future repository traits compose uniformly.
pub trait Repository: Send + Sync {}
