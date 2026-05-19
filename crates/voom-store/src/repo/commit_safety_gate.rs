//! Commit safety gate types and Phase A entry point — Sprint 1 §9.3.
//!
//! Home for the three-phase destructive-commit gate. Phase A
//! (`prepare_destructive_commit`) lands here; Phase B / C / abort / list
//! land in later commits.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use sqlx::{Row, SqlitePool};
use time::OffsetDateTime;
use voom_core::VoomError;
use voom_core::ids::{
    BundleId, CommitId, EvidenceId, FileAssetId, FileLocationId, FileVersionId, UseLeaseId,
};
use voom_events::payload::{
    CommitAbortedByClosureIncompletePayload, CommitAbortedByStaleEvidencePayload,
    CommitAbortedByUseLeasePayload, CommitIntentRecordedPayload,
};
use voom_events::{Event, EventEnvelope, SubjectType};

use crate::repo::common::{i64_from_u64, iso8601, u64_from_i64};
use crate::repo::events::EventRepo;
use crate::repo::identity::{
    FileLocationKind, IdentityEvidenceTarget, IdentityRepo, LocationProof,
};
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

// ============================================================================
// Pending-commit lock helper (sub-slice 5)
// ============================================================================

/// Single source of truth for the "any in-flight commit covers this
/// scope?" question (M3 sequencing doc §5.1). Reads `commit_intents`
/// rows in `state IN ('pending', 'authorized')` that have at least one
/// `commit_intent_scope_members` row matching the supplied scope, and
/// returns the first hit as `(commit_id, offending_scope)` — or `None`
/// if no in-flight commit covers it.
///
/// Each `LeaseScope` variant consults exactly one `scope_*_id` column
/// of `commit_intent_scope_members`:
/// `Asset` → `scope_asset_id`, `Bundle` → `scope_bundle_id`,
/// `Version` → `scope_version_id`, `Location` → `scope_location_id`.
/// The migration-level CHECK constraint guarantees exactly one
/// `scope_*_id` is non-NULL per row, so a single-column lookup is
/// sufficient.
///
/// Callers translate a hit into the lock's caller-facing error variant
/// (`VoomError::Conflict(...)` for `UseLeaseRepo::acquire_in_tx` and
/// the `IdentityRepo::record_discovered_file_in_tx::AliasAttached`
/// branch, per sprint spec §9.2 / §8.7). `IdentityRepo::reconcile_rename_in_tx`
/// deliberately does NOT consult this helper (arch spec lines 697–708;
/// sprint spec §8.7 architectural exemption — renames must be allowed
/// to land against an in-flight commit so external moves never deadlock
/// the gate).
///
/// # Errors
///
/// `VoomError::Database` on storage failures from the underlying
/// `commit_intents` / `commit_intent_scope_members` join.
pub(crate) async fn consult_pending_commit_lock_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    scope: &LeaseScope,
) -> Result<Option<(CommitId, LeaseScope)>, VoomError> {
    // Column-driven query: each variant queries exactly one column.
    // The CHECK constraint on commit_intent_scope_members ensures
    // the same row cannot collide across granularities.
    let (sql, bind_id): (&str, i64) = match scope {
        LeaseScope::Asset(id) => (
            "SELECT ci.id FROM commit_intents ci \
             JOIN commit_intent_scope_members m ON m.commit_intent_id = ci.id \
             WHERE ci.state IN ('pending', 'authorized') \
               AND m.scope_asset_id = ? \
             ORDER BY ci.id ASC LIMIT 1",
            i64_from_u64(id.0),
        ),
        LeaseScope::Bundle(id) => (
            "SELECT ci.id FROM commit_intents ci \
             JOIN commit_intent_scope_members m ON m.commit_intent_id = ci.id \
             WHERE ci.state IN ('pending', 'authorized') \
               AND m.scope_bundle_id = ? \
             ORDER BY ci.id ASC LIMIT 1",
            i64_from_u64(id.0),
        ),
        LeaseScope::Version(id) => (
            "SELECT ci.id FROM commit_intents ci \
             JOIN commit_intent_scope_members m ON m.commit_intent_id = ci.id \
             WHERE ci.state IN ('pending', 'authorized') \
               AND m.scope_version_id = ? \
             ORDER BY ci.id ASC LIMIT 1",
            i64_from_u64(id.0),
        ),
        LeaseScope::Location(id) => (
            "SELECT ci.id FROM commit_intents ci \
             JOIN commit_intent_scope_members m ON m.commit_intent_id = ci.id \
             WHERE ci.state IN ('pending', 'authorized') \
               AND m.scope_location_id = ? \
             ORDER BY ci.id ASC LIMIT 1",
            i64_from_u64(id.0),
        ),
    };
    let row: Option<i64> = sqlx::query_scalar(sql)
        .bind(bind_id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(|e| VoomError::Database(format!("consult_pending_commit_lock: {e}")))?;
    Ok(row.map(|raw| (CommitId(u64_from_i64(raw)), *scope)))
}

// ============================================================================
// Phase A entry point — `prepare_destructive_commit` + abort helper
// ============================================================================

/// Disposition of a `prepare_destructive_commit` call.
///
/// `Pending` carries the durable `CommitIntent` (row landed in
/// `state = 'pending'`). `Blocked` carries the abort outcome — a
/// `commit_intents` row landed in `state = 'aborted'` with `commit_id`
/// referring to that row, and the matching `commit.aborted_by_*` event
/// row sits alongside it in `events`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrepareOutcome {
    Pending(CommitIntent),
    Blocked {
        commit_id: CommitId,
        result: CommitGateResult,
    },
}

