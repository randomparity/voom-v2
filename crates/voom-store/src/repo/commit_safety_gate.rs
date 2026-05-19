//! Commit safety gate types and Phase A entry point — Sprint 1 §9.3.
//!
//! Home for the three-phase destructive-commit gate. Phase A
//! (`prepare_destructive_commit`) lands here; Phase B / C / abort / list
//! land in later commits.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use sqlx::{Acquire, Row, Sqlite, SqlitePool, Transaction};
use time::OffsetDateTime;
use voom_core::VoomError;
use voom_core::ids::{
    BundleId, CommitId, EvidenceId, FileAssetId, FileLocationId, FileVersionId, UseLeaseId,
};
use voom_events::payload::{
    CommitAbortedByClosureGrewPayload, CommitAbortedByClosureIncompletePayload,
    CommitAbortedByPendingCommitPayload, CommitAbortedByStaleEvidencePayload,
    CommitAbortedByUseLeasePayload, CommitAbortedPostMutationPayload,
    CommitAbortedPreMutationPayload, CommitAuthorizedPayload, CommitCompletedPayload,
    CommitForcedOverridePayload, CommitIntentRecordedPayload, CommitRecoveryRequiredPayload,
    TargetEpochDriftWire,
};
use voom_events::{Event, EventEnvelope, SubjectType};

use crate::repo::common::{i64_from_u64, iso8601, parse_iso8601, u64_from_i64};
use crate::repo::events::EventRepo;
use crate::repo::identity::{
    FileLocationKind, IdentityEvidenceTarget, IdentityRepo, LocationProof, NewFileLocation,
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
    /// Round-7 finding #1: post-trip-wire DB mutation (identity
    /// dispatch / intent completion / event append) failed AFTER the
    /// caller had already performed the durable filesystem mutation.
    /// Phase C wraps that block in a SAVEPOINT; on Err the savepoint
    /// rolls back and the outer tx transitions the intent to
    /// `recovery_required` with `recovery_reason = 'mutation_failed'`.
    /// `error` carries the inner error's diagnostic string so the
    /// caller has enough context for the recovery worker / audit.
    BlockedByMutationFailed { error: String },
}

/// Input to `prepare_destructive_commit`. `override_token` carries the
/// optional force-path bypass token (commit 10); `None` is the default
/// gate-respecting path that aborts on any closure-walk
/// `AliasResolutionError::Unreachable`. `Some(token)` after
/// `validate_bypass` passes drives the closure-incomplete bypass branch
/// (see `prepare_destructive_commit` / `authorize_destructive_commit`).
/// The token JSON is persisted to `commit_intents.override_token`
/// atomically with the `commit.intent_recorded` insert so Phase B can
/// re-read it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DestructiveCommit {
    pub target: CommitTarget,
    pub accepted_evidence_ids: Vec<EvidenceId>,
    pub override_token: Option<ForcePathToken>,
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

/// Validate a force-path token's bypass set before any state change.
/// Sprint 1 ships exactly one sanctioned `BypassKind` variant
/// (`ClosureIncomplete`); any other bit is rejected with
/// `VoomError::Config("force-path bypass not supported: <name>")`.
///
/// The single-variant `BypassKind` enum makes "any other bit" currently
/// unrepresentable at the type level, so this function is presently a
/// no-op for every constructible token. It exists as the forward-compat
/// gate: when Sprint 5+ adds new bypass kinds, the new variants flow
/// through here and the validator decides which ones the gate accepts.
/// Wiring it into `prepare_destructive_commit` ahead of the new variants
/// (rather than alongside them) avoids a parallel review burden.
///
/// # Errors
///
/// `VoomError::Config` with a descriptive message naming the
/// unsupported `BypassKind` tag.
pub fn validate_bypass(token: &ForcePathToken) -> Result<(), VoomError> {
    for kind in &token.bypass {
        match kind {
            BypassKind::ClosureIncomplete => {}
        }
    }
    Ok(())
}

/// `snake_case` wire-format tag for one `BypassKind`. Matches the
/// `#[serde(rename_all = "snake_case")]` on the enum so the
/// `commit.forced_override` payload's `bypass` array shape carries
/// the same vocabulary the JSON column write does.
fn bypass_kind_str(k: BypassKind) -> &'static str {
    match k {
        BypassKind::ClosureIncomplete => "closure_incomplete",
    }
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
    /// Round-7 finding #3: another in-flight `commit_intents` row
    /// (`state IN ('pending','authorized')`) already covers a scope
    /// member of the new commit's `closure_initial`. Carries the
    /// existing commit's `commit_id` and the offending scope so the
    /// blocked caller can wait / take-over without re-querying.
    PendingCommit {
        pending_commit_id: CommitId,
        offending_scope: LeaseScope,
    },
}

impl PhaseAAbort {
    fn abort_reason(&self) -> AbortReason {
        match self {
            Self::UseLease { .. } => AbortReason::FreshLease,
            Self::StaleEvidence { .. } => AbortReason::StaleEvidence,
            Self::ClosureIncomplete { .. } => AbortReason::ClosureIncomplete,
            // The new pending-commit abort reuses `AbortReason::Other`
            // because the existing variant set is closed (round-2 fix);
            // the `"pending_commit"` string is the durable column value.
            Self::PendingCommit { .. } => AbortReason::Other("pending_commit".to_owned()),
        }
    }

    fn abort_reason_str(&self) -> &'static str {
        match self {
            Self::UseLease { .. } => "fresh_lease",
            Self::StaleEvidence { .. } => "stale_evidence",
            Self::ClosureIncomplete { .. } => "closure_incomplete",
            Self::PendingCommit { .. } => "pending_commit",
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
            Self::PendingCommit {
                pending_commit_id,
                offending_scope,
            } => CommitGateResult::BlockedByPendingCommit {
                commit_id: pending_commit_id,
                offending_scope,
            },
        }
    }
}

/// Open a commit-safety-gate transaction with `BEGIN IMMEDIATE` so
/// `SQLite` takes a RESERVED lock at tx start (round-8 finding #2).
/// Every gate entry point — `prepare_destructive_commit`,
/// `authorize_destructive_commit`, `finalize_destructive_commit`,
/// `abort_destructive_commit` — plus the two-tx helper
/// `phase_a_gate_abort_with_event` routes through this function.
///
/// The Phase 2 spec ("one IMMEDIATE transaction") is now enforced at
/// the API boundary: with `pool.begin()` (deferred mode), two
/// concurrent prepares on overlapping scope could both read "no
/// overlap" before either inserted `scope_members` rows, racing through
/// the in-tx overlapping-prepare consult. RESERVED-on-BEGIN forces
/// the second writer to either wait on `busy_timeout` or receive
/// `SQLITE_BUSY` — the duplicate-pending-rows outcome becomes
/// impossible at the lock layer rather than relying solely on a
/// re-check inside the deferred tx.
///
/// # Errors
///
/// Returns `VoomError::Database` if acquiring the pool connection or
/// emitting `BEGIN IMMEDIATE` fails.
pub(crate) async fn begin_gate_tx(
    pool: &SqlitePool,
) -> Result<Transaction<'static, Sqlite>, VoomError> {
    pool.begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(|e| VoomError::Database(format!("gate tx begin IMMEDIATE: {e}")))
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
    // two-tx: tx 1 inserts the aborted row. Round-8 finding #2: both
    // legs of the two-tx pattern route through `begin_gate_tx` so the
    // gate's BEGIN IMMEDIATE invariant holds even on the abort path.
    let started_iso = iso8601(row.started_at)?;
    let aborted_iso = iso8601(aborted_at)?;
    let mut tx1 = begin_gate_tx(pool).await?;
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
    let mut tx2 = begin_gate_tx(pool).await?;
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
        PhaseAAbort::PendingCommit {
            pending_commit_id,
            offending_scope,
        } => Event::CommitAbortedByPendingCommit(CommitAbortedByPendingCommitPayload {
            commit_id,
            pending_commit_id: *pending_commit_id,
            scope_type: offending_scope.type_str().to_owned(),
            scope_id: offending_scope.id_u64(),
            phase: "prepare".to_owned(),
            aborted_at,
        }),
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

/// JSON-encode a `ForcePathToken` for the
/// `commit_intents.override_token` column. Uses the struct's derived
/// serde — `actor` / `reason` are plain strings; `bypass: BTreeSet<BypassKind>`
/// serializes as an ordered JSON array of `snake_case` tags
/// (`["closure_incomplete"]` in Sprint 1). The decoder
/// (`decode_force_path_token`) is the inverse.
fn encode_force_path_token(token: &ForcePathToken) -> Result<String, VoomError> {
    serde_json::to_string(token)
        .map_err(|e| VoomError::Internal(format!("encode override_token: {e}")))
}

/// Inverse of `encode_force_path_token`. The
/// `commit_intents.override_token` column is written exclusively by
/// `prepare_destructive_commit` (commit 10) and never mutated; a parse
/// failure is `VoomError::Database` because that's the on-disk
/// corruption case.
fn decode_force_path_token(json: &str) -> Result<ForcePathToken, VoomError> {
    serde_json::from_str(json)
        .map_err(|e| VoomError::Database(format!("decode override_token: {e}")))
}

// ----- inverse wire-format decoders (commit 6) ------------------------------
//
// Phase B reads back the JSON columns that Phase A wrote (`target`,
// `closure_initial`) so it can re-emit closure-grew payloads and surface
// state through `PendingCommitIntent`. The decoders mirror the encoder
// shapes exactly — they are deliberately module-private so the on-disk
// JSON contract has a single owning module.

fn decode_target(json: &str) -> Result<CommitTarget, VoomError> {
    let wire: CommitTargetWire = serde_json::from_str(json)
        .map_err(|e| VoomError::Database(format!("decode commit_target: {e}")))?;
    commit_target_from_wire(wire)
}

fn decode_closure(json: &str) -> Result<AffectedScopeClosure, VoomError> {
    let wire: AffectedScopeClosureWire = serde_json::from_str(json)
        .map_err(|e| VoomError::Database(format!("decode closure: {e}")))?;
    Ok(closure_from_wire(wire))
}

fn commit_target_from_wire(w: CommitTargetWire) -> Result<CommitTarget, VoomError> {
    Ok(match w {
        CommitTargetWire::Delete { retired } => CommitTarget::DeleteFileLocation(retired),
        CommitTargetWire::Replace { retired, new } => CommitTarget::ReplaceFileLocation {
            retired,
            new: file_location_proposal_from_wire(new)?,
        },
        CommitTargetWire::Move { retired, new } => CommitTarget::MoveFileLocation {
            retired,
            new: file_location_proposal_from_wire(new)?,
        },
    })
}

fn file_location_proposal_from_wire(
    w: FileLocationProposalWire,
) -> Result<FileLocationProposal, VoomError> {
    let proof = decode_proof(w.proof_kind.as_deref(), w.proof_value.as_deref())?;
    Ok(FileLocationProposal {
        kind: FileLocationKind::parse(&w.kind)?,
        value: w.value,
        proof,
        observed_at: w.observed_at,
    })
}

fn decode_proof(
    kind: Option<&str>,
    value: Option<&str>,
) -> Result<Option<LocationProof>, VoomError> {
    let (Some(kind), Some(value)) = (kind, value) else {
        return Ok(None);
    };
    let parsed: JsonValue = serde_json::from_str(value)
        .map_err(|e| VoomError::Database(format!("decode proof_value: {e}")))?;
    match kind {
        "file_id_generation" => {
            let file_id = parsed
                .get("file_id")
                .and_then(JsonValue::as_str)
                .ok_or_else(|| VoomError::Database("decode proof: missing file_id".to_owned()))?
                .parse::<u128>()
                .map_err(|e| VoomError::Database(format!("decode proof: file_id u128: {e}")))?;
            let generation = parsed
                .get("generation")
                .and_then(JsonValue::as_u64)
                .ok_or_else(|| {
                    VoomError::Database("decode proof: missing generation".to_owned())
                })?;
            Ok(Some(LocationProof::LocalFileIdGeneration {
                file_id,
                generation,
            }))
        }
        "object_version_id" => {
            let bucket = parsed
                .get("bucket")
                .and_then(JsonValue::as_str)
                .ok_or_else(|| VoomError::Database("decode proof: missing bucket".to_owned()))?
                .to_owned();
            let key = parsed
                .get("key")
                .and_then(JsonValue::as_str)
                .ok_or_else(|| VoomError::Database("decode proof: missing key".to_owned()))?
                .to_owned();
            let version_id = parsed
                .get("version_id")
                .and_then(JsonValue::as_str)
                .ok_or_else(|| VoomError::Database("decode proof: missing version_id".to_owned()))?
                .to_owned();
            Ok(Some(LocationProof::ObjectStoreVersion {
                bucket,
                key,
                version_id,
            }))
        }
        other => Err(VoomError::Database(format!(
            "decode proof: unknown kind {other:?}"
        ))),
    }
}

fn closure_from_wire(w: AffectedScopeClosureWire) -> AffectedScopeClosure {
    AffectedScopeClosure {
        file_assets: w.file_assets,
        file_versions: w.file_versions,
        file_locations: w.file_locations,
        bundles: w.bundles,
        resolution_warnings: w
            .resolution_warnings
            .into_iter()
            .map(|w| ClosureWarning { message: w.message })
            .collect(),
    }
}

// ----- per-member epoch snapshot wire format (commit 6) ---------------------
//
// `commit_intents.target_row_epochs` is a JSON array of [kind, row_id,
// epoch] triples (sprint plan §3 "Per-member epoch guard"). Phase B
// writes it atomically with `state='authorized'`; Phase C re-reads it
// (commit 7) and uses each `epoch` as the `expected_epoch` argument to
// the matching `IdentityRepo` destructive mutation. `kind` round-trips
// through `TargetMemberKind`'s `Serialize/Deserialize` impl
// (`#[serde(rename_all = "snake_case")]`).

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TargetRowEpochTriple(TargetMemberKind, u64, u64);

