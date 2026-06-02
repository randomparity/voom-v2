use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use voom_core::{CommitId, EvidenceId, UseLeaseId};

// --- M3 Phase 2 — commit safety gate (Phase A subset) -----------------------
//
// Sprint 1 §9.3 destructive-commit gate. Phase A emits one of four events
// per `prepare_destructive_commit` call: `commit.intent_recorded` on the
// success path (a `commit_intents` row landed in `state = 'pending'`),
// and one of three abort kinds for the matching Phase A `Blocked*` exits.
// Phases B / C land in later commits with their own dedicated event kinds.

/// `commit.intent_recorded` — Phase A success. The row is in
/// `state = 'pending'` and the gate's closure walk has been persisted
/// alongside the target. Carries the granularity-bucketed member counts
/// so an audit reader can size the closure without re-deserializing the
/// JSON column.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitIntentRecordedPayload {
    pub commit_id: CommitId,
    /// Wire-format tag identifying the `CommitTarget` variant (one of
    /// `"delete_file_location"`, `"replace_file_location"`,
    /// `"move_file_location"`). Carried separately from the durable
    /// `target` JSON column so audit readers can filter without parsing.
    pub target_kind: String,
    pub closure_asset_count: u32,
    pub closure_bundle_count: u32,
    pub closure_version_count: u32,
    pub closure_location_count: u32,
    pub accepted_evidence_count: u32,
    #[serde(with = "time::serde::iso8601")]
    pub started_at: OffsetDateTime,
}

/// `commit.aborted_by_use_lease` — Phase A or Phase B trip-wire: a
/// blocking use-lease overlapped the closure. The phase tag distinguishes
/// the two emission points (Phase A is the two-tx pattern; Phase B
/// commits in-tx — both pin the same payload shape).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitAbortedByUseLeasePayload {
    pub commit_id: CommitId,
    pub lease_id: UseLeaseId,
    /// One of `"asset" | "bundle" | "version" | "location"` —
    /// mirrors `LeaseScope::type_str`.
    pub lease_scope_type: String,
    pub lease_scope_id: u64,
    /// Which gate phase fired this abort. `"prepare"` for Phase A;
    /// `"authorize"` for Phase B (Phase B emission lands in commit 6).
    pub phase: String,
    #[serde(with = "time::serde::iso8601")]
    pub aborted_at: OffsetDateTime,
}

/// `commit.aborted_by_stale_evidence` — Phase A or Phase B trip-wire: at
/// least one accepted-evidence pin no longer matches current state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitAbortedByStaleEvidencePayload {
    pub commit_id: CommitId,
    pub evidence_id: EvidenceId,
    /// One of `"pinned_file_version_retired" | "pinned_hash_differs" |
    /// "pinned_location_retired"` — mirrors the `EvidenceDrift` enum
    /// variants (`snake_case`).
    pub drift_kind: String,
    pub phase: String,
    #[serde(with = "time::serde::iso8601")]
    pub aborted_at: OffsetDateTime,
}

/// `commit.aborted_by_closure_incomplete` — the closure walker could not
/// enumerate every required member (alias-resolver `Unreachable` in
/// Sprint 1). `message` carries the resolver's diagnostic so an operator
/// can act on the failed mount / object store / FS endpoint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitAbortedByClosureIncompletePayload {
    pub commit_id: CommitId,
    pub phase: String,
    pub message: String,
    #[serde(with = "time::serde::iso8601")]
    pub aborted_at: OffsetDateTime,
}

/// `commit.aborted_by_pending_commit` — Phase A trip-wire (round-7).
/// Another in-flight `commit_intents` row (`state IN ('pending',
/// 'authorized')`) already covers a scope in the new commit's
/// `closure_initial`. Carries the offending scope (`scope_type`,
/// `scope_id`) so an operator can route the wait / takeover decision
/// without a race-prone re-query. `pending_commit_id` identifies the
/// existing in-flight row that won the lock; `commit_id` is the newly
/// landed `aborted` row that recorded the abort.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitAbortedByPendingCommitPayload {
    pub commit_id: CommitId,
    /// ID of the in-flight commit that already covers `scope_*`.
    pub pending_commit_id: CommitId,
    /// One of `"asset" | "bundle" | "version" | "location"` — mirrors
    /// `LeaseScope::type_str`.
    pub scope_type: String,
    pub scope_id: u64,
    /// `"prepare"` — only Phase A emits this event (Phase B / C cannot
    /// reach the overlap branch; they operate on a single committed
    /// intent row).
    pub phase: String,
    #[serde(with = "time::serde::iso8601")]
    pub aborted_at: OffsetDateTime,
}