/// Reason a Phase A gate-check aborted before any durable mutation could
/// land. The helper translates one of these into the matching
/// `AbortReason` row value AND the matching `commit.aborted_by_*` event.
#[derive(Debug, Clone)]
enum PhaseAAbort {
    UseLease {
        lease_id: UseLeaseId,
        lease_scope: LeaseScope,
    },
    StaleEvidence {
        evidence_id: EvidenceId,
        drift: EvidenceDrift,
    },
    ClosureIncomplete {
        message: String,
    },
}

impl PhaseAAbort {
    fn abort_reason(&self) -> AbortReason {
        match self {
            Self::UseLease { .. } => AbortReason::FreshLease,
            Self::StaleEvidence { .. } => AbortReason::StaleEvidence,
            Self::ClosureIncomplete { .. } => AbortReason::ClosureIncomplete,
        }
    }

    fn abort_reason_str(&self) -> &'static str {
        match self {
            Self::UseLease { .. } => "fresh_lease",
            Self::StaleEvidence { .. } => "stale_evidence",
            Self::ClosureIncomplete { .. } => "closure_incomplete",
        }
    }

    fn into_gate_result(self) -> CommitGateResult {
        match self {
            Self::UseLease {
                lease_id,
                lease_scope,
            } => CommitGateResult::BlockedByUseLease {
                lease_id,
                lease_scope,
            },
            Self::StaleEvidence { evidence_id, drift } => {
                CommitGateResult::BlockedByStaleEvidence { evidence_id, drift }
            }
            Self::ClosureIncomplete { message } => CommitGateResult::BlockedByClosureIncomplete {
                reason: ClosureFailure::AliasUnreachable {
                    message: message.clone(),
                },
                unreachable: vec![ClosureWarning { message }],
            },
        }
    }
}

/// Snapshot of the JSON-encoded `commit_intents` row body, captured
/// once before the closure walk so the gate's IMMEDIATE tx and the
/// two-tx abort helper bind the same column values. The four fields
/// mirror the four `commit_intents` columns the Phase A entry point
/// populates regardless of outcome.
struct CommitIntentRowBody<'a> {
    target_json: &'a str,
    closure_initial_json: &'a str,
    accepted_evidence_ids_json: &'a str,
    started_at: OffsetDateTime,
}

/// Phase A gate-check abort using the two-tx pattern (sequencing doc
/// §5.2). The two-tx pattern is **only** used for Phase A gate-check
/// aborts (raised before the `commit_intents` row would land in
/// `'pending'`). Phase B aborts, Phase C trip-wire aborts, and the
/// dedicated `abort_destructive_commit` entry point all commit the
/// intent-state transition and the event row in a single IMMEDIATE
/// transaction.
///
/// Tx 1 inserts the `commit_intents` row directly in `state = 'aborted'`
/// (no prior `pending` write — the gate check tripped before any
/// closure-bearing state landed). Tx 2 emits the matching
/// `commit.aborted_by_*` event. The split keeps the in-tx event-append
/// composition the rest of the codebase uses inaccessible from Phase A
/// abort paths, which would otherwise need to materialize an empty
/// closure into the durable `closure_initial` column under a tx the
/// gate's later phases never own.
///
/// Returns the durable `CommitId` of the aborted row.
async fn phase_a_gate_abort_with_event(
    pool: &SqlitePool,
    event_repo: &dyn EventRepo,
    row: &CommitIntentRowBody<'_>,
    aborted_at: OffsetDateTime,
    abort: PhaseAAbort,
) -> Result<CommitId, VoomError> {
    // two-tx: tx 1 inserts the aborted row.
    let started_iso = iso8601(row.started_at)?;
    let aborted_iso = iso8601(aborted_at)?;
    let mut tx1 = pool
        .begin()
        .await
        .map_err(|e| VoomError::Database(format!("phase A abort tx1 begin: {e}")))?;
    let insert = sqlx::query(
        "INSERT INTO commit_intents \
         (target, closure_initial, accepted_evidence_ids, state, started_at, \
          aborted_at, abort_reason) \
         VALUES (?, ?, ?, 'aborted', ?, ?, ?)",
    )
    .bind(row.target_json)
    .bind(row.closure_initial_json)
    .bind(row.accepted_evidence_ids_json)
    .bind(&started_iso)
    .bind(&aborted_iso)
    .bind(abort.abort_reason_str())
    .execute(&mut *tx1)
    .await
    .map_err(|e| VoomError::Database(format!("commit_intents abort insert: {e}")))?;
    let commit_id = CommitId(u64_from_i64(insert.last_insert_rowid()));
    tx1.commit()
        .await
        .map_err(|e| VoomError::Database(format!("phase A abort tx1 commit: {e}")))?;

    // two-tx: tx 2 emits the matching event.
    let payload = phase_a_abort_event(commit_id, aborted_at, &abort);
    let mut tx2 = pool
        .begin()
        .await
        .map_err(|e| VoomError::Database(format!("phase A abort tx2 begin: {e}")))?;
    event_repo
        .append_in_tx(
            &mut tx2,
            EventEnvelope {
                occurred_at: aborted_at,
                subject_type: SubjectType::CommitIntent,
                subject_id: Some(commit_id.0),
                trace_id: None,
                payload,
            },
        )
        .await?;
    tx2.commit()
        .await
        .map_err(|e| VoomError::Database(format!("phase A abort tx2 commit: {e}")))?;

    // Reference fields once so `PhaseAAbort` does not need additional
    // accessors. The abort_reason call also pins the
    // `AbortReason` ↔ `PhaseAAbort` mapping (used here for audit /
    // debug; the durable column carries the snake_case string).
    let _ = abort.abort_reason();
    Ok(commit_id)
}

