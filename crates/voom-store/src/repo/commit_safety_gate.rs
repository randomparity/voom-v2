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

use crate::repo::identity::{FileLocationKind, LocationProof};
use crate::repo::use_leases::LeaseScope;

/// Caller-facing proposal for a new `FileLocation` inside a destructive
/// commit's `ReplaceFileLocation` / `MoveFileLocation` target. The
/// `file_version_id` is intentionally absent: it is inferred from the
/// retired location's current `FileVersion` inside Phase C, which makes
/// a cross-version target unrepresentable at the type level. Phase C
/// converts a `FileLocationProposal` into a `NewFileLocation` by
/// pairing it with the retired row's `file_version_id`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileLocationProposal {
    pub kind: FileLocationKind,
    pub value: String,
    pub proof: Option<LocationProof>,
    pub observed_at: OffsetDateTime,
}

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
    pub file_assets: BTreeSet<FileAssetId>,
    pub file_versions: BTreeSet<FileVersionId>,
    pub file_locations: BTreeSet<FileLocationId>,
    pub bundles: BTreeSet<BundleId>,
    pub resolution_warnings: Vec<ClosureWarning>,
}

impl AffectedScopeClosure {
    /// Compute the member delta between `self` and `other`,
    /// considering only the four ID sets. `added_*` is
    /// `other - self`; `removed_*` is `self - other`. The
    /// `resolution_warnings` field is intentionally excluded —
    /// non-fatal warnings may differ between Phase A and Phase B
    /// snapshots even when no protected ID row has changed, and
    /// the drift check must not treat warning churn as closure
    /// growth.
    #[must_use]
    pub fn id_member_delta(&self, other: &Self) -> ClosureMemberDelta {
        ClosureMemberDelta {
            added_assets: other
                .file_assets
                .difference(&self.file_assets)
                .copied()
                .collect(),
            removed_assets: self
                .file_assets
                .difference(&other.file_assets)
                .copied()
                .collect(),
            added_bundles: other.bundles.difference(&self.bundles).copied().collect(),
            removed_bundles: self.bundles.difference(&other.bundles).copied().collect(),
            added_versions: other
                .file_versions
                .difference(&self.file_versions)
                .copied()
                .collect(),
            removed_versions: self
                .file_versions
                .difference(&other.file_versions)
                .copied()
                .collect(),
            added_locations: other
                .file_locations
                .difference(&self.file_locations)
                .copied()
                .collect(),
            removed_locations: self
                .file_locations
                .difference(&other.file_locations)
                .copied()
                .collect(),
        }
    }
}

/// Four-set delta between two `AffectedScopeClosure` snapshots,
/// computed by `AffectedScopeClosure::id_member_delta`. Mirrors the
/// shape of `CommitGateResult::BlockedByClosureGrew` so the drift
/// check hands it straight through. Warnings on the underlying
/// closures are intentionally excluded: they are non-fatal audit
/// annotations whose ordering and multiplicity can vary between
/// Phase A and Phase B even when the protected ID rows are unchanged.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ClosureMemberDelta {
    pub added_assets: BTreeSet<FileAssetId>,
    pub added_bundles: BTreeSet<BundleId>,
    pub added_versions: BTreeSet<FileVersionId>,
    pub added_locations: BTreeSet<FileLocationId>,
    pub removed_assets: BTreeSet<FileAssetId>,
    pub removed_bundles: BTreeSet<BundleId>,
    pub removed_versions: BTreeSet<FileVersionId>,
    pub removed_locations: BTreeSet<FileLocationId>,
}

impl ClosureMemberDelta {
    /// True iff every add/remove set is empty — i.e., the two
    /// closures protect exactly the same ID rows.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.added_assets.is_empty()
            && self.added_bundles.is_empty()
            && self.added_versions.is_empty()
            && self.added_locations.is_empty()
            && self.removed_assets.is_empty()
            && self.removed_bundles.is_empty()
            && self.removed_versions.is_empty()
            && self.removed_locations.is_empty()
    }
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
    /// affected closure. The blocked caller uses `offending_scope` to
    /// scope its wait / takeover decision without a race-prone re-query.
    BlockedByPendingCommit {
        commit_id: CommitId,
        offending_scope: LeaseScope,
    },
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
    /// Closure delta detected between two snapshots (Phase A vs
    /// Phase B, or Phase B vs Phase C). The delta excludes
    /// `resolution_warnings` so transient warning churn cannot
    /// falsely trigger this variant — see
    /// `AffectedScopeClosure::id_member_delta`.
    BlockedByClosureGrew { delta: ClosureMemberDelta },
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