/// `commit.authorized` — Phase B success. The intent transitioned from
/// `pending` to `authorized`; the gate's recomputed `closure_authorized`
/// + per-member epoch snapshot are durably persisted on the row.
///
/// Carries the granularity-bucketed member counts so an audit reader
/// can size the authorized closure without re-deserializing the JSON
/// column.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitAuthorizedPayload {
    pub commit_id: CommitId,
    pub closure_asset_count: u32,
    pub closure_bundle_count: u32,
    pub closure_version_count: u32,
    pub closure_location_count: u32,
    /// Number of `[kind, row_id, epoch]` triples written to the
    /// `commit_intents.target_row_epochs` JSON column.
    pub target_row_epoch_count: u32,
    #[serde(with = "time::serde::iso8601")]
    pub authorized_at: OffsetDateTime,
}

/// `commit.aborted_by_closure_grew` — Phase B trip-wire: the closure
/// walker found a non-empty `ClosureMemberDelta` between Phase A
/// (`closure_initial`) and Phase B (`closure_authorized`). Carries the
/// per-granularity add/remove counts so an audit reader can characterize
/// the drift without re-deserializing the closure JSON columns.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitAbortedByClosureGrewPayload {
    pub commit_id: CommitId,
    pub added_asset_count: u32,
    pub added_bundle_count: u32,
    pub added_version_count: u32,
    pub added_location_count: u32,
    pub removed_asset_count: u32,
    pub removed_bundle_count: u32,
    pub removed_version_count: u32,
    pub removed_location_count: u32,
    pub phase: String,
    #[serde(with = "time::serde::iso8601")]
    pub aborted_at: OffsetDateTime,
}

// --- M3 Phase 2 — commit safety gate (Phase C) ------------------------------
//
// Sprint 1 §9.3.2 Phase C. The four payload shapes below correspond to
// `finalize_destructive_commit`'s exit branches:
//   - `commit.completed` — silent dispatch fired, durable mutation landed.
//   - `commit.aborted_pre_mutation` — `MutationOutcome::NotPerformed`
//     (`prior_state='authorized'`) or `abort_destructive_commit`
//     (`prior_state='pending'`).
//   - `commit.aborted_post_mutation` — Phase C defensive trip-wire
//     (`closure_grew` | `fresh_lease` | `closure_grew_and_fresh_lease` |
//     `stale_target_epoch`).
//   - `commit.recovery_required` — emitted alongside the
//     `aborted_post_mutation` payload to flag the durable row for the
//     Sprint 5+ recovery worker. Mirrors the trip-wire fields so the
//     recovery worker can decode the reason from a single row.

/// One drifted target row from the Phase C `stale_target_epoch`
/// trip-wire. Wire-format mirror of the in-memory `TargetEpochDrift`
/// struct that lives in `voom-store` so the on-disk JSON shape is the
/// single source of truth.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TargetEpochDriftWire {
    /// One of `"file_asset" | "file_version" | "file_location" |
    /// "bundle"` — mirrors `TargetMemberKind`'s `snake_case` serde tag.
    pub kind: String,
    pub id: u64,
    pub expected: u64,
    pub observed: u64,
}

/// `commit.completed` — Phase C success. The durable identity mutation
/// has been applied to the matching `IdentityRepo` in the same tx the
/// `commit_intents` row transitioned to `completed`. Carries the
/// granularity-bucketed member counts of `closure_final` so an audit
/// reader can size the silent-path closure without re-deserializing
/// the JSON column.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitCompletedPayload {
    pub commit_id: CommitId,
    /// Wire-format tag identifying the `CommitTarget` variant the gate
    /// dispatched (one of `"delete_file_location"`,
    /// `"replace_file_location"`, `"move_file_location"`).
    pub target_kind: String,
    pub closure_asset_count: u32,
    pub closure_bundle_count: u32,
    pub closure_version_count: u32,
    pub closure_location_count: u32,
    #[serde(with = "time::serde::iso8601")]
    pub finalized_at: OffsetDateTime,
}