fn encode_target_row_epochs(triples: &[TargetRowEpochTriple]) -> Result<String, VoomError> {
    serde_json::to_string(triples)
        .map_err(|e| VoomError::Internal(format!("encode target_row_epochs: {e}")))
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
/// `input.override_token` is the sanctioned force-path bypass (commit
/// 10). `None` (the default) routes any `AliasResolutionError::Unreachable`
/// from the closure walker straight to `BlockedByClosureIncomplete`.
/// `Some(token)` after `validate_bypass` accepts the token funnels the
/// matching `Unreachable` into the bypass branch (the closure walk
/// proceeds with whatever DB-internal aliases were already enumerated;
/// the external resolver's contribution is lost). The token JSON is
/// persisted to `commit_intents.override_token` atomically with the
/// `commit.intent_recorded` insert; `commit.forced_override` is emitted
/// once at prepare time (authorize does not re-emit). The audit signal
/// and the bypass logic ship together in this same tx — no in-tree
/// caller has access to a bypass branch without the matching audit row.
///
/// # Errors
///
/// `VoomError::Config` if `input.override_token = Some(token)` and the
/// token's bypass set contains an unsupported `BypassKind` (validation
/// runs before any tx opens; no row materializes). `VoomError::Database`
/// / `VoomError::Internal` on storage failures (including
/// `AliasResolutionError::Database` from an external alias source).
/// Gate-check failures return `Ok(PrepareOutcome::Blocked)` rather than
/// `Err` — `Err` is reserved for genuine storage failures that the
/// caller cannot reason about.
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
        override_token,
    } = input;

    // Validate the token before opening any tx — an invalid bypass bit
    // never lands a commit_intents row.
    if let Some(token) = &override_token {
        validate_bypass(token)?;
    }

    let target_json = encode_target(&target)?;
    let accepted_evidence_ids_json = encode_evidence_ids(&accepted_evidence_ids)?;
    let override_token_json = match &override_token {
        None => None,
        Some(token) => Some(encode_force_path_token(token)?),
    };
    let bypass_set: BTreeSet<BypassKind> = override_token
        .as_ref()
        .map(|t| t.bypass.clone())
        .unwrap_or_default();

    let mut tx = begin_gate_tx(pool).await?;

    let walk_outcome = run_phase_a_gate_in_tx(
        &mut tx,
        identity_repo,
        alias_resolver,
        &target,
        &accepted_evidence_ids,
        &bypass_set,
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
        override_token_json.as_deref(),
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
    if let Some(token) = &override_token {
        emit_forced_override(event_repo, &mut tx, commit_id, token, now).await?;
    }
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

/// Which gate phase is driving the closure walk. The walker's
/// precondition checks differ between Phase A (which surfaces
/// stale-target handles as `ClosureIncomplete`) and Phase B (which
/// treats the same condition as drift and lets the recompute fall
/// through to the closure-grew trip-wire).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GatePhase {
    Prepare,
    Authorize,
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
/// failure the caller cannot reason about. `bypass` carries the
/// caller's sanctioned `BypassKind` set — `ClosureIncomplete` here
/// silences the `Unreachable` abort path and lets the walk proceed
/// with the DB-internal closure (commit 10).
async fn run_phase_a_gate_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    identity_repo: &dyn IdentityRepo,
    alias_resolver: &dyn AliasResolver,
    target: &CommitTarget,
    accepted_evidence_ids: &[EvidenceId],
    bypass: &BTreeSet<BypassKind>,
) -> Result<Result<GateWalkOk, GateWalkAbort>, VoomError> {
    // Step 1: closure walk on the gate's IMMEDIATE tx.
    let closure = match build_closure(
        tx,
        identity_repo,
        alias_resolver,
        target,
        GatePhase::Prepare,
        bypass,
    )
    .await?
    {
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

    // Step 4 (round-7): overlapping-prepare check. Consult the
    // pending-commit lock for every scope member of `closure` BEFORE
    // landing the new `pending` row. Without this, two operators
    // preparing destructive commits on overlapping scope both end up
    // with `pending` (and later `authorized`) intents. Iterate from
    // fine-grained to coarse (location → version → bundle → asset) so
    // the most specific offending scope wins the report. First match
    // aborts via the two-tx pattern; the caller turns it into
    // `BlockedByPendingCommit { commit_id, offending_scope }`.
    if let Some((pending_commit_id, offending_scope)) =
        first_pending_commit_overlap_in_tx(tx, &closure).await?
    {
        return Ok(Err(GateWalkAbort {
            closure_initial: closure,
            abort: PhaseAAbort::PendingCommit {
                pending_commit_id,
                offending_scope,
            },
        }));
    }

    Ok(Ok(GateWalkOk {
        closure,
        evaluated_lease_ids,
        revalidated_evidence,
    }))
}

/// First overlap between an in-flight `commit_intents` row
/// (`state IN ('pending','authorized')`) and `closure`. Probed via
/// `consult_pending_commit_lock_in_tx` for every member of the closure,
/// ordered from finest to coarsest granularity so the most specific
/// offending scope wins the report. Returns the first hit as
/// `(pending_commit_id, offending_scope)` or `None` if no in-flight
/// commit covers any member.
async fn first_pending_commit_overlap_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    closure: &AffectedScopeClosure,
) -> Result<Option<(CommitId, LeaseScope)>, VoomError> {
    for id in &closure.file_locations {
        let scope = LeaseScope::Location(*id);
        if let Some(hit) = consult_pending_commit_lock_in_tx(tx, &scope).await? {
            return Ok(Some(hit));
        }
    }
    for id in &closure.file_versions {
        let scope = LeaseScope::Version(*id);
        if let Some(hit) = consult_pending_commit_lock_in_tx(tx, &scope).await? {
            return Ok(Some(hit));
        }
    }
    for id in &closure.bundles {
        let scope = LeaseScope::Bundle(*id);
        if let Some(hit) = consult_pending_commit_lock_in_tx(tx, &scope).await? {
            return Ok(Some(hit));
        }
    }
    for id in &closure.file_assets {
        let scope = LeaseScope::Asset(*id);
        if let Some(hit) = consult_pending_commit_lock_in_tx(tx, &scope).await? {
            return Ok(Some(hit));
        }
    }
    Ok(None)
}

async fn insert_pending_intent(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    target_json: &str,
    closure_initial_json: &str,
    accepted_evidence_ids_json: &str,
    override_token_json: Option<&str>,
    started_at: OffsetDateTime,
) -> Result<CommitId, VoomError> {
    let started_iso = iso8601(started_at)?;
    let res = sqlx::query(
        "INSERT INTO commit_intents \
         (target, closure_initial, accepted_evidence_ids, override_token, state, started_at) \
         VALUES (?, ?, ?, ?, 'pending', ?)",
    )
    .bind(target_json)
    .bind(closure_initial_json)
    .bind(accepted_evidence_ids_json)
    .bind(override_token_json)
    .bind(&started_iso)
    .execute(&mut **tx)
    .await
    .map_err(|e| VoomError::Database(format!("commit_intents pending insert: {e}")))?;
    Ok(CommitId(u64_from_i64(res.last_insert_rowid())))
}

/// Emit `commit.forced_override` once at prepare time, atomically
/// with the `commit.intent_recorded` insert and the
/// `commit_intents.override_token` column write. Authorize does not
/// re-emit — the audit signal is single-shot per commit. The payload
/// carries every `BypassKind` bit the operator supplied as
/// `snake_case` strings; Sprint 1 ships exactly `"closure_incomplete"`.
async fn emit_forced_override(
    event_repo: &dyn EventRepo,
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    commit_id: CommitId,
    token: &ForcePathToken,
    recorded_at: OffsetDateTime,
) -> Result<(), VoomError> {
    let bypass: Vec<String> = token
        .bypass
        .iter()
        .map(|k| bypass_kind_str(*k).to_owned())
        .collect();
    let payload = Event::CommitForcedOverride(CommitForcedOverridePayload {
        commit_id,
        actor: token.actor.clone(),
        reason: token.reason.clone(),
        bypass,
        recorded_at,
    });
    event_repo
        .append_in_tx(
            tx,
            EventEnvelope {
                occurred_at: recorded_at,
                subject_type: SubjectType::CommitIntent,
                subject_id: Some(commit_id.0),
                trace_id: None,
                payload,
            },
        )
        .await?;
    Ok(())
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
///
/// `bypass` is the active force-path bypass set (commit 10). When it
/// contains `BypassKind::ClosureIncomplete`, an
/// `AliasResolutionError::Unreachable` from the external resolver is
/// swallowed: the walk proceeds with whatever DB-internal aliases were
/// already enumerated rather than aborting. Phase C does not pipe this
/// flag through (the bypass is consumed once at prepare and re-applied
/// at authorize; Phase C's `Authorize` walker never receives it because
/// the closure walker only surfaces `Unreachable` when a fresh
/// `AliasResolutionError::Unreachable` fires — see `run_phase_c_trip_wires_in_tx`'s
/// internal-error escape on closure-incomplete at finalize).
async fn build_closure(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    identity_repo: &dyn IdentityRepo,
    alias_resolver: &dyn AliasResolver,
    target: &CommitTarget,
    phase: GatePhase,
    bypass: &BTreeSet<BypassKind>,
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
    // Phase A surfaces an already-retired target as
    // closure-incomplete (operator handed a stale handle; abort
    // eagerly so the audit row records the precondition trip). Phase B
    // is structurally different: a target that became retired between
    // prepare and authorize is closure drift, not closure-incomplete —
    // the recomputed closure simply loses the retired row and (often)
    // gains the rename-introduced replacement, surfacing as
    // `BlockedByClosureGrew` further down. The Phase-A trip-wire would
    // mask the drift signal e2e callers depend on, so it stays
    // Phase-A-gated.
    if phase == GatePhase::Prepare && location.retired_at.is_some() {
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
            // Force-path bypass: a token carrying
            // `BypassKind::ClosureIncomplete` suppresses the abort.
            // The walk continues with the partial closure (the
            // external resolver's contribution is lost; the
            // DB-internal `live_locations` already in hand are the
            // best evidence the gate has). The bypass is recorded
            // separately via `commit.forced_override` — the absence
            // of `commit.aborted_by_closure_incomplete` is the
            // visible difference in the audit trail.
            if bypass.contains(&BypassKind::ClosureIncomplete) {
                alias_warnings.push(ClosureWarning {
                    message: format!("force-path bypass honored: {message}"),
                });
            } else {
                return Ok(Err(PhaseAAbort::ClosureIncomplete { message }));
            }
        }
        Err(AliasResolutionError::Database(msg)) => {
            return Err(VoomError::Database(format!("alias resolver: {msg}")));
        }
    }

    let mut file_locations = live_locations;
    for id in external_locations {
        file_locations.insert(id);
    }
    // Phase A guards against the target already being terminal upstream,
    // so a non-terminal target is always live and already present in
    // `live_locations`; the defense-in-depth insert here keeps the
    // invariant explicit (no-op if the row is live; pins the target
    // member when the live-listing query and the target's row state
    // diverge mid-walk). Phase B is structurally different: a retired
    // target should fall OUT of the closure (closure drift signal), so
    // re-adding it here would mask the trip-wire.
    if phase == GatePhase::Prepare {
        file_locations.insert(retired_location_id);
    }

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

    // Warnings stay empty unless the force-path bypass swallowed an
    // `Unreachable` (commit 10) — in which case the dropped resolver
    // message rides along on the closure as a non-fatal annotation.
    // Round-3 invariant: warnings do NOT contribute to closure drift
    // (`id_member_delta` ignores them), so the bypass-introduced
    // warning cannot mask the Phase B closure-grew trip-wire.
    let closure = AffectedScopeClosure {
        file_assets,
        file_versions,
        file_locations: file_locations.clone(),
        bundles,
        resolution_warnings: std::mem::take(&mut alias_warnings),
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

// ============================================================================
// Phase B entry point — `authorize_destructive_commit`
// ============================================================================

/// Disposition of an `authorize_destructive_commit` call.
///
/// `Authorized` carries the opaque `CommitPermit` — the intent row landed
/// in `state = 'authorized'` with `closure_authorized` and
/// `target_row_epochs` persisted atomically. `Blocked` carries the
/// Phase B abort outcome — a `commit_intents` row was transitioned to
/// `aborted` with `commit_id` referring to that row, and the matching
/// `commit.aborted_by_*` event row sits alongside it in `events`. Phase B
/// commits the abort in-tx (no two-tx pattern; that pattern is reserved
/// for Phase A gate-check aborts only).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthorizeOutcome {
    Authorized(CommitPermit),
    Blocked {
        commit_id: CommitId,
        result: CommitGateResult,
    },
}

/// Phase B of the destructive-commit gate — sub-slice 6 of the M3
/// Phase 2 plan. Reads the `commit_intents` row in `state = 'pending'`,
/// recomputes the affected-scope closure against current DB state,
/// runs the three Phase B trip-wires (closure drift, fresh blocking
/// lease, accepted-evidence drift), snapshots per-member epochs into
/// the `target_row_epochs` JSON column, and transitions the row to
/// `state = 'authorized'`. All work runs inside one IMMEDIATE tx —
/// Phase B aborts in-tx (no two-tx pattern; sequencing doc §5.2).
///
/// `alias_resolver` covers **external** (non-DB) alias sources only.
/// DB-internal alias enumeration goes through
/// `IdentityRepo::list_live_file_locations_by_version_in_tx` on the
/// gate's tx handle (round-5 fix).
///
/// On success the durable row state is:
/// - `state = 'authorized'`
/// - `closure_authorized` = JSON-encoded recomputed closure
/// - `target_row_epochs` = JSON array of `[kind, row_id, epoch]`
///   triples covering every member of the authorized closure
/// - `authorized_at` = `now`, `epoch` bumped
///
/// The returned `CommitPermit` carries the same `commit_id`, the
/// authorized closure, the lease IDs evaluated against it, the
/// evidence revalidation results, and the row's post-update `epoch`.
/// The per-member epoch snapshot is NOT carried on the permit — Phase C
/// re-reads it from `commit_intents.target_row_epochs` (commit 7).
///
/// # Errors
///
/// - `VoomError::Database` / `VoomError::Internal` on storage failures
///   (including `AliasResolutionError::Database` from an external
///   alias source).
/// - `VoomError::Conflict` if the row does not exist, is in a state
///   other than `pending`, or has had its `epoch` bumped between
///   `prepare` and `authorize` (race against a concurrent operator
///   action). Phase B trip-wires return `Ok(Blocked)` rather than
///   `Err` — `Err` is reserved for genuine storage failures and
///   precondition violations the caller cannot reason about.
pub async fn authorize_destructive_commit(
    pool: &SqlitePool,
    identity_repo: &dyn IdentityRepo,
    event_repo: &dyn EventRepo,
    alias_resolver: &dyn AliasResolver,
    commit_id: CommitId,
    now: OffsetDateTime,
) -> Result<AuthorizeOutcome, VoomError> {
    let mut tx = begin_gate_tx(pool).await?;

    let row = read_pending_intent_in_tx(&mut tx, commit_id).await?;
    let walk_outcome = run_phase_b_gate_in_tx(
        &mut tx,
        identity_repo,
        event_repo,
        alias_resolver,
        &row,
        now,
    )
    .await?;
    let walk = match walk_outcome {
        Ok(w) => w,
        Err(result) => {
            tx.commit()
                .await
                .map_err(|e| VoomError::Database(format!("authorize: commit abort: {e}")))?;
            return Ok(AuthorizeOutcome::Blocked { commit_id, result });
        }
    };

    let permit = finalize_phase_b_authorize_in_tx(
        &mut tx,
        event_repo,
        commit_id,
        row.epoch,
        &row.closure_initial,
        walk,
        now,
    )
    .await?;
    tx.commit()
        .await
        .map_err(|e| VoomError::Database(format!("authorize: commit success: {e}")))?;
    Ok(AuthorizeOutcome::Authorized(permit))
}

/// Phase B success-path state carried out of the trip-wire gate into
/// `finalize_phase_b_authorize_in_tx`. Holds the recomputed closure
/// plus the lease IDs and evidence revalidation results evaluated
/// against it (both for `CommitPermit` construction on success).
struct PhaseBGatePass {
    closure_authorized: AffectedScopeClosure,
    evaluated_lease_ids: Vec<UseLeaseId>,
    revalidated_evidence: Vec<EvidenceRevalidationResult>,
}

/// Run the three Phase B trip-wires (closure recompute / drift,
/// blocking-lease re-evaluation, accepted-evidence revalidation)
/// inside the open tx. `Ok(Ok(_))` on a passing walk (caller proceeds
/// to write the authorized row); `Ok(Err(_))` on a trip-wire abort
/// (the helper UPDATEs the row to `aborted` + emits the event in-tx;
/// caller commits the tx); `Err(_)` on a genuine storage failure.
async fn run_phase_b_gate_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    identity_repo: &dyn IdentityRepo,
    event_repo: &dyn EventRepo,
    alias_resolver: &dyn AliasResolver,
    row: &PendingIntentRow,
    now: OffsetDateTime,
) -> Result<Result<PhaseBGatePass, CommitGateResult>, VoomError> {
    // Re-apply the prepare-side bypass set. The token JSON was
    // validated at prepare time; we trust the persisted value here
    // (the column write was atomic with the `pending` insert and the
    // intervening row state cannot mutate it — only prepare writes
    // this column).
    let bypass: BTreeSet<BypassKind> = row
        .override_token
        .as_ref()
        .map(|t| t.bypass.clone())
        .unwrap_or_default();
    let closure_authorized = match build_closure(
        tx,
        identity_repo,
        alias_resolver,
        &row.target,
        GatePhase::Authorize,
        &bypass,
    )
    .await?
    {
        Ok((closure, _)) => closure,
        Err(PhaseAAbort::ClosureIncomplete { message }) => {
            let result = abort_pending_intent_in_tx(
                tx,
                event_repo,
                row,
                now,
                PhaseBAbort::ClosureIncomplete { message },
            )
            .await?;
            return Ok(Err(result));
        }
        Err(other) => {
            return Err(VoomError::Internal(format!(
                "authorize: unexpected closure-walk abort kind: {other:?}"
            )));
        }
    };

    let delta = row.closure_initial.id_member_delta(&closure_authorized);
    if !delta.is_empty() {
        let result = abort_pending_intent_in_tx(
            tx,
            event_repo,
            row,
            now,
            PhaseBAbort::ClosureGrew { delta },
        )
        .await?;
        return Ok(Err(result));
    }

    let evaluated_lease_ids = list_blocking_leases_in_tx(tx, &closure_authorized).await?;
    if let Some((lease_id, lease_scope)) =
        first_blocking_overlap_in_tx(tx, &closure_authorized).await?
    {
        let result = abort_pending_intent_in_tx(
            tx,
            event_repo,
            row,
            now,
            PhaseBAbort::UseLease {
                lease_id,
                lease_scope,
            },
        )
        .await?;
        return Ok(Err(result));
    }

    let revalidated_evidence =
        revalidate_evidence_in_tx(tx, identity_repo, &row.accepted_evidence_ids).await?;
    if let Some((evidence_id, drift)) = first_evidence_drift(&revalidated_evidence) {
        let drift = drift.clone();
        let result = abort_pending_intent_in_tx(
            tx,
            event_repo,
            row,
            now,
            PhaseBAbort::StaleEvidence { evidence_id, drift },
        )
        .await?;
        return Ok(Err(result));
    }

    Ok(Ok(PhaseBGatePass {
        closure_authorized,
        evaluated_lease_ids,
        revalidated_evidence,
    }))
}

