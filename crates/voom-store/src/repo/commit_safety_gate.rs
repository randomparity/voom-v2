//! Commit safety gate types — Sprint 1 §9.3.
//!
//! Home for the three-phase destructive-commit gate. The current state
//! of the module is type stubs only; algorithms (prepare / authorize /
//! finalize / abort / list, plus the `AliasResolver` trait) land
//! alongside their respective slices and reference these types.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use voom_core::ids::{
    BundleId, CommitId, EvidenceId, FileAssetId, FileLocationId, FileVersionId, UseLeaseId,
};

use crate::repo::identity::NewFileLocation;
use crate::repo::use_leases::LeaseScope;

/// Stand-in for the parent spec's `FileLocationProposal`. Aliased to
/// `NewFileLocation` for Sprint 1 — the existing `IdentityRepo` input
/// type already carries every field needed to propose a replacement
/// location. If a semantic distinction surfaces later, replace the
/// alias with a dedicated newtype.
pub type FileLocationProposal = NewFileLocation;

/// The destructive operation a commit-safety-gate caller is asking
/// the gate to authorize. Sprint 1 ships four variants; the parent
/// spec's `ArchiveFileVersion` is deferred (the `file_versions`
/// schema carries no `archived_at` column). `ArchiveBundle` and
/// `DeleteBundle` are likewise deferred.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommitTarget {
    DeleteFileLocation(FileLocationId),
    DeleteFileVersion(FileVersionId),
    ReplaceFileLocation {
        retired: FileLocationId,
        new: FileLocationProposal,
    },
    MoveFileLocation {
        retired: FileLocationId,
        new: FileLocationProposal,
    },
}

/// The set of identity rows the gate must protect from concurrent
/// writes for the lifetime of one commit. Computed by the closure walk
/// at `prepare_destructive_commit` (Phase A) and re-computed at
/// `authorize_destructive_commit` (Phase B); the two snapshots are
/// compared to detect closure drift.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AffectedScopeClosure {
    pub file_assets: Vec<FileAssetId>,
    pub file_versions: Vec<FileVersionId>,
    pub file_locations: Vec<FileLocationId>,
    pub bundles: Vec<BundleId>,
    pub resolution_warnings: Vec<ClosureWarning>,
}

/// Non-fatal note attached to an `AffectedScopeClosure` walk — e.g.,
/// an alias resolver reported that one location's filesystem mount is
/// unhealthy but other locations under the same `FileVersion` are
/// still reachable. Warnings do not block the commit; failures do
/// (see `ClosureFailure`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClosureWarning {
    pub message: String,
}

/// Why the closure walk could not complete. Carried inside
/// `CommitGateResult::BlockedByClosureIncomplete`. A sanctioned force
/// path is the only bypass for `AliasUnreachable`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClosureFailure {
    AliasUnreachable { message: String },
}

/// How an accepted-evidence pin has drifted from current state.
/// Carried inside `CommitGateResult::BlockedByStaleEvidence`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EvidenceDrift {
    /// The pinned `FileVersion` row is now retired.
    PinnedFileVersionRetired,
    /// The pinned content hash no longer matches the current hash on
    /// the same `FileVersion`.
    PinnedHashDiffers,
    /// One of the pinned `FileLocation` rows is now retired.
    PinnedLocationRetired,
}

/// Granularity tag for a member of `AffectedScopeClosure`. Used by
/// `CommitPermit.target_row_epochs` to drive the destructive dispatch
/// lookup by `(kind, id)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TargetMemberKind {
    FileAsset,
    FileVersion,
    FileLocation,
    Bundle,
}

/// One drifted row in the per-member epoch trip-wire result. The `id`
/// field is a raw `u64` rather than a tagged ID newtype because the
/// concrete granularity is carried separately in `kind`; consumers
/// looking up the row by `(kind, id)` re-tag at the boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetEpochDrift {
    pub kind: TargetMemberKind,
    pub id: u64,
    pub expected: u64,
    pub observed: u64,
}

/// One row from the `commit_intents` `state` enum. Mirrors the SQL
/// CHECK constraint values defined by migration 0004
/// (`pending` | `authorized` | `completed` | `aborted` |
/// `recovery_required`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CommitIntentState {
    Pending,
    Authorized,
    Completed,
    Aborted,
    RecoveryRequired,
}

/// What the caller did with its filesystem mutation between
/// `authorize_destructive_commit` and `finalize_destructive_commit`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MutationOutcome {
    /// The caller performed the durable filesystem mutation. The
    /// optional `observed` closure carries any aliases the caller saw
    /// that the gate could not enumerate on its own — Phase C's
    /// defensive trip-wire compares it against the recomputed closure.
    Applied {
        observed: Option<AffectedScopeClosure>,
    },
    /// The caller obtained a permit but decided not to mutate.
    /// `finalize` transitions the intent to `aborted` with
    /// `abort_reason = OperatorCancel`. Only sanctioned
    /// post-authorize termination path; see parent spec §9.3.2.
    NotPerformed,
}