fn phase_a_abort_event(
    commit_id: CommitId,
    aborted_at: OffsetDateTime,
    abort: &PhaseAAbort,
) -> Event {
    match abort {
        PhaseAAbort::UseLease {
            lease_id,
            lease_scope,
        } => Event::CommitAbortedByUseLease(CommitAbortedByUseLeasePayload {
            commit_id,
            lease_id: *lease_id,
            lease_scope_type: lease_scope.type_str().to_owned(),
            lease_scope_id: lease_scope.id_u64(),
            phase: "prepare".to_owned(),
            aborted_at,
        }),
        PhaseAAbort::StaleEvidence { evidence_id, drift } => {
            Event::CommitAbortedByStaleEvidence(CommitAbortedByStaleEvidencePayload {
                commit_id,
                evidence_id: *evidence_id,
                drift_kind: evidence_drift_str(drift).to_owned(),
                phase: "prepare".to_owned(),
                aborted_at,
            })
        }
        PhaseAAbort::ClosureIncomplete { message } => {
            Event::CommitAbortedByClosureIncomplete(CommitAbortedByClosureIncompletePayload {
                commit_id,
                phase: "prepare".to_owned(),
                message: message.clone(),
                aborted_at,
            })
        }
    }
}

fn evidence_drift_str(d: &EvidenceDrift) -> &'static str {
    match d {
        EvidenceDrift::PinnedFileVersionRetired => "pinned_file_version_retired",
        EvidenceDrift::PinnedHashDiffers => "pinned_hash_differs",
        EvidenceDrift::PinnedLocationRetired => "pinned_location_retired",
    }
}

fn commit_target_kind_str(t: &CommitTarget) -> &'static str {
    match t {
        CommitTarget::DeleteFileLocation(_) => "delete_file_location",
        CommitTarget::ReplaceFileLocation { .. } => "replace_file_location",
        CommitTarget::MoveFileLocation { .. } => "move_file_location",
    }
}