/// Phase B success path inside the open tx: snapshot per-member
/// epochs, reconcile `scope_members`, transition the row to
/// `authorized`, and emit the `commit.authorized` event. Returns the
/// `CommitPermit` the caller surfaces on `AuthorizeOutcome::Authorized`.
async fn finalize_phase_b_authorize_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    event_repo: &dyn EventRepo,
    commit_id: CommitId,
    expected_epoch: u64,
    closure_initial: &AffectedScopeClosure,
    walk: PhaseBGatePass,
    now: OffsetDateTime,
) -> Result<CommitPermit, VoomError> {
    let PhaseBGatePass {
        closure_authorized,
        evaluated_lease_ids,
        revalidated_evidence,
    } = walk;
    let triples = snapshot_target_row_epochs_in_tx(tx, &closure_authorized).await?;
    let target_row_epochs_json = encode_target_row_epochs(&triples)?;
    let closure_authorized_json = encode_closure(&closure_authorized)?;
    reconcile_scope_members(tx, commit_id, closure_initial, &closure_authorized).await?;
    let new_epoch = transition_pending_to_authorized_in_tx(
        tx,
        commit_id,
        expected_epoch,
        &closure_authorized_json,
        &target_row_epochs_json,
        now,
    )
    .await?;
    emit_authorized_event(
        event_repo,
        tx,
        commit_id,
        &closure_authorized,
        u32::try_from(triples.len()).unwrap_or(u32::MAX),
        now,
    )
    .await?;
    Ok(CommitPermit {
        commit_id,
        authorized_at: now,
        closure_authorized,
        evaluated_lease_ids,
        revalidated_evidence,
        epoch: new_epoch,
    })
}

/// Snapshot of the durable `commit_intents` row body Phase B carries
/// across in-tx steps. Loaded once before the closure recompute so the
/// trip-wire branches all bind the same column values.
struct PendingIntentRow {
    commit_id: CommitId,
    target: CommitTarget,
    closure_initial: AffectedScopeClosure,
    accepted_evidence_ids: Vec<EvidenceId>,
    /// Decoded `commit_intents.override_token` JSON column. `None`
    /// when the column is NULL (default path); `Some(token)` when
    /// `prepare_destructive_commit` persisted a force-path token.
    /// Phase B re-applies the same `BypassKind` set the prepare-side
    /// walk used so the closure-incomplete bypass is honored
    /// identically across phases. Phase B does NOT re-emit
    /// `commit.forced_override` — the audit signal is single-shot per
    /// commit (recorded once at prepare).
    override_token: Option<ForcePathToken>,
    epoch: u64,
}

/// Read the `commit_intents` row for `commit_id`, require `state =
/// 'pending'`, decode the JSON columns, and return the in-memory shape
/// Phase B operates on. Any state other than `pending` is `Conflict` —
/// callers must `prepare` first.
async fn read_pending_intent_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    commit_id: CommitId,
) -> Result<PendingIntentRow, VoomError> {
    let row = sqlx::query(
        "SELECT state, target, closure_initial, accepted_evidence_ids, override_token, epoch \
         FROM commit_intents WHERE id = ?",
    )
    .bind(i64_from_u64(commit_id.0))
    .fetch_optional(&mut **tx)
    .await
    .map_err(|e| VoomError::Database(format!("authorize: read intent: {e}")))?;
    let row = row.ok_or_else(|| {
        VoomError::Conflict(format!(
            "authorize: commit_intents row {commit_id} not found"
        ))
    })?;
    let state: String = row
        .try_get("state")
        .map_err(|e| VoomError::Database(format!("authorize: read state: {e}")))?;
    if state != "pending" {
        return Err(VoomError::Conflict(format!(
            "authorize: commit_intents row {commit_id} is in state {state:?}, expected 'pending'"
        )));
    }
    let target_json: String = row
        .try_get("target")
        .map_err(|e| VoomError::Database(format!("authorize: read target: {e}")))?;
    let closure_initial_json: String = row
        .try_get("closure_initial")
        .map_err(|e| VoomError::Database(format!("authorize: read closure_initial: {e}")))?;
    let accepted_evidence_ids_json: String = row
        .try_get("accepted_evidence_ids")
        .map_err(|e| VoomError::Database(format!("authorize: read accepted_evidence_ids: {e}")))?;
    let override_token_json: Option<String> = row
        .try_get("override_token")
        .map_err(|e| VoomError::Database(format!("authorize: read override_token: {e}")))?;
    let epoch_raw: i64 = row
        .try_get("epoch")
        .map_err(|e| VoomError::Database(format!("authorize: read epoch: {e}")))?;
    let target = decode_target(&target_json)?;
    let closure_initial = decode_closure(&closure_initial_json)?;
    let accepted_evidence_ids: Vec<EvidenceId> = serde_json::from_str(&accepted_evidence_ids_json)
        .map_err(|e| {
            VoomError::Database(format!("authorize: decode accepted_evidence_ids: {e}"))
        })?;
    let override_token = match override_token_json {
        None => None,
        Some(json) => Some(decode_force_path_token(&json)?),
    };
    Ok(PendingIntentRow {
        commit_id,
        target,
        closure_initial,
        accepted_evidence_ids,
        override_token,
        epoch: u64_from_i64(epoch_raw),
    })
}

/// Phase B trip-wire bundle. Each variant carries the data needed to
/// drive both the durable row transition (`abort_reason` column) and
/// the matching `commit.aborted_by_*` event payload.
#[derive(Debug, Clone)]
enum PhaseBAbort {
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
    ClosureGrew {
        delta: ClosureMemberDelta,
    },
}

impl PhaseBAbort {
    fn abort_reason_str(&self) -> &'static str {
        match self {
            Self::UseLease { .. } => "fresh_lease",
            Self::StaleEvidence { .. } => "stale_evidence",
            Self::ClosureIncomplete { .. } => "closure_incomplete",
            Self::ClosureGrew { .. } => "closure_grew",
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
            Self::ClosureGrew { delta } => CommitGateResult::BlockedByClosureGrew { delta },
        }
    }
}

/// Abort a `pending` intent in-tx: UPDATE to `state='aborted'` +
/// `abort_reason`, emit the matching event, and return the
/// `CommitGateResult` the caller surfaces to its consumer. Does NOT
/// commit — the caller commits the tx once. Phase B's in-tx abort
/// pattern is deliberately distinct from Phase A's two-tx helper
/// (sequencing doc §5.2: the two-tx pattern is reserved for Phase A
/// gate-check aborts that fire BEFORE a `pending` row would have
/// landed).
async fn abort_pending_intent_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    event_repo: &dyn EventRepo,
    row: &PendingIntentRow,
    aborted_at: OffsetDateTime,
    abort: PhaseBAbort,
) -> Result<CommitGateResult, VoomError> {
    let aborted_iso = iso8601(aborted_at)?;
    let reason_str = abort.abort_reason_str();
    let commit_id = row.commit_id;
    let res = sqlx::query(
        "UPDATE commit_intents SET state = 'aborted', aborted_at = ?, \
            abort_reason = ?, epoch = epoch + 1 \
         WHERE id = ? AND state = 'pending' AND epoch = ?",
    )
    .bind(&aborted_iso)
    .bind(reason_str)
    .bind(i64_from_u64(commit_id.0))
    .bind(i64_from_u64(row.epoch))
    .execute(&mut **tx)
    .await
    .map_err(|e| VoomError::Database(format!("authorize: abort UPDATE: {e}")))?;
    if res.rows_affected() != 1 {
        return Err(VoomError::Conflict(format!(
            "authorize: abort UPDATE on {commit_id} affected {} rows; concurrent state mutation",
            res.rows_affected()
        )));
    }

    emit_phase_b_abort_event(event_repo, tx, commit_id, &abort, aborted_at).await?;
    Ok(abort.into_gate_result())
}

fn phase_b_abort_event(
    commit_id: CommitId,
    aborted_at: OffsetDateTime,
    abort: &PhaseBAbort,
) -> Event {
    match abort {
        PhaseBAbort::UseLease {
            lease_id,
            lease_scope,
        } => Event::CommitAbortedByUseLease(CommitAbortedByUseLeasePayload {
            commit_id,
            lease_id: *lease_id,
            lease_scope_type: lease_scope.type_str().to_owned(),
            lease_scope_id: lease_scope.id_u64(),
            phase: "authorize".to_owned(),
            aborted_at,
        }),
        PhaseBAbort::StaleEvidence { evidence_id, drift } => {
            Event::CommitAbortedByStaleEvidence(CommitAbortedByStaleEvidencePayload {
                commit_id,
                evidence_id: *evidence_id,
                drift_kind: evidence_drift_str(drift).to_owned(),
                phase: "authorize".to_owned(),
                aborted_at,
            })
        }
        PhaseBAbort::ClosureIncomplete { message } => {
            Event::CommitAbortedByClosureIncomplete(CommitAbortedByClosureIncompletePayload {
                commit_id,
                phase: "authorize".to_owned(),
                message: message.clone(),
                aborted_at,
            })
        }
        PhaseBAbort::ClosureGrew { delta } => {
            Event::CommitAbortedByClosureGrew(CommitAbortedByClosureGrewPayload {
                commit_id,
                added_asset_count: u32::try_from(delta.added_assets.len()).unwrap_or(u32::MAX),
                added_bundle_count: u32::try_from(delta.added_bundles.len()).unwrap_or(u32::MAX),
                added_version_count: u32::try_from(delta.added_versions.len()).unwrap_or(u32::MAX),
                added_location_count: u32::try_from(delta.added_locations.len())
                    .unwrap_or(u32::MAX),
                removed_asset_count: u32::try_from(delta.removed_assets.len()).unwrap_or(u32::MAX),
                removed_bundle_count: u32::try_from(delta.removed_bundles.len())
                    .unwrap_or(u32::MAX),
                removed_version_count: u32::try_from(delta.removed_versions.len())
                    .unwrap_or(u32::MAX),
                removed_location_count: u32::try_from(delta.removed_locations.len())
                    .unwrap_or(u32::MAX),
                phase: "authorize".to_owned(),
                aborted_at,
            })
        }
    }
}

async fn emit_phase_b_abort_event(
    event_repo: &dyn EventRepo,
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    commit_id: CommitId,
    abort: &PhaseBAbort,
    aborted_at: OffsetDateTime,
) -> Result<(), VoomError> {
    let payload = phase_b_abort_event(commit_id, aborted_at, abort);
    event_repo
        .append_in_tx(
            tx,
            EventEnvelope {
                occurred_at: aborted_at,
                subject_type: SubjectType::CommitIntent,
                subject_id: Some(commit_id.0),
                trace_id: None,
                payload,
            },
        )
        .await?;
    Ok(())
}