/// `commit.aborted_pre_mutation` — emitted when a `commit_intents` row
/// is durably transitioned to `aborted` BEFORE any filesystem mutation
/// applied. Two emission sites: `abort_destructive_commit`
/// (`prior_state='pending'`) and `finalize_destructive_commit` called
/// with `MutationOutcome::NotPerformed` (`prior_state='authorized'`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitAbortedPreMutationPayload {
    pub commit_id: CommitId,
    /// One of `"pending" | "authorized"` — the durable state the row
    /// was in immediately before this transition. Distinguishes
    /// "operator aborted before authorize" from "operator obtained a
    /// permit and chose not to mutate".
    pub prior_state: String,
    /// `AbortReason` `snake_case` tag — one of `"operator_cancel" |
    /// "mutation_failed" | "other"` in Sprint 1 (the other variants
    /// are reserved for gate-driven aborts that route through their
    /// dedicated event kinds).
    pub reason: String,
    #[serde(with = "time::serde::iso8601")]
    pub aborted_at: OffsetDateTime,
}

/// `commit.aborted_post_mutation` — Phase C defensive trip-wire.
/// Sprint spec §9.3.2 unified schema: carries the closure delta
/// (vs. `closure_authorized`), the fresh-lease IDs, and (when the
/// `stale_target_epoch` trip-wire fires) the drifted target-row
/// triples. Empty arrays for dimensions that did not fire. The
/// `reason` tag names the dominant trip-wire so audit/recovery tools
/// can route without re-deriving from the array shapes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitAbortedPostMutationPayload {
    pub commit_id: CommitId,
    /// One of `"closure_grew" | "fresh_lease" |
    /// "closure_grew_and_fresh_lease" | "stale_target_epoch"`. Single
    /// source of truth for the trip-wire signal; the durable row's
    /// `recovery_reason` column carries the same value.
    pub reason: String,
    pub added_asset_count: u32,
    pub added_bundle_count: u32,
    pub added_version_count: u32,
    pub added_location_count: u32,
    pub removed_asset_count: u32,
    pub removed_bundle_count: u32,
    pub removed_version_count: u32,
    pub removed_location_count: u32,
    /// `UseLeaseId.0` values for every fresh blocking lease the Phase C
    /// recheck saw against `closure_final`. Possibly empty.
    pub fresh_lease_ids: Vec<u64>,
    /// Drifted `(kind, id, expected, observed)` triples from the
    /// `stale_target_epoch` recheck. Possibly empty (only populated
    /// when `reason='stale_target_epoch'`).
    pub target_epoch_drift: Vec<TargetEpochDriftWire>,
    #[serde(with = "time::serde::iso8601")]
    pub aborted_at: OffsetDateTime,
}

/// `commit.forced_override` — emitted by `prepare_destructive_commit`
/// when the caller threads a non-`None` `ForcePathToken` through
/// `DestructiveCommit.override_token`. Recorded once at prepare time,
/// atomically with the `commit.intent_recorded` insert /
/// `commit_intents.override_token` column write. Authorize does not
/// re-emit — the audit signal is single-shot per commit.
///
/// `bypass` is the canonical `snake_case` rendering of every
/// `BypassKind` bit in the token's set (sorted ascending; the on-disk
/// `BTreeSet` ordering carries over directly).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitForcedOverridePayload {
    pub commit_id: CommitId,
    pub actor: String,
    pub reason: String,
    /// `snake_case` tags for every `BypassKind` bit set on the token —
    /// `"closure_incomplete"` in Sprint 1; the array shape leaves room
    /// for future bypass kinds without a payload schema change.
    pub bypass: Vec<String>,
    #[serde(with = "time::serde::iso8601")]
    pub recorded_at: OffsetDateTime,
}

/// `commit.recovery_required` — emitted alongside
/// `commit.aborted_post_mutation` to flag the durable row for the
/// Sprint 5+ recovery worker. Mirrors the trip-wire payload's
/// `reason` / drift fields so the recovery worker can decode the
/// signal from a single row without joining back to the
/// post-mutation event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitRecoveryRequiredPayload {
    pub commit_id: CommitId,
    /// Mirror of `commit_intents.recovery_reason`. Same vocabulary as
    /// `CommitAbortedPostMutationPayload.reason`.
    pub recovery_reason: String,
    pub added_asset_count: u32,
    pub added_bundle_count: u32,
    pub added_version_count: u32,
    pub added_location_count: u32,
    pub removed_asset_count: u32,
    pub removed_bundle_count: u32,
    pub removed_version_count: u32,
    pub removed_location_count: u32,
    pub fresh_lease_ids: Vec<u64>,
    pub target_epoch_drift: Vec<TargetEpochDriftWire>,
    #[serde(with = "time::serde::iso8601")]
    pub recorded_at: OffsetDateTime,
}

#[cfg(test)]
#[path = "commit_test.rs"]
mod tests;
