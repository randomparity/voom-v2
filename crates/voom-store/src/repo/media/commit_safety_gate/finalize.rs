use super::codecs::{TargetRowEpochTriple, decode_closure, decode_target};
use super::prepare::{GatePhase, commit_target_kind_str};
use super::scope::{build_closure, first_blocking_overlap_in_tx, list_blocking_leases_in_tx};
use super::{
    Acquire, AffectedScopeClosure, AliasResolver, BTreeSet, ClosureMemberDelta,
    CommitAbortedPostMutationPayload, CommitAbortedPreMutationPayload, CommitCompletedPayload,
    CommitGateContext, CommitGateOutcome, CommitGateResult, CommitId, CommitPermit,
    CommitRecoveryRequiredPayload, CommitTarget, Event, EventEnvelope, EventRepo, IdentityRepo,
    LeaseScope, MutationOutcome, OffsetDateTime, Row, Sqlite, SubjectType, TargetEpochDrift,
    TargetEpochDriftWire, TargetMemberKind, Transaction, UseLeaseId, VoomError, begin_gate_tx,
    i64_from_u64, iso8601, u64_from_i64,
};
use crate::repo::media::identity::NewFileLocation;

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

/// Phase C of the destructive-commit gate. Re-reads the
/// `commit_intents` row in `state = 'authorized'`, validates the
/// permit, optionally runs the defensive trip-wires against the
/// recomputed closure / leases / per-member epochs, dispatches the
/// durable identity mutation, and transitions the row to `completed`
/// (silent path) / `recovery_required` (trip-wire) / `aborted`
/// (`NotPerformed`). All work runs inside one IMMEDIATE tx; the
/// two-transaction pattern is reserved for Phase A gate-check aborts.
///
/// `alias_resolver` covers **external** (non-DB) alias sources only;
/// DB-internal alias enumeration goes through
/// `IdentityRepo::list_live_file_locations_by_version_in_tx` on the
/// gate's tx handle, preserving the gate snapshot and avoiding nested
/// connection waits.
///
/// The four defensive trip-wire branches, plus the per-member epoch guard:
/// - Closure grew/shifted (no fresh lease, no epoch drift) →
///   `recovery_reason = 'closure_grew'`, return `BlockedByClosureGrew`.
/// - Fresh blocking lease (empty closure delta, no epoch drift) →
///   `recovery_reason = 'fresh_lease'`, return `BlockedByUseLease`.
/// - Closure grew AND fresh lease (no epoch drift) →
///   `recovery_reason = 'closure_grew_and_fresh_lease'`, return
///   `BlockedByClosureGrew` (closure shift is the dominant signal;
///   the fresh-lease check would have been re-evaluated against the
///   wrong baseline).
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
    context: CommitGateContext<'_>,
    permit: CommitPermit,
    outcome: MutationOutcome,
    now: OffsetDateTime,
) -> Result<FinalizeOutcome, VoomError> {
    let CommitGateContext {
        pool,
        identity_repo,
        event_repo,
        alias_resolver,
    } = context;
    let mut tx = begin_gate_tx(pool).await?;

    let row = read_authorized_intent_in_tx(&mut tx, permit.commit_id(), permit.epoch()).await?;

    // Destructure Applied { observed }. The caller's observed-alias
    // set (if any) is merged with the recomputed
    // closure_final inside the trip-wire path so members the caller saw
    // but the resolver/DB did not surface drive `BlockedByClosureGrew`
    // with the merged delta. NotPerformed never carries observed.
    let observed = match outcome {
        MutationOutcome::NotPerformed => {
            let outcome =
                finalize_not_performed_in_tx(&mut tx, event_repo, &permit, &row, now).await?;
            tx.commit()
                .await
                .map_err(|e| VoomError::database_context("finalize: commit NotPerformed", e))?;
            return Ok(FinalizeOutcome::CancelledAfterAuthorize(outcome));
        }
        MutationOutcome::Applied { observed } => observed,
    };

    // Applied accept point. The caller has performed the durable
    // filesystem mutation; from here on, EVERY post-mutation failure
    // path must transition the row to `recovery_required` rather than
    // propagate Err and leave the row stuck in `'authorized'`.
    //
    // The recovery boundary wraps the entire post-Applied block
    // (snapshot decode, trip-wire recompute, and either silent path or
    // trip-wire branch). The single outer savepoint ensures any inner
    // Err rolls back to pre-Applied-accept state and the outer tx
    // writes the `mutation_failed` recovery transition.
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

/// Recovery boundary covering every post-Applied-accept failure path.
/// Wraps the snapshot decode,
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
            VoomError::database_context("finalize: applied recovery savepoint begin", e)
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
                    VoomError::database(format!(
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
                .map_err(|e| VoomError::database_context("finalize: commit applied", e))?;
            Ok(outcome)
        }
        Err(inner) => {
            // closure_final is intentionally empty: any sub-step may
            // have failed, so we cannot trust a partially-built
            // closure. The mutation-failure path is orthogonal to the
            // four defensive trip-wires; the post-mutation event's
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
                VoomError::database_context("finalize: commit mutation_failed recovery", e)
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
    // Decode failures must route through the recovery boundary rather
    // than propagate out of finalize without a recovery transition.
    let snapshot = decode_target_row_epochs(&row.target_row_epochs_json)?;
    // Run the defensive trip-wires. Any Err inside this call rolls
    // the savepoint back and routes through
    // `finalize_mutation_failed_in_tx` instead of leaving the row in
    // `authorized`.
    let trip_wire = run_phase_c_trip_wires_in_tx(
        sp,
        identity_repo,
        alias_resolver,
        row,
        permit.closure_authorized(),
        &snapshot,
        observed,
        now,
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
    .map_err(|e| VoomError::database_context("finalize: read intent", e))?;
    let row = row.ok_or_else(|| {
        VoomError::Conflict(format!(
            "finalize: commit_intents row {commit_id} not found"
        ))
    })?;
    let state: String = row
        .try_get("state")
        .map_err(|e| VoomError::database_context("finalize: read state", e))?;
    if state != "authorized" {
        return Err(VoomError::Conflict(format!(
            "finalize: commit_intents row {commit_id} is in state {state:?}, expected 'authorized'"
        )));
    }
    let epoch_raw: i64 = row
        .try_get("epoch")
        .map_err(|e| VoomError::database_context("finalize: read epoch", e))?;
    let row_epoch = u64_from_i64(epoch_raw);
    if row_epoch != expected_epoch {
        return Err(VoomError::Conflict(format!(
            "finalize: commit_intents row {commit_id} epoch {row_epoch} != permit epoch {expected_epoch}"
        )));
    }
    let target_json: String = row
        .try_get("target")
        .map_err(|e| VoomError::database_context("finalize: read target", e))?;
    let closure_initial_json: String = row
        .try_get("closure_initial")
        .map_err(|e| VoomError::database_context("finalize: read closure_initial", e))?;
    let closure_authorized_json: Option<String> = row
        .try_get("closure_authorized")
        .map_err(|e| VoomError::database_context("finalize: read closure_authorized", e))?;
    let target_row_epochs_json: Option<String> = row
        .try_get("target_row_epochs")
        .map_err(|e| VoomError::database_context("finalize: read target_row_epochs", e))?;
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
/// skipped on this branch.
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
    .map_err(|e| VoomError::database_context("finalize: NotPerformed UPDATE", e))?;
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
        // NotPerformed carries the authorized closure as
        // `closure_final` because no FS mutation was applied and the
        // trip-wire is skipped.
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
    /// (regardless of the other two).
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
///    `BlockedByClosureGrew`.
#[expect(
    clippy::too_many_arguments,
    reason = "Phase C recheck needs the full execution context plus the clock for TTL-aware lease evaluation; splitting would scatter the trip-wire contract"
)]
async fn run_phase_c_trip_wires_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    identity_repo: &dyn IdentityRepo,
    alias_resolver: &dyn AliasResolver,
    row: &AuthorizedIntentRow,
    closure_authorized: &AffectedScopeClosure,
    snapshot: &[TargetRowEpochTriple],
    observed: Option<&AffectedScopeClosure>,
    now: OffsetDateTime,
) -> Result<PhaseCRecheck, VoomError> {
    // Step 1: recompute closure. A retired target now appears as
    // closure drift (Phase B walker semantics). The force-path bypass
    // is NOT piped through Phase C: the persisted token was consumed
    // at prepare + authorize, and a Phase C
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
            // partial closure. The force-path bypass does not extend
            // beyond prepare/authorize. Return an internal error so the
            // caller surfaces it as `VoomError::Internal`.
            return Err(VoomError::Internal(format!(
                "finalize: closure walker reported abort during Phase C recompute on commit {} \
                 — alias resolver should have observed the same closure as Phase B",
                row.commit_id
            )));
        }
    };

    // Merge any caller-observed closure with the recomputed one.
    // Members the caller saw but the resolver/DB didn't enumerate must
    // contribute to the drift signal — otherwise
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
    let evaluated_at_finalize = list_blocking_leases_in_tx(tx, &closure_final, now).await?;
    let first_fresh_lease = first_blocking_overlap_in_tx(tx, &closure_final, now).await?;
    let fresh_lease_ids = evaluated_at_finalize;

    // Stale target epoch is the dominant signal — fires regardless of
    // whether other wires also fired.
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
        .map_err(|e| VoomError::database_context(format!("finalize: epoch probe {table}"), e))?;
    // Row gone between authorize and finalize → treat as drift with
    // observed = u64::MAX sentinel. The recovery worker will surface
    // it as a deleted member; the gate's audit row carries the snapshot
    // value as `expected` and the sentinel as `observed`.
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
/// completed event. The recovery-boundary savepoint is owned by
/// `finalize_applied_with_recovery_boundary` (the caller of this
/// function), so every post-Applied failure path shares the same
/// rollback and `mutation_failed` recovery transition.
#[expect(
    clippy::too_many_arguments,
    reason = "Phase C silent path keeps mutation and completion invariants together"
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
    .map_err(|e| VoomError::database_context("finalize: completed UPDATE", e))?;
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