/// Snapshot per-member epochs for every member of `closure` inside the
/// gate's IMMEDIATE tx. Returns the `[kind, row_id, epoch]` triples
/// Phase B writes atomically to `commit_intents.target_row_epochs`.
/// One SELECT per granularity; the granularity-tagged result is the
/// authoritative source Phase C re-reads (commit 7).
async fn snapshot_target_row_epochs_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    closure: &AffectedScopeClosure,
) -> Result<Vec<TargetRowEpochTriple>, VoomError> {
    let mut triples: Vec<TargetRowEpochTriple> = Vec::new();
    let asset_ids: Vec<i64> = closure
        .file_assets
        .iter()
        .map(|id| i64_from_u64(id.0))
        .collect();
    snapshot_one_granularity_in_tx(
        tx,
        "file_assets",
        TargetMemberKind::FileAsset,
        &asset_ids,
        &mut triples,
    )
    .await?;
    let version_ids: Vec<i64> = closure
        .file_versions
        .iter()
        .map(|id| i64_from_u64(id.0))
        .collect();
    snapshot_one_granularity_in_tx(
        tx,
        "file_versions",
        TargetMemberKind::FileVersion,
        &version_ids,
        &mut triples,
    )
    .await?;
    let location_ids: Vec<i64> = closure
        .file_locations
        .iter()
        .map(|id| i64_from_u64(id.0))
        .collect();
    snapshot_one_granularity_in_tx(
        tx,
        "file_locations",
        TargetMemberKind::FileLocation,
        &location_ids,
        &mut triples,
    )
    .await?;
    let bundle_ids: Vec<i64> = closure
        .bundles
        .iter()
        .map(|id| i64_from_u64(id.0))
        .collect();
    snapshot_one_granularity_in_tx(
        tx,
        "asset_bundles",
        TargetMemberKind::Bundle,
        &bundle_ids,
        &mut triples,
    )
    .await?;
    Ok(triples)
}

async fn snapshot_one_granularity_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    table: &'static str,
    kind: TargetMemberKind,
    ids: &[i64],
    out: &mut Vec<TargetRowEpochTriple>,
) -> Result<(), VoomError> {
    if ids.is_empty() {
        return Ok(());
    }
    let ids_json = serde_json::to_string(ids)
        .map_err(|e| VoomError::Internal(format!("encode {table} id snapshot: {e}")))?;
    // `table` is a static internal string — never caller-supplied — so
    // a `format!` SQL stitch is safe here (sqlx does not expose runtime
    // table-name binding).
    let sql = format!("SELECT id, epoch FROM {table} WHERE id IN (SELECT value FROM json_each(?))");
    let rows = sqlx::query(&sql)
        .bind(&ids_json)
        .fetch_all(&mut **tx)
        .await
        .map_err(|e| VoomError::Database(format!("snapshot {table}: {e}")))?;
    for row in &rows {
        let id: i64 = row
            .try_get("id")
            .map_err(|e| VoomError::Database(format!("snapshot {table} row id: {e}")))?;
        let epoch: i64 = row
            .try_get("epoch")
            .map_err(|e| VoomError::Database(format!("snapshot {table} row epoch: {e}")))?;
        out.push(TargetRowEpochTriple(
            kind,
            u64_from_i64(id),
            u64_from_i64(epoch),
        ));
    }
    Ok(())
}

/// Reconcile `commit_intent_scope_members` with the recomputed closure:
/// DELETE rows whose scope_*_id is no longer in the authorized closure,
/// INSERT new rows for added members. Compares the Phase A
/// `closure_initial` against the Phase B `closure_authorized` to derive
/// the delta — the row deletes and inserts are keyed off the four
/// granularity-specific delta sets so a no-op closure produces zero
/// writes.
async fn reconcile_scope_members(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    commit_id: CommitId,
    initial: &AffectedScopeClosure,
    authorized: &AffectedScopeClosure,
) -> Result<(), VoomError> {
    let cid = i64_from_u64(commit_id.0);
    // Removed members → DELETE matching scope_*_id rows.
    for asset in initial.file_assets.difference(&authorized.file_assets) {
        sqlx::query(
            "DELETE FROM commit_intent_scope_members \
             WHERE commit_intent_id = ? AND scope_asset_id = ?",
        )
        .bind(cid)
        .bind(i64_from_u64(asset.0))
        .execute(&mut **tx)
        .await
        .map_err(|e| VoomError::Database(format!("scope_members asset delete: {e}")))?;
    }
    for bundle in initial.bundles.difference(&authorized.bundles) {
        sqlx::query(
            "DELETE FROM commit_intent_scope_members \
             WHERE commit_intent_id = ? AND scope_bundle_id = ?",
        )
        .bind(cid)
        .bind(i64_from_u64(bundle.0))
        .execute(&mut **tx)
        .await
        .map_err(|e| VoomError::Database(format!("scope_members bundle delete: {e}")))?;
    }
    for version in initial.file_versions.difference(&authorized.file_versions) {
        sqlx::query(
            "DELETE FROM commit_intent_scope_members \
             WHERE commit_intent_id = ? AND scope_version_id = ?",
        )
        .bind(cid)
        .bind(i64_from_u64(version.0))
        .execute(&mut **tx)
        .await
        .map_err(|e| VoomError::Database(format!("scope_members version delete: {e}")))?;
    }
    for location in initial
        .file_locations
        .difference(&authorized.file_locations)
    {
        sqlx::query(
            "DELETE FROM commit_intent_scope_members \
             WHERE commit_intent_id = ? AND scope_location_id = ?",
        )
        .bind(cid)
        .bind(i64_from_u64(location.0))
        .execute(&mut **tx)
        .await
        .map_err(|e| VoomError::Database(format!("scope_members location delete: {e}")))?;
    }

    // Added members → INSERT new rows.
    for asset in authorized.file_assets.difference(&initial.file_assets) {
        sqlx::query(
            "INSERT INTO commit_intent_scope_members \
             (commit_intent_id, scope_asset_id) VALUES (?, ?)",
        )
        .bind(cid)
        .bind(i64_from_u64(asset.0))
        .execute(&mut **tx)
        .await
        .map_err(|e| VoomError::Database(format!("scope_members asset insert: {e}")))?;
    }
    for bundle in authorized.bundles.difference(&initial.bundles) {
        sqlx::query(
            "INSERT INTO commit_intent_scope_members \
             (commit_intent_id, scope_bundle_id) VALUES (?, ?)",
        )
        .bind(cid)
        .bind(i64_from_u64(bundle.0))
        .execute(&mut **tx)
        .await
        .map_err(|e| VoomError::Database(format!("scope_members bundle insert: {e}")))?;
    }
    for version in authorized.file_versions.difference(&initial.file_versions) {
        sqlx::query(
            "INSERT INTO commit_intent_scope_members \
             (commit_intent_id, scope_version_id) VALUES (?, ?)",
        )
        .bind(cid)
        .bind(i64_from_u64(version.0))
        .execute(&mut **tx)
        .await
        .map_err(|e| VoomError::Database(format!("scope_members version insert: {e}")))?;
    }
    for location in authorized
        .file_locations
        .difference(&initial.file_locations)
    {
        sqlx::query(
            "INSERT INTO commit_intent_scope_members \
             (commit_intent_id, scope_location_id) VALUES (?, ?)",
        )
        .bind(cid)
        .bind(i64_from_u64(location.0))
        .execute(&mut **tx)
        .await
        .map_err(|e| VoomError::Database(format!("scope_members location insert: {e}")))?;
    }
    Ok(())
}

/// Transition the `pending` row to `authorized`. Guards on
/// `(id, state='pending', epoch=row.epoch)` so a concurrent operator
/// action (abort, racing authorize) cannot land a half-written row.
/// Bumps the epoch and returns the new value the caller carries in
/// the returned `CommitPermit`.
async fn transition_pending_to_authorized_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    commit_id: CommitId,
    expected_epoch: u64,
    closure_authorized_json: &str,
    target_row_epochs_json: &str,
    authorized_at: OffsetDateTime,
) -> Result<u64, VoomError> {
    let authorized_iso = iso8601(authorized_at)?;
    let new_epoch = expected_epoch + 1;
    let res = sqlx::query(
        "UPDATE commit_intents SET \
            state = 'authorized', \
            closure_authorized = ?, \
            target_row_epochs = ?, \
            authorized_at = ?, \
            epoch = ? \
         WHERE id = ? AND state = 'pending' AND epoch = ?",
    )
    .bind(closure_authorized_json)
    .bind(target_row_epochs_json)
    .bind(&authorized_iso)
    .bind(i64_from_u64(new_epoch))
    .bind(i64_from_u64(commit_id.0))
    .bind(i64_from_u64(expected_epoch))
    .execute(&mut **tx)
    .await
    .map_err(|e| VoomError::Database(format!("authorize: UPDATE to authorized: {e}")))?;
    if res.rows_affected() != 1 {
        return Err(VoomError::Conflict(format!(
            "authorize: UPDATE to authorized on {commit_id} affected {} rows; \
             concurrent state mutation between read and write",
            res.rows_affected()
        )));
    }
    Ok(new_epoch)
}

async fn emit_authorized_event(
    event_repo: &dyn EventRepo,
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    commit_id: CommitId,
    closure: &AffectedScopeClosure,
    target_row_epoch_count: u32,
    authorized_at: OffsetDateTime,
) -> Result<(), VoomError> {
    let payload = Event::CommitAuthorized(CommitAuthorizedPayload {
        commit_id,
        closure_asset_count: u32::try_from(closure.file_assets.len()).unwrap_or(u32::MAX),
        closure_bundle_count: u32::try_from(closure.bundles.len()).unwrap_or(u32::MAX),
        closure_version_count: u32::try_from(closure.file_versions.len()).unwrap_or(u32::MAX),
        closure_location_count: u32::try_from(closure.file_locations.len()).unwrap_or(u32::MAX),
        target_row_epoch_count,
        authorized_at,
    });
    event_repo
        .append_in_tx(
            tx,
            EventEnvelope {
                occurred_at: authorized_at,
                subject_type: SubjectType::CommitIntent,
                subject_id: Some(commit_id.0),
                trace_id: None,
                payload,
            },
        )
        .await?;
    Ok(())
}

// ============================================================================
// Phase C entry point — `finalize_destructive_commit`
// ============================================================================

/// Disposition of a `finalize_destructive_commit` call. Mirrors the
/// shape of `PrepareOutcome` / `AuthorizeOutcome`: the durable
/// `commit_intents` row is in its terminal-for-this-call state by the
/// time the function returns.
///
/// `Completed` is the silent-path success — the durable identity
/// mutation has been applied in the same tx the row transitioned to
/// `state = 'completed'`. `CancelledAfterAuthorize` is the
/// `MutationOutcome::NotPerformed` branch: the row is in `state =
/// 'aborted'` with `abort_reason = 'operator_cancel'`; no durable
/// mutation ran. `Blocked` covers all four defensive trip-wire
/// branches plus the `BlockedByStaleTargetEpoch` per-member epoch
/// guard: the row is in `state = 'recovery_required'` with
/// `recovery_reason` set to the matching trip-wire tag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FinalizeOutcome {
    Completed(CommitGateOutcome),
    CancelledAfterAuthorize(CommitGateOutcome),
    Blocked(CommitGateOutcome),
}

/// Phase C of the destructive-commit gate — sub-slice 7 of the M3
/// Phase 2 plan. Re-reads the `commit_intents` row in
/// `state = 'authorized'`, validates the permit, optionally runs the
/// defensive trip-wires against the recomputed closure / leases /
/// per-member epochs, dispatches the durable identity mutation, and
/// transitions the row to `completed` (silent path) /
/// `recovery_required` (trip-wire) / `aborted` (`NotPerformed`). All
/// work runs inside one IMMEDIATE tx; the two-tx pattern is reserved
/// for Phase A gate-check aborts (sequencing doc §5.2).
///
/// `alias_resolver` covers **external** (non-DB) alias sources only;
/// DB-internal alias enumeration goes through
/// `IdentityRepo::list_live_file_locations_by_version_in_tx` on the
/// gate's tx handle (round-5 fix).
///
/// The four trip-wire sub-branches per sprint spec §9.3.2 Phase C
/// step 3, plus the per-member epoch guard added under §3:
/// - Closure grew/shifted (no fresh lease, no epoch drift) →
///   `recovery_reason = 'closure_grew'`, return `BlockedByClosureGrew`.
/// - Fresh blocking lease (empty closure delta, no epoch drift) →
///   `recovery_reason = 'fresh_lease'`, return `BlockedByUseLease`.
/// - Closure grew AND fresh lease (no epoch drift) →
///   `recovery_reason = 'closure_grew_and_fresh_lease'`, return
///   `BlockedByClosureGrew` (closure shift is the dominant signal;
///   the fresh-lease check would have been re-evaluated against the
///   wrong baseline anyway — spec §9.3.2).
/// - Stale target epoch (any member's current `epoch` differs from
///   the durable snapshot, regardless of the other two trip-wires)
///   → `recovery_reason = 'stale_target_epoch'`, return
///   `BlockedByStaleTargetEpoch { drift }`.
///
/// On the silent path, each target member's snapshotted `expected_epoch`
/// is sourced from the `commit_intents.target_row_epochs` JSON
/// snapshot Phase B wrote, decoded inside this same tx, and passed to
/// the matching `IdentityRepo` mutation:
/// `DeleteFileLocation` → `retire_file_location_in_tx`,
/// `ReplaceFileLocation` / `MoveFileLocation` →
/// `replace_file_location_in_tx`. The conversion of
/// `FileLocationProposal` → `NewFileLocation` happens here, in Phase C,
/// by reading the retired row's `file_version_id` inside the tx —
/// the gate boundary makes a cross-version target unrepresentable.
///
/// # Errors
///
/// - `VoomError::Database` / `VoomError::Internal` on storage failures
///   or invariant violations (e.g. a row in `state = 'authorized'`
///   with NULL `target_row_epochs`; migration 0005's CHECK prevents
///   this and Phase B is the sole writer of the column).
/// - `VoomError::Conflict` if the row does not exist, is in a state
///   other than `authorized`, or has had its `epoch` bumped between
///   `authorize` and `finalize` (stale permit). Defensive trip-wire
///   firings return `Ok(Blocked)` rather than `Err` — `Err` is
///   reserved for genuine storage failures and precondition violations
///   the caller cannot reason about.
pub async fn finalize_destructive_commit(
    pool: &SqlitePool,
    identity_repo: &dyn IdentityRepo,
    event_repo: &dyn EventRepo,
    alias_resolver: &dyn AliasResolver,
    permit: CommitPermit,
    outcome: MutationOutcome,
    now: OffsetDateTime,
) -> Result<FinalizeOutcome, VoomError> {
    let mut tx = begin_gate_tx(pool).await?;

    let row = read_authorized_intent_in_tx(&mut tx, permit.commit_id(), permit.epoch()).await?;

    // Round-7 finding #2: destructure Applied { observed }. The caller's
    // observed-alias set (if any) is merged with the recomputed
    // closure_final inside the trip-wire path so members the caller saw
    // but the resolver/DB did not surface drive `BlockedByClosureGrew`
    // with the merged delta. NotPerformed never carries observed.
    let observed = match outcome {
        MutationOutcome::NotPerformed => {
            let outcome =
                finalize_not_performed_in_tx(&mut tx, event_repo, &permit, &row, now).await?;
            tx.commit()
                .await
                .map_err(|e| VoomError::Database(format!("finalize: commit NotPerformed: {e}")))?;
            return Ok(FinalizeOutcome::CancelledAfterAuthorize(outcome));
        }
        MutationOutcome::Applied { observed } => observed,
    };

    // Applied accept point. The caller has performed the durable
    // filesystem mutation; from here on, EVERY post-mutation failure
    // path must transition the row to `recovery_required` rather than
    // propagate Err and leave the row stuck in `'authorized'`.
    //
    // Round-8 finding #1: the recovery boundary now wraps the entire
    // post-Applied block (snapshot decode + trip-wire recompute +
    // either silent path or trip-wire branch). Round-7 wrapped only
    // the silent dispatch + completion + event append; the trip-wire
    // recompute itself ran through `?` and could propagate
    // `VoomError::Internal` from a Phase C closure-walker abort,
    // bypassing recovery entirely. The single outer savepoint
    // subsumes the round-7 inner savepoint; on inner Err the
    // savepoint rolls back to pre-Applied-accept state and the outer
    // tx writes the `mutation_failed` recovery transition.
    finalize_applied_with_recovery_boundary(
        tx,
        identity_repo,
        event_repo,
        alias_resolver,
        permit,
        row,
        observed,
        now,
    )
    .await
}