/// Error returned by an `AliasResolver` when it cannot enumerate the
/// live `FileLocation`s under a supplied `FileVersion`. The two
/// variants are deliberately distinct: `Unreachable` is a
/// physical-world condition the gate's force-path bypass (commit 10)
/// is designed to override; `Database` is our own storage layer
/// failing and surfaces at the gate boundary as
/// `VoomError::Database`, never as a closure-incomplete abort.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AliasResolutionError {
    /// External alias source (filesystem mount, object store, remote
    /// node) cannot enumerate live locations for the supplied
    /// `FileVersion`. Caller surfaces as `BlockedByClosureIncomplete`
    /// in Phase A and Phase B (commit 4 / commit 6).
    Unreachable { message: String },
    /// Underlying storage failure during alias resolution — not the
    /// "external mount offline" case; this is "our own DB layer
    /// broke." Surfaces at the gate boundary as `VoomError::Database`.
    Database(String),
}

/// Closure-walk input: enumerates every live `FileLocation` ID that
/// represents the same physical bytes as the supplied `FileVersion`.
/// Sprint 1 ships `SqliteAliasResolver` (returns live DB rows on the
/// version). Sprint 4 + 5 will layer FS / object-store-aware
/// resolvers behind the same trait so the closure walk can pick up
/// hardlinks, bind mounts, and shared mounts without changing the
/// gate's call sites. Fail-closed semantics: on `Unreachable`, the
/// gate aborts with `BlockedByClosureIncomplete` (commits 4 and 6).
///
/// Object-safe by construction: no generics, no associated types.
/// Callers in Phase A / B / C hold a `&dyn AliasResolver`.
#[async_trait::async_trait]
pub trait AliasResolver: Send + Sync {
    /// Return the IDs of every live `FileLocation` on `file_version_id`.
    /// Order is unspecified; callers fold the result into a
    /// `BTreeSet<FileLocationId>` so canonical ordering and dedup
    /// happen at the closure level (see
    /// `AffectedScopeClosure::file_locations`).
    ///
    /// # Errors
    ///
    /// Returns `AliasResolutionError::Unreachable` if the alias source
    /// is offline; `AliasResolutionError::Database` if the underlying
    /// storage layer fails.
    async fn aliases_for_version(
        &self,
        file_version_id: FileVersionId,
    ) -> Result<Vec<FileLocationId>, AliasResolutionError>;
}

/// Production `AliasResolver` backed by the live `file_locations`
/// table. Returns every `FileLocation` row where `file_version_id`
/// matches the supplied version and `retired_at IS NULL`. Order is
/// unspecified at the trait level — see `AliasResolver`.
#[derive(Debug, Clone)]
pub struct SqliteAliasResolver {
    pool: sqlx::SqlitePool,
}

impl SqliteAliasResolver {
    #[must_use]
    pub fn new(pool: sqlx::SqlitePool) -> Self {
        Self { pool }
    }
}

#[async_trait::async_trait]
impl AliasResolver for SqliteAliasResolver {
    async fn aliases_for_version(
        &self,
        file_version_id: FileVersionId,
    ) -> Result<Vec<FileLocationId>, AliasResolutionError> {
        let rows = sqlx::query_scalar::<_, i64>(
            "SELECT id FROM file_locations \
             WHERE file_version_id = ? AND retired_at IS NULL \
             ORDER BY id ASC",
        )
        .bind(i64::try_from(file_version_id.0).map_err(|e| {
            AliasResolutionError::Database(format!("alias resolver: id overflow: {e}"))
        })?)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| AliasResolutionError::Database(format!("alias resolver: {e}")))?;

        rows.into_iter()
            .map(|id| {
                u64::try_from(id).map(FileLocationId).map_err(|e| {
                    AliasResolutionError::Database(format!("alias resolver: id signedness: {e}"))
                })
            })
            .collect()
    }
}

#[cfg(test)]
#[path = "commit_safety_gate_test.rs"]
mod tests;