/// Recovery branch for post-filesystem mutation database failures.
/// After the recovery-boundary savepoint rolls back on an inner Err,
/// transition the commit-intent row to `recovery_required` with
/// `recovery_reason = 'mutation_failed'`, emit the matching audit
/// events, and return a `CommitGateOutcome` carrying
/// `BlockedByMutationFailed { error }`. The caller commits the outer
/// tx. The closure delta / fresh-lease arrays on the post-mutation
/// event are empty because no trip-wire fired; the mutation-failure
/// path is distinct from the four defensive trip-wires.
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
    .map_err(|e| VoomError::database_context("finalize: mutation_failed recovery UPDATE", e))?;
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
/// the retired row's `file_version_id` inside the tx. Version- and
/// bundle-level dispatch paths are absent because their safe cascade
/// semantics are not part of the current `CommitTarget` contract.
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
            // Read the retired row inside the tx to pair
            // `FileLocationProposal` with `file_version_id`. The
            // proposal type carries no version field by
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
    .map_err(|e| VoomError::database_context("finalize: recovery_required UPDATE", e))?;
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

pub(super) async fn emit_aborted_pre_mutation_event(
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

/// Emit `commit.aborted_post_mutation` with `reason='mutation_failed'`.
/// The delta / lease / drift arrays are empty because the mutation-
/// failure path is orthogonal to the four defensive trip-wires. Audit
/// consumers route on the `reason` tag.
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

/// Emit `commit.recovery_required` with
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

/// Merge the caller-observed closure (when `Some(_)`) with the
/// recomputed `closure_final`. Members the caller saw but the resolver
/// / DB-internal listing didn't surface end up in the merged set; the
/// closure-grew trip-wire then sees them as `added_*` entries on the
/// delta. The four ID sets are unioned; `resolution_warnings` is
/// intentionally NOT carried over from `observed` (warnings do not
/// contribute to drift — see `AffectedScopeClosure::id_member_delta`
/// doc). Returns `walked` unchanged when `observed` is `None`.
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