/// Round-8 finding #1: the recovery boundary covering every
/// post-Applied-accept failure path. Wraps the snapshot decode,
/// trip-wire recompute, silent-path dispatch + completion + event
/// append, and trip-wire UPDATE + events inside a single savepoint.
/// On Ok, releases the savepoint and commits the outer tx. On Err,
/// rolls the savepoint back to pre-Applied-accept state and routes
/// through `finalize_mutation_failed_in_tx` on the outer tx so the
/// caller observes `FinalizeOutcome::Blocked(BlockedByMutationFailed)`
/// regardless of which sub-step failed.
#[expect(
    clippy::too_many_arguments,
    reason = "Phase C recovery boundary needs the full execution context; splitting would scatter the savepoint contract across multiple helpers"
)]
async fn finalize_applied_with_recovery_boundary(
    mut tx: Transaction<'_, Sqlite>,
    identity_repo: &dyn IdentityRepo,
    event_repo: &dyn EventRepo,
    alias_resolver: &dyn AliasResolver,
    permit: CommitPermit,
    row: AuthorizedIntentRow,
    observed: Option<AffectedScopeClosure>,
    now: OffsetDateTime,
) -> Result<FinalizeOutcome, VoomError> {
    let result = {
        let mut sp = tx.begin().await.map_err(|e| {
            VoomError::Database(format!("finalize: applied recovery savepoint begin: {e}"))
        })?;
        let inner = finalize_applied_inner(
            &mut sp,
            identity_repo,
            event_repo,
            alias_resolver,
            &permit,
            &row,
            observed.as_ref(),
            now,
        )
        .await;
        match inner {
            Ok(outcome) => {
                sp.commit().await.map_err(|e| {
                    VoomError::Database(format!(
                        "finalize: applied recovery savepoint release: {e}"
                    ))
                })?;
                Ok(outcome)
            }
            Err(e) => {
                // Drop the savepoint so the outer tx is restored to the
                // pre-Applied-accept state. `sqlx` rolls back the
                // savepoint on Drop of an unconsumed `Transaction`
                // (savepoint) handle.
                drop(sp);
                Err(e)
            }
        }
    };

    match result {
        Ok(outcome) => {
            tx.commit()
                .await
                .map_err(|e| VoomError::Database(format!("finalize: commit applied: {e}")))?;
            Ok(outcome)
        }
        Err(inner) => {
            // closure_final is intentionally empty: any sub-step may
            // have failed, so we cannot trust a partially-built
            // closure. The mutation-failure path is orthogonal to the
            // four §9.3.2 trip-wires; the post-mutation event's
            // delta / lease / drift arrays are empty by contract.
            let outcome = finalize_mutation_failed_in_tx(
                &mut tx,
                event_repo,
                &permit,
                &row,
                AffectedScopeClosure::default(),
                inner,
                now,
            )
            .await?;
            tx.commit().await.map_err(|e| {
                VoomError::Database(format!("finalize: commit mutation_failed recovery: {e}"))
            })?;
            Ok(FinalizeOutcome::Blocked(outcome))
        }
    }
}

/// Body of the recovery-boundary helper. Runs the snapshot decode,
/// trip-wire recompute, and either the silent-path success branch or
/// the trip-wire branch inside the caller-supplied savepoint. Every
/// `?` exit returns Err to the savepoint owner so the savepoint can
/// roll back and the outer tx can route through the
/// `mutation_failed` recovery transition.
#[expect(
    clippy::too_many_arguments,
    reason = "Phase C recovery boundary needs the full execution context; splitting would scatter the savepoint contract across multiple helpers"
)]
async fn finalize_applied_inner(
    sp: &mut Transaction<'_, Sqlite>,
    identity_repo: &dyn IdentityRepo,
    event_repo: &dyn EventRepo,
    alias_resolver: &dyn AliasResolver,
    permit: &CommitPermit,
    row: &AuthorizedIntentRow,
    observed: Option<&AffectedScopeClosure>,
    now: OffsetDateTime,
) -> Result<FinalizeOutcome, VoomError> {
    // Decode the durable per-member epoch snapshot Phase B wrote.
    // A decode failure here is one of the round-8 failure modes the
    // recovery boundary closes: previously `?` propagated out of
    // finalize without a recovery transition.
    let snapshot = decode_target_row_epochs(&row.target_row_epochs_json)?;
    // Run the defensive trip-wires. Round-8 finding #1: a Phase C
    // closure-walker abort (translated to `VoomError::Internal`) used
    // to propagate via `?` here, leaving the row stuck in
    // `'authorized'`. Now any Err inside this call rolls the
    // savepoint back and routes through `finalize_mutation_failed_in_tx`.
    let trip_wire = run_phase_c_trip_wires_in_tx(
        sp,
        identity_repo,
        alias_resolver,
        row,
        permit.closure_authorized(),
        &snapshot,
        observed,
    )
    .await?;

    match trip_wire {
        PhaseCRecheck::Pass { closure_final } => {
            let outcome = finalize_silent_path_in_tx(
                sp,
                identity_repo,
                event_repo,
                permit,
                row,
                &snapshot,
                closure_final,
                now,
            )
            .await?;
            Ok(FinalizeOutcome::Completed(outcome))
        }
        PhaseCRecheck::Trip(trip) => {
            let outcome = finalize_trip_wire_in_tx(sp, event_repo, permit, row, *trip, now).await?;
            Ok(FinalizeOutcome::Blocked(outcome))
        }
    }
}

/// Outcome of `abort_destructive_commit`. Carries the now-aborted
/// `commit_id` and the post-update `epoch` for callers that want to
/// confirm the durable transition. The function never returns
/// "no-op" — a row that cannot be aborted surfaces as
/// `VoomError::Conflict`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AbortOutcome {
    Aborted { commit_id: CommitId, epoch: u64 },
}

/// Caller-initiated abort of a `pending` `commit_intents` row — sub-slice
/// 8 of the M3 Phase 2 plan. The only sanctioned entry point for the
/// "operator changed their mind between `prepare` and `authorize`"
/// transition. One IMMEDIATE tx:
/// 1. Read the `commit_intents` row by `commit_id`. Require
///    `state = 'pending'`. Missing, `authorized`, or any terminal state
///    surfaces as `VoomError::Conflict` — `authorized` rows are not
///    abortable through this entry; the only sanctioned post-authorize
///    termination is `finalize_destructive_commit(_,
///    MutationOutcome::NotPerformed, _)` (recovery contract).
/// 2. UPDATE the row to `state = 'aborted'`, set `aborted_at = now`,
///    write `reason`'s `snake_case` tag into `abort_reason`, bump the
///    `epoch`.
/// 3. Emit `commit.aborted_pre_mutation` with `prior_state = 'pending'`
///    and the reason tag. (The event kind is shared with the
///    `NotPerformed` branch of `finalize_destructive_commit`, which
///    emits with `prior_state = 'authorized'` — commit 7.)
///
/// `reason` must be one of the pre-mutation `AbortReason` variants:
/// `OperatorCancel`, `MutationFailed`, or `Other(_)`. Gate-driven
/// variants (`ClosureGrew`, `FreshLease`, `ClosureIncomplete`,
/// `StaleEvidence`) route through their dedicated `commit.aborted_by_*`
/// event kinds inside the gate itself; `StaleTargetEpoch` is Phase-C
/// only and writes to `recovery_reason`, not `abort_reason`. Passing
/// any of these returns `VoomError::Config` without touching the row.
///
/// # Errors
///
/// - `VoomError::Config` if `reason` is not a sanctioned caller-supplied
///   variant for this entry point.
/// - `VoomError::Conflict` if the row does not exist or is in a state
///   other than `pending` (including `authorized` — recovery contract).
/// - `VoomError::Database` / `VoomError::Internal` on storage failures.
pub async fn abort_destructive_commit(
    pool: &SqlitePool,
    event_repo: &dyn EventRepo,
    commit_id: CommitId,
    reason: AbortReason,
    now: OffsetDateTime,
) -> Result<AbortOutcome, VoomError> {
    let reason_str = caller_abort_reason_str(&reason)?;

    let mut tx = begin_gate_tx(pool).await?;

    let row = read_pending_intent_in_tx(&mut tx, commit_id).await?;

    let aborted_iso = iso8601(now)?;
    let new_epoch = row.epoch + 1;
    let res = sqlx::query(
        "UPDATE commit_intents SET state = 'aborted', aborted_at = ?, \
            abort_reason = ?, epoch = ? \
         WHERE id = ? AND state = 'pending' AND epoch = ?",
    )
    .bind(&aborted_iso)
    .bind(reason_str)
    .bind(i64_from_u64(new_epoch))
    .bind(i64_from_u64(commit_id.0))
    .bind(i64_from_u64(row.epoch))
    .execute(&mut *tx)
    .await
    .map_err(|e| VoomError::Database(format!("abort: UPDATE: {e}")))?;
    if res.rows_affected() != 1 {
        return Err(VoomError::Conflict(format!(
            "abort: UPDATE on {commit_id} affected {} rows; concurrent state mutation",
            res.rows_affected()
        )));
    }

    emit_aborted_pre_mutation_event(event_repo, &mut tx, commit_id, "pending", reason_str, now)
        .await?;

    tx.commit()
        .await
        .map_err(|e| VoomError::Database(format!("abort: commit: {e}")))?;

    Ok(AbortOutcome::Aborted {
        commit_id,
        epoch: new_epoch,
    })
}

/// Validate that `reason` is a sanctioned caller-supplied
/// `AbortReason` for the pending-only abort entry point and return its
/// `snake_case` tag for the durable `abort_reason` column and the
/// `commit.aborted_pre_mutation` event payload. Gate-driven and
/// post-mutation variants are rejected with `VoomError::Config`
/// before any tx opens.
fn caller_abort_reason_str(reason: &AbortReason) -> Result<&'static str, VoomError> {
    match reason {
        AbortReason::OperatorCancel => Ok("operator_cancel"),
        AbortReason::MutationFailed => Ok("mutation_failed"),
        AbortReason::Other(_) => Ok("other"),
        AbortReason::ClosureGrew
        | AbortReason::FreshLease
        | AbortReason::ClosureIncomplete
        | AbortReason::StaleEvidence
        | AbortReason::StaleTargetEpoch => Err(VoomError::Config(format!(
            "abort: {reason:?} is a gate-driven or post-mutation variant; \
             callers may only pass OperatorCancel, MutationFailed, or Other(_)"
        ))),
    }
}

/// Read-only listing over in-flight `commit_intents` rows — sub-slice 9
/// of the M3 Phase 2 plan. Returns every row in
/// `state IN ('pending', 'authorized')`, ordered by `started_at ASC`
/// with `id ASC` as a tie-breaker for non-unique `started_at`. Terminal
/// states (`completed`, `aborted`, `recovery_required`) are excluded.
///
/// Pass `older_than = Some(cutoff)` to restrict the result to rows
/// whose `started_at` is strictly less than `cutoff` — the entry point
/// triage tooling uses to surface stale in-flight commits. `None`
/// disables the time filter. The query is shaped to ride the
/// `commit_intents_in_flight` partial index defined in migration 0005
/// (`(state, started_at) WHERE state IN ('pending','authorized')`).
///
/// Read-only and stateless: opens no transaction, emits no events,
/// mutates nothing. The closure / target / evidence JSON columns are
/// decoded through the same inverse wire mappers Phase B / Phase C use
/// so on-disk shape stays single-source.
///
/// # Errors
///
/// - `VoomError::Database` on storage failures or unparseable column
///   values written by an earlier phase (the wire decoders surface
///   their own `VoomError::Database` errors verbatim).
pub async fn list_pending_commit_intents(
    pool: &SqlitePool,
    older_than: Option<OffsetDateTime>,
) -> Result<Vec<PendingCommitIntent>, VoomError> {
    let rows = match older_than {
        Some(cutoff) => {
            let cutoff_iso = iso8601(cutoff)?;
            sqlx::query(
                "SELECT id, target, closure_initial, closure_authorized, \
                        accepted_evidence_ids, state, started_at, authorized_at \
                 FROM commit_intents \
                 WHERE state IN ('pending','authorized') AND started_at < ? \
                 ORDER BY started_at ASC, id ASC",
            )
            .bind(&cutoff_iso)
            .fetch_all(pool)
            .await
        }
        None => {
            sqlx::query(
                "SELECT id, target, closure_initial, closure_authorized, \
                        accepted_evidence_ids, state, started_at, authorized_at \
                 FROM commit_intents \
                 WHERE state IN ('pending','authorized') \
                 ORDER BY started_at ASC, id ASC",
            )
            .fetch_all(pool)
            .await
        }
    }
    .map_err(|e| VoomError::Database(format!("list_pending_commit_intents: query: {e}")))?;

    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        out.push(decode_pending_commit_intent_row(&row)?);
    }
    Ok(out)
}

