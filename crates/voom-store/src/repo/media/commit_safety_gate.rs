//! Commit safety gate public surface.
//!
//! Shared types and transaction helpers for the three-phase destructive
//! commit gate. Phase-specific control flow lives in sibling modules.

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

use crate::repo::audit::events::EventRepo;
use crate::repo::common::{i64_from_u64, iso8601, parse_iso8601, u64_from_i64};
use crate::repo::media::identity::{
    FileLocationKind, IdentityEvidenceTarget, IdentityRepo, LocationProof,
};
use crate::repo::media::use_leases::LeaseScope;

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
/// the gate to authorize. The current gate supports destructive
/// `file_locations` operations only: delete, replace, and move.
///
/// `file_versions` and `asset_bundles` mutations are intentionally not
/// represented here. Retiring a version would require atomically
/// retiring every live location under it using snapshotted epochs, and
/// archive-style targets need durable schema columns that do not exist
/// yet. Keeping those targets out of the enum makes unsupported
/// cascades unrepresentable.
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
    /// `abort_reason = OperatorCancel`. This is the only sanctioned
    /// post-authorize termination path that does not perform the
    /// filesystem mutation.
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
/// can fabricate or inspect them. External consumers reach state
/// through the accessor methods. Phase B builds permits in-module via
/// the struct literal; no crate-visible constructor is exposed, because
/// exposing one would re-open the bypass path the module-private fields
/// are there to close.
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

/// Disposition of a commit-safety-gate phase.
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
    /// A post-trip-wire DB mutation (identity dispatch, intent
    /// completion, or event append) failed after the caller had
    /// already performed the durable filesystem mutation.
    /// Phase C wraps that block in a SAVEPOINT; on Err the savepoint
    /// rolls back and the outer tx transitions the intent to
    /// `recovery_required` with `recovery_reason = 'mutation_failed'`.
    /// `error` carries the inner error's diagnostic string so the
    /// caller has enough context for the recovery worker / audit.
    BlockedByMutationFailed { error: String },
}

/// Input to `prepare_destructive_commit`. `override_token` carries the
/// optional force-path bypass token; `None` is the default
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

/// One bit in `ForcePathToken.bypass`. `ClosureIncomplete` covers an
/// offline external alias source that prevents the resolver from
/// enumerating the full closure for a `FileVersion`. An operator with
/// out-of-band knowledge of the affected aliases may use this bit to
/// force the commit anyway. The bypass-validation pass lives with the
/// force-path entry point.
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
/// `ClosureIncomplete` is the only sanctioned bypass kind; any other
/// bit would be rejected with
/// `VoomError::Config("force-path bypass not supported: <name>")`.
///
/// The single-variant `BypassKind` enum makes any unsupported bit
/// unrepresentable today. Keeping this check in the state-changing
/// path makes the force-bypass invariant explicit at the gate boundary.
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
/// physical-world condition the gate's force-path bypass is designed
/// to override; `Database` is our own storage layer
/// failing and surfaces at the gate boundary as
/// `VoomError::Database`, never as a closure-incomplete abort.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AliasResolutionError {
    /// External alias source (filesystem mount, object store, remote
    /// node) cannot enumerate live locations for the supplied
    /// `FileVersion`. Caller surfaces as `BlockedByClosureIncomplete`
    /// during prepare and authorize.
    Unreachable { message: String },
    /// Underlying storage failure during alias resolution — not the
    /// "external mount offline" case; this is "our own DB layer
    /// broke." Surfaces at the gate boundary as `VoomError::Database`.
    Database(String),
}

/// Resolver for **external** (non-database) alias sources — e.g.
/// filesystem mounts that expose hardlinks/bind mounts, object
/// stores that mirror live `FileLocation` rows under a different
/// URL scheme. No production external alias resolver is registered by
/// default; callers that need filesystem- or object-store-aware alias
/// enumeration provide an implementation behind this trait.
///
/// **DB-internal alias enumeration does NOT use this trait.** The
/// gate's closure walker reads live `file_locations` rows directly via
/// `IdentityRepo::list_live_file_locations_by_version_in_tx`,
/// inside the same IMMEDIATE transaction the gate's safety checks
/// run under. Mixing DB-internal enumeration with this trait
/// (which has no transaction parameter) would either observe
/// rows outside the gate's tx snapshot or, on single-connection
/// pools, deadlock waiting for the connection already held by the
/// open tx. The previously shipped `SqliteAliasResolver` was deleted
/// to keep DB-internal enumeration on the gate transaction.
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

/// Shared dependencies for the destructive commit safety gate phases.
///
/// The three phase entry points all need the same store connection,
/// identity repository, event repository, and external alias resolver.
/// Grouping them keeps each public phase API focused on its phase-specific
/// inputs.
#[derive(Clone, Copy)]
pub struct CommitGateContext<'a> {
    /// Pool used to open the gate's IMMEDIATE transactions.
    pub pool: &'a SqlitePool,
    /// Identity repository used for closure walks and Phase C mutations.
    pub identity_repo: &'a dyn IdentityRepo,
    /// Event repository used to append gate audit events.
    pub event_repo: &'a dyn EventRepo,
    /// External alias resolver used during closure walks.
    pub alias_resolver: &'a dyn AliasResolver,
}

impl std::fmt::Debug for CommitGateContext<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CommitGateContext")
            .field("pool", self.pool)
            .field("identity_repo", &"<dyn IdentityRepo>")
            .field("event_repo", &"<dyn EventRepo>")
            .field("alias_resolver", &"<dyn AliasResolver>")
            .finish()
    }
}

// ============================================================================
// Pending-commit lock helper
// ============================================================================

/// Single source of truth for the "any in-flight commit covers this
/// scope?" question. Reads `commit_intents`
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
/// (`VoomError::Conflict(...)` for `SqliteUseLeaseRepo::acquire_in_tx` and
/// the `IdentityRepo::record_discovered_file_in_tx::AliasAttached`
/// branch). `IdentityRepo::reconcile_rename_in_tx` deliberately does
/// NOT consult this helper: rename reconciliation must be allowed to
/// land against an in-flight commit so external moves never deadlock
/// the gate.
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

/// Open a commit-safety-gate transaction with `BEGIN IMMEDIATE` so
/// `SQLite` takes a RESERVED lock at tx start.
/// Every gate entry point — `prepare_destructive_commit`,
/// `authorize_destructive_commit`, `finalize_destructive_commit`,
/// `abort_destructive_commit` — plus the two-tx helper
/// `phase_a_gate_abort_with_event` routes through this function.
///
/// The one-IMMEDIATE-transaction invariant is enforced at the API
/// boundary: with `pool.begin()` (deferred mode), two
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

mod abort_list;
mod authorize;
mod codecs;
mod finalize;
mod prepare;
mod scope;

pub use abort_list::{AbortOutcome, abort_destructive_commit, list_pending_commit_intents};
pub use authorize::{AuthorizeOutcome, authorize_destructive_commit};
pub use finalize::{FinalizeOutcome, finalize_destructive_commit};
pub use prepare::{PrepareOutcome, prepare_destructive_commit};

#[cfg(test)]
#[path = "commit_safety_gate_test.rs"]
mod tests;