// ----- JSON wire formats for the `commit_intents` JSON columns ----------------
//
// `commit_intents.target` and `commit_intents.closure_initial` are
// JSON-encoded; `accepted_evidence_ids` is a JSON array. The Rust-side
// public types intentionally do NOT derive `Serialize`/`Deserialize`
// (some embed M2 types like `FileLocationKind` and `LocationProof`
// that do not derive serde, and adding derives there would force a
// wider M2 touch). Dedicated wire-format structs keep the on-disk JSON
// shape stable and isolated; later commits read the same columns back
// via the inverse mappers.

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum CommitTargetWire {
    #[serde(rename = "delete_file_location")]
    Delete { retired: FileLocationId },
    #[serde(rename = "replace_file_location")]
    Replace {
        retired: FileLocationId,
        new: FileLocationProposalWire,
    },
    #[serde(rename = "move_file_location")]
    Move {
        retired: FileLocationId,
        new: FileLocationProposalWire,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct FileLocationProposalWire {
    kind: String,
    value: String,
    proof_kind: Option<String>,
    proof_value: Option<String>,
    #[serde(with = "time::serde::iso8601")]
    observed_at: OffsetDateTime,
}

impl FileLocationProposalWire {
    fn from_proposal(p: &FileLocationProposal) -> Self {
        let (proof_kind, proof_value) = match &p.proof {
            None => (None, None),
            Some(proof) => (
                Some(proof_kind_str(proof).to_owned()),
                Some(proof_value_str(proof)),
            ),
        };
        Self {
            kind: p.kind.as_str().to_owned(),
            value: p.value.clone(),
            proof_kind,
            proof_value,
            observed_at: p.observed_at,
        }
    }
}

fn proof_kind_str(proof: &LocationProof) -> &'static str {
    match proof {
        LocationProof::LocalFileIdGeneration { .. } => "file_id_generation",
        LocationProof::ObjectStoreVersion { .. } => "object_version_id",
    }
}

fn proof_value_str(proof: &LocationProof) -> String {
    match proof {
        LocationProof::LocalFileIdGeneration {
            file_id,
            generation,
        } => serde_json::json!({
            "file_id": file_id.to_string(),
            "generation": generation,
        })
        .to_string(),
        LocationProof::ObjectStoreVersion {
            bucket,
            key,
            version_id,
        } => serde_json::json!({
            "bucket": bucket,
            "key": key,
            "version_id": version_id,
        })
        .to_string(),
    }
}

fn commit_target_to_wire(t: &CommitTarget) -> CommitTargetWire {
    match t {
        CommitTarget::DeleteFileLocation(id) => CommitTargetWire::Delete { retired: *id },
        CommitTarget::ReplaceFileLocation { retired, new } => CommitTargetWire::Replace {
            retired: *retired,
            new: FileLocationProposalWire::from_proposal(new),
        },
        CommitTarget::MoveFileLocation { retired, new } => CommitTargetWire::Move {
            retired: *retired,
            new: FileLocationProposalWire::from_proposal(new),
        },
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AffectedScopeClosureWire {
    file_assets: BTreeSet<FileAssetId>,
    file_versions: BTreeSet<FileVersionId>,
    file_locations: BTreeSet<FileLocationId>,
    bundles: BTreeSet<BundleId>,
    resolution_warnings: Vec<ClosureWarningWire>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ClosureWarningWire {
    message: String,
}

fn closure_to_wire(c: &AffectedScopeClosure) -> AffectedScopeClosureWire {
    AffectedScopeClosureWire {
        file_assets: c.file_assets.clone(),
        file_versions: c.file_versions.clone(),
        file_locations: c.file_locations.clone(),
        bundles: c.bundles.clone(),
        resolution_warnings: c
            .resolution_warnings
            .iter()
            .map(|w| ClosureWarningWire {
                message: w.message.clone(),
            })
            .collect(),
    }
}

fn encode_target(t: &CommitTarget) -> Result<String, VoomError> {
    serde_json::to_string(&commit_target_to_wire(t))
        .map_err(|e| VoomError::Internal(format!("encode commit_target: {e}")))
}

fn encode_closure(c: &AffectedScopeClosure) -> Result<String, VoomError> {
    serde_json::to_string(&closure_to_wire(c))
        .map_err(|e| VoomError::Internal(format!("encode closure: {e}")))
}

fn encode_evidence_ids(ids: &[EvidenceId]) -> Result<String, VoomError> {
    serde_json::to_string(ids).map_err(|e| VoomError::Internal(format!("encode evidence_ids: {e}")))
}

// ----- Phase A main entry point --------------------------------------------

/// Phase A of the destructive-commit gate — sub-slice 4 of the M3 Phase 2
/// plan. Computes the affected-scope closure, evaluates the three Phase A
/// gate checks (blocking use-lease, accepted-evidence drift,
/// closure-walk reachability), and persists either a `state = 'pending'`
/// `commit_intents` row (success) or a `state = 'aborted'` row (gate
/// check tripped) along with the matching event.
///
/// The success path runs inside one IMMEDIATE transaction:
/// closure-walk → lease check → evidence revalidation → INSERT pending
/// row → expand `commit_intent_scope_members` → emit
/// `commit.intent_recorded` → COMMIT. The abort paths rollback the
/// gate's IMMEDIATE tx and use `phase_a_gate_abort_with_event` to land
/// the aborted row and event in two sequential transactions (sequencing
/// doc §5.2).
///
/// `alias_resolver` covers **external** (non-DB) alias sources only.
/// DB-internal alias enumeration goes through
/// `IdentityRepo::list_live_file_locations_by_version_in_tx` on the
/// gate's tx handle (round-5 fix).
///
/// Sprint 1 ships no `override_token` parameter — commit 10 retrofits
/// `DestructiveCommit` and this signature together. Calls that hit
/// `AliasResolutionError::Unreachable` abort unconditionally for Phase A
/// in Sprint 1 through commit 9.
///
/// # Errors
///
/// `VoomError::Database` / `VoomError::Internal` on storage failures
/// (including `AliasResolutionError::Database` from an external alias
/// source). Gate-check failures return `Ok(PrepareOutcome::Blocked)`
/// rather than `Err` — `Err` is reserved for genuine storage failures
/// that the caller cannot reason about.
pub async fn prepare_destructive_commit(
    pool: &SqlitePool,
    identity_repo: &dyn IdentityRepo,
    event_repo: &dyn EventRepo,
    alias_resolver: &dyn AliasResolver,
    input: DestructiveCommit,
    now: OffsetDateTime,
) -> Result<PrepareOutcome, VoomError> {
    let DestructiveCommit {
        target,
        accepted_evidence_ids,
    } = input;
    let target_json = encode_target(&target)?;
    let accepted_evidence_ids_json = encode_evidence_ids(&accepted_evidence_ids)?;

    let mut tx = pool
        .begin()
        .await
        .map_err(|e| VoomError::Database(format!("prepare: tx begin: {e}")))?;

    let walk_outcome = run_phase_a_gate_in_tx(
        &mut tx,
        identity_repo,
        alias_resolver,
        &target,
        &accepted_evidence_ids,
    )
    .await;
    let walk = match walk_outcome {
        Ok(Ok(w)) => w,
        Ok(Err(abort_outcome)) => {
            tx.rollback()
                .await
                .map_err(|e| VoomError::Database(format!("prepare: rollback: {e}")))?;
            let closure_initial_json = encode_closure(&abort_outcome.closure_initial)?;
            let row = CommitIntentRowBody {
                target_json: &target_json,
                closure_initial_json: &closure_initial_json,
                accepted_evidence_ids_json: &accepted_evidence_ids_json,
                started_at: now,
            };
            let commit_id = phase_a_gate_abort_with_event(
                pool,
                event_repo,
                &row,
                now,
                abort_outcome.abort.clone(),
            )
            .await?;
            return Ok(PrepareOutcome::Blocked {
                commit_id,
                result: abort_outcome.abort.into_gate_result(),
            });
        }
        Err(e) => return Err(e),
    };

    let closure_initial_json = encode_closure(&walk.closure)?;
    let commit_id = insert_pending_intent(
        &mut tx,
        &target_json,
        &closure_initial_json,
        &accepted_evidence_ids_json,
        now,
    )
    .await?;
    expand_scope_members(&mut tx, commit_id, &walk.closure).await?;
    emit_intent_recorded(
        event_repo,
        &mut tx,
        commit_id,
        &target,
        &walk.closure,
        accepted_evidence_ids.len(),
        now,
    )
    .await?;
    tx.commit()
        .await
        .map_err(|e| VoomError::Database(format!("prepare: commit: {e}")))?;

    Ok(PrepareOutcome::Pending(CommitIntent {
        commit_id,
        closure_initial: walk.closure,
        evaluated_lease_ids: walk.evaluated_lease_ids,
        revalidated_evidence: walk.revalidated_evidence,
        epoch: 0,
    }))
}

struct GateWalkOk {
    closure: AffectedScopeClosure,
    evaluated_lease_ids: Vec<UseLeaseId>,
    revalidated_evidence: Vec<EvidenceRevalidationResult>,
}

struct GateWalkAbort {
    closure_initial: AffectedScopeClosure,
    abort: PhaseAAbort,
}

/// Run all three Phase A gate checks inside the gate's IMMEDIATE tx.
/// Returns `Ok(Ok(_))` on a passing walk (caller proceeds to insert
/// the `pending` row); `Ok(Err(_))` on a gate-check abort (caller
/// rolls back and runs the two-tx abort helper); `Err(_)` on a storage
/// failure the caller cannot reason about.
async fn run_phase_a_gate_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    identity_repo: &dyn IdentityRepo,
    alias_resolver: &dyn AliasResolver,
    target: &CommitTarget,
    accepted_evidence_ids: &[EvidenceId],
) -> Result<Result<GateWalkOk, GateWalkAbort>, VoomError> {
    // Step 1: closure walk on the gate's IMMEDIATE tx.
    let closure = match build_closure(tx, identity_repo, alias_resolver, target).await? {
        Ok((c, _)) => c,
        Err(abort) => {
            return Ok(Err(GateWalkAbort {
                closure_initial: AffectedScopeClosure::default(),
                abort,
            }));
        }
    };

    // Step 2: blocking-lease check.
    let evaluated_lease_ids = list_blocking_leases_in_tx(tx, &closure).await?;
    if let Some((lease_id, lease_scope)) = first_blocking_overlap_in_tx(tx, &closure).await? {
        return Ok(Err(GateWalkAbort {
            closure_initial: closure,
            abort: PhaseAAbort::UseLease {
                lease_id,
                lease_scope,
            },
        }));
    }

    // Step 3: accepted-evidence revalidation.
    let revalidated_evidence =
        revalidate_evidence_in_tx(tx, identity_repo, accepted_evidence_ids).await?;
    if let Some((evidence_id, drift)) = first_evidence_drift(&revalidated_evidence) {
        return Ok(Err(GateWalkAbort {
            closure_initial: closure,
            abort: PhaseAAbort::StaleEvidence {
                evidence_id,
                drift: drift.clone(),
            },
        }));
    }

    Ok(Ok(GateWalkOk {
        closure,
        evaluated_lease_ids,
        revalidated_evidence,
    }))
}

async fn insert_pending_intent(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    target_json: &str,
    closure_initial_json: &str,
    accepted_evidence_ids_json: &str,
    started_at: OffsetDateTime,
) -> Result<CommitId, VoomError> {
    let started_iso = iso8601(started_at)?;
    let res = sqlx::query(
        "INSERT INTO commit_intents \
         (target, closure_initial, accepted_evidence_ids, state, started_at) \
         VALUES (?, ?, ?, 'pending', ?)",
    )
    .bind(target_json)
    .bind(closure_initial_json)
    .bind(accepted_evidence_ids_json)
    .bind(&started_iso)
    .execute(&mut **tx)
    .await
    .map_err(|e| VoomError::Database(format!("commit_intents pending insert: {e}")))?;
    Ok(CommitId(u64_from_i64(res.last_insert_rowid())))
}

async fn emit_intent_recorded(
    event_repo: &dyn EventRepo,
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    commit_id: CommitId,
    target: &CommitTarget,
    closure: &AffectedScopeClosure,
    accepted_evidence_count: usize,
    started_at: OffsetDateTime,
) -> Result<(), VoomError> {
    let payload = Event::CommitIntentRecorded(CommitIntentRecordedPayload {
        commit_id,
        target_kind: commit_target_kind_str(target).to_owned(),
        closure_asset_count: u32::try_from(closure.file_assets.len()).unwrap_or(u32::MAX),
        closure_bundle_count: u32::try_from(closure.bundles.len()).unwrap_or(u32::MAX),
        closure_version_count: u32::try_from(closure.file_versions.len()).unwrap_or(u32::MAX),
        closure_location_count: u32::try_from(closure.file_locations.len()).unwrap_or(u32::MAX),
        accepted_evidence_count: u32::try_from(accepted_evidence_count).unwrap_or(u32::MAX),
        started_at,
    });
    event_repo
        .append_in_tx(
            tx,
            EventEnvelope {
                occurred_at: started_at,
                subject_type: SubjectType::CommitIntent,
                subject_id: Some(commit_id.0),
                trace_id: None,
                payload,
            },
        )
        .await?;
    Ok(())
}

/// Resolve the destructive target into the set of `FileLocation` rows
/// the closure walk anchors on. Returns `None` if the target's retired
/// row is missing or already terminal — Phase C will trip the epoch
/// guard regardless, but Phase A surfaces it eagerly as a closure-walk
/// failure so the operator does not wait for the round trip.
async fn build_closure(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    identity_repo: &dyn IdentityRepo,
    alias_resolver: &dyn AliasResolver,
    target: &CommitTarget,
) -> Result<Result<(AffectedScopeClosure, Vec<FileLocationId>), PhaseAAbort>, VoomError> {
    let retired_location_id = match target {
        CommitTarget::DeleteFileLocation(id) => *id,
        CommitTarget::ReplaceFileLocation { retired, .. }
        | CommitTarget::MoveFileLocation { retired, .. } => *retired,
    };

    let location = identity_repo
        .get_file_location_in_tx(tx, retired_location_id)
        .await?;
    let Some(location) = location else {
        return Ok(Err(PhaseAAbort::ClosureIncomplete {
            message: format!("target file_location {retired_location_id} not found"),
        }));
    };
    if location.retired_at.is_some() {
        return Ok(Err(PhaseAAbort::ClosureIncomplete {
            message: format!("target file_location {retired_location_id} already retired"),
        }));
    }

    let version = identity_repo
        .get_file_version_in_tx(tx, location.file_version_id)
        .await?;
    let Some(version) = version else {
        return Ok(Err(PhaseAAbort::ClosureIncomplete {
            message: format!("target file_version {} not found", location.file_version_id),
        }));
    };

    // DB-internal live alias enumeration on the same tx (round-5 fix).
    let live_locations: BTreeSet<FileLocationId> = identity_repo
        .list_live_file_locations_by_version_in_tx(tx, version.id)
        .await?
        .into_iter()
        .collect();

    // External alias enumeration through the trait — Sprint 1 ships
    // only `FailingAliasResolver`, which returns `Unreachable` to drive
    // the closure-incomplete abort branch in tests.
    let mut alias_warnings: Vec<ClosureWarning> = Vec::new();
    let mut external_locations: BTreeSet<FileLocationId> = BTreeSet::new();
    match alias_resolver.aliases_for_version(version.id).await {
        Ok(extra) => {
            for id in extra {
                external_locations.insert(id);
            }
        }
        Err(AliasResolutionError::Unreachable { message }) => {
            return Ok(Err(PhaseAAbort::ClosureIncomplete { message }));
        }
        Err(AliasResolutionError::Database(msg)) => {
            return Err(VoomError::Database(format!("alias resolver: {msg}")));
        }
    }

    let mut file_locations = live_locations;
    for id in external_locations {
        file_locations.insert(id);
    }
    // The retired target itself is always part of the closure even if
    // the live-listing query already excluded retired rows: Phase A
    // guards against the target already being terminal upstream, so
    // a non-terminal target is always live and already present, but a
    // defense-in-depth insert here keeps the invariant explicit.
    file_locations.insert(retired_location_id);

    // Bundle membership for the owning FileAsset.
    let mut bundles: BTreeSet<BundleId> = BTreeSet::new();
    let bundle_rows: Vec<i64> =
        sqlx::query_scalar("SELECT bundle_id FROM asset_bundle_members WHERE file_asset_id = ?")
            .bind(i64_from_u64(version.file_asset_id.0))
            .fetch_all(&mut **tx)
            .await
            .map_err(|e| VoomError::Database(format!("asset_bundle_members lookup: {e}")))?;
    for raw in bundle_rows {
        bundles.insert(BundleId(u64_from_i64(raw)));
    }

    let mut file_versions: BTreeSet<FileVersionId> = BTreeSet::new();
    file_versions.insert(version.id);

    let mut file_assets: BTreeSet<FileAssetId> = BTreeSet::new();
    file_assets.insert(version.file_asset_id);

    let _ = std::mem::take(&mut alias_warnings); // warnings stay empty in Sprint 1; placeholder for FS resolvers.
    let closure = AffectedScopeClosure {
        file_assets,
        file_versions,
        file_locations: file_locations.clone(),
        bundles,
        resolution_warnings: Vec::new(),
    };
    let target_locations: Vec<FileLocationId> = file_locations.into_iter().collect();
    Ok(Ok((closure, target_locations)))
}

/// Read every live blocking use-lease whose scope_*_id column matches a
/// member of `closure`. Returns the list of `UseLeaseId`s evaluated
/// (used as `CommitIntent.evaluated_lease_ids` on the success path).
async fn list_blocking_leases_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    closure: &AffectedScopeClosure,
) -> Result<Vec<UseLeaseId>, VoomError> {
    let mut ids: Vec<UseLeaseId> = Vec::new();
    let raw_rows = blocking_lease_rows_in_tx(tx, closure).await?;
    for (id, _) in raw_rows {
        ids.push(id);
    }
    Ok(ids)
}

/// First overlap between a live blocking use-lease and `closure`. The
/// return shape carries both the lease id and the lease's scope so the
/// abort payload can report the offending scope without a second
/// lookup.
async fn first_blocking_overlap_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    closure: &AffectedScopeClosure,
) -> Result<Option<(UseLeaseId, LeaseScope)>, VoomError> {
    Ok(blocking_lease_rows_in_tx(tx, closure)
        .await?
        .into_iter()
        .next())
}

/// Underlying query: returns every (`lease_id`, scope) pair where the
/// lease is live (`release_reason IS NULL`), blocking, and its scope
/// matches a member of `closure`. Ordered by `id ASC` so the
/// "first overlap" path is deterministic across test runs.
async fn blocking_lease_rows_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    closure: &AffectedScopeClosure,
) -> Result<Vec<(UseLeaseId, LeaseScope)>, VoomError> {
    if closure.file_assets.is_empty()
        && closure.bundles.is_empty()
        && closure.file_versions.is_empty()
        && closure.file_locations.is_empty()
    {
        return Ok(Vec::new());
    }
    let assets_json = serde_json::to_string(&closure.file_assets)
        .map_err(|e| VoomError::Internal(format!("encode closure.file_assets: {e}")))?;
    let bundles_json = serde_json::to_string(&closure.bundles)
        .map_err(|e| VoomError::Internal(format!("encode closure.bundles: {e}")))?;
    let versions_json = serde_json::to_string(&closure.file_versions)
        .map_err(|e| VoomError::Internal(format!("encode closure.file_versions: {e}")))?;
    let locations_json = serde_json::to_string(&closure.file_locations)
        .map_err(|e| VoomError::Internal(format!("encode closure.file_locations: {e}")))?;

    // SQLite `json_each` produces one row per element of the bound JSON
    // array; the UNION ALL across the four scope columns is the
    // four-granularity overlap check from §9.3. `release_reason IS NULL`
    // restricts to live leases; `blocking_mode = 'blocking'` honors the
    // arch-spec distinction between blocking and advisory.
    let rows = sqlx::query(
        "SELECT id, scope_asset_id, scope_bundle_id, scope_version_id, scope_location_id \
         FROM asset_use_leases \
         WHERE release_reason IS NULL AND blocking_mode = 'blocking' AND ( \
             scope_asset_id    IN (SELECT value FROM json_each(?)) \
          OR scope_bundle_id   IN (SELECT value FROM json_each(?)) \
          OR scope_version_id  IN (SELECT value FROM json_each(?)) \
          OR scope_location_id IN (SELECT value FROM json_each(?)) \
         ) \
         ORDER BY id ASC",
    )
    .bind(&assets_json)
    .bind(&bundles_json)
    .bind(&versions_json)
    .bind(&locations_json)
    .fetch_all(&mut **tx)
    .await
    .map_err(|e| VoomError::Database(format!("blocking-lease overlap: {e}")))?;

    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        let id: i64 = row
            .try_get("id")
            .map_err(|e| VoomError::Database(format!("blocking-lease row: {e}")))?;
        let sa: Option<i64> = row
            .try_get("scope_asset_id")
            .map_err(|e| VoomError::Database(format!("blocking-lease row: {e}")))?;
        let sb: Option<i64> = row
            .try_get("scope_bundle_id")
            .map_err(|e| VoomError::Database(format!("blocking-lease row: {e}")))?;
        let sv: Option<i64> = row
            .try_get("scope_version_id")
            .map_err(|e| VoomError::Database(format!("blocking-lease row: {e}")))?;
        let sl: Option<i64> = row
            .try_get("scope_location_id")
            .map_err(|e| VoomError::Database(format!("blocking-lease row: {e}")))?;
        let scope = match (sa, sb, sv, sl) {
            (Some(v), None, None, None) => LeaseScope::Asset(FileAssetId(u64_from_i64(v))),
            (None, Some(v), None, None) => LeaseScope::Bundle(BundleId(u64_from_i64(v))),
            (None, None, Some(v), None) => LeaseScope::Version(FileVersionId(u64_from_i64(v))),
            (None, None, None, Some(v)) => LeaseScope::Location(FileLocationId(u64_from_i64(v))),
            other => {
                return Err(VoomError::Database(format!(
                    "blocking-lease row: scope_*_id columns are not exactly-one: {other:?}"
                )));
            }
        };
        out.push((UseLeaseId(u64_from_i64(id)), scope));
    }
    Ok(out)
}