/// Map one `commit_intents` row (limited to `pending` / `authorized` by
/// the caller's `WHERE` clause) into the public `PendingCommitIntent`
/// shape. Decodes JSON columns through the canonical inverse wire
/// mappers and enforces the migration 0005 invariant that
/// `closure_authorized` / `authorized_at` are non-NULL exactly for
/// `state = 'authorized'`.
fn decode_pending_commit_intent_row(
    row: &sqlx::sqlite::SqliteRow,
) -> Result<PendingCommitIntent, VoomError> {
    let id_raw: i64 = row
        .try_get("id")
        .map_err(|e| VoomError::Database(format!("list_pending_commit_intents: read id: {e}")))?;
    let commit_id = CommitId(u64_from_i64(id_raw));
    let state_str: String = row.try_get("state").map_err(|e| {
        VoomError::Database(format!("list_pending_commit_intents: read state: {e}"))
    })?;
    let state = parse_in_flight_state(&state_str, commit_id)?;
    let target_json: String = row.try_get("target").map_err(|e| {
        VoomError::Database(format!("list_pending_commit_intents: read target: {e}"))
    })?;
    let closure_initial_json: String = row.try_get("closure_initial").map_err(|e| {
        VoomError::Database(format!(
            "list_pending_commit_intents: read closure_initial: {e}"
        ))
    })?;
    let closure_authorized_json: Option<String> =
        row.try_get("closure_authorized").map_err(|e| {
            VoomError::Database(format!(
                "list_pending_commit_intents: read closure_authorized: {e}"
            ))
        })?;
    let accepted_evidence_ids_json: String = row.try_get("accepted_evidence_ids").map_err(|e| {
        VoomError::Database(format!(
            "list_pending_commit_intents: read accepted_evidence_ids: {e}"
        ))
    })?;
    let started_at_str: String = row.try_get("started_at").map_err(|e| {
        VoomError::Database(format!("list_pending_commit_intents: read started_at: {e}"))
    })?;
    let authorized_at_str: Option<String> = row.try_get("authorized_at").map_err(|e| {
        VoomError::Database(format!(
            "list_pending_commit_intents: read authorized_at: {e}"
        ))
    })?;

    // Migration 0005 CHECK: closure_authorized IS NOT NULL iff
    // state = 'authorized', and authorized_at moves in lockstep with
    // closure_authorized. Cross-validate so a corrupt row surfaces
    // here rather than as a misleading `None` in the public shape.
    // The two terminal-state variants are excluded by both the SQL
    // `WHERE` clause and `parse_in_flight_state` above; this match is
    // exhaustive against the in-flight subset.
    let (closure_authorized, authorized_at) = match state {
        CommitIntentState::Pending => {
            if closure_authorized_json.is_some() || authorized_at_str.is_some() {
                return Err(VoomError::Internal(format!(
                    "list_pending_commit_intents: commit_intents row {commit_id} is pending but \
                     has closure_authorized or authorized_at set; migration 0005 CHECK should \
                     have prevented this"
                )));
            }
            (None, None)
        }
        CommitIntentState::Authorized => {
            let closure_json = closure_authorized_json.ok_or_else(|| {
                VoomError::Internal(format!(
                    "list_pending_commit_intents: commit_intents row {commit_id} is authorized \
                     but closure_authorized is NULL; migration 0005 CHECK should have prevented this"
                ))
            })?;
            let authorized_at_iso = authorized_at_str.ok_or_else(|| {
                VoomError::Internal(format!(
                    "list_pending_commit_intents: commit_intents row {commit_id} is authorized \
                     but authorized_at is NULL; migration 0005 CHECK should have prevented this"
                ))
            })?;
            (
                Some(decode_closure(&closure_json)?),
                Some(parse_iso8601(&authorized_at_iso)?),
            )
        }
        CommitIntentState::Completed
        | CommitIntentState::Aborted
        | CommitIntentState::RecoveryRequired => {
            // `parse_in_flight_state` only ever returns Pending /
            // Authorized; reaching this arm means that contract was
            // violated upstream. Treat as an invariant violation.
            return Err(VoomError::Internal(format!(
                "list_pending_commit_intents: commit_intents row {commit_id} surfaced terminal \
                 state {state_str:?}; parser should have rejected it"
            )));
        }
    };

    let target = decode_target(&target_json)?;
    let closure_initial = decode_closure(&closure_initial_json)?;
    let accepted_evidence_ids: Vec<EvidenceId> = serde_json::from_str(&accepted_evidence_ids_json)
        .map_err(|e| {
            VoomError::Database(format!(
                "list_pending_commit_intents: decode accepted_evidence_ids: {e}"
            ))
        })?;
    let started_at = parse_iso8601(&started_at_str)?;

    Ok(PendingCommitIntent {
        commit_id,
        target,
        state,
        closure_initial,
        closure_authorized,
        accepted_evidence_ids,
        started_at,
        authorized_at,
    })
}

/// Parse a `commit_intents.state` string into `CommitIntentState`,
/// limited to the two in-flight values the listing query selects for.
/// Terminal states surface as `VoomError::Internal` because reaching
/// this parser with one would mean the SQL `WHERE` clause was
/// bypassed; an unknown string is `VoomError::Database` because that's
/// the on-disk corruption case (a CHECK constraint violation that
/// somehow landed).
fn parse_in_flight_state(s: &str, commit_id: CommitId) -> Result<CommitIntentState, VoomError> {
    match s {
        "pending" => Ok(CommitIntentState::Pending),
        "authorized" => Ok(CommitIntentState::Authorized),
        "completed" | "aborted" | "recovery_required" => Err(VoomError::Internal(format!(
            "list_pending_commit_intents: commit_intents row {commit_id} surfaced terminal \
             state {s:?}; WHERE clause should have excluded it"
        ))),
        other => Err(VoomError::Database(format!(
            "list_pending_commit_intents: commit_intents row {commit_id} has unknown state {other:?}"
        ))),
    }
}

/// Snapshot of the durable `commit_intents` row body Phase C carries
/// across in-tx steps. Loaded once at the head of the finalize tx so
/// every branch binds the same column values.
struct AuthorizedIntentRow {
    commit_id: CommitId,
    target: CommitTarget,
    closure_initial: AffectedScopeClosure,
    closure_authorized: AffectedScopeClosure,
    target_row_epochs_json: String,
    epoch: u64,
}

/// Read the `commit_intents` row for `commit_id` under the Phase C
/// preconditions: `state = 'authorized'` AND `epoch == expected_epoch`.
/// Either precondition failing returns `Conflict` without writing.
async fn read_authorized_intent_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    commit_id: CommitId,
    expected_epoch: u64,
) -> Result<AuthorizedIntentRow, VoomError> {
    let row = sqlx::query(
        "SELECT state, target, closure_initial, closure_authorized, target_row_epochs, epoch \
         FROM commit_intents WHERE id = ?",
    )
    .bind(i64_from_u64(commit_id.0))
    .fetch_optional(&mut **tx)
    .await
    .map_err(|e| VoomError::Database(format!("finalize: read intent: {e}")))?;
    let row = row.ok_or_else(|| {
        VoomError::Conflict(format!(
            "finalize: commit_intents row {commit_id} not found"
        ))
    })?;
    let state: String = row
        .try_get("state")
        .map_err(|e| VoomError::Database(format!("finalize: read state: {e}")))?;
    if state != "authorized" {
        return Err(VoomError::Conflict(format!(
            "finalize: commit_intents row {commit_id} is in state {state:?}, expected 'authorized'"
        )));
    }
    let epoch_raw: i64 = row
        .try_get("epoch")
        .map_err(|e| VoomError::Database(format!("finalize: read epoch: {e}")))?;
    let row_epoch = u64_from_i64(epoch_raw);
    if row_epoch != expected_epoch {
        return Err(VoomError::Conflict(format!(
            "finalize: commit_intents row {commit_id} epoch {row_epoch} != permit epoch {expected_epoch}"
        )));
    }
    let target_json: String = row
        .try_get("target")
        .map_err(|e| VoomError::Database(format!("finalize: read target: {e}")))?;
    let closure_initial_json: String = row
        .try_get("closure_initial")
        .map_err(|e| VoomError::Database(format!("finalize: read closure_initial: {e}")))?;
    let closure_authorized_json: Option<String> = row
        .try_get("closure_authorized")
        .map_err(|e| VoomError::Database(format!("finalize: read closure_authorized: {e}")))?;
    let target_row_epochs_json: Option<String> = row
        .try_get("target_row_epochs")
        .map_err(|e| VoomError::Database(format!("finalize: read target_row_epochs: {e}")))?;
    let closure_authorized_json = closure_authorized_json.ok_or_else(|| {
        // Migration 0005's CHECK requires closure_authorized IS NOT NULL
        // for state='authorized'. Reaching this branch means the schema
        // CHECK has been bypassed — that's an invariant violation, not
        // user-recoverable.
        VoomError::Internal(format!(
            "finalize: commit_intents row {commit_id} is authorized but closure_authorized is NULL; \
             migration 0005 CHECK should have prevented this"
        ))
    })?;
    let target_row_epochs_json = target_row_epochs_json.ok_or_else(|| {
        VoomError::Internal(format!(
            "finalize: commit_intents row {commit_id} is authorized but target_row_epochs is NULL; \
             migration 0005 CHECK should have prevented this"
        ))
    })?;
    let target = decode_target(&target_json)?;
    let closure_initial = decode_closure(&closure_initial_json)?;
    let closure_authorized = decode_closure(&closure_authorized_json)?;
    Ok(AuthorizedIntentRow {
        commit_id,
        target,
        closure_initial,
        closure_authorized,
        target_row_epochs_json,
        epoch: row_epoch,
    })
}

/// Decode the `commit_intents.target_row_epochs` JSON column written by
/// Phase B. Each triple identifies one member of `closure_authorized`
/// and the per-row `epoch` snapshotted at the moment Phase B committed.
fn decode_target_row_epochs(json: &str) -> Result<Vec<TargetRowEpochTriple>, VoomError> {
    serde_json::from_str(json).map_err(|e| {
        // The column is written exclusively by Phase B and never
        // mutated; an unparseable value is an invariant violation
        // rather than user-recoverable input.
        VoomError::Internal(format!("finalize: decode target_row_epochs: {e}"))
    })
}

/// `MutationOutcome::NotPerformed` branch. Transitions the row to
/// `aborted` with `abort_reason = 'operator_cancel'`, bumps the epoch,
/// and emits `commit.aborted_pre_mutation` (`prior_state='authorized'`).
/// `closure_final` carries the authorized closure unchanged because no
/// FS mutation was applied and the Phase C defensive trip-wire is
/// skipped on this branch (§9.3.2 Phase C step 2).
async fn finalize_not_performed_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    event_repo: &dyn EventRepo,
    permit: &CommitPermit,
    row: &AuthorizedIntentRow,
    now: OffsetDateTime,
) -> Result<CommitGateOutcome, VoomError> {
    let aborted_iso = iso8601(now)?;
    let new_epoch = row.epoch + 1;
    let res = sqlx::query(
        "UPDATE commit_intents SET state = 'aborted', aborted_at = ?, \
            abort_reason = 'operator_cancel', epoch = ? \
         WHERE id = ? AND state = 'authorized' AND epoch = ?",
    )
    .bind(&aborted_iso)
    .bind(i64_from_u64(new_epoch))
    .bind(i64_from_u64(row.commit_id.0))
    .bind(i64_from_u64(row.epoch))
    .execute(&mut **tx)
    .await
    .map_err(|e| VoomError::Database(format!("finalize: NotPerformed UPDATE: {e}")))?;
    if res.rows_affected() != 1 {
        return Err(VoomError::Conflict(format!(
            "finalize: NotPerformed UPDATE on {} affected {} rows; concurrent state mutation",
            row.commit_id,
            res.rows_affected()
        )));
    }
    emit_aborted_pre_mutation_event(
        event_repo,
        tx,
        row.commit_id,
        "authorized",
        "operator_cancel",
        now,
    )
    .await?;
    Ok(CommitGateOutcome {
        commit_id: row.commit_id,
        closure_initial: row.closure_initial.clone(),
        closure_authorized: row.closure_authorized.clone(),
        // §9.3.2 Phase C step 2: NotPerformed carries the authorized
        // closure as `closure_final` because no FS mutation was applied
        // and the trip-wire is skipped.
        closure_final: row.closure_authorized.clone(),
        evaluated_lease_ids: permit.evaluated_lease_ids().to_vec(),
        revalidated_evidence: permit.revalidated_evidence().to_vec(),
        result: CommitGateResult::CancelledAfterAuthorize,
    })
}

/// Phase C defensive trip-wire outcome bundle. `Pass` carries the
/// recomputed `closure_final` for the silent dispatch step; `Trip`
/// carries the four-sub-branch tag, the delta vs. `closure_authorized`,
/// the fresh lease IDs, the drift triples, and the recomputed
/// `closure_final` so the abort path can record it on
/// `CommitGateOutcome`.
enum PhaseCRecheck {
    Pass { closure_final: AffectedScopeClosure },
    Trip(Box<PhaseCTripWire>),
}

struct PhaseCTripWire {
    reason: PhaseCTripWireReason,
    closure_final: AffectedScopeClosure,
    delta: ClosureMemberDelta,
    fresh_lease_ids: Vec<UseLeaseId>,
    /// `None` when the closure-grew / fresh-lease wires fired with no
    /// epoch drift; `Some(_)` when the stale-target-epoch wire fired
    /// (regardless of the other two — spec §9.3.2 Phase C step 3 last
    /// bullet).
    target_epoch_drift: Vec<TargetEpochDrift>,
    /// First fresh blocking lease for the `BlockedByUseLease` return
    /// path (only populated when `reason == FreshLease`).
    first_fresh_lease: Option<(UseLeaseId, LeaseScope)>,
}

/// Which of the four Phase C trip-wire sub-branches fired. Drives the
/// `recovery_reason` column write, the `commit.aborted_post_mutation`
/// event `reason` field, and the returned `CommitGateResult` variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PhaseCTripWireReason {
    ClosureGrew,
    FreshLease,
    ClosureGrewAndFreshLease,
    StaleTargetEpoch,
}

impl PhaseCTripWireReason {
    fn as_str(self) -> &'static str {
        match self {
            Self::ClosureGrew => "closure_grew",
            Self::FreshLease => "fresh_lease",
            Self::ClosureGrewAndFreshLease => "closure_grew_and_fresh_lease",
            Self::StaleTargetEpoch => "stale_target_epoch",
        }
    }
}

