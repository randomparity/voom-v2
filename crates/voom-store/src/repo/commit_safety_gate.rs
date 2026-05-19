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
/// the gate to authorize. Sprint 1 ships three variants —
/// `DeleteFileLocation`, `ReplaceFileLocation`, `MoveFileLocation` —
/// all operating on `file_locations` rows. Targets that mutate
/// `file_versions` or `asset_bundles` are deferred to Sprint 5+
/// because the schema and the cascade semantics needed to retire
/// those rows safely are not yet defined:
/// - `DeleteFileVersion`: retiring a version leaves live
///   `FileLocation` rows pointing at it (Codex round-5). The safe
///   cascade — atomically retire every location under the version
///   using the snapshotted epochs — needs its own design pass.
/// - `ArchiveFileVersion`: `file_versions` schema (migration 0003)
///   has no `archived_at` column.
/// - `ArchiveBundle` / `DeleteBundle`: same schema-column gap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommitTarget {
    DeleteFileLocation(FileLocationId),
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

/// Granularity tag for a member of `AffectedScopeClosure`. Used by the
/// `commit_intents.target_row_epochs` JSON snapshot (migration 0005) to
/// drive the destructive dispatch lookup by `(kind, id)`. Serde-encoded
/// in `snake_case` so the JSON column form matches the existing
/// per-table vocabulary (`file_asset`, `file_version`, `file_location`,
/// `bundle`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
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
/// `recovery_required`). One enum, two destination columns: the gate
/// writes the value into `commit_intents.abort_reason` for `aborted`
/// rows and into `commit_intents.recovery_reason` for
/// `recovery_required` rows (migration 0005 enforces the per-state
/// column shape). Most variants belong unambiguously to the
/// pre-mutation abort path; `ClosureGrew`, `FreshLease`, and
/// `MutationFailed` can drive either column depending on which phase
/// fired the trip-wire — Phase B writes to `abort_reason`, Phase C
/// writes to `recovery_reason`.
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
    /// `commit_intents.target_row_epochs`. Drives `recovery_required`;
    /// stored in `commit_intents.recovery_reason`. This variant never
    /// reaches `abort_reason`.
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

/// Opaque handle returned by `authorize_destructive_commit` on
/// success. Carries the authorized closure, the lease IDs and evidence
/// revalidation results evaluated against it, and the `commit_id`
/// Phase C uses to re-read the durable per-member epoch snapshot. The
/// per-member epochs are NOT carried inline: they are persisted
/// atomically with the `state = 'authorized'` transition in
/// `commit_intents.target_row_epochs` (migration 0005), and Phase C
/// re-reads them by `commit_id` inside the finalize tx. This makes the
/// stale-target-epoch trip-wire authoritative against the DB rather
/// than against caller-held state, and makes the permit
/// reconstructible after a process crash between authorize and
/// finalize.
///
/// Fields are module-private — only code inside `commit_safety_gate`
/// (Phase B's `authorize_destructive_commit` in commit 6, plus the
/// sibling tests under the `tests` child module) can fabricate or
/// inspect them. External consumers reach state through the accessor
/// methods. Phase B builds permits in-module via the struct literal;
/// no crate-visible constructor is exposed, because exposing one
/// would re-open the bypass path the module-private fields are there
/// to close.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitPermit {
    commit_id: CommitId,
    authorized_at: OffsetDateTime,
    closure_authorized: AffectedScopeClosure,
    evaluated_lease_ids: Vec<UseLeaseId>,
    revalidated_evidence: Vec<EvidenceRevalidationResult>,
    epoch: u64,
}

impl CommitPermit {
    #[must_use]
    pub fn commit_id(&self) -> CommitId {
        self.commit_id
    }

    #[must_use]
    pub fn authorized_at(&self) -> OffsetDateTime {
        self.authorized_at
    }

    #[must_use]
    pub fn closure_authorized(&self) -> &AffectedScopeClosure {
        &self.closure_authorized
    }

    #[must_use]
    pub fn evaluated_lease_ids(&self) -> &[UseLeaseId] {
        &self.evaluated_lease_ids
    }

    #[must_use]
    pub fn revalidated_evidence(&self) -> &[EvidenceRevalidationResult] {
        &self.revalidated_evidence
    }

    #[must_use]
    pub fn epoch(&self) -> u64 {
        self.epoch
    }
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
    /// snapshotted in `commit_intents.target_row_epochs` at Phase B.
    /// Drives the commit to `recovery_required` and emits
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

/// Resolver for **external** (non-database) alias sources — e.g.
/// filesystem mounts that expose hardlinks/bind mounts, object
/// stores that mirror live `FileLocation` rows under a different
/// URL scheme. Sprint 1 ships no production resolver; Sprint 4 and
/// Sprint 5 introduce filesystem-aware and object-store-aware
/// resolvers behind this trait.
///
/// **DB-internal alias enumeration does NOT use this trait.** The
/// gate's closure walker (commit 4 / Phase A) reads live
/// `file_locations` rows directly via
/// `IdentityRepo::list_live_file_locations_by_version_in_tx`,
/// inside the same IMMEDIATE transaction the gate's safety checks
/// run under. Mixing DB-internal enumeration with this trait
/// (which has no transaction parameter) would either observe
/// rows outside the gate's tx snapshot or, on single-connection
/// pools, deadlock waiting for the connection already held by the
/// open tx. Codex round-5 review surfaced this hazard; the
/// previously-shipped `SqliteAliasResolver` (commit 2) was deleted
/// as part of the round-5 fix.
///
/// Object-safe by construction: no generics, no associated types.
/// Callers in Phase A / B / C hold a `&dyn AliasResolver` for
/// external sources only.
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

#[cfg(test)]
#[path = "commit_safety_gate_test.rs"]
mod tests;