/// Revalidate every accepted-evidence pin against current state. For
/// each `evidence_id`, look up the row inside the gate's tx, decode the
/// pinned columns, and compare against current state. Returns one
/// result per id (`drift = None` for pins that still match).
async fn revalidate_evidence_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    identity_repo: &dyn IdentityRepo,
    ids: &[EvidenceId],
) -> Result<Vec<EvidenceRevalidationResult>, VoomError> {
    let mut out = Vec::with_capacity(ids.len());
    for id in ids {
        let evidence = identity_repo
            .get_identity_evidence_in_tx(tx, *id)
            .await?
            .ok_or_else(|| VoomError::NotFound(format!("identity_evidence {id} not found")))?;
        // Phase A only consults accepted rows. Treat un-accepted /
        // superseded pins as drift so the gate cannot proceed against
        // evidence that no longer carries an authoritative pin.
        if evidence.accepted_at.is_none() {
            out.push(EvidenceRevalidationResult {
                evidence_id: *id,
                drift: Some(EvidenceDrift::PinnedFileVersionRetired),
            });
            continue;
        }

        let drift = first_evidence_pin_drift(tx, &evidence).await?;
        out.push(EvidenceRevalidationResult {
            evidence_id: *id,
            drift,
        });
        // `IdentityEvidenceTarget` exists in `identity.rs` and is
        // imported here so the round-trip parsing of the row's
        // `target_type` is the single source of truth; the variant
        // itself is unused in Sprint 1 evidence revalidation.
        let _ = std::marker::PhantomData::<IdentityEvidenceTarget>;
    }
    Ok(out)
}