/// Run the Phase C defensive trip-wires inside the open tx. The closure
/// walker re-uses `GatePhase::Authorize` semantics (a retired target
/// falls out of the closure; the recompute surfaces it as drift rather
/// than a closure-incomplete abort — the Phase-A trip-wire on
/// `retired_at.is_some()` is gated behind `GatePhase::Prepare`).
///
/// Ordering of the four sub-branches:
/// 1. Compare every member's current `epoch` to the snapshot.
///    Any drift wins — `stale_target_epoch` is the dominant signal
///    because the durable mutation has already happened on the FS but
///    the snapshotted target row has been mutated underneath us, so
///    the silent dispatch would either fail the epoch guard inside
///    the `IdentityRepo` mutation (best case) or silently apply the
///    update to a row the operator did not authorize against (worst
///    case). The trip-wire fires regardless of whether the other
///    two wires also fired.
/// 2. Compute the closure delta vs. `closure_authorized`. Non-empty →
///    closure grew/shifted.
/// 3. Re-evaluate the blocking-lease query against `closure_final`.
///    Match → fresh blocking lease.
/// 4. The combined-trip-wire branch fires only when (2) AND (3) both
///    fire AND (1) did not. `ClosureGrew` is the dominant signal
///    inside the combined case; the gate returns
///    `BlockedByClosureGrew` (spec §9.3.2 step 3 third bullet).
async fn run_phase_c_trip_wires_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    identity_repo: &dyn IdentityRepo,
    alias_resolver: &dyn AliasResolver,
    row: &AuthorizedIntentRow,
    closure_authorized: &AffectedScopeClosure,
    snapshot: &[TargetRowEpochTriple],
    observed: Option<&AffectedScopeClosure>,
) -> Result<PhaseCRecheck, VoomError> {
    // Step 1: recompute closure. A retired target now appears as
    // closure drift (Phase B walker semantics). The force-path bypass
    // (commit 10) is NOT piped through Phase C — the persisted token
    // was consumed at prepare + authorize, and a Phase C
    // closure-incomplete abort surfaces as the internal-error escape
    // below rather than honoring the bypass a second time. The
    // closure walker therefore receives an empty bypass set.
    let closure_final_walked = match build_closure(
        tx,
        identity_repo,
        alias_resolver,
        &row.target,
        GatePhase::Authorize,
        &BTreeSet::new(),
    )
    .await?
    {
        Ok((c, _)) => c,
        Err(_abort) => {
            // ClosureIncomplete from the alias resolver at Phase C is a
            // resolver-changed-its-mind escape — surface as a stale
            // target-epoch invariant violation rather than abort with a
            // partial closure (Sprint 1 ships no Phase C closure-
            // incomplete branch; the force-path slice does not extend
            // here either, per §4 commit 7). Return an internal error
            // so the caller surfaces it as `VoomError::Internal`.
            return Err(VoomError::Internal(format!(
                "finalize: closure walker reported abort during Phase C recompute on commit {} \
                 — alias resolver should have observed the same closure as Phase B",
                row.commit_id
            )));
        }
    };

    // Round-7 finding #2: merge any caller-observed closure with the
    // recomputed one. Members the caller saw but the resolver/DB
    // didn't enumerate must contribute to the drift signal — otherwise
    // the trip-wire silently drops aliases the caller already touched.
    // The union is the authoritative `closure_final` for the recheck.
    let closure_final = merge_observed_into_closure(&closure_final_walked, observed);

    // Step 2: per-member epoch comparison against the snapshot.
    let target_epoch_drift = per_member_epoch_drift_in_tx(tx, closure_authorized, snapshot).await?;

    // Step 3: closure delta vs. authorized. Computed against the
    // merged closure so caller-observed-only members surface as
    // `added_*` entries on the delta and on the post-mutation event.
    let delta = closure_authorized.id_member_delta(&closure_final);

    // Step 4: blocking-lease re-evaluation. Evaluated against the
    // merged closure so a lease scoped to a caller-observed-only alias
    // still counts as a fresh blocking lease.
    let evaluated_at_finalize = list_blocking_leases_in_tx(tx, &closure_final).await?;
    let first_fresh_lease = first_blocking_overlap_in_tx(tx, &closure_final).await?;
    let fresh_lease_ids = evaluated_at_finalize;

    // Stale target epoch is the dominant signal — fires regardless of
    // whether other wires also fired (spec §9.3.2 step 3 last bullet).
    if !target_epoch_drift.is_empty() {
        return Ok(PhaseCRecheck::Trip(Box::new(PhaseCTripWire {
            reason: PhaseCTripWireReason::StaleTargetEpoch,
            closure_final,
            delta,
            fresh_lease_ids,
            target_epoch_drift,
            first_fresh_lease,
        })));
    }

    let closure_grew = !delta.is_empty();
    let fresh_lease_overlap = first_fresh_lease.is_some();
    match (closure_grew, fresh_lease_overlap) {
        (true, true) => Ok(PhaseCRecheck::Trip(Box::new(PhaseCTripWire {
            reason: PhaseCTripWireReason::ClosureGrewAndFreshLease,
            closure_final,
            delta,
            fresh_lease_ids,
            target_epoch_drift: Vec::new(),
            first_fresh_lease,
        }))),
        (true, false) => Ok(PhaseCRecheck::Trip(Box::new(PhaseCTripWire {
            reason: PhaseCTripWireReason::ClosureGrew,
            closure_final,
            delta,
            fresh_lease_ids,
            target_epoch_drift: Vec::new(),
            first_fresh_lease: None,
        }))),
        (false, true) => Ok(PhaseCRecheck::Trip(Box::new(PhaseCTripWire {
            reason: PhaseCTripWireReason::FreshLease,
            closure_final,
            delta,
            fresh_lease_ids,
            target_epoch_drift: Vec::new(),
            first_fresh_lease,
        }))),
        (false, false) => Ok(PhaseCRecheck::Pass { closure_final }),
    }
}

/// Compare every member of `closure_authorized` against the durable
/// per-member epoch snapshot. Returns the list of drifted rows; an
/// empty result means every member's current `epoch` matches the
/// snapshot value. Matched by `(kind, id)` — the snapshot is the
/// authoritative shape (Phase B wrote one triple per closure member),
/// so a member missing from the snapshot is an invariant violation
/// (member should have been snapshotted at authorize time).
async fn per_member_epoch_drift_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    closure_authorized: &AffectedScopeClosure,
    snapshot: &[TargetRowEpochTriple],
) -> Result<Vec<TargetEpochDrift>, VoomError> {
    use std::collections::HashMap;
    let mut by_kind_id: HashMap<(TargetMemberKind, u64), u64> =
        HashMap::with_capacity(snapshot.len());
    for triple in snapshot {
        by_kind_id.insert((triple.0, triple.1), triple.2);
    }
    let mut drift: Vec<TargetEpochDrift> = Vec::new();
    for id in &closure_authorized.file_assets {
        push_drift_if_mismatch(
            tx,
            "file_assets",
            TargetMemberKind::FileAsset,
            id.0,
            &by_kind_id,
            &mut drift,
        )
        .await?;
    }
    for id in &closure_authorized.file_versions {
        push_drift_if_mismatch(
            tx,
            "file_versions",
            TargetMemberKind::FileVersion,
            id.0,
            &by_kind_id,
            &mut drift,
        )
        .await?;
    }
    for id in &closure_authorized.file_locations {
        push_drift_if_mismatch(
            tx,
            "file_locations",
            TargetMemberKind::FileLocation,
            id.0,
            &by_kind_id,
            &mut drift,
        )
        .await?;
    }
    for id in &closure_authorized.bundles {
        push_drift_if_mismatch(
            tx,
            "asset_bundles",
            TargetMemberKind::Bundle,
            id.0,
            &by_kind_id,
            &mut drift,
        )
        .await?;
    }
    Ok(drift)
}

async fn push_drift_if_mismatch(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    table: &'static str,
    kind: TargetMemberKind,
    id: u64,
    by_kind_id: &std::collections::HashMap<(TargetMemberKind, u64), u64>,
    drift: &mut Vec<TargetEpochDrift>,
) -> Result<(), VoomError> {
    let expected = by_kind_id.get(&(kind, id)).copied().ok_or_else(|| {
        VoomError::Internal(format!(
            "finalize: closure_authorized member ({kind:?}, {id}) absent from target_row_epochs snapshot"
        ))
    })?;
    // `table` is a static internal string — never caller-supplied — so
    // the format!() SQL stitch is safe (sqlx does not expose runtime
    // table-name binding).
    let sql = format!("SELECT epoch FROM {table} WHERE id = ?");
    let observed: Option<i64> = sqlx::query_scalar(&sql)
        .bind(i64_from_u64(id))
        .fetch_optional(&mut **tx)
        .await
        .map_err(|e| VoomError::Database(format!("finalize: epoch probe {table}: {e}")))?;
    // Row gone between authorize and finalize → treat as drift with
    // observed = u64::MAX sentinel. The recovery worker will surface
    // it as a deleted member; in Sprint 1 the gate's audit row carries
    // the snapshot value as `expected` and the sentinel as `observed`.
    let observed = match observed {
        Some(raw) => u64_from_i64(raw),
        None => u64::MAX,
    };
    if observed != expected {
        drift.push(TargetEpochDrift {
            kind,
            id,
            expected,
            observed,
        });
    }
    Ok(())
}

/// Silent-path success branch. Dispatches the durable identity
/// mutation, transitions the row to `completed`, and emits the
/// completed event. Round-8 finding #1: the recovery-boundary
/// savepoint is owned by `finalize_applied_with_recovery_boundary`
/// (the caller of this function); the round-7 inner savepoint has
/// been subsumed because the outer boundary now covers every
/// post-Applied failure path, including the trip-wire recompute that
/// previously ran outside the savepoint.
#[expect(
    clippy::too_many_arguments,
    reason = "Phase C silent path needs the full execution context; splitting would scatter the §9.3.2 step 4-5 invariants under one helper"
)]
async fn finalize_silent_path_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    identity_repo: &dyn IdentityRepo,
    event_repo: &dyn EventRepo,
    permit: &CommitPermit,
    row: &AuthorizedIntentRow,
    snapshot: &[TargetRowEpochTriple],
    closure_final: AffectedScopeClosure,
    now: OffsetDateTime,
) -> Result<CommitGateOutcome, VoomError> {
    dispatch_durable_mutation_in_tx(tx, identity_repo, &row.target, snapshot, now).await?;
    let new_epoch = row.epoch + 1;
    let finalized_iso = iso8601(now)?;
    let res = sqlx::query(
        "UPDATE commit_intents SET state = 'completed', finalized_at = ?, epoch = ? \
         WHERE id = ? AND state = 'authorized' AND epoch = ?",
    )
    .bind(&finalized_iso)
    .bind(i64_from_u64(new_epoch))
    .bind(i64_from_u64(row.commit_id.0))
    .bind(i64_from_u64(row.epoch))
    .execute(&mut **tx)
    .await
    .map_err(|e| VoomError::Database(format!("finalize: completed UPDATE: {e}")))?;
    if res.rows_affected() != 1 {
        return Err(VoomError::Conflict(format!(
            "finalize: completed UPDATE on {} affected {} rows; concurrent state mutation",
            row.commit_id,
            res.rows_affected()
        )));
    }
    emit_completed_event(
        event_repo,
        tx,
        row.commit_id,
        &row.target,
        &closure_final,
        now,
    )
    .await?;
    Ok(CommitGateOutcome {
        commit_id: row.commit_id,
        closure_initial: row.closure_initial.clone(),
        closure_authorized: row.closure_authorized.clone(),
        closure_final,
        evaluated_lease_ids: permit.evaluated_lease_ids().to_vec(),
        revalidated_evidence: permit.revalidated_evidence().to_vec(),
        result: CommitGateResult::Allowed,
    })
}

/// Round-7 finding #1 recovery branch. After the silent-path savepoint
/// rolls back on an inner Err, transition the commit-intent row to
/// `recovery_required` with `recovery_reason = 'mutation_failed'`,
/// emit `commit.aborted_post_mutation` (`reason='mutation_failed'`)
/// plus `commit.recovery_required`, and return a `CommitGateOutcome`
/// carrying `BlockedByMutationFailed { error }`. The caller commits the
/// outer tx. The closure delta / fresh-lease arrays on the post-
/// mutation event are empty because no trip-wire fired; the
/// mutation-failure path is distinct from the four §9.3.2 trip-wires.
async fn finalize_mutation_failed_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    event_repo: &dyn EventRepo,
    permit: &CommitPermit,
    row: &AuthorizedIntentRow,
    closure_final: AffectedScopeClosure,
    inner: VoomError,
    now: OffsetDateTime,
) -> Result<CommitGateOutcome, VoomError> {
    let new_epoch = row.epoch + 1;
    let res = sqlx::query(
        "UPDATE commit_intents SET state = 'recovery_required', recovery_reason = ?, \
            epoch = ? \
         WHERE id = ? AND state = 'authorized' AND epoch = ?",
    )
    .bind("mutation_failed")
    .bind(i64_from_u64(new_epoch))
    .bind(i64_from_u64(row.commit_id.0))
    .bind(i64_from_u64(row.epoch))
    .execute(&mut **tx)
    .await
    .map_err(|e| VoomError::Database(format!("finalize: mutation_failed recovery UPDATE: {e}")))?;
    if res.rows_affected() != 1 {
        return Err(VoomError::Conflict(format!(
            "finalize: mutation_failed recovery UPDATE on {} affected {} rows; concurrent state mutation",
            row.commit_id,
            res.rows_affected()
        )));
    }
    let error_string = format!("{inner:?}");
    emit_mutation_failed_post_mutation_event(event_repo, tx, row.commit_id, now).await?;
    emit_mutation_failed_recovery_required_event(event_repo, tx, row.commit_id, now).await?;
    Ok(CommitGateOutcome {
        commit_id: row.commit_id,
        closure_initial: row.closure_initial.clone(),
        closure_authorized: row.closure_authorized.clone(),
        closure_final,
        evaluated_lease_ids: permit.evaluated_lease_ids().to_vec(),
        revalidated_evidence: permit.revalidated_evidence().to_vec(),
        result: CommitGateResult::BlockedByMutationFailed {
            error: error_string,
        },
    })
}

