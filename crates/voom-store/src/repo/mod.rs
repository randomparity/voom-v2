//! Concrete `SQLite` repositories plus the few traits used as abstraction boundaries.

pub mod audit;
pub(crate) mod common;
pub mod execution;
pub mod external;
pub mod library;
pub mod media;
pub mod policy;

pub use audit::{events, schema_meta};
pub use execution::{
    jobs, leases, nodes, remote_idempotency, scheduler_decisions, scheduler_node_limits, tickets,
    workers, workflow_summaries,
};
pub use external::SqliteExternalSystemRepo;
pub use external::links::{ExternalLinkTargetType, ExternalSystemLink, NewExternalLink};
pub use external::path_mappings::{
    ExternalPathMapping, NewExternalPathMapping, PathMappingUpdate, PathVisibility,
};
pub use external::systems::{
    ExternalSystem, ExternalSystemHealth, ExternalSystemKind, NewExternalSystem,
};
pub use media::{
    artifact_access_plans, artifacts, backups, bundles, commit_safety_gate, identity, scan_facts,
    use_leases,
};
pub use policy::{
    issues, policies, policy_inputs, safety_policies, scheduling_policies, video_profiles,
};

pub use artifact_access_plans::{
    ArtifactAccessMode, ArtifactAccessPlan, ArtifactAccessPlanStatus, NewArtifactAccessPlan,
    SqliteArtifactAccessPlanRepo,
};
pub use artifacts::{
    ArtifactCommitRepo, ArtifactHandle, ArtifactHandleRepo, ArtifactLineage, ArtifactLocation,
    ArtifactVerificationRepo, NewArtifactHandle, NewArtifactLineage, NewArtifactLocation,
    SqliteArtifactRepo,
};
pub use backups::{Backup, BackupFailureDetail, BackupStatus, NewBackup, SqliteBackupRepo};
pub use commit_safety_gate::{
    AbortReason, AffectedScopeClosure, AliasResolutionError, AliasResolver, BypassKind,
    ClosureFailure, ClosureMemberDelta, ClosureWarning, CommitGateContext, CommitGateOutcome,
    CommitGateResult, CommitIntent, CommitIntentState, CommitPermit, CommitTarget,
    DestructiveCommit, EvidenceDrift, EvidenceRevalidationResult, FileLocationProposal,
    ForcePathToken, LineageCommitLeaseCheck, MutationOutcome, PendingCommitIntent, PrepareOutcome,
    TargetEpochDrift, TargetMemberKind, check_lineage_commit_leases_in_tx,
    prepare_destructive_commit,
};
pub use events::{EventFilter, EventPage, EventRepo, EventRow, Page, SqliteEventRepo};
pub use issues::{
    IssueFilter, IssueListPage, IssueRecord, IssueStatus, PolicyIssueDraft, PolicyIssueMutation,
    PolicyIssueMutationKind, PolicyIssueRow, PolicyIssueStatus, SqliteIssueRepo,
    TerminalFailureIssueDraft,
};
pub use jobs::{Job, JobFilter, JobState, NewJob, SqliteJobRepo};
pub use leases::{
    ExpireReport, ForceReleaseOutcome, Lease, LeaseFilter, LeaseState, NewLease, ReleaseReason,
    SqliteLeaseRepo,
};
pub use library::libraries::{Library, LibraryMediaKind, LibraryUpdate, NewLibrary};
pub use library::library_roots::{
    HiddenFilePolicy, LibraryRoot, LibraryRootKind, LibraryRootUpdate, LibraryScanMode,
    NewLibraryRoot, SymlinkPolicy,
};
pub use library::{SqliteLibraryRepo, libraries, library_roots};
pub use nodes::{NewNode, Node, NodeAuthRecord, NodeKind, NodeStatus, SqliteNodeRepo};
pub use policies::{
    CreatedPolicyVersion, NewPolicyDocumentVersion, PolicyDocument, PolicyDocumentSummary,
    PolicyVersion, SqlitePolicyRepo,
};
pub use policy_inputs::{
    PolicyBundleTargetInput, PolicyIdentityEvidenceInput, PolicyInputSet, PolicyInputSetSummary,
    PolicyInputTargetRef, PolicyMediaSnapshotInput, PolicyQualityProfileSelection,
    PolicySyntheticTarget, SqlitePolicyInputRepo,
};
pub use remote_idempotency::{
    IdempotencyOutcome, RemoteIdempotencyInput, RemoteMutationReplay, SqliteRemoteIdempotencyRepo,
};
pub use safety_policies::{
    CommitMode, NewSafetyPolicy, SAFETY_POLICY_SCHEMA_VERSION, SafetyPolicy,
    SqliteSafetyPolicyRepo, VerificationLevel,
};
pub use scheduler_decisions::{
    NewSchedulerDecision, SchedulerDecision, SchedulerDecisionFilter, SchedulerDecisionKind,
    SchedulerDecisionOutcome, SchedulerReasonCode, SchedulerRequestSource,
    SqliteSchedulerDecisionRepo,
};
pub use scheduler_node_limits::{SchedulerNodeLimit, SqliteSchedulerNodeLimitRepo};
pub use scheduling_policies::{
    NewSchedulingPolicy, SCHEDULING_POLICY_SCHEMA_VERSION, SchedulePriority, SchedulingPolicy,
    SqliteSchedulingPolicyRepo,
};
pub use schema_meta::SqliteSchemaMetaRepo;
pub use tickets::{NewTicket, SqliteTicketRepo, Ticket, TicketFilter, TicketState};
pub use use_leases::{
    BlockingMode, ExpireReport as UseLeaseExpireReport, IssuerKind, LeaseScope, NewUseLease,
    ReanchorReport, SqliteUseLeaseRepo, UseLease, UseLeaseKind, UseLeaseReleaseReason,
};
pub use video_profiles::{SqliteVideoProfileRepo, VideoProfile};
pub use workers::{
    Capability, Grant, NewCapability, NewGrant, NewWorker, SqliteWorkerRepo, Worker,
    WorkerInspection, WorkerKind, WorkerNodeContext, WorkerOperationEligibility, WorkerStatus,
};
pub use workflow_summaries::{
    FilePhaseOutcome, FilePhaseSummary, NewFilePhaseSummary, NewPhaseSummary, NewWorkflowSummary,
    PhaseOutcome, PhaseReport, PhaseSummary, SqliteWorkflowSummaryRepo, WorkflowSummary,
};

/// Marker trait so future repository traits compose uniformly.
pub trait Repository: Send + Sync {}