async fn first_evidence_pin_drift(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    evidence: &crate::repo::identity::IdentityEvidence,
) -> Result<Option<EvidenceDrift>, VoomError> {
    // Pinned FileVersion IDs — any retired version → drift.
    if let Some(versions_json) = &evidence.pinned_file_version_ids {
        for vid in pinned_u64_array(versions_json, "pinned_file_version_ids")? {
            if version_is_retired(tx, FileVersionId(vid)).await? {
                return Ok(Some(EvidenceDrift::PinnedFileVersionRetired));
            }
        }
    }
    // Pinned locations — any retired location → drift.
    if let Some(locs_json) = &evidence.pinned_locations {
        for lid in pinned_u64_array(locs_json, "pinned_locations")? {
            if location_is_retired(tx, FileLocationId(lid)).await? {
                return Ok(Some(EvidenceDrift::PinnedLocationRetired));
            }
        }
    }
    // Pinned hashes — compare against current `file_versions.content_hash`.
    // The pin shape ships as `[ [version_id, hash], ... ]` per sprint
    // §8.7; rows where the stored hash no longer matches drive the
    // `PinnedHashDiffers` exit.
    if let Some(hashes_json) = &evidence.pinned_hashes {
        for (vid, expected) in pinned_hash_pairs(hashes_json, "pinned_hashes")? {
            if let Some(current) = version_content_hash(tx, FileVersionId(vid)).await? {
                if current != expected {
                    return Ok(Some(EvidenceDrift::PinnedHashDiffers));
                }
            } else {
                // Pinned to a version that no longer exists — surface
                // the retired-version drift kind so the operator's
                // diagnostic path is consistent.
                return Ok(Some(EvidenceDrift::PinnedFileVersionRetired));
            }
        }
    }
    Ok(None)
}