/// Why a commit-intent row was transitioned to `aborted` (or
/// `recovery_required`). Stored in the SQL `commit_intents.abort_reason`
/// column at the moment the gate decides to fail the commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AbortReason {
    OperatorCancel,
    MutationFailed,
    ClosureGrew,
    ClosureIncomplete,
    FreshLease,
    StaleEvidence,
    /// Phase C defensive trip-wire: a member of `closure_authorized`
    /// has a different `epoch` than the value snapshotted in
    /// `CommitPermit.target_row_epochs`. Drives `recovery_required`.
    StaleTargetEpoch,
    Other(String),
}

/// Returned by `prepare_destructive_commit` on success. Carries the
/// initial closure walk and the commit-intent's identifier so the
/// caller can later invoke `authorize_destructive_commit` against it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitIntent {
    pub commit_id: CommitId,
    pub closure_initial: AffectedScopeClosure,
    pub evaluated_lease_ids: Vec<UseLeaseId>,
    pub revalidated_evidence: Vec<EvidenceRevalidationResult>,
    pub epoch: u64,
}

/// Returned by `authorize_destructive_commit` on success. Carries
/// the recomputed closure, the lease IDs evaluated against it, the
/// evidence revalidation results, **and** the per-member epoch
/// snapshot `target_row_epochs` taken inside the authorize
/// transaction. Phase C consumes `target_row_epochs` as the
/// `expected_epoch` arguments to the destructive `IdentityRepo`
/// mutations.
///
/// `target_row_epochs` is a flat triple-list `(kind, row_id, epoch)`
/// rather than a `HashMap<(kind, id), epoch>` so the type stays
/// serde-friendly and append-ordering can be preserved for audit. Phase
/// C looks up by `(kind, id)` regardless of ordering.
///
/// Note the shape asymmetry with `CommitGateResult::BlockedByStaleTargetEpoch.drift`:
/// the snapshot here is a 3-tuple `(kind, row_id, snapshot_epoch)`, while the drift
/// report is the 4-field `TargetEpochDrift { kind, id, expected, observed }`. The
/// drift report's `expected` field is sourced from this snapshot at Phase C, and
/// `observed` is the row's current epoch — so the drift carries strictly more
/// information than the snapshot does, by design.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitPermit {
    pub commit_id: CommitId,
    pub authorized_at: OffsetDateTime,
    pub closure_authorized: AffectedScopeClosure,
    pub evaluated_lease_ids: Vec<UseLeaseId>,
    pub revalidated_evidence: Vec<EvidenceRevalidationResult>,
    pub target_row_epochs: Vec<(TargetMemberKind, u64, u64)>,
    pub epoch: u64,
}

/// Returned by `finalize_destructive_commit` (and surfaced through
/// the lifecycle for audit). Carries the three closure snapshots
/// (initial / authorized / final) together with the final
/// `CommitGateResult` discriminating among `Allowed`,
/// `CancelledAfterAuthorize`, and the six `Blocked*` failure modes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitGateOutcome {
    pub commit_id: CommitId,
    pub closure_initial: AffectedScopeClosure,
    pub closure_authorized: AffectedScopeClosure,
    pub closure_final: AffectedScopeClosure,
    pub evaluated_lease_ids: Vec<UseLeaseId>,
    pub revalidated_evidence: Vec<EvidenceRevalidationResult>,
    pub result: CommitGateResult,
}