/// Dispatch the durable identity mutation for the `CommitTarget`,
/// sourcing `expected_epoch` from the snapshot decoded from
/// `commit_intents.target_row_epochs`. `FileLocationProposal` →
/// `NewFileLocation` conversion happens here, in Phase C, by reading
/// the retired row's `file_version_id` inside the tx — the round-6
/// enforcement point. `DeleteFileVersion` / `ArchiveFileVersion`
/// dispatch is deferred per the round-5 fix (§3) and these variants
/// do not exist in Sprint 1's `CommitTarget`.
async fn dispatch_durable_mutation_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    identity_repo: &dyn IdentityRepo,
    target: &CommitTarget,
    snapshot: &[TargetRowEpochTriple],
    now: OffsetDateTime,
) -> Result<(), VoomError> {
    match target {
        CommitTarget::DeleteFileLocation(location_id) => {
            let expected =
                expected_epoch_for(snapshot, TargetMemberKind::FileLocation, location_id.0)?;
            identity_repo
                .retire_file_location_in_tx(tx, *location_id, now, expected)
                .await?;
            Ok(())
        }
        CommitTarget::ReplaceFileLocation { retired, new }
        | CommitTarget::MoveFileLocation { retired, new } => {
            let expected = expected_epoch_for(snapshot, TargetMemberKind::FileLocation, retired.0)?;
            // Round-6 enforcement: read the retired row inside the tx
            // to pair `FileLocationProposal` with `file_version_id`.
            // The proposal type carries no version field by
            // construction; this is the single sanctioned conversion
            // site and the inner-ring cross-version invariant inside
            // `replace_file_location_in_tx` is the matching defense.
            let retired_row = identity_repo
                .get_file_location_in_tx(tx, *retired)
                .await?
                .ok_or_else(|| {
                    VoomError::Conflict(format!(
                        "finalize: retired file_location {retired} not found"
                    ))
                })?;
            let new_location = NewFileLocation {
                file_version_id: retired_row.file_version_id,
                kind: new.kind,
                value: new.value.clone(),
                proof: new.proof.clone(),
                observed_at: new.observed_at,
            };
            identity_repo
                .replace_file_location_in_tx(tx, *retired, expected, new_location, now)
                .await?;
            Ok(())
        }
    }
}

fn expected_epoch_for(
    snapshot: &[TargetRowEpochTriple],
    kind: TargetMemberKind,
    id: u64,
) -> Result<u64, VoomError> {
    snapshot
        .iter()
        .find(|t| t.0 == kind && t.1 == id)
        .map(|t| t.2)
        .ok_or_else(|| {
            VoomError::Internal(format!(
                "finalize: target ({kind:?}, {id}) absent from target_row_epochs snapshot"
            ))
        })
}

/// Trip-wire branch — transitions the row to `recovery_required` with
/// the matching `recovery_reason` (NOT `abort_reason`; migration 0005
/// enforces this split). Emits one `commit.aborted_post_mutation` event
/// with the unified-schema payload AND one `commit.recovery_required`
/// event so the durable row carries a single recovery signal even when
/// read independently of the post-mutation event log.
async fn finalize_trip_wire_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    event_repo: &dyn EventRepo,
    permit: &CommitPermit,
    row: &AuthorizedIntentRow,
    trip: PhaseCTripWire,
    now: OffsetDateTime,
) -> Result<CommitGateOutcome, VoomError> {
    let reason_str = trip.reason.as_str();
    let new_epoch = row.epoch + 1;
    let res = sqlx::query(
        "UPDATE commit_intents SET state = 'recovery_required', recovery_reason = ?, \
            epoch = ? \
         WHERE id = ? AND state = 'authorized' AND epoch = ?",
    )
    .bind(reason_str)
    .bind(i64_from_u64(new_epoch))
    .bind(i64_from_u64(row.commit_id.0))
    .bind(i64_from_u64(row.epoch))
    .execute(&mut **tx)
    .await
    .map_err(|e| VoomError::Database(format!("finalize: recovery_required UPDATE: {e}")))?;
    if res.rows_affected() != 1 {
        return Err(VoomError::Conflict(format!(
            "finalize: recovery_required UPDATE on {} affected {} rows; concurrent state mutation",
            row.commit_id,
            res.rows_affected()
        )));
    }
    emit_aborted_post_mutation_event(event_repo, tx, row.commit_id, &trip, now).await?;
    emit_recovery_required_event(event_repo, tx, row.commit_id, &trip, now).await?;

    let gate_result = match trip.reason {
        PhaseCTripWireReason::ClosureGrew | PhaseCTripWireReason::ClosureGrewAndFreshLease => {
            CommitGateResult::BlockedByClosureGrew {
                delta: trip.delta.clone(),
            }
        }
        PhaseCTripWireReason::FreshLease => {
            // `first_fresh_lease` is guaranteed populated under the
            // FreshLease branch (the recheck builds the variant
            // alongside `first_blocking_overlap_in_tx`'s return). Fall
            // back to a synthesized internal-error if the invariant is
            // ever broken.
            let (lease_id, lease_scope) = trip.first_fresh_lease.ok_or_else(|| {
                VoomError::Internal(
                    "finalize: FreshLease trip-wire with no first_fresh_lease bound".to_owned(),
                )
            })?;
            CommitGateResult::BlockedByUseLease {
                lease_id,
                lease_scope,
            }
        }
        PhaseCTripWireReason::StaleTargetEpoch => CommitGateResult::BlockedByStaleTargetEpoch {
            drift: trip.target_epoch_drift.clone(),
        },
    };

    Ok(CommitGateOutcome {
        commit_id: row.commit_id,
        closure_initial: row.closure_initial.clone(),
        closure_authorized: row.closure_authorized.clone(),
        closure_final: trip.closure_final,
        evaluated_lease_ids: permit.evaluated_lease_ids().to_vec(),
        revalidated_evidence: permit.revalidated_evidence().to_vec(),
        result: gate_result,
    })
}

fn target_member_kind_str(k: TargetMemberKind) -> &'static str {
    match k {
        TargetMemberKind::FileAsset => "file_asset",
        TargetMemberKind::FileVersion => "file_version",
        TargetMemberKind::FileLocation => "file_location",
        TargetMemberKind::Bundle => "bundle",
    }
}

fn target_epoch_drift_wire(drift: &[TargetEpochDrift]) -> Vec<TargetEpochDriftWire> {
    drift
        .iter()
        .map(|d| TargetEpochDriftWire {
            kind: target_member_kind_str(d.kind).to_owned(),
            id: d.id,
            expected: d.expected,
            observed: d.observed,
        })
        .collect()
}

async fn emit_completed_event(
    event_repo: &dyn EventRepo,
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    commit_id: CommitId,
    target: &CommitTarget,
    closure_final: &AffectedScopeClosure,
    finalized_at: OffsetDateTime,
) -> Result<(), VoomError> {
    let payload = Event::CommitCompleted(CommitCompletedPayload {
        commit_id,
        target_kind: commit_target_kind_str(target).to_owned(),
        closure_asset_count: u32::try_from(closure_final.file_assets.len()).unwrap_or(u32::MAX),
        closure_bundle_count: u32::try_from(closure_final.bundles.len()).unwrap_or(u32::MAX),
        closure_version_count: u32::try_from(closure_final.file_versions.len()).unwrap_or(u32::MAX),
        closure_location_count: u32::try_from(closure_final.file_locations.len())
            .unwrap_or(u32::MAX),
        finalized_at,
    });
    event_repo
        .append_in_tx(
            tx,
            EventEnvelope {
                occurred_at: finalized_at,
                subject_type: SubjectType::CommitIntent,
                subject_id: Some(commit_id.0),
                trace_id: None,
                payload,
            },
        )
        .await?;
    Ok(())
}

async fn emit_aborted_pre_mutation_event(
    event_repo: &dyn EventRepo,
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    commit_id: CommitId,
    prior_state: &str,
    reason: &str,
    aborted_at: OffsetDateTime,
) -> Result<(), VoomError> {
    let payload = Event::CommitAbortedPreMutation(CommitAbortedPreMutationPayload {
        commit_id,
        prior_state: prior_state.to_owned(),
        reason: reason.to_owned(),
        aborted_at,
    });
    event_repo
        .append_in_tx(
            tx,
            EventEnvelope {
                occurred_at: aborted_at,
                subject_type: SubjectType::CommitIntent,
                subject_id: Some(commit_id.0),
                trace_id: None,
                payload,
            },
        )
        .await?;
    Ok(())
}

async fn emit_aborted_post_mutation_event(
    event_repo: &dyn EventRepo,
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    commit_id: CommitId,
    trip: &PhaseCTripWire,
    aborted_at: OffsetDateTime,
) -> Result<(), VoomError> {
    let payload = Event::CommitAbortedPostMutation(CommitAbortedPostMutationPayload {
        commit_id,
        reason: trip.reason.as_str().to_owned(),
        added_asset_count: u32::try_from(trip.delta.added_assets.len()).unwrap_or(u32::MAX),
        added_bundle_count: u32::try_from(trip.delta.added_bundles.len()).unwrap_or(u32::MAX),
        added_version_count: u32::try_from(trip.delta.added_versions.len()).unwrap_or(u32::MAX),
        added_location_count: u32::try_from(trip.delta.added_locations.len()).unwrap_or(u32::MAX),
        removed_asset_count: u32::try_from(trip.delta.removed_assets.len()).unwrap_or(u32::MAX),
        removed_bundle_count: u32::try_from(trip.delta.removed_bundles.len()).unwrap_or(u32::MAX),
        removed_version_count: u32::try_from(trip.delta.removed_versions.len()).unwrap_or(u32::MAX),
        removed_location_count: u32::try_from(trip.delta.removed_locations.len())
            .unwrap_or(u32::MAX),
        fresh_lease_ids: trip.fresh_lease_ids.iter().map(|l| l.0).collect(),
        target_epoch_drift: target_epoch_drift_wire(&trip.target_epoch_drift),
        aborted_at,
    });
    event_repo
        .append_in_tx(
            tx,
            EventEnvelope {
                occurred_at: aborted_at,
                subject_type: SubjectType::CommitIntent,
                subject_id: Some(commit_id.0),
                trace_id: None,
                payload,
            },
        )
        .await?;
    Ok(())
}

async fn emit_recovery_required_event(
    event_repo: &dyn EventRepo,
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    commit_id: CommitId,
    trip: &PhaseCTripWire,
    recorded_at: OffsetDateTime,
) -> Result<(), VoomError> {
    let payload = Event::CommitRecoveryRequired(CommitRecoveryRequiredPayload {
        commit_id,
        recovery_reason: trip.reason.as_str().to_owned(),
        added_asset_count: u32::try_from(trip.delta.added_assets.len()).unwrap_or(u32::MAX),
        added_bundle_count: u32::try_from(trip.delta.added_bundles.len()).unwrap_or(u32::MAX),
        added_version_count: u32::try_from(trip.delta.added_versions.len()).unwrap_or(u32::MAX),
        added_location_count: u32::try_from(trip.delta.added_locations.len()).unwrap_or(u32::MAX),
        removed_asset_count: u32::try_from(trip.delta.removed_assets.len()).unwrap_or(u32::MAX),
        removed_bundle_count: u32::try_from(trip.delta.removed_bundles.len()).unwrap_or(u32::MAX),
        removed_version_count: u32::try_from(trip.delta.removed_versions.len()).unwrap_or(u32::MAX),
        removed_location_count: u32::try_from(trip.delta.removed_locations.len())
            .unwrap_or(u32::MAX),
        fresh_lease_ids: trip.fresh_lease_ids.iter().map(|l| l.0).collect(),
        target_epoch_drift: target_epoch_drift_wire(&trip.target_epoch_drift),
        recorded_at,
    });
    event_repo
        .append_in_tx(
            tx,
            EventEnvelope {
                occurred_at: recorded_at,
                subject_type: SubjectType::CommitIntent,
                subject_id: Some(commit_id.0),
                trace_id: None,
                payload,
            },
        )
        .await?;
    Ok(())
}

/// Round-7 finding #1: emit `commit.aborted_post_mutation` with
/// `reason='mutation_failed'`. The delta / lease / drift arrays are
/// empty — the mutation-failure path is orthogonal to the four §9.3.2
/// trip-wires. Audit consumers route on the `reason` tag.
async fn emit_mutation_failed_post_mutation_event(
    event_repo: &dyn EventRepo,
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    commit_id: CommitId,
    aborted_at: OffsetDateTime,
) -> Result<(), VoomError> {
    let payload = Event::CommitAbortedPostMutation(CommitAbortedPostMutationPayload {
        commit_id,
        reason: "mutation_failed".to_owned(),
        added_asset_count: 0,
        added_bundle_count: 0,
        added_version_count: 0,
        added_location_count: 0,
        removed_asset_count: 0,
        removed_bundle_count: 0,
        removed_version_count: 0,
        removed_location_count: 0,
        fresh_lease_ids: Vec::new(),
        target_epoch_drift: Vec::new(),
        aborted_at,
    });
    event_repo
        .append_in_tx(
            tx,
            EventEnvelope {
                occurred_at: aborted_at,
                subject_type: SubjectType::CommitIntent,
                subject_id: Some(commit_id.0),
                trace_id: None,
                payload,
            },
        )
        .await?;
    Ok(())
}

/// Round-7 finding #1: emit `commit.recovery_required` with
/// `recovery_reason='mutation_failed'` alongside the post-mutation
/// event so recovery tooling can decode the signal from a single row
/// without joining back to the post-mutation event.
async fn emit_mutation_failed_recovery_required_event(
    event_repo: &dyn EventRepo,
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    commit_id: CommitId,
    recorded_at: OffsetDateTime,
) -> Result<(), VoomError> {
    let payload = Event::CommitRecoveryRequired(CommitRecoveryRequiredPayload {
        commit_id,
        recovery_reason: "mutation_failed".to_owned(),
        added_asset_count: 0,
        added_bundle_count: 0,
        added_version_count: 0,
        added_location_count: 0,
        removed_asset_count: 0,
        removed_bundle_count: 0,
        removed_version_count: 0,
        removed_location_count: 0,
        fresh_lease_ids: Vec::new(),
        target_epoch_drift: Vec::new(),
        recorded_at,
    });
    event_repo
        .append_in_tx(
            tx,
            EventEnvelope {
                occurred_at: recorded_at,
                subject_type: SubjectType::CommitIntent,
                subject_id: Some(commit_id.0),
                trace_id: None,
                payload,
            },
        )
        .await?;
    Ok(())
}

/// Round-7 finding #2: merge the caller-observed closure (when
/// `Some(_)`) with the recomputed `closure_final`. Members the caller
/// saw but the resolver / DB-internal listing didn't surface end up in
/// the merged set; the closure-grew trip-wire then sees them as
/// `added_*` entries on the delta. The four ID sets are unioned;
/// `resolution_warnings` is intentionally NOT carried over from
/// `observed` (warnings do not contribute to drift — see
/// `AffectedScopeClosure::id_member_delta` doc). Returns `walked`
/// unchanged when `observed` is `None`.
fn merge_observed_into_closure(
    walked: &AffectedScopeClosure,
    observed: Option<&AffectedScopeClosure>,
) -> AffectedScopeClosure {
    let Some(obs) = observed else {
        return walked.clone();
    };
    let mut merged = walked.clone();
    merged.file_assets.extend(obs.file_assets.iter().copied());
    merged
        .file_versions
        .extend(obs.file_versions.iter().copied());
    merged
        .file_locations
        .extend(obs.file_locations.iter().copied());
    merged.bundles.extend(obs.bundles.iter().copied());
    merged
}

#[cfg(test)]
#[path = "commit_safety_gate_test.rs"]
mod tests;