fn pinned_u64_array(value: &JsonValue, field: &str) -> Result<Vec<u64>, VoomError> {
    let arr = value
        .as_array()
        .ok_or_else(|| VoomError::Database(format!("{field}: expected JSON array")))?;
    let mut out = Vec::with_capacity(arr.len());
    for v in arr {
        let n = v
            .as_u64()
            .ok_or_else(|| VoomError::Database(format!("{field}: expected u64 element")))?;
        out.push(n);
    }
    Ok(out)
}

fn pinned_hash_pairs(value: &JsonValue, field: &str) -> Result<Vec<(u64, String)>, VoomError> {
    let arr = value
        .as_array()
        .ok_or_else(|| VoomError::Database(format!("{field}: expected JSON array")))?;
    let mut out = Vec::with_capacity(arr.len());
    for pair in arr {
        let row = pair
            .as_array()
            .ok_or_else(|| VoomError::Database(format!("{field}: expected JSON array element")))?;
        if row.len() != 2 {
            return Err(VoomError::Database(format!(
                "{field}: expected 2-element [version_id, hash] arrays"
            )));
        }
        let vid = row[0]
            .as_u64()
            .ok_or_else(|| VoomError::Database(format!("{field}: version_id not u64")))?;
        let hash = row[1]
            .as_str()
            .ok_or_else(|| VoomError::Database(format!("{field}: hash not str")))?
            .to_owned();
        out.push((vid, hash));
    }
    Ok(out)
}