/// Disposition of a commit-safety-gate phase. Eight Sprint-1 variants.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommitGateResult {
    /// All three phases passed; durable identity mutation has landed.
    Allowed,
    /// `finalize_destructive_commit` was called with
    /// `MutationOutcome::NotPerformed`. The intent is durably
    /// transitioned to `aborted` with `abort_reason = OperatorCancel`;
    /// emits `commit.aborted_pre_mutation`. Distinct from `Allowed`
    /// (commit did not happen).
    CancelledAfterAuthorize,
    /// A blocking use-lease overlaps a scope in the affected closure.
    BlockedByUseLease {
        lease_id: UseLeaseId,
        lease_scope: LeaseScope,
    },
    /// Another in-flight commit-intent owns one of the scopes in the
    /// affected closure.
    BlockedByPendingCommit { commit_id: CommitId },
    /// An accepted-evidence pin no longer matches current state.
    BlockedByStaleEvidence {
        evidence_id: EvidenceId,
        drift: EvidenceDrift,
    },
    /// The closure walk could not enumerate every required member.
    BlockedByClosureIncomplete {
        reason: ClosureFailure,
        unreachable: Vec<ClosureWarning>,
    },
    /// Closure delta detected. Eight disjoint Vecs encode
    /// `(closure_authorized or closure_final) - closure_initial` in
    /// `added_*` and the inverse in `removed_*`.
    BlockedByClosureGrew {
        added_assets: Vec<FileAssetId>,
        added_bundles: Vec<BundleId>,
        added_versions: Vec<FileVersionId>,
        added_locations: Vec<FileLocationId>,
        removed_assets: Vec<FileAssetId>,
        removed_bundles: Vec<BundleId>,
        removed_versions: Vec<FileVersionId>,
        removed_locations: Vec<FileLocationId>,
    },
    /// Phase C defensive trip-wire: one or more members of
    /// `closure_authorized` have a different `epoch` than the value
    /// snapshotted in `CommitPermit.target_row_epochs`. Drives the
    /// commit to `recovery_required` and emits
    /// `commit.aborted_post_mutation` with
    /// `reason = 'stale_target_epoch'`.
    BlockedByStaleTargetEpoch { drift: Vec<TargetEpochDrift> },
}

/// Input to `prepare_destructive_commit`. The current shape has no
/// `override_token` field; the force-path slice extends this struct
/// with `pub override_token: Option<ForcePathToken>` and adjusts the
/// `prepare` / `authorize` signatures together.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DestructiveCommit {
    pub target: CommitTarget,
    pub accepted_evidence_ids: Vec<EvidenceId>,
}

/// Detail payload for `CommitGateResult::BlockedByPendingCommit`. The
/// gate fills this in when the pending-commit-lock check detects an
/// in-flight intent overlapping the closure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockedByPendingCommitDetail {
    pub commit_id: CommitId,
    pub offending_scope: LeaseScope,
}

/// Detail payload for `CommitGateResult::BlockedByClosureGrew`. The
/// eight Vecs encode the delta across all four granularities; sets
/// are disjoint by construction. Mirrors the parent spec's
/// `BlockedByClosureGrew` variant fields one-to-one so payload
/// translation later is mechanical.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BlockedByClosureGrewDetail {
    pub added_assets: Vec<FileAssetId>,
    pub added_bundles: Vec<BundleId>,
    pub added_versions: Vec<FileVersionId>,
    pub added_locations: Vec<FileLocationId>,
    pub removed_assets: Vec<FileAssetId>,
    pub removed_bundles: Vec<BundleId>,
    pub removed_versions: Vec<FileVersionId>,
    pub removed_locations: Vec<FileLocationId>,
}

/// Per-pin result of accepted-evidence revalidation. Phase A and Phase
/// B both produce one of these per `evidence_id` in
/// `DestructiveCommit.accepted_evidence_ids`. `drift = None` means the
/// pin still matches current state; `Some(_)` carries the kind of
/// drift detected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvidenceRevalidationResult {
    pub evidence_id: EvidenceId,
    pub drift: Option<EvidenceDrift>,
}

/// Inspection record returned by `list_pending_commit_intents`.
/// Covers both `pending` and `authorized` rows; terminal states are
/// read via a separate filtered list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingCommitIntent {
    pub commit_id: CommitId,
    pub target: CommitTarget,
    pub state: CommitIntentState,
    pub closure_initial: AffectedScopeClosure,
    pub closure_authorized: Option<AffectedScopeClosure>,
    pub accepted_evidence_ids: Vec<EvidenceId>,
    pub started_at: OffsetDateTime,
    pub authorized_at: Option<OffsetDateTime>,
}

/// One bit in `ForcePathToken.bypass`. Sprint 1 ships exactly one
/// kind: `ClosureIncomplete` — the rationale being that an offline
/// filesystem mount can prevent the alias resolver from enumerating
/// the full closure for a `FileVersion`, and an operator with
/// out-of-band knowledge of the affected aliases may need to commit
/// anyway. The bypass-validation pass (which rejects any other bit
/// with `VoomError::Config(...)`) lives with the force-path entry point.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BypassKind {
    ClosureIncomplete,
}

/// Force-path bypass token. Stored as a JSON blob in
/// `commit_intents.override_token`. The struct shape, derive-based
/// JSON serde, and the `BTreeSet<BypassKind>` carrier for ordered
/// deduplicated bypass bits live here; canonical serde, the
/// `validate_bypass` helper, and `commit.forced_override` emission
/// live with the force-path entry point.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForcePathToken {
    pub actor: String,
    pub reason: String,
    pub bypass: BTreeSet<BypassKind>,
}

#[cfg(test)]
#[path = "commit_safety_gate_test.rs"]
mod tests;