async fn version_is_retired(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    id: FileVersionId,
) -> Result<bool, VoomError> {
    let row: Option<Option<String>> =
        sqlx::query_scalar("SELECT retired_at FROM file_versions WHERE id = ?")
            .bind(i64_from_u64(id.0))
            .fetch_optional(&mut **tx)
            .await
            .map_err(|e| VoomError::Database(format!("file_versions retired probe: {e}")))?;
    Ok(matches!(row, Some(Some(_))))
}

async fn location_is_retired(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    id: FileLocationId,
) -> Result<bool, VoomError> {
    let row: Option<Option<String>> =
        sqlx::query_scalar("SELECT retired_at FROM file_locations WHERE id = ?")
            .bind(i64_from_u64(id.0))
            .fetch_optional(&mut **tx)
            .await
            .map_err(|e| VoomError::Database(format!("file_locations retired probe: {e}")))?;
    Ok(matches!(row, Some(Some(_))))
}

async fn version_content_hash(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    id: FileVersionId,
) -> Result<Option<String>, VoomError> {
    let row: Option<String> =
        sqlx::query_scalar("SELECT content_hash FROM file_versions WHERE id = ?")
            .bind(i64_from_u64(id.0))
            .fetch_optional(&mut **tx)
            .await
            .map_err(|e| VoomError::Database(format!("file_versions hash probe: {e}")))?;
    Ok(row)
}

fn first_evidence_drift(
    results: &[EvidenceRevalidationResult],
) -> Option<(EvidenceId, &EvidenceDrift)> {
    for r in results {
        if let Some(d) = &r.drift {
            return Some((r.evidence_id, d));
        }
    }
    None
}

/// Insert one `commit_intent_scope_members` row per closure member,
/// across all four granularities. Per migration 0005's CHECK exactly
/// one of the four `scope_*_id` columns is non-NULL per row.
async fn expand_scope_members(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    commit_id: CommitId,
    closure: &AffectedScopeClosure,
) -> Result<(), VoomError> {
    let cid = i64_from_u64(commit_id.0);
    for id in &closure.file_assets {
        sqlx::query(
            "INSERT INTO commit_intent_scope_members \
             (commit_intent_id, scope_asset_id) VALUES (?, ?)",
        )
        .bind(cid)
        .bind(i64_from_u64(id.0))
        .execute(&mut **tx)
        .await
        .map_err(|e| VoomError::Database(format!("scope_members asset insert: {e}")))?;
    }
    for id in &closure.bundles {
        sqlx::query(
            "INSERT INTO commit_intent_scope_members \
             (commit_intent_id, scope_bundle_id) VALUES (?, ?)",
        )
        .bind(cid)
        .bind(i64_from_u64(id.0))
        .execute(&mut **tx)
        .await
        .map_err(|e| VoomError::Database(format!("scope_members bundle insert: {e}")))?;
    }
    for id in &closure.file_versions {
        sqlx::query(
            "INSERT INTO commit_intent_scope_members \
             (commit_intent_id, scope_version_id) VALUES (?, ?)",
        )
        .bind(cid)
        .bind(i64_from_u64(id.0))
        .execute(&mut **tx)
        .await
        .map_err(|e| VoomError::Database(format!("scope_members version insert: {e}")))?;
    }
    for id in &closure.file_locations {
        sqlx::query(
            "INSERT INTO commit_intent_scope_members \
             (commit_intent_id, scope_location_id) VALUES (?, ?)",
        )
        .bind(cid)
        .bind(i64_from_u64(id.0))
        .execute(&mut **tx)
        .await
        .map_err(|e| VoomError::Database(format!("scope_members location insert: {e}")))?;
    }
    Ok(())
}

#[cfg(test)]
#[path = "commit_safety_gate_test.rs"]
mod tests;
